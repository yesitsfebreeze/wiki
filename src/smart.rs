use crate::classifier;
use crate::http;
use crate::io as wiki_io;
use crate::search;
use crate::store::{self, Document};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MAX_SNIPPET: usize = 800;
const EMB_BATCH: usize = 64;
const RRF_K: f32 = 60.0;

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

fn snippet(s: &str) -> String {
	if s.len() <= MAX_SNIPPET {
		s.to_string()
	} else {
		// Slice on a char boundary.
		let mut end = MAX_SNIPPET;
		while !s.is_char_boundary(end) && end > 0 {
			end -= 1;
		}
		format!("{}…", &s[..end])
	}
}

pub async fn expand_questions(prompt: &str, n: usize) -> Result<Vec<String>> {
	#[derive(Deserialize)]
	struct Expanded {
		queries: Vec<String>,
	}
	let sys = "You generate retrieval queries to surface relevant context from a personal knowledge base. \
		Given a user prompt, return JSON {\"queries\":[\"...\",\"...\"]} with N short, diverse search queries. \
		Cover: named entities, the underlying intent, prerequisites/related concepts, and likely-adjacent topics. \
		Each query 3-12 words, no prose, no numbering, no quotes inside strings.";
	let user = format!("N={}\nPrompt: {}", n, prompt);
	let content = http::chat_json(sys, &user).await?;
	let parsed: Expanded = serde_json::from_str(&content)
		.map_err(|e| anyhow!("expand parse: {} body: {}", e, content))?;
	Ok(parsed
		.queries
		.into_iter()
		.map(|s| s.trim().to_string())
		.filter(|s| !s.is_empty())
		.take(n)
		.collect())
}

async fn rerank_via_openai(question: &str, cands: &[Candidate<'_>]) -> Result<Vec<RankedItem>> {
	let cands_json = serde_json::to_string(cands)?;
	let sys = "You rerank candidate wiki documents for relevance to a user's question. \
		Return JSON: {\"ranked\":[{\"id\":\"...\",\"score\":0.0-1.0,\"reason\":\"why this matches or not\"}]}. \
		Score 1.0 = direct answer, 0.0 = irrelevant. Include all candidate ids exactly once. \
		Reasons must be one short sentence.";
	let user = format!(
		"Question: {}\n\nCandidates (JSON array, includes BM25 score):\n{}",
		question, cands_json
	);
	let content = http::chat_json(sys, &user).await?;
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
	writeln!(f, "{}", entry)?;
	Ok(())
}

// ── Embedding pool (sidecar vec files keyed by doc id) ──────────────────────

fn emb_dir(root: &Path) -> PathBuf {
	root.join(".search").join("emb")
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

async fn ensure_doc_embeddings(root: &Path) -> Result<Vec<(String, Document, Vec<f32>)>> {
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
		let hash = format!("{:x}", wiki_io::fnv64(&doc.content));
		let vec_p = dir.join(format!("{}.vec", doc.id));
		let hash_p = dir.join(format!("{}.hash", doc.id));
		let fresh = vec_p.exists()
			&& std::fs::read_to_string(&hash_p)
				.map(|s| s.trim() == hash)
				.unwrap_or(false);
		if fresh {
			if let Ok(v) = wiki_io::read_vec_f32(&vec_p, None) {
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
		let embs = http::embed_batch(batch_texts).await?;
		for (offset, emb) in embs.into_iter().enumerate() {
			let i = stale_idx[cursor + offset];
			let doc = &out[i].1;
			let hash = format!("{:x}", wiki_io::fnv64(&doc.content));
			let vec_p = dir.join(format!("{}.vec", doc.id));
			let hash_p = dir.join(format!("{}.hash", doc.id));
			wiki_io::write_vec_f32(&vec_p, &emb)?;
			wiki_io::write_atomic_str(&hash_p, &hash)?;
			out[i].2 = Some(emb);
		}
		cursor = end;
	}

	Ok(out
		.into_iter()
		.filter_map(|(dt, d, e)| e.map(|emb| (dt, d, emb)))
		.collect())
}

pub async fn smart_search(
	root: &Path,
	question: &str,
	tag_filter: Option<&str>,
	k: usize,
	top_n: usize,
) -> Result<serde_json::Value> {
	let pool_size = k.max(top_n * 4).max(20);

	// Run BM25 indexing/search and the embedding pool refresh in parallel.
	let bm25_fut = async {
		let index_path = root.join(".search");
		let index = search::create_index(&index_path)?;
		let hits = search::search_topk(&index, question, tag_filter, pool_size)?;
		Ok::<_, anyhow::Error>(hits)
	};
	let pool_fut = ensure_doc_embeddings(root);
	let q_text = vec![question.to_string()];
	let q_emb_fut = http::embed_batch(&q_text);

	let (bm25_hits, pool, q_emb_vec) = futures::try_join!(bm25_fut, pool_fut, q_emb_fut)?;
	let q_emb = q_emb_vec
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

	let hits: Vec<(Document, f32)> = fused
		.iter()
		.filter_map(|(id, fused_score)| docs_by_id.get(id).map(|d| (d.clone(), *fused_score)))
		.collect();

	let ranked = match rerank_via_openai(question, &cands).await {
		Ok(r) => r,
		Err(e) => hits
			.iter()
			.map(|(d, s)| RankedItem {
				id: d.id.clone(),
				score: *s,
				reason: format!("BM25 fallback ({})", e),
			})
			.collect(),
	};

	let mut indexed: HashMap<&str, &Document> =
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
