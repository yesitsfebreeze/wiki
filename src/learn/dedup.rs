//! Embedding-based dedupe primitives + entity near-duplicate finder + paragraph
//! dedupe (used by linking).

use super::infra::{alias_threshold, build_entity_index, dedupe_threshold, EntityRef};
use crate::io::fnv64;
use crate::{classifier, http};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

/// Pure: drop candidate indices whose max cosine against any existing
/// embedding meets/exceeds `threshold`. Returns indices kept, in input order.
pub fn dedupe_candidates_by_embedding(
	cand_embs: &[Vec<f32>],
	existing_embs: &[Vec<f32>],
	threshold: f32,
) -> Vec<usize> {
	let mut kept = Vec::with_capacity(cand_embs.len());
	for (i, c) in cand_embs.iter().enumerate() {
		let mut dup = false;
		for e in existing_embs {
			if classifier::cosine(c, e) >= threshold {
				dup = true;
				break;
			}
		}
		if !dup {
			kept.push(i);
		}
	}
	kept
}

pub async fn find_near_duplicate_entity(root: &Path, title: &str, content: &str) -> Result<Option<EntityRef>> {
	let threshold = alias_threshold();
	let entities = build_entity_index(root).await?;
	let title_lc = title.trim().to_lowercase();

	for e in entities.iter() {
		if e.title.to_lowercase() == title_lc
			|| e.aliases.iter().any(|a| a.to_lowercase() == title_lc)
		{
			return Ok(Some(e.clone()));
		}
	}

	if !content.is_empty() {
		if let Ok(embs) = http::embed_batch(&[content.to_string()]).await {
			if let Some(content_emb) = embs.into_iter().next() {
				for e in entities.iter() {
					if let Some(ev) = &e.body_embedding {
						if classifier::cosine(&content_emb, ev) >= threshold {
							return Ok(Some(e.clone()));
						}
					}
				}
			}
		}
	}

	Ok(None)
}

pub(crate) async fn dedupe_paragraphs(
	body: &str,
	entities: &[EntityRef],
	self_id: &str,
) -> (String, Vec<(String, String)>) {
	let paragraphs: Vec<String> = body.split("\n\n").map(|s| s.to_string()).collect();
	let nonempty: Vec<(usize, String)> = paragraphs
		.iter()
		.enumerate()
		.map(|(i, p)| (i, p.trim().to_string()))
		.filter(|(_, p)| !p.is_empty() && p.len() >= 40)
		.collect();
	if nonempty.is_empty() {
		return (body.to_string(), Vec::new());
	}
	let texts: Vec<String> = nonempty.iter().map(|(_, p)| p.clone()).collect();
	let embs = match http::embed_batch(&texts).await {
		Ok(v) => v,
		Err(_) => return (body.to_string(), Vec::new()),
	};
	let threshold = dedupe_threshold();
	let mut drop_idx: HashMap<usize, (String, String)> = HashMap::new();
	for ((idx, para), emb) in nonempty.iter().zip(embs.iter()) {
		for e in entities {
			if e.id == self_id {
				continue;
			}
			let Some(ev) = &e.body_embedding else { continue };
			if classifier::cosine(emb, ev) >= threshold {
				drop_idx.insert(*idx, (e.id.clone(), format!("{:x}", fnv64(para))));
				break;
			}
		}
	}
	let mut merges = Vec::new();
	let mut kept = Vec::new();
	for (i, p) in paragraphs.iter().enumerate() {
		match drop_idx.remove(&i) {
			Some(merge) => merges.push(merge),
			None => kept.push(p.clone()),
		}
	}
	(kept.join("\n\n"), merges)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::cache;
	use crate::learn::infra::question_dedupe_threshold;
	use crate::learn::raise::gather_open_question_embeddings;
	use crate::learn::infra::fnv_question_id;
	use crate::store;
	use tempfile::TempDir;

	fn seed_pool_entry(root: &Path, id: &str, doc_type: &str, title: &str, content: &str, vec: Vec<f32>) {
		let doc = store::Document {
			id: id.to_string(),
			title: title.to_string(),
			tags: vec![],
			purpose: None,
			source_doc_id: None,
			created_at: String::new(),
			updated_at: String::new(),
			content: content.to_string(),
		};
		cache::pool_insert(root, cache::PoolEntry {
			doc_type: doc_type.to_string(),
			doc,
			content_hash: "x".to_string(),
			vec,
		});
	}

	fn seed_open_question(root: &Path, purpose: &str, title: &str, vec: Vec<f32>) -> String {
		let hash = fnv_question_id(title);
		let tags = vec!["question".to_string(), purpose.to_string(), hash];
		let q = store::create_document(root, "questions", title, "body", tags, Some(purpose), None).unwrap();
		seed_pool_entry(root, &q.id, "questions", title, "body", vec);
		q.id
	}

	#[tokio::test]
	async fn dedupe_drops_near_duplicate() {
		std::env::remove_var("WIKI_QUESTION_DEDUPE_THRESHOLD");
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let e1 = vec![1.0, 0.0, 0.0];
		let qid = seed_open_question(root, "p1", "What causes X?", e1.clone());
		let e2 = vec![1.0, 0.0, 0.0];
		let existing = gather_open_question_embeddings(root, "p1").await.unwrap();
		assert_eq!(existing.len(), 1);
		let kept = dedupe_candidates_by_embedding(&[e2], &existing, 0.88);
		assert!(kept.is_empty(), "near-duplicate must be dropped");
		cache::pool_remove(root, &qid);
	}

	#[tokio::test]
	async fn dedupe_keeps_distinct() {
		std::env::remove_var("WIKI_QUESTION_DEDUPE_THRESHOLD");
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let qid = seed_open_question(root, "p1", "What causes X?", vec![1.0, 0.0, 0.0]);
		let cand = vec![0.0, 1.0, 0.0];
		let existing = gather_open_question_embeddings(root, "p1").await.unwrap();
		let kept = dedupe_candidates_by_embedding(&[cand], &existing, 0.88);
		assert_eq!(kept, vec![0]);
		cache::pool_remove(root, &qid);
	}

	#[tokio::test]
	async fn dedupe_threshold_respects_env() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let qid = seed_open_question(root, "p1", "What causes X?", vec![1.0, 0.0, 0.0]);
		let cand = vec![0.94, 0.34, 0.0];
		let cos = classifier::cosine(&cand, &[1.0, 0.0, 0.0]);
		assert!(cos > 0.88 && cos < 0.99, "cos={}", cos);

		std::env::remove_var("WIKI_QUESTION_DEDUPE_THRESHOLD");
		let t_default = question_dedupe_threshold();
		assert!((t_default - 0.88).abs() < 1e-6);
		let existing = gather_open_question_embeddings(root, "p1").await.unwrap();
		assert!(dedupe_candidates_by_embedding(std::slice::from_ref(&cand), &existing, t_default).is_empty());

		std::env::set_var("WIKI_QUESTION_DEDUPE_THRESHOLD", "0.99");
		let t_strict = question_dedupe_threshold();
		assert!((t_strict - 0.99).abs() < 1e-6);
		assert_eq!(
			dedupe_candidates_by_embedding(&[cand], &existing, t_strict),
			vec![0]
		);

		std::env::remove_var("WIKI_QUESTION_DEDUPE_THRESHOLD");
		cache::pool_remove(root, &qid);
	}
}
