//! Connect-step: graph densification independent of the question loop.
//! For a sampled doc, query top-K semantic neighbors across the vault, ask the
//! LLM to classify each candidate edge by kind, and `create_reason` for any
//! edge ≥ `cfg.edge_threshold` that does not already exist between the pair.

use super::infra::{allowed_kind, PassConfig};
use crate::cache;
use crate::{http, smart, store};
use anyhow::Result;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Deserialize, Debug)]
struct ConnectCand {
	picked_id: String,
	#[serde(default)]
	score: f32,
	#[serde(default)]
	kind: String,
	#[serde(default)]
	body: String,
}

#[derive(Deserialize, Debug)]
struct ConnectResp {
	#[serde(default)]
	scored: Vec<ConnectCand>,
}

const CONNECT_KINDS: &[&str] = &[
	"Supports",
	"Contradicts",
	"Extends",
	"Requires",
	"References",
	"Derives",
	"Instances",
	"PartOf",
];

/// Returns the set of doc-ids that already have an outbound reason from `from_id`.
fn existing_outbound_targets(root: &Path, from_id: &str) -> HashSet<String> {
	let mut out = HashSet::new();
	let adj = cache::reason_index_lookup(root, from_id);
	for rid in &adj.from {
		if let Some((from, to, _, _)) = super::infra::read_reason_meta(root, rid) {
			if from == from_id {
				out.insert(to);
			}
		}
	}
	out
}

/// Densify the graph around `doc`. Returns `(edges_added, llm_calls_used)`.
pub async fn connect_doc(
	root: &Path,
	doc: &store::Document,
	cfg: &PassConfig,
) -> Result<(u64, usize)> {
	let res = smart::query(root, &doc.title, doc.purpose.as_deref(), cfg.connect_k, cfg.connect_k).await?;
	let results = res.get("results").and_then(|v| v.as_array()).cloned().unwrap_or_default();
	if results.is_empty() {
		return Ok((0, 0));
	}

	let already = existing_outbound_targets(root, &doc.id);
	let mut id_to_dt: HashMap<String, String> = HashMap::new();
	let mut filtered = Vec::new();
	for r in &results {
		let Some(id) = r.get("id").and_then(|v| v.as_str()) else { continue };
		if id == doc.id || already.contains(id) {
			continue;
		}
		let mut found = false;
		for dt in &["entities", "thoughts", "conclusions", "questions"] {
			if store::get_document(root, dt, id).is_ok() {
				id_to_dt.insert(id.to_string(), (*dt).to_string());
				found = true;
				break;
			}
		}
		if found {
			filtered.push(r.clone());
		}
	}
	if filtered.is_empty() {
		return Ok((0, 0));
	}

	let cand_json = serde_json::to_string(&filtered)?;
	let kinds_csv = CONNECT_KINDS.join("|");
	let sys = format!(
		"You score typed graph edges between a source doc and candidate docs. \
		For each candidate, pick the strongest edge kind from {kinds_csv} and \
		score 0..1 for confidence that the edge is real. Be conservative: \
		score < 0.5 if the relation is weak, hypothetical, or only topical. \
		`body` is one short sentence WHY the edge holds. Return JSON \
		{{\"scored\":[{{\"picked_id\":string,\"score\":number,\"kind\":string,\"body\":string}}]}}. \
		Include every candidate id."
	);
	let user = format!(
		"Source doc title: {}\nSource doc body:\n{}\n\nCandidates:\n{}",
		doc.title, doc.content, cand_json
	);
	let raw = http::chat_json(&sys, &user).await?;
	let parsed: ConnectResp = serde_json::from_str(&raw)
		.map_err(|e| anyhow::anyhow!("connect parse: {} body: {}", e, raw))?;

	let mut added = 0u64;
	for c in parsed.scored {
		if c.score < cfg.edge_threshold {
			continue;
		}
		if !id_to_dt.contains_key(&c.picked_id) {
			continue;
		}
		if already.contains(&c.picked_id) {
			continue;
		}
		let kind = allowed_kind(&c.kind);
		let body = if c.body.trim().is_empty() {
			format!("connect: {} (score {:.2})", kind, c.score)
		} else {
			c.body
		};
		if store::create_reason(root, &doc.id, &c.picked_id, kind, &body, doc.purpose.as_deref()).is_ok() {
			added += 1;
		}
	}

	Ok((added, 1))
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	#[test]
	fn existing_outbound_targets_filters_correctly() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let a = store::create_document(root, "thoughts", "A", "a", vec!["thought".into(), "general".into()], Some("general"), None).unwrap();
		let b = store::create_document(root, "thoughts", "B", "b", vec!["thought".into(), "general".into()], Some("general"), None).unwrap();
		let c = store::create_document(root, "thoughts", "C", "c", vec!["thought".into(), "general".into()], Some("general"), None).unwrap();
		store::create_reason(root, &a.id, &b.id, "Supports", "x", Some("general")).unwrap();
		cache::invalidate_indexes(root);
		let out = existing_outbound_targets(root, &a.id);
		assert!(out.contains(&b.id));
		assert!(!out.contains(&c.id));
	}
}
