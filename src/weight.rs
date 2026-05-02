//! Per-doc importance weight computed from the typed reason graph.
//!
//! Writes `node_size` (int in [6, 100]) into doc frontmatter so Obsidian's
//! CustomNodeSize plugin scales graph nodes by importance. The same `raw`
//! score is reused by `query` ranking.

use crate::cache;
use crate::store;
use anyhow::Result;
use std::path::Path;

const DOC_TYPES: &[&str] = &["thoughts", "entities", "questions", "conclusions"];

const NODE_SIZE_MIN: f64 = 6.0;
const NODE_SIZE_MAX: f64 = 100.0;

fn type_prior(doc_type: &str) -> f64 {
	match doc_type {
		"conclusions" => 3.0,
		"entities" => 2.0,
		"thoughts" => 1.0,
		"questions" => 0.5,
		_ => 0.0, // reasons & unknown
	}
}

fn kind_weight(kind: &str) -> f64 {
	match kind {
		"Answers" => 2.0,
		"Derives" => 1.5,
		"Consolidates" => 1.5,
		"Supports" => 1.0,
		"Extends" => 0.7,
		"References" => 0.3,
		"Contradicts" => -0.5,
		_ => 0.5,
	}
}

/// Read the `kind` frontmatter of a reason doc, or `""` if missing.
fn reason_kind(root: &Path, reason_id: &str) -> String {
	store::get_document(root, "reasons", reason_id)
		.ok()
		.and_then(|d| {
			let raw = std::fs::read_to_string(
				store::find_document_path_by_id(&root.join("reasons"), &d.id).ok()?,
			)
			.ok()?;
			let (fm, _) = store::parse_frontmatter(&raw).ok()?;
			fm.get("kind").and_then(|v| v.as_str()).map(String::from)
		})
		.unwrap_or_default()
}

/// Whether a question doc has any incoming `Answers` reasons.
fn question_is_answered(root: &Path, doc_id: &str) -> bool {
	let adj = cache::reason_index_lookup(root, doc_id);
	adj.to.iter().any(|rid| reason_kind(root, rid) == "Answers")
}

/// Compute the raw importance score for a single doc.
pub fn compute_weight(root: &Path, doc_id: &str, doc_type: &str) -> f64 {
	let prior = type_prior(doc_type);
	let adj = cache::reason_index_lookup(root, doc_id);
	let weighted_in: f64 = adj
		.to
		.iter()
		.map(|rid| kind_weight(&reason_kind(root, rid)))
		.sum();
	prior * (1.0_f64 + (1.0_f64 + weighted_in.max(0.0)).ln())
		// Apply contradicts penalty by re-summing negative contributions:
		// the ln() above clipped them at 0; subtract the absolute negative mass
		// proportionally so contradicts still depress the final score.
		+ prior * weighted_in.min(0.0) * 0.25
}

fn normalize(raw: f64, min_raw: f64, max_raw: f64) -> u8 {
	let span = (max_raw - min_raw).max(1e-9);
	let n = NODE_SIZE_MIN + ((raw - min_raw) / span) * (NODE_SIZE_MAX - NODE_SIZE_MIN);
	n.round().clamp(NODE_SIZE_MIN, NODE_SIZE_MAX) as u8
}

/// Persist `node_size` for a doc. Reasons are skipped at the call site.
pub fn write_node_size(
	root: &Path,
	doc_type: &str,
	doc_id: &str,
	node_size: u8,
) -> Result<()> {
	store::set_frontmatter_field(
		root,
		doc_type,
		doc_id,
		"node_size",
		serde_json::json!(node_size as u64),
	)
}

/// Compute weights for every non-reason doc and write the normalized
/// `node_size` field. Returns the count of docs whose frontmatter was written.
/// Unanswered questions are skipped (left to plugin default).
pub fn recompute_all(root: &Path) -> Result<usize> {
	recompute_inner(root, false)
}

fn collect_plan(root: &Path) -> Result<Vec<(String, String, u8, f64)>> {
	let mut entries: Vec<(String, String, f64)> = Vec::new();
	for dt in DOC_TYPES {
		let docs = store::list_documents(root, dt)?;
		for d in docs {
			if *dt == "questions" && !question_is_answered(root, &d.id) {
				continue;
			}
			let raw = compute_weight(root, &d.id, dt);
			entries.push((dt.to_string(), d.id, raw));
		}
	}
	if entries.is_empty() {
		return Ok(Vec::new());
	}
	let min_raw = entries.iter().map(|e| e.2).fold(f64::INFINITY, f64::min);
	let max_raw = entries.iter().map(|e| e.2).fold(f64::NEG_INFINITY, f64::max);
	Ok(entries
		.into_iter()
		.map(|(dt, id, raw)| {
			let ns = normalize(raw, min_raw, max_raw);
			(dt, id, ns, raw)
		})
		.collect())
}

fn recompute_inner(root: &Path, dry: bool) -> Result<usize> {
	let plan = collect_plan(root)?;
	if dry {
		for (dt, id, ns, raw) in &plan {
			println!("{}/{} raw={:.4} node_size={}", dt, id, raw, ns);
		}
		return Ok(plan.len());
	}
	let mut n = 0usize;
	for (dt, id, ns, _) in plan {
		if write_node_size(root, &dt, &id, ns).is_ok() {
			n += 1;
		}
	}
	Ok(n)
}

/// CLI entry point — exposed so `main.rs` can dispatch `wiki recompute-weights`.
pub fn run_cli(root: &Path, dry_run: bool) -> Result<usize> {
	recompute_inner(root, dry_run)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::store;
	use tempfile::TempDir;

	fn fresh() -> TempDir {
		let dir = TempDir::new().unwrap();
		store::bootstrap(dir.path()).unwrap();
		dir
	}

	#[test]
	fn weight_zero_for_isolated_thought() {
		let dir = fresh();
		let root = dir.path();
		let t = store::create_document(root, "thoughts", "T", "t", vec![], None, None).unwrap();
		let raw = compute_weight(root, &t.id, "thoughts");
		assert!((raw - type_prior("thoughts")).abs() < 1e-9);
		let n = recompute_all(root).unwrap();
		assert_eq!(n, 1);
		let raw_doc = std::fs::read_to_string(
			store::find_document_path_by_id(&root.join("thoughts"), &t.id).unwrap(),
		)
		.unwrap();
		assert!(raw_doc.contains("node_size: 6"));
	}

	#[test]
	fn weight_higher_for_answered_conclusion() {
		let dir = fresh();
		let root = dir.path();
		let c1 = store::create_document(root, "conclusions", "C1", "c1", vec![], None, None).unwrap();
		let c2 = store::create_document(root, "conclusions", "C2", "c2", vec![], None, None).unwrap();
		// Three Answers edges into c1.
		for i in 0..3 {
			let q = store::create_document(
				root, "questions", &format!("Q{}", i), "q", vec![], None, None,
			)
			.unwrap();
			store::create_reason(root, &q.id, &c1.id, "Answers", "ans", None).unwrap();
		}
		cache::invalidate_indexes(root);
		let r1 = compute_weight(root, &c1.id, "conclusions");
		let r2 = compute_weight(root, &c2.id, "conclusions");
		assert!(r1 > r2, "answered conclusion ({}) should outweigh isolated ({})", r1, r2);
	}

	#[test]
	fn weight_decreases_with_contradicts() {
		let dir = fresh();
		let root = dir.path();
		let a = store::create_document(root, "entities", "A", "a", vec![], None, None).unwrap();
		let b = store::create_document(root, "entities", "B", "b", vec![], None, None).unwrap();
		let baseline = compute_weight(root, &a.id, "entities");
		store::create_reason(root, &b.id, &a.id, "Contradicts", "x", None).unwrap();
		cache::invalidate_indexes(root);
		let after = compute_weight(root, &a.id, "entities");
		assert!(after < baseline, "contradicts must reduce weight ({} >= {})", after, baseline);
	}

	#[test]
	fn recompute_writes_node_size() {
		let dir = fresh();
		let root = dir.path();
		let a = store::create_document(root, "entities", "A", "a", vec![], None, None).unwrap();
		let b = store::create_document(root, "entities", "B", "b", vec![], None, None).unwrap();
		store::create_reason(root, &a.id, &b.id, "Supports", "s", None).unwrap();
		cache::invalidate_indexes(root);
		let n = recompute_all(root).unwrap();
		assert_eq!(n, 2);
		for id in [&a.id, &b.id] {
			let p = store::find_document_path_by_id(&root.join("entities"), id).unwrap();
			let raw = std::fs::read_to_string(&p).unwrap();
			let (fm, _) = store::parse_frontmatter(&raw).unwrap();
			let ns = fm.get("node_size").and_then(|v| v.as_u64()).unwrap();
			assert!((6..=100).contains(&(ns as u8)));
		}
	}

	#[test]
	fn recompute_idempotent() {
		let dir = fresh();
		let root = dir.path();
		let a = store::create_document(root, "thoughts", "A", "a", vec![], None, None).unwrap();
		let b = store::create_document(root, "conclusions", "B", "b", vec![], None, None).unwrap();
		store::create_reason(root, &a.id, &b.id, "Derives", "d", None).unwrap();
		cache::invalidate_indexes(root);
		recompute_all(root).unwrap();
		let read_ns = |dt: &str, id: &str| -> u64 {
			let p = store::find_document_path_by_id(&root.join(dt), id).unwrap();
			let raw = std::fs::read_to_string(&p).unwrap();
			let (fm, _) = store::parse_frontmatter(&raw).unwrap();
			fm.get("node_size").and_then(|v| v.as_u64()).unwrap()
		};
		let a1 = read_ns("thoughts", &a.id);
		let b1 = read_ns("conclusions", &b.id);
		cache::invalidate_indexes(root);
		recompute_all(root).unwrap();
		let a2 = read_ns("thoughts", &a.id);
		let b2 = read_ns("conclusions", &b.id);
		assert_eq!(a1, a2);
		assert_eq!(b1, b2);
	}

	#[test]
	fn reason_docs_skipped() {
		let dir = fresh();
		let root = dir.path();
		let a = store::create_document(root, "entities", "A", "a", vec![], None, None).unwrap();
		let b = store::create_document(root, "entities", "B", "b", vec![], None, None).unwrap();
		let r = store::create_reason(root, &a.id, &b.id, "Supports", "s", None).unwrap();
		cache::invalidate_indexes(root);
		recompute_all(root).unwrap();
		let p = store::find_document_path_by_id(&root.join("reasons"), &r.id).unwrap();
		let raw = std::fs::read_to_string(&p).unwrap();
		assert!(!raw.contains("node_size:"), "reason docs must not get node_size");
	}

	#[test]
	fn unanswered_question_skipped() {
		let dir = fresh();
		let root = dir.path();
		let q = store::create_document(root, "questions", "Q", "q", vec![], None, None).unwrap();
		cache::invalidate_indexes(root);
		recompute_all(root).unwrap();
		let p = store::find_document_path_by_id(&root.join("questions"), &q.id).unwrap();
		let raw = std::fs::read_to_string(&p).unwrap();
		assert!(!raw.contains("node_size:"), "unanswered questions must skip");
	}
}
