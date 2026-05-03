//! Raise open questions from search misses + LLM-driven raise during /learn pass.

use super::dedup::dedupe_candidates_by_embedding;
use super::infra::{
	find_question_by_hash, fnv_question_id, question_dedupe_threshold, RaisedQItem, RaisedQResp,
	RaisedQuestion,
};
use crate::cache;
use crate::{http, store};
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

/// Collect embeddings for OPEN (not `resolved`) questions in `purpose_tag`.
pub async fn gather_open_question_embeddings(
	root: &Path,
	purpose_tag: &str,
) -> Result<Vec<Vec<f32>>> {
	let by_purpose = cache::tag_index_lookup(root, purpose_tag);
	let resolved: HashSet<String> = cache::tag_index_lookup(root, "resolved")
		.into_iter()
		.filter(|d| d.doc_type == "questions")
		.map(|d| d.id)
		.collect();

	let mut from_pool: Vec<Vec<f32>> = Vec::new();
	let mut miss_titles: Vec<String> = Vec::new();
	for dref in by_purpose {
		if dref.doc_type != "questions" || resolved.contains(&dref.id) {
			continue;
		}
		if let Some(entry) = cache::pool_get(root, &dref.id) {
			from_pool.push(entry.vec.clone());
		} else if let Ok(qd) = store::get_document(root, "questions", &dref.id) {
			miss_titles.push(qd.title);
		}
	}
	if !miss_titles.is_empty() {
		let live = http::embed_batch(&miss_titles).await?;
		from_pool.extend(live);
	}
	Ok(from_pool)
}

/// Filter LLM-emitted candidates: drop empties, drop template-shaped titles.
pub fn filter_raised_candidates(items: Vec<RaisedQItem>) -> (Vec<RaisedQItem>, Vec<String>) {
	let mut kept = Vec::new();
	let mut skipped = Vec::new();
	for q in items.into_iter().take(3) {
		let title = q.title.trim().to_string();
		if title.is_empty() {
			continue;
		}
		if crate::config::is_template_question(&title) {
			skipped.push(title);
			continue;
		}
		kept.push(RaisedQItem { title, body: q.body });
	}
	(kept, skipped)
}

/// LLM-driven question raise for a single doc. Used by `/learn --raise` only;
/// ingest path must not call this (drift prevention).
pub async fn raise_questions_for_doc(
	root: &Path,
	doc: &store::Document,
	dry_run: bool,
) -> Result<Vec<RaisedQuestion>> {
	let cap = crate::config::open_questions_per_purpose_cap();
	let open_now = count_open_questions_in_purpose(root, doc.purpose.as_deref());
	if open_now >= cap {
		eprintln!(
			"raise_questions_for_doc: purpose {:?} at cap ({} >= {}), skipping raise",
			doc.purpose.as_deref().unwrap_or("general"),
			open_now,
			cap,
		);
		return Ok(Vec::new());
	}

	let sys = "You read a wiki doc and produce open questions IT raises (not answers). \
		Return JSON {\"questions\": [{\"title\": string, \"body\": string}]}. \
		Skip if doc already self-explanatory. Max 3. \
		Hard rules: \
		- Reject questions any reasonable reader could answer from the doc body alone. \
		- No templated phrasing. No 'How does X relate to similar concepts'. \
		No 'What are the implications of X'. No 'What are the key characteristics of X'. \
		- Each question must require >=2 sentences of context in the body, not just a title.";
	let user = format!("Title: {}\n\nBody:\n{}", doc.title, doc.content);
	let raw = http::chat_json(sys, &user).await?;
	let parsed: RaisedQResp = serde_json::from_str(&raw)
		.map_err(|e| anyhow::anyhow!("raise parse: {} body: {}", e, raw))?;

	let (kept, skipped) = filter_raised_candidates(parsed.questions);
	for s in &skipped {
		eprintln!("raise_questions_for_doc: skipped templated question {:?}", s);
	}

	let purpose = doc.purpose.clone();
	let purpose_tag_for_dedupe = purpose.clone().unwrap_or_else(|| "general".to_string());
	let templated_dropped = skipped.len();

	let mut kept = kept;
	let mut semantic_dropped = 0usize;
	if !kept.is_empty() {
		let cand_titles: Vec<String> = kept.iter().map(|q| q.title.clone()).collect();
		let cand_embs = http::embed_batch(&cand_titles).await?;
		let existing_embs = gather_open_question_embeddings(root, &purpose_tag_for_dedupe).await?;
		let threshold = question_dedupe_threshold();
		let keep_idx = dedupe_candidates_by_embedding(&cand_embs, &existing_embs, threshold);
		semantic_dropped = kept.len() - keep_idx.len();
		let kept_set: HashSet<usize> = keep_idx.into_iter().collect();
		kept = kept
			.into_iter()
			.enumerate()
			.filter(|(i, _)| kept_set.contains(i))
			.map(|(_, q)| q)
			.collect();
	}

	let existing_open = count_open_questions_in_purpose(root, doc.purpose.as_deref());
	let slots = cap.saturating_sub(existing_open);
	if kept.len() > slots {
		kept.truncate(slots);
	}

	eprintln!(
		"raise_questions_for_doc: deduped {} templated, {} semantic, {} passed",
		templated_dropped,
		semantic_dropped,
		kept.len(),
	);

	let mut out = Vec::new();
	for q in kept {
		let title = q.title;
		let hash = fnv_question_id(&title);
		if let Some(existing_id) = find_question_by_hash(root, &hash) {
			out.push(RaisedQuestion {
				question_id: existing_id,
				title,
				purpose: purpose.clone(),
				created: false,
			});
			continue;
		}
		if dry_run {
			out.push(RaisedQuestion {
				question_id: hash.clone(),
				title,
				purpose: purpose.clone(),
				created: false,
			});
			continue;
		}
		let body = if q.body.trim().is_empty() { title.clone() } else { q.body };
		let purpose_tag = purpose.clone().unwrap_or_else(|| "general".to_string());
		let tags = vec!["question".to_string(), purpose_tag.clone(), hash.clone()];
		let qdoc = store::create_document(
			root, "questions", &title, &body, tags, Some(&purpose_tag), None,
		)?;
		let _ = store::create_reason(root, &qdoc.id, &doc.id, "References", "raised by", purpose.as_deref());
		out.push(RaisedQuestion {
			question_id: qdoc.id,
			title,
			purpose: purpose.clone(),
			created: true,
		});
	}
	Ok(out)
}

/// Counts open (not yet `resolved`) questions for the given purpose using the
/// cached tag index. `purpose` of `None` falls back to `"general"`.
pub fn count_open_questions_in_purpose(root: &Path, purpose: Option<&str>) -> usize {
	let purpose_tag = purpose.unwrap_or("general");
	let by_purpose = cache::tag_index_lookup(root, purpose_tag);
	let resolved: HashSet<String> = cache::tag_index_lookup(root, "resolved")
		.into_iter()
		.filter(|d| d.doc_type == "questions")
		.map(|d| d.id)
		.collect();
	by_purpose
		.into_iter()
		.filter(|d| d.doc_type == "questions" && !resolved.contains(&d.id))
		.count()
}

/// Raise a single open question from a search query that returned no useful
/// hits. Idempotent via `fnv_question_id` — same query returns the existing
/// question id. Bypasses LLM: the query itself is the question title.
///
/// Returns `Some(question_id)` if a question was created or matched; `None`
/// if the query is too short, the purpose cap is hit, or persistence failed.
pub async fn raise_question_from_search_miss(
	root: &Path,
	query: &str,
	purpose: Option<&str>,
) -> Option<String> {
	let title = query.trim();
	if title.len() < 5 {
		return None;
	}
	let hash = fnv_question_id(title);
	if let Some(existing) = find_question_by_hash(root, &hash) {
		return Some(existing);
	}
	let cap = crate::config::open_questions_per_purpose_cap();
	if count_open_questions_in_purpose(root, purpose) >= cap {
		return None;
	}
	let purpose_tag = purpose.unwrap_or("general").to_string();
	let tags = vec!["question".to_string(), purpose_tag.clone(), hash.clone()];
	let body = format!("Raised from search miss: {}", title);
	store::create_document(
		root, "questions", title, &body, tags, Some(&purpose_tag), None,
	)
	.ok()
	.map(|d| d.id)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::store;
	use tempfile::TempDir;

	#[test]
	fn purpose_cap_blocks_new_raises() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		std::env::set_var("WIKI_OPEN_QUESTIONS_PER_PURPOSE_CAP", "3");
		for i in 0..3 {
			let title = format!("Open question #{}?", i);
			let hash = fnv_question_id(&title);
			let tags = vec!["question".to_string(), "phyons".to_string(), hash];
			store::create_document(root, "questions", &title, "b", tags, Some("phyons"), None).unwrap();
		}
		assert_eq!(count_open_questions_in_purpose(root, Some("phyons")), 3);
		let title = "Resolved Q?";
		let hash = fnv_question_id(title);
		let tags = vec!["question".to_string(), "phyons".to_string(), hash, "resolved".to_string()];
		store::create_document(root, "questions", title, "b", tags, Some("phyons"), None).unwrap();
		assert_eq!(count_open_questions_in_purpose(root, Some("phyons")), 3);
		std::env::remove_var("WIKI_OPEN_QUESTIONS_PER_PURPOSE_CAP");
	}
}
