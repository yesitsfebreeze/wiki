use crate::http::{self, EMBED_DIM};
use crate::io as wiki_io;
use crate::store::{self, Purpose};
use anyhow::Result;
use std::path::{Path, PathBuf};

const DEFAULT_THRESHOLD: f32 = 0.35;

pub fn similarity_threshold() -> f32 {
	std::env::var("WIKI_SIMILARITY_THRESHOLD")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(DEFAULT_THRESHOLD)
}

fn vec_path(purpose: &Purpose) -> PathBuf {
	purpose.path.with_extension("vec")
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
	matches!((purpose_mtime, vec_mtime), (Some(p), Some(v)) if v >= p)
}

pub async fn ensure_purpose_embeddings(root: &Path) -> Result<Vec<(Purpose, Vec<f32>)>> {
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
		let embeds = http::embed_batch(&stale_text).await?;
		for (slot_i, embed) in stale_idx.iter().zip(embeds.into_iter()) {
			wiki_io::write_vec_f32(&vec_path(&purposes[*slot_i]), &embed)?;
			new_vecs[*slot_i] = Some(embed);
		}
	}

	let mut out = Vec::with_capacity(purposes.len());
	for (i, p) in purposes.into_iter().enumerate() {
		let v = match new_vecs[i].take() {
			Some(v) => v,
			None => wiki_io::read_vec_f32(&vec_path(&p), Some(EMBED_DIM))?,
		};
		out.push((p, v));
	}
	Ok(out)
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
	let mut dot = 0.0f32;
	let mut na = 0.0f32;
	let mut nb = 0.0f32;
	let n = a.len().min(b.len());
	for i in 0..n {
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
pub async fn classify(root: &Path, texts: &[String]) -> Result<Vec<String>> {
	if texts.is_empty() {
		return Ok(vec![]);
	}
	let purposes = ensure_purpose_embeddings(root).await?;
	if purposes.is_empty() {
		return Ok(texts.iter().map(|_| "general".to_string()).collect());
	}
	let threshold = similarity_threshold();
	let embeds = http::embed_batch(texts).await?;
	let mut out = Vec::with_capacity(embeds.len());
	for emb in &embeds {
		let mut best_tag = "general";
		let mut best_score = -1.0f32;
		for (p, pv) in &purposes {
			let s = cosine(emb, pv);
			if s > best_score {
				best_score = s;
				best_tag = &p.tag;
			}
		}
		out.push(if best_score < threshold {
			"general".to_string()
		} else {
			best_tag.to_string()
		});
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
