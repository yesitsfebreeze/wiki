use crate::classifier;
use crate::search;
use crate::store::{self, Document};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const DEFAULT_RERANK_MODEL: &str = "gpt-4o-mini";
const MAX_SNIPPET: usize = 800;

fn rerank_model() -> String {
	std::env::var("WIKI_RERANK_MODEL").unwrap_or_else(|_| DEFAULT_RERANK_MODEL.to_string())
}

fn openai_key() -> Result<String> {
	std::env::var("OPENAI_API_KEY").map_err(|_| anyhow!("OPENAI_API_KEY not set"))
}

#[derive(Serialize)]
struct Candidate<'a> {
	id: &'a str,
	title: &'a str,
	tags: &'a [String],
	snippet: String,
	bm25: f32,
	cosine: f32,
}

#[derive(Deserialize, Debug)]
struct RankedItem {
	id: String,
	score: f32,
	#[serde(default)]
	reason: String,
}

#[derive(Deserialize, Debug)]
struct RankedResp {
	ranked: Vec<RankedItem>,
}

#[derive(Deserialize)]
struct ChatChoice {
	message: ChatMessage,
}
#[derive(Deserialize)]
struct ChatMessage {
	content: String,
}
#[derive(Deserialize)]
struct ChatResp {
	choices: Vec<ChatChoice>,
}

fn snippet(s: &str) -> String {
	if s.len() <= MAX_SNIPPET {
		s.to_string()
	} else {
		format!("{}…", &s[..MAX_SNIPPET])
	}
}

pub fn chat_json(system: &str, user: &str) -> Result<String> {
	let key = openai_key()?;
	let model = rerank_model();
	let body = serde_json::json!({
		"model": model,
		"messages": [
			{"role": "system", "content": system},
			{"role": "user", "content": user},
		],
		"response_format": {"type": "json_object"},
		"temperature": 0,
	});
	let client = reqwest::blocking::Client::new();
	let resp: ChatResp = client
		.post("https://api.openai.com/v1/chat/completions")
		.header("Authorization", format!("Bearer {}", key))
		.json(&body)
		.send()?
		.error_for_status()?
		.json()?;
	Ok(resp
		.choices
		.into_iter()
		.next()
		.ok_or_else(|| anyhow!("no choices"))?
		.message
		.content)
}

fn rerank_via_openai(question: &str, cands: &[Candidate]) -> Result<Vec<RankedItem>> {
	let cands_json = serde_json::to_string(cands)?;
	let sys = "You rerank candidate wiki documents for relevance to a user's question. \
		Return JSON: {\"ranked\":[{\"id\":\"...\",\"score\":0.0-1.0,\"reason\":\"why this matches or not\"}]}. \
		Score 1.0 = direct answer, 0.0 = irrelevant. Include all candidate ids exactly once. \
		Reasons must be one short sentence.";
	let user = format!(
		"Question: {}\n\nCandidates (JSON array, includes BM25 score):\n{}",
		question, cands_json
	);
	let content = chat_json(sys, &user)?;
	let parsed: RankedResp = serde_json::from_str(&content)
		.map_err(|e| anyhow!("rerank parse: {} body: {}", e, content))?;
	Ok(parsed.ranked)
}

fn append_feedback(root: &Path, entry: &serde_json::Value) -> Result<()> {
	use std::io::Write;
	let path = root.join("feedback.jsonl");
	let mut f = std::fs::OpenOptions::new()
		.create(true)
		.append(true)
		.open(&path)?;
	writeln!(f, "{}", entry.to_string())?;
	Ok(())
}

// ── Embedding pool (sidecar vec files keyed by doc id) ──────────────────────

const EMB_BATCH: usize = 64;
const RRF_K: f32 = 60.0;

fn emb_dir(root: &Path) -> PathBuf {
	root.join(".search").join("emb")
}

fn fnv64(s: &str) -> u64 {
	let mut h: u64 = 0xcbf29ce484222325;
	for b in s.bytes() {
		h ^= b as u64;
		h = h.wrapping_mul(0x100000001b3);
	}
	h
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
		return Err(anyhow!("corrupt vec"));
	}
	let mut out = Vec::with_capacity(bytes.len() / 4);
	for c in bytes.chunks_exact(4) {
		out.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
	}
	Ok(out)
}

fn load_all_docs(root: &Path) -> Vec<(String, Document)> {
	let mut out = Vec::new();
	for dt in &["entities", "thoughts", "conclusions", "reasons", "questions"] {
		if let Ok(docs) = store::list_documents(root, dt) {
			for d in docs {
				out.push(((*dt).to_string(), d));
			}
		}
	}
	out
}

fn ensure_doc_embeddings(root: &Path) -> Result<Vec<(String, Document, Vec<f32>)>> {
	let dir = emb_dir(root);
	std::fs::create_dir_all(&dir)?;
	let docs = load_all_docs(root);

	let mut out: Vec<(String, Document, Option<Vec<f32>>)> = docs
		.into_iter()
		.map(|(dt, d)| (dt, d, None))
		.collect();

	let mut stale_idx: Vec<usize> = Vec::new();
	let mut stale_text: Vec<String> = Vec::new();

	for (i, (_, doc, slot)) in out.iter_mut().enumerate() {
		let hash = format!("{:x}", fnv64(&doc.content));
		let vec_p = dir.join(format!("{}.vec", doc.id));
		let hash_p = dir.join(format!("{}.hash", doc.id));
		let fresh = vec_p.exists()
			&& std::fs::read_to_string(&hash_p)
				.map(|s| s.trim() == hash)
				.unwrap_or(false);
		if fresh {
			if let Ok(v) = read_vec(&vec_p) {
				*slot = Some(v);
				continue;
			}
		}
		stale_idx.push(i);
		stale_text.push(format!("{}\n\n{}", doc.title, doc.content));
	}

	let mut cursor = 0usize;
	while cursor < stale_idx.len() {
		let end = (cursor + EMB_BATCH).min(stale_idx.len());
		let batch_texts = &stale_text[cursor..end];
		let embs = classifier::embed_batch(batch_texts)?;
		for (offset, emb) in embs.into_iter().enumerate() {
			let i = stale_idx[cursor + offset];
			let doc = &out[i].1;
			let hash = format!("{:x}", fnv64(&doc.content));
			let vec_p = dir.join(format!("{}.vec", doc.id));
			let hash_p = dir.join(format!("{}.hash", doc.id));
			write_vec(&vec_p, &emb)?;
			std::fs::write(&hash_p, hash)?;
			out[i].2 = Some(emb);
		}
		cursor = end;
	}

	Ok(out
		.into_iter()
		.filter_map(|(dt, d, e)| e.map(|emb| (dt, d, emb)))
		.collect())
}

pub fn smart_search(
	root: &Path,
	question: &str,
	tag_filter: Option<&str>,
	k: usize,
	top_n: usize,
) -> Result<serde_json::Value> {
	let pool_size = k.max(top_n * 4).max(20);

	// BM25 leg
	let index_path = root.join(".search");
	let index = search::create_index(&index_path)?;
	let bm25_hits: Vec<(Document, f32)> =
		search::search_topk(&index, question, tag_filter, pool_size)?;
	drop(index);

	// Vector leg
	let pool = ensure_doc_embeddings(root)?;
	let q_emb = classifier::embed_batch(&[question.to_string()])?
		.into_iter()
		.next()
		.ok_or_else(|| anyhow!("query embed empty"))?;
	let mut vec_scored: Vec<(String, f32, Document, String)> = pool
		.into_iter()
		.filter(|(_, d, _)| {
			tag_filter
				.map(|t| d.tags.iter().any(|x| x == t))
				.unwrap_or(true)
		})
		.map(|(dt, d, e)| {
			let s = classifier::cosine(&q_emb, &e);
			(d.id.clone(), s, d, dt)
		})
		.collect();
	vec_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
	vec_scored.truncate(pool_size);

	if bm25_hits.is_empty() && vec_scored.is_empty() {
		return Ok(serde_json::json!({
			"question": question,
			"results": [],
			"note": "no matches (BM25 or vector)",
		}));
	}

	// Reciprocal Rank Fusion
	let mut rrf: HashMap<String, f32> = HashMap::new();
	let mut docs_by_id: HashMap<String, Document> = HashMap::new();
	let mut bm25_by_id: HashMap<String, f32> = HashMap::new();
	let mut cos_by_id: HashMap<String, f32> = HashMap::new();
	for (rank, (d, s)) in bm25_hits.iter().enumerate() {
		*rrf.entry(d.id.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
		bm25_by_id.insert(d.id.clone(), *s);
		docs_by_id.entry(d.id.clone()).or_insert_with(|| d.clone());
	}
	for (rank, (id, s, d, _)) in vec_scored.iter().enumerate() {
		*rrf.entry(id.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
		cos_by_id.insert(id.clone(), *s);
		docs_by_id.entry(id.clone()).or_insert_with(|| d.clone());
	}

	let mut fused: Vec<(String, f32)> = rrf.into_iter().collect();
	fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
	fused.truncate(pool_size);

	let cands: Vec<Candidate> = fused
		.iter()
		.filter_map(|(id, _)| docs_by_id.get(id))
		.map(|d| Candidate {
			id: &d.id,
			title: &d.title,
			tags: &d.tags,
			snippet: snippet(&d.content),
			bm25: *bm25_by_id.get(&d.id).unwrap_or(&0.0),
			cosine: *cos_by_id.get(&d.id).unwrap_or(&0.0),
		})
		.collect();

	// keep original `hits` name for downstream feedback log
	let hits: Vec<(Document, f32)> = fused
		.iter()
		.filter_map(|(id, fused_score)| docs_by_id.get(id).map(|d| (d.clone(), *fused_score)))
		.collect();

	let ranked = match rerank_via_openai(question, &cands) {
		Ok(r) => r,
		Err(e) => {
			// fall back to raw BM25 order
			let fallback: Vec<RankedItem> = hits
				.iter()
				.map(|(d, s)| RankedItem {
					id: d.id.clone(),
					score: *s,
					reason: format!("BM25 fallback ({})", e),
				})
				.collect();
			fallback
		}
	};

	let mut indexed: std::collections::HashMap<&str, &Document> =
		hits.iter().map(|(d, _)| (d.id.as_str(), d)).collect();
	let mut sorted = ranked;
	sorted.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
	sorted.truncate(top_n);

	let mut out = Vec::new();
	for r in &sorted {
		if let Some(d) = indexed.remove(r.id.as_str()) {
			out.push(serde_json::json!({
				"id": d.id,
				"title": d.title,
				"tags": d.tags,
				"score": r.score,
				"reason": r.reason,
				"content": d.content,
			}));
		}
	}

	let _ = append_feedback(
		root,
		&serde_json::json!({
			"ts": chrono::Utc::now().to_rfc3339(),
			"question": question,
			"tag_filter": tag_filter,
			"picked": sorted.iter().map(|r| &r.id).collect::<Vec<_>>(),
			"reasons": sorted.iter().map(|r| (&r.id, &r.reason)).collect::<Vec<_>>(),
		}),
	);

	Ok(serde_json::json!({
		"question": question,
		"k_retrieved": hits.len(),
		"results": out,
	}))
}
