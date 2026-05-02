//! Raise open questions from search misses + helpers for question-pool counting.

use super::infra::{find_question_by_hash, fnv_question_id};
use crate::cache;
use crate::store;
use std::collections::HashSet;
use std::path::Path;

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
