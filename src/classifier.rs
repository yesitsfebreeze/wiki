use crate::store::{self, Purpose};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const EMBED_MODEL: &str = "text-embedding-3-small";
const EMBED_DIM: usize = 1536;
const DEFAULT_THRESHOLD: f32 = 0.35;

#[derive(Deserialize)]
struct EmbedResponse {
	data: Vec<EmbedItem>,
}

#[derive(Deserialize)]
struct EmbedItem {
	embedding: Vec<f32>,
}

pub fn similarity_threshold() -> f32 {
	std::env::var("WIKI_SIMILARITY_THRESHOLD")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(DEFAULT_THRESHOLD)
}

fn openai_key() -> Result<String> {
	std::env::var("OPENAI_API_KEY")
		.map_err(|_| anyhow!("OPENAI_API_KEY not set"))
}

pub fn embed_batch(texts: &[String]) -> Result<Vec<Vec<f32>>> {
	if texts.is_empty() {
		return Ok(vec![]);
	}
	let key = openai_key()?;
	let client = reqwest::blocking::Client::new();
	let body = serde_json::json!({
		"model": EMBED_MODEL,
		"input": texts,
	});
	let resp = client
		.post("https://api.openai.com/v1/embeddings")
		.header("Authorization", format!("Bearer {}", key))
		.json(&body)
		.send()?
		.error_for_status()?
		.json::<EmbedResponse>()?;
	Ok(resp.data.into_iter().map(|d| d.embedding).collect())
}

fn vec_path(purpose: &Purpose) -> PathBuf {
	purpose.path.with_extension("vec")
}

fn write_vec(path: &Path, v: &[f32]) -> Result<()> {
	let mut bytes = Vec::with_capacity(v.len() * 4);
	for f in v {
		bytes.extend_from_slice(&f.to_le_bytes());
	}
	std::fs::write(path, bytes)?;
	Ok(())
}

fn read_vec(path: &Path) -> Result<Vec<f32>> {
	let bytes = std::fs::read(path)?;
	if bytes.len() % 4 != 0 {
		return Err(anyhow!("Corrupt vector file"));
	}
	let mut out = Vec::with_capacity(bytes.len() / 4);
	for chunk in bytes.chunks_exact(4) {
		out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
	}
	if out.len() != EMBED_DIM {
		return Err(anyhow!("Unexpected vector dim: {}", out.len()));
	}
	Ok(out)
}

fn vec_is_fresh(purpose: &Purpose) -> bool {
	let vp = vec_path(purpose);
	if !vp.exists() {
		return false;
	}
	let purpose_mtime = std::fs::metadata(&purpose.path)
		.and_then(|m| m.modified())
		.ok();
	let vec_mtime = std::fs::metadata(&vp).and_then(|m| m.modified()).ok();
	match (purpose_mtime, vec_mtime) {
		(Some(p), Some(v)) => v >= p,
		_ => false,
	}
}

pub fn ensure_purpose_embeddings(root: &Path) -> Result<Vec<(Purpose, Vec<f32>)>> {
	let purposes = store::list_purposes(root)?;
	if purposes.is_empty() {
		return Ok(vec![]);
	}

	let mut stale_idx = Vec::new();
	let mut stale_text = Vec::new();
	for (i, p) in purposes.iter().enumerate() {
		if !vec_is_fresh(p) {
			stale_idx.push(i);
			stale_text.push(format!("{}\n\n{}", p.title, p.description));
		}
	}

	let mut new_vecs: Vec<Option<Vec<f32>>> = vec![None; purposes.len()];
	if !stale_idx.is_empty() {
		let embeds = embed_batch(&stale_text)?;
		for (slot_i, embed) in stale_idx.iter().zip(embeds.into_iter()) {
			write_vec(&vec_path(&purposes[*slot_i]), &embed)?;
			new_vecs[*slot_i] = Some(embed);
		}
	}

	let mut out = Vec::with_capacity(purposes.len());
	for (i, p) in purposes.into_iter().enumerate() {
		let v = match new_vecs[i].take() {
			Some(v) => v,
			None => read_vec(&vec_path(&p))?,
		};
		out.push((p, v));
	}
	Ok(out)
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
	let mut dot = 0.0f32;
	let mut na = 0.0f32;
	let mut nb = 0.0f32;
	for i in 0..a.len().min(b.len()) {
		dot += a[i] * b[i];
		na += a[i] * a[i];
		nb += b[i] * b[i];
	}
	if na == 0.0 || nb == 0.0 {
		return 0.0;
	}
	dot / (na.sqrt() * nb.sqrt())
}

/// Returns top-1 purpose tag for each input, or "general" if below threshold.
pub fn classify(root: &Path, texts: &[String]) -> Result<Vec<String>> {
	if texts.is_empty() {
		return Ok(vec![]);
	}
	let purposes = ensure_purpose_embeddings(root)?;
	if purposes.is_empty() {
		return Ok(texts.iter().map(|_| "general".to_string()).collect());
	}
	let threshold = similarity_threshold();
	let embeds = embed_batch(texts)?;
	let mut out = Vec::with_capacity(embeds.len());
	for emb in &embeds {
		let mut best = ("general".to_string(), -1.0f32);
		for (p, pv) in &purposes {
			let s = cosine(emb, pv);
			if s > best.1 {
				best = (p.tag.clone(), s);
			}
		}
		if best.1 < threshold {
			out.push("general".to_string());
		} else {
			out.push(best.0);
		}
	}
	Ok(out)
}

pub fn ensure_general_purpose(root: &Path) -> Result<()> {
	let path = root.join("purposes").join("general.md");
	if path.exists() {
		return Ok(());
	}
	store::create_purpose(
		root,
		"general",
		"General",
		"Catch-all bucket for content that does not match any specific purpose with sufficient confidence.",
	)?;
	Ok(())
}
