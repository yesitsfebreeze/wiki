//! Embedding-based dedupe primitives + entity near-duplicate finder + paragraph
//! dedupe (used by linking).

use super::infra::{alias_threshold, build_entity_index, dedupe_threshold, EntityRef};
use crate::io::fnv64;
use crate::{classifier, http};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

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

