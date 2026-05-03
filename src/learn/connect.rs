//! Connect-step: graph densification independent of the question loop.
//! For a sampled doc, query top-K semantic neighbors, classify each edge kind
//! deterministically from cosine similarity and purpose overlap, and create a
//! reason for any edge above `cfg.edge_threshold` that does not already exist.

use super::infra::{allowed_kind, PassConfig};
use crate::cache;
use crate::{smart, store};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;

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

/// Pick an edge kind from cosine + purpose overlap — no LLM needed.
///
/// Same purpose: high cosine → content is very similar → `Supports`.
///               lower cosine → one elaborates/extends the other → `Extends`.
/// Cross-purpose: any cosine → cross-domain pointer → `References`.
fn classify_kind(src_purpose: Option<&str>, tgt_purpose: Option<&str>, cosine: f32) -> &'static str {
	let same = matches!((src_purpose, tgt_purpose), (Some(a), Some(b)) if a == b);
	if same {
		if cosine >= 0.85 { "Supports" } else { "Extends" }
	} else {
		"References"
	}
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

	// id → (doc_type, purpose) for candidates that exist in the store
	let mut id_to_meta: HashMap<String, (String, Option<String>)> = HashMap::new();
	for r in &results {
		let Some(id) = r.get("id").and_then(|v| v.as_str()) else { continue };
		if id == doc.id || already.contains(id) {
			continue;
		}
		for dt in &["entities", "thoughts", "conclusions", "questions"] {
			if let Ok(d) = store::get_document(root, dt, id) {
				id_to_meta.insert(id.to_string(), ((*dt).to_string(), d.purpose));
				break;
			}
		}
	}
	if id_to_meta.is_empty() {
		return Ok((0, 0));
	}

	let mut added = 0u64;
	for r in &results {
		let Some(id) = r.get("id").and_then(|v| v.as_str()) else { continue };
		let Some((_, tgt_purpose)) = id_to_meta.get(id) else { continue };
		let cosine = r.get("cosine").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
		if cosine < cfg.edge_threshold {
			continue;
		}
		let raw_kind = classify_kind(doc.purpose.as_deref(), tgt_purpose.as_deref(), cosine);
		let kind = allowed_kind(raw_kind);
		let body = format!("Semantically related (cosine: {cosine:.2})");
		if store::create_reason(root, &doc.id, id, kind, &body, doc.purpose.as_deref()).is_ok() {
			added += 1;
		}
	}

	Ok((added, 0))
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

	#[test]
	fn classify_kind_same_purpose_high_cosine() {
		assert_eq!(classify_kind(Some("rust"), Some("rust"), 0.90), "Supports");
	}

	#[test]
	fn classify_kind_same_purpose_lower_cosine() {
		assert_eq!(classify_kind(Some("rust"), Some("rust"), 0.75), "Extends");
	}

	#[test]
	fn classify_kind_cross_purpose() {
		assert_eq!(classify_kind(Some("rust"), Some("python"), 0.95), "References");
		assert_eq!(classify_kind(Some("rust"), None, 0.90), "References");
		assert_eq!(classify_kind(None, None, 0.90), "References");
	}
}
