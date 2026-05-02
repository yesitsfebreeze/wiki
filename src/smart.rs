use crate::cache;
use crate::classifier;
use crate::http;
use crate::io as wiki_io;
use crate::search;
use crate::store::{self, Document};
use crate::walk::{tag_walk, WalkOpts};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_SNIPPET: usize = 800;
const EMB_BATCH: usize = 64;
const RRF_K: f32 = 60.0;
const MMR_LAMBDA: f32 = 0.72;
const PURPOSE_BIAS: f32 = 0.05;

#[derive(Serialize)]
struct Candidate<'a> {
	id: &'a str,
	title: &'a str,
	doc_type: &'a str,
	tags: &'a [String],
	purpose: Option<&'a str>,
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

/// Pick the most query-relevant paragraph as a snippet — falls back to
/// head of body if no paragraph wins. Char-boundary safe.
fn best_snippet(content: &str, query_terms: &[String]) -> String {
	if query_terms.is_empty() {
		return wiki_io::truncate_chars(content, MAX_SNIPPET);
	}
	let mut best_score = 0usize;
	let mut best_para = "";
	for para in content.split("\n\n") {
		let p = para.trim();
		if p.len() < 40 { continue; }
		let lc = p.to_lowercase();
		let hits: usize = query_terms.iter().map(|t| lc.matches(t.as_str()).count()).sum();
		if hits > best_score {
			best_score = hits;
			best_para = p;
		}
	}
	if best_score == 0 {
		return wiki_io::truncate_chars(content, MAX_SNIPPET);
	}
	wiki_io::truncate_chars(best_para, MAX_SNIPPET)
}

fn lowercase_terms(query: &str) -> Vec<String> {
	query
		.split(|c: char| !c.is_alphanumeric())
		.filter(|t| t.len() >= 3)
		.map(|t| t.to_lowercase())
		.collect()
}

pub async fn expand_questions(prompt: &str, n: usize) -> Result<Vec<String>> {
	if let Some(cached) = cache::expand_get(prompt) {
		return Ok(cached);
	}
	#[derive(Deserialize)]
	struct Expanded { queries: Vec<String> }
	let sys = "You generate retrieval queries to surface relevant context from a personal knowledge base. \
		Given a user prompt, return JSON {\"queries\":[\"...\",\"...\"]} with N short, diverse search queries. \
		Cover: named entities, the underlying intent, prerequisites/related concepts, and likely-adjacent topics. \
		Each query 3-12 words, no prose, no numbering, no quotes inside strings.";
	let user = format!("N={}\nPrompt: {}", n, prompt);
	let content = http::chat_json(sys, &user).await?;
	let parsed: Expanded = serde_json::from_str(&content)
		.map_err(|e| anyhow!("expand parse: {} body: {}", e, content))?;
	let queries: Vec<String> = parsed.queries.into_iter()
		.map(|s| s.trim().to_string())
		.filter(|s| !s.is_empty())
		.take(n)
		.collect();
	cache::expand_set(prompt, queries.clone());
	Ok(queries)
}

/// HyDE: generate a hypothetical 1-2 sentence answer and embed it.
/// Returns the answer embedding, cached for the prompt. Caller can blend
/// with query embedding for richer recall on semantic queries.
pub async fn hyde_embedding(prompt: &str) -> Result<Vec<f32>> {
	if let Some(cached) = cache::hyde_get(prompt) {
		return Ok(cached);
	}
	let sys = "Given a user question, write a 1-2 sentence hypothetical answer that would \
		ideally appear in a knowledge base entry for this question. No preamble, no caveats, \
		just the answer text. Return JSON: {\"answer\":\"...\"}.";
	let content = http::chat_json(sys, &format!("Question: {}", prompt)).await?;
	#[derive(Deserialize)]
	struct A { answer: String }
	let parsed: A = serde_json::from_str(&content)
		.map_err(|e| anyhow!("hyde parse: {} body: {}", e, content))?;
	let texts = vec![parsed.answer];
	let embs = http::embed_batch(&texts).await?;
	let emb = embs.into_iter().next().ok_or_else(|| anyhow!("hyde embed empty"))?;
	cache::hyde_set(prompt, emb.clone());
	Ok(emb)
}

fn hyde_enabled() -> bool {
	std::env::var("WIKI_HYDE").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false)
}

fn blend(a: &[f32], b: &[f32], w_a: f32) -> Vec<f32> {
	let n = a.len().min(b.len());
	let mut out = Vec::with_capacity(n);
	for i in 0..n {
		out.push(a[i] * w_a + b[i] * (1.0 - w_a));
	}
	out
}

async fn rerank_via_openai(question: &str, cands: &[Candidate<'_>]) -> Result<Vec<RankedItem>> {
	let cands_json = serde_json::to_string(cands)?;
	let sys = "You rerank candidate wiki documents for relevance to a user's question. \
		Return JSON: {\"ranked\":[{\"id\":\"...\",\"score\":0.0-1.0,\"reason\":\"why this matches or not\"}]}. \
		Score 1.0 = direct answer, 0.0 = irrelevant. Include all candidate ids exactly once. \
		Reasons must be one short sentence. Use doc_type, tags, and purpose to disambiguate.";
	let user = format!(
		"Question: {}\n\nCandidates (JSON array, includes BM25 score, cosine, doc_type, tags, purpose):\n{}",
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
		.create(true).append(true).open(&path)?;
	writeln!(f, "{}", entry)?;
	Ok(())
}

// ── Embedding pool ──────────────────────────────────────────────────────────

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

/// Refresh in-memory + on-disk embedding cache. On the hot path most calls
/// skip OpenAI entirely (memory hit). Cold path falls back to the on-disk
/// hash sidecars, then OpenAI for actually-stale entries.
async fn refresh_pool(root: &Path) -> Result<Vec<Arc<cache::PoolEntry>>> {
	let dir = emb_dir(root);
	std::fs::create_dir_all(&dir)?;

	let pool_was_seeded = cache::pool_seeded();

	// Hot path (already seeded): only re-check ids in the dirty set, then
	// return the full pool snapshot. Avoids walking 250+ docs per query.
	if pool_was_seeded {
		let dirty: HashSet<String> = cache::drain_dirty_pool().into_iter().collect();
		if !dirty.is_empty() {
			let docs: Vec<(String, Document)> = load_all_docs(root)
				.into_iter()
				.filter(|(_, d)| dirty.contains(&d.id))
				.collect();
			let mut stale_text: Vec<String> = Vec::new();
			let mut stale_meta: Vec<(String, Document, String)> = Vec::new();
			for (dt, doc) in docs.iter() {
				let hash = format!("{:x}", wiki_io::fnv64(&doc.content));
				// Disk sidecar may already be fresh from a peer process.
				let vec_p = dir.join(format!("{}.vec", doc.id));
				let hash_p = dir.join(format!("{}.hash", doc.id));
				let disk_fresh = vec_p.exists()
					&& std::fs::read_to_string(&hash_p)
						.map(|s| s.trim() == hash)
						.unwrap_or(false);
				if disk_fresh {
					if let Ok(v) = wiki_io::read_vec_f32(&vec_p, None) {
						cache::pool_insert(cache::PoolEntry {
							doc_type: dt.clone(),
							doc: doc.clone(),
							content_hash: hash,
							vec: v,
						});
						continue;
					}
				}
				stale_text.push(format!("{}\n\n{}", doc.title, doc.content));
				stale_meta.push((dt.clone(), doc.clone(), hash));
			}
			let mut cursor = 0usize;
			while cursor < stale_meta.len() {
				let end = (cursor + EMB_BATCH).min(stale_meta.len());
				let embs = http::embed_batch(&stale_text[cursor..end]).await?;
				for (offset, emb) in embs.into_iter().enumerate() {
					let (dt, doc, hash) = &stale_meta[cursor + offset];
					let vec_p = dir.join(format!("{}.vec", doc.id));
					let hash_p = dir.join(format!("{}.hash", doc.id));
					wiki_io::write_vec_f32(&vec_p, &emb)?;
					wiki_io::write_atomic_str(&hash_p, hash)?;
					cache::pool_insert(cache::PoolEntry {
						doc_type: dt.clone(),
						doc: doc.clone(),
						content_hash: hash.clone(),
						vec: emb,
					});
				}
				cursor = end;
			}
			// Deleted docs were already evicted by `on_doc_deleted` before
			// being marked dirty — nothing to clean up here.
		}
		return Ok(cache::pool_snapshot());
	}

	// Cold path (first call): full walk to seed the pool.
	let docs = load_all_docs(root);
	let mut stale_idx: Vec<usize> = Vec::new();
	let mut stale_text: Vec<String> = Vec::new();
	let mut entries: Vec<Option<Arc<cache::PoolEntry>>> = Vec::with_capacity(docs.len());

	for (i, (dt, doc)) in docs.iter().enumerate() {
		let hash = format!("{:x}", wiki_io::fnv64(&doc.content));
		// In-memory hit?
		if let Some(existing) = cache::pool_get(&doc.id) {
			if existing.content_hash == hash {
				entries.push(Some(existing));
				continue;
			}
		}
		// Disk sidecar hit?
		let vec_p = dir.join(format!("{}.vec", doc.id));
		let hash_p = dir.join(format!("{}.hash", doc.id));
		let disk_fresh = vec_p.exists()
			&& std::fs::read_to_string(&hash_p)
				.map(|s| s.trim() == hash)
				.unwrap_or(false);
		if disk_fresh {
			if let Ok(v) = wiki_io::read_vec_f32(&vec_p, None) {
				let entry = cache::PoolEntry {
					doc_type: dt.clone(),
					doc: doc.clone(),
					content_hash: hash,
					vec: v,
				};
				cache::pool_insert(cache::PoolEntry {
					doc_type: entry.doc_type.clone(),
					doc: entry.doc.clone(),
					content_hash: entry.content_hash.clone(),
					vec: entry.vec.clone(),
				});
				entries.push(cache::pool_get(&doc.id));
				continue;
			}
		}
		stale_idx.push(i);
		stale_text.push(format!("{}\n\n{}", doc.title, doc.content));
		entries.push(None);
	}

	// Embed all stale in EMB_BATCH-sized chunks.
	let mut cursor = 0usize;
	while cursor < stale_idx.len() {
		let end = (cursor + EMB_BATCH).min(stale_idx.len());
		let embs = http::embed_batch(&stale_text[cursor..end]).await?;
		for (offset, emb) in embs.into_iter().enumerate() {
			let i = stale_idx[cursor + offset];
			let (dt, doc) = &docs[i];
			let hash = format!("{:x}", wiki_io::fnv64(&doc.content));
			let vec_p = dir.join(format!("{}.vec", doc.id));
			let hash_p = dir.join(format!("{}.hash", doc.id));
			wiki_io::write_vec_f32(&vec_p, &emb)?;
			wiki_io::write_atomic_str(&hash_p, &hash)?;
			cache::pool_insert(cache::PoolEntry {
				doc_type: dt.clone(),
				doc: doc.clone(),
				content_hash: hash,
				vec: emb,
			});
			entries[i] = cache::pool_get(&doc.id);
		}
		cursor = end;
	}

	// Drop pool entries whose docs no longer exist (only on first seed and
	// after deletes — rare path).
	if !pool_was_seeded {
		let live_ids: HashSet<&str> = docs.iter().map(|(_, d)| d.id.as_str()).collect();
		for stale in cache::pool_snapshot()
			.into_iter()
			.filter(|e| !live_ids.contains(e.doc.id.as_str()))
		{
			cache::pool_remove(&stale.doc.id);
		}
		cache::mark_pool_seeded();
		// Cold path embedded everything; nothing left to redo.
		let _ = cache::drain_dirty_pool();
	}

	Ok(entries.into_iter().flatten().collect())
}

/// Maximal Marginal Relevance: trade off relevance to query against
/// novelty vs already-selected results. Returns indices into `pool`.
fn mmr_select(
	pool: &[(String, f32, Vec<f32>)],
	query_emb: &[f32],
	top_n: usize,
	lambda: f32,
) -> Vec<usize> {
	let n = pool.len().min(top_n);
	if n == 0 { return vec![]; }
	let mut chosen: Vec<usize> = Vec::with_capacity(n);
	let mut remaining: HashSet<usize> = (0..pool.len()).collect();
	while chosen.len() < n && !remaining.is_empty() {
		let mut best_idx = *remaining.iter().next().unwrap();
		let mut best_score = f32::MIN;
		for &i in &remaining {
			let rel = classifier::cosine(query_emb, &pool[i].2);
			let max_sim = chosen
				.iter()
				.map(|&j| classifier::cosine(&pool[i].2, &pool[j].2))
				.fold(0.0f32, f32::max);
			let score = lambda * rel - (1.0 - lambda) * max_sim;
			if score > best_score {
				best_score = score;
				best_idx = i;
			}
		}
		chosen.push(best_idx);
		remaining.remove(&best_idx);
	}
	chosen
}

/// Tier-2: tag-walk over reason graph. Zero-LLM. Returns Some when it
/// produces >= 3 hits; otherwise lets the BM25/HyDE/MMR fallback run.
fn tag_walk_tier(root: &Path, question: &str, tag_filter: Option<&str>, k: usize) -> Option<serde_json::Value> {
	let opts = WalkOpts { k: k.max(20), ..WalkOpts::default() };
	let hits = tag_walk(root, question, opts).ok()?;
	let hits: Vec<_> = hits.into_iter()
		.filter(|h| tag_filter.map(|t| {
			store::get_document(root, &h.doc_type, &h.doc_id)
				.map(|d| d.tags.iter().any(|x| x == t))
				.unwrap_or(false)
		}).unwrap_or(true))
		.collect();
	if hits.len() < 3 { return None; }
	let results: Vec<serde_json::Value> = hits.iter().map(|h| {
		let doc = store::get_document(root, &h.doc_type, &h.doc_id).ok();
		serde_json::json!({
			"id": h.doc_id,
			"doc_type": h.doc_type,
			"score": h.score,
			"path": h.path,
			"title": doc.as_ref().map(|d| d.title.clone()),
			"tags": doc.as_ref().map(|d| d.tags.clone()),
			"purpose": doc.as_ref().and_then(|d| d.purpose.clone()),
			"content": doc.as_ref().map(|d| d.content.clone()),
		})
	}).collect();
	Some(serde_json::json!({
		"question": question,
		"entry_kind": "tag_walk",
		"results": results,
	}))
}

/// Detect a known-purpose mention in the query — boosts docs of that purpose.
fn detect_purpose_bias(root: &Path, query: &str) -> Option<String> {
	let purposes = store::list_purposes(root).ok()?;
	let qlc = query.to_lowercase();
	for p in &purposes {
		if qlc.contains(&p.tag.to_lowercase()) || qlc.contains(&p.title.to_lowercase()) {
			return Some(p.tag.clone());
		}
	}
	None
}

pub async fn smart_search(
	root: &Path,
	question: &str,
	tag_filter: Option<&str>,
	k: usize,
	top_n: usize,
) -> Result<serde_json::Value> {
	if let Some(tree) = conclusions_first(root, question, tag_filter, k)? {
		return Ok(tree);
	}
	if let Some(walk) = tag_walk_tier(root, question, tag_filter, k) {
		return Ok(walk);
	}
	let q_text = vec![question.to_string()];
	let q_emb_vec = http::embed_batch(&q_text).await?;
	let q_emb = q_emb_vec.into_iter().next().ok_or_else(|| anyhow!("query embed empty"))?;
	let mut v = smart_search_with_qemb(root, question, tag_filter, k, top_n, &q_emb).await?;
	if let Some(obj) = v.as_object_mut() {
		obj.entry("entry_kind").or_insert(serde_json::json!("fallback"));
	}
	Ok(v)
}

/// Score a conclusion against the query: count of lowercase term hits in
/// title (×3) + body. Cheap deterministic scoring — no embeddings needed.
fn score_conclusion(doc: &Document, qterms: &[String]) -> usize {
	if qterms.is_empty() { return 0; }
	let title_lc = doc.title.to_lowercase();
	let body_lc = doc.content.to_lowercase();
	qterms.iter()
		.map(|t| title_lc.matches(t.as_str()).count() * 3 + body_lc.matches(t.as_str()).count())
		.sum()
}

/// Conclusions-first stage: if the query hits any conclusion, return its
/// depth-1 reason fanout (cap 5) as the primary tree. None → caller falls
/// back to the hybrid path.
pub(crate) fn conclusions_first(
	root: &Path,
	query: &str,
	tag_filter: Option<&str>,
	_k: usize,
) -> Result<Option<serde_json::Value>> {
	let qterms = lowercase_terms(query);
	if qterms.is_empty() { return Ok(None); }

	let conclusions = match store::list_documents(root, "conclusions") {
		Ok(v) => v,
		Err(_) => return Ok(None),
	};

	let mut scored: Vec<(usize, Document)> = conclusions
		.into_iter()
		.filter(|d| tag_filter.map(|t| d.tags.iter().any(|x| x == t)).unwrap_or(true))
		.filter_map(|d| {
			let s = score_conclusion(&d, &qterms);
			if s > 0 { Some((s, d)) } else { None }
		})
		.collect();
	if scored.is_empty() { return Ok(None); }
	scored.sort_by(|a, b| b.0.cmp(&a.0));
	scored.truncate(3);

	let mut tree = Vec::with_capacity(scored.len());
	for (_, conc) in &scored {
		let reasons = store::search_reasons_for(root, &conc.id, "from")
			.unwrap_or_default();
		let reasons_json: Vec<serde_json::Value> = reasons.into_iter().take(5)
			.map(|r| {
				let from_id = &conc.id;
				let to_id = extract_to_id(&r);
				let kind = extract_kind(&r);
				let target = to_id.as_deref()
					.and_then(|tid| resolve_doc(root, tid));
				serde_json::json!({
					"reason_id": r.id,
					"from_id": from_id,
					"to_id": to_id,
					"kind": kind,
					"target_doc": target,
				})
			})
			.collect();
		tree.push(serde_json::json!({
			"conclusion": {
				"id": conc.id,
				"title": conc.title,
				"tags": conc.tags,
				"purpose": conc.purpose,
				"content": conc.content,
			},
			"reasons": reasons_json,
		}));
	}

	Ok(Some(serde_json::json!({
		"question": query,
		"entry_kind": "conclusions",
		"tree": tree,
		"results": [],
	})))
}

/// Reason docs encode `to_id`/`kind` in their title `"<from> -[<kind>]-> <to>"`.
/// We don't re-parse the frontmatter — the title is canonical.
fn extract_to_id(reason: &Document) -> Option<String> {
	reason.title.split("]-> ").nth(1).map(|s| s.trim().to_string())
}

fn extract_kind(reason: &Document) -> Option<String> {
	let t = &reason.title;
	let start = t.find("-[")? + 2;
	let end = t[start..].find("]->")? + start;
	Some(t[start..end].to_string())
}

fn resolve_doc(root: &Path, id: &str) -> Option<serde_json::Value> {
	for dt in &["conclusions", "thoughts", "entities", "questions", "reasons"] {
		if let Ok(d) = store::get_document(root, dt, id) {
			return Some(serde_json::json!({
				"id": d.id,
				"doc_type": dt,
				"title": d.title,
				"tags": d.tags,
				"content": d.content,
			}));
		}
	}
	None
}

/// Search variant accepting a pre-computed query embedding. Lets callers
/// (e.g. the prompt-submit hook) batch all sub-query embeds into one
/// OpenAI call before fanning out N parallel searches.
pub async fn smart_search_with_qemb(
	root: &Path,
	question: &str,
	tag_filter: Option<&str>,
	k: usize,
	top_n: usize,
	q_emb: &[f32],
) -> Result<serde_json::Value> {
	if let Some(tree) = conclusions_first(root, question, tag_filter, k)? {
		return Ok(tree);
	}
	if let Some(walk) = tag_walk_tier(root, question, tag_filter, k) {
		return Ok(walk);
	}
	let pool_size = k.max(top_n * 4).max(20);

	let bm25_fut = async {
		let index = cache::search_index(root)?;
		let hits = search::search_topk(&index, question, tag_filter, pool_size)?;
		Ok::<_, anyhow::Error>(hits)
	};
	let pool_fut = refresh_pool(root);

	let hyde_fut = async {
		if hyde_enabled() {
			hyde_embedding(question).await.ok()
		} else {
			None
		}
	};

	let (bm25_hits, pool, hyde_emb) = futures::try_join!(
		bm25_fut,
		pool_fut,
		async { Ok::<_, anyhow::Error>(hyde_fut.await) }
	)?;

	let effective_q_emb: Vec<f32> = match hyde_emb {
		Some(h) => blend(q_emb, &h, 0.6),
		None => q_emb.to_vec(),
	};

	let purpose_bias = detect_purpose_bias(root, question);
	let qterms = lowercase_terms(question);

	let mut vec_scored: Vec<(String, f32, Document, String)> = pool
		.iter()
		.filter(|e| {
			tag_filter
				.map(|t| e.doc.tags.iter().any(|x| x == t))
				.unwrap_or(true)
		})
		.map(|e| {
			let mut s = classifier::cosine(&effective_q_emb, &e.vec);
			if let Some(bp) = &purpose_bias {
				if e.doc.purpose.as_deref() == Some(bp.as_str()) {
					s += PURPOSE_BIAS;
				}
			}
			(e.doc.id.clone(), s, e.doc.clone(), e.doc_type.clone())
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
	let mut dt_by_id: HashMap<String, String> = HashMap::new();
	for (rank, (d, s)) in bm25_hits.iter().enumerate() {
		*rrf.entry(d.id.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
		bm25_by_id.insert(d.id.clone(), *s);
		docs_by_id.entry(d.id.clone()).or_insert_with(|| d.clone());
	}
	for (rank, (id, s, d, dt)) in vec_scored.iter().enumerate() {
		*rrf.entry(id.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
		cos_by_id.insert(id.clone(), *s);
		dt_by_id.insert(id.clone(), dt.clone());
		docs_by_id.entry(id.clone()).or_insert_with(|| d.clone());
	}

	let mut fused: Vec<(String, f32)> = rrf.into_iter().collect();
	fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
	fused.truncate(pool_size);

	// MMR diversity pass — re-pick `pool_size.min(top_n*3)` for the reranker
	// using cached embeddings where available.
	let mut mmr_pool: Vec<(String, f32, Vec<f32>)> = Vec::new();
	for (id, fused_score) in &fused {
		if let Some(entry) = cache::pool_get(id) {
			mmr_pool.push((id.clone(), *fused_score, entry.vec.clone()));
		}
	}
	let mmr_topn = (top_n * 3).max(top_n + 5).min(mmr_pool.len());
	let mmr_idx = mmr_select(&mmr_pool, &effective_q_emb, mmr_topn, MMR_LAMBDA);
	let mut mmr_ids: Vec<String> = mmr_idx.into_iter().map(|i| mmr_pool[i].0.clone()).collect();

	// Append BM25-only hits (no embedding) so they aren't dropped.
	let mut seen: HashSet<String> = mmr_ids.iter().cloned().collect();
	for (id, _) in &fused {
		if !seen.contains(id) {
			mmr_ids.push(id.clone());
			seen.insert(id.clone());
		}
	}
	mmr_ids.truncate(pool_size);

	let cands: Vec<Candidate> = mmr_ids
		.iter()
		.filter_map(|id| docs_by_id.get(id))
		.map(|d| Candidate {
			id: &d.id,
			title: &d.title,
			doc_type: dt_by_id.get(&d.id).map(String::as_str).unwrap_or("?"),
			tags: &d.tags,
			purpose: d.purpose.as_deref(),
			snippet: best_snippet(&d.content, &qterms),
			bm25: *bm25_by_id.get(&d.id).unwrap_or(&0.0),
			cosine: *cos_by_id.get(&d.id).unwrap_or(&0.0),
		})
		.collect();

	let hits: Vec<(Document, f32)> = mmr_ids
		.iter()
		.filter_map(|id| docs_by_id.get(id).map(|d| {
			let s = *cos_by_id.get(id).unwrap_or(&0.0);
			(d.clone(), s)
		}))
		.collect();

	let ranked = match rerank_via_openai(question, &cands).await {
		Ok(r) => r,
		Err(e) => hits.iter().map(|(d, s)| RankedItem {
			id: d.id.clone(),
			score: *s,
			reason: format!("BM25 fallback ({})", e),
		}).collect(),
	};

	let mut indexed: HashMap<&str, &Document> = hits.iter().map(|(d, _)| (d.id.as_str(), d)).collect();
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
				"purpose": d.purpose,
				"score": r.score,
				"reason": r.reason,
				"content": d.content,
			}));
		}
	}

	let _ = append_feedback(root, &serde_json::json!({
		"ts": chrono::Utc::now().to_rfc3339(),
		"question": question,
		"tag_filter": tag_filter,
		"picked": sorted.iter().map(|r| &r.id).collect::<Vec<_>>(),
		"reasons": sorted.iter().map(|r| (&r.id, &r.reason)).collect::<Vec<_>>(),
	}));

	Ok(serde_json::json!({
		"question": question,
		"k_retrieved": hits.len(),
		"hyde": hyde_enabled(),
		"purpose_bias": purpose_bias,
		"results": out,
	}))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::store::{create_document, create_reason, ensure_wiki_layout};
	use tempfile::TempDir;

	#[test]
	fn conclusions_first_returns_none_on_empty() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		ensure_wiki_layout(root).unwrap();
		let r = conclusions_first(root, "anything matters", None, 10).unwrap();
		assert!(r.is_none());
	}

	#[test]
	fn conclusions_first_walks_reasons() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		ensure_wiki_layout(root).unwrap();
		let conc = create_document(
			root, "conclusions", "Quantum entanglement summary",
			"Quantum entanglement is correlated state.",
			vec!["conclusion".into()], None, None,
		).unwrap();
		let t1 = create_document(
			root, "thoughts", "Bell test note", "Bell inequalities body",
			vec!["thought".into()], None, None,
		).unwrap();
		let t2 = create_document(
			root, "thoughts", "EPR paradox", "EPR body",
			vec!["thought".into()], None, None,
		).unwrap();
		create_reason(root, &conc.id, &t1.id, "References", "see bell", None).unwrap();
		create_reason(root, &conc.id, &t2.id, "References", "see epr", None).unwrap();

		let v = conclusions_first(root, "quantum entanglement", None, 10)
			.unwrap()
			.expect("must hit conclusion");
		assert_eq!(v["entry_kind"], "conclusions");
		let tree = v["tree"].as_array().unwrap();
		assert_eq!(tree.len(), 1);
		assert_eq!(tree[0]["conclusion"]["id"], conc.id);
		let reasons = tree[0]["reasons"].as_array().unwrap();
		let target_ids: Vec<String> = reasons.iter()
			.filter_map(|r| r["target_doc"]["id"].as_str().map(String::from))
			.collect();
		assert!(target_ids.contains(&t1.id), "missing t1: {:?}", target_ids);
		assert!(target_ids.contains(&t2.id), "missing t2: {:?}", target_ids);
	}

	#[test]
	fn smart_search_falls_back_when_no_conclusions() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		ensure_wiki_layout(root).unwrap();
		create_document(
			root, "thoughts", "Lone thought", "lonely body content",
			vec!["thought".into()], None, None,
		).unwrap();
		// helper alone should return None — no conclusions exist
		let r = conclusions_first(root, "lonely thought", None, 10).unwrap();
		assert!(r.is_none(), "expected fallback (None) when no conclusions");
	}
}
