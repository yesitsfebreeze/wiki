//! Tag-walk retrieval tier — zero-LLM, deterministic graph walk.
//!
//! Resolves a query into candidate tags (matching purpose tags + entity
//! slugs/aliases + `q-<hash>` if present), seeds docs via the tag index,
//! then BFS-walks reason edges with kind-weighted distance decay. Used as
//! the second tier in `smart_search` between `conclusions_first` and the
//! BM25/HyDE/MMR fallback.

use crate::cache::{self, DocRef};
use crate::store;
use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

/// Edge weights by reason kind. Drive both seed scoring (when relevant) and
/// BFS edge contributions.
fn edge_weight(kind: &str) -> f32 {
	match kind {
		"Answers" => 2.0,
		"Derives" | "Consolidates" => 1.5,
		"Supports" => 1.0,
		"Extends" => 0.7,
		"References" => 0.3,
		"Contradicts" => -0.5,
		_ => 0.1,
	}
}

const DEFAULT_KINDS: &[&str] = &["Answers", "Derives", "Consolidates", "Supports"];
const DECAY: f32 = 0.6;

#[derive(Clone, Debug)]
pub struct WalkOpts {
	pub depth: u8,
	pub fanout: u8,
	pub kinds: Option<Vec<String>>,
	pub types: Option<Vec<String>>,
	pub k: usize,
}

impl Default for WalkOpts {
	fn default() -> Self {
		Self { depth: 2, fanout: 8, kinds: None, types: None, k: 20 }
	}
}

#[derive(Clone, Debug)]
pub struct WalkHit {
	pub doc_id: String,
	pub doc_type: String,
	pub score: f32,
	/// Path of doc ids from a seed to this hit. First element is the seed.
	pub path: Vec<String>,
}

fn type_prior(doc_type: &str) -> f32 {
	match doc_type {
		"conclusions" => 3.0,
		"entities" => 2.0,
		"thoughts" => 1.0,
		_ => 0.5,
	}
}

/// Lowercase tokens of length >= 3 with non-alphanumeric splits.
fn tokenize(query: &str) -> Vec<String> {
	query
		.split(|c: char| !c.is_alphanumeric())
		.filter(|t| t.len() >= 3)
		.map(|t| t.to_lowercase())
		.collect()
}

/// Resolve a query into candidate tags by matching tokens against purpose
/// tags, entity slugs/aliases, and `q-<hash>` literal occurrences in the
/// query.
fn resolve_candidate_tags(root: &Path, query: &str) -> Vec<String> {
	let tokens = tokenize(query);
	if tokens.is_empty() {
		return Vec::new();
	}
	let token_set: HashSet<&str> = tokens.iter().map(|s| s.as_str()).collect();
	let mut out: HashSet<String> = HashSet::new();

	// Purpose tags.
	if let Ok(purposes) = store::list_purposes(root) {
		for p in purposes {
			let tag_lc = p.tag.to_lowercase();
			let title_lc = p.title.to_lowercase();
			if token_set.contains(tag_lc.as_str()) {
				out.insert(p.tag.clone());
				continue;
			}
			for t in &tokens {
				if title_lc.contains(t.as_str()) || tag_lc.contains(t.as_str()) {
					out.insert(p.tag.clone());
					break;
				}
			}
		}
	}

	// Entity slugs + aliases. Cheap full scan via store::list_documents.
	if let Ok(entities) = store::list_documents(root, "entities") {
		for e in entities {
			let title_lc = e.title.to_lowercase();
			let id_lc = e.id.to_lowercase();
			let mut matched = false;
			for t in &tokens {
				if title_lc.contains(t.as_str()) || id_lc.contains(t.as_str()) {
					matched = true;
					break;
				}
			}
			if matched {
				// Use entity title-as-tag heuristic: an entity is reachable
				// directly via its `id` (DocRef). Tags on entities are
				// typically the purpose tag, so we add purpose if present
				// plus a synthetic seed: we promote the entity DocRef
				// directly in seeding rather than via tags.
				out.insert(format!("__entity__:{}", e.id));
				if let Some(p) = e.purpose {
					out.insert(p);
				}
			}
		}
	}

	// q-<hash> literal in query.
	for tok in query.split(|c: char| c.is_whitespace() || c == ',') {
		let t = tok.trim();
		if t.starts_with("q-") && t.len() > 2 {
			out.insert(t.to_string());
		}
	}

	out.into_iter().collect()
}

fn collect_seeds(
	root: &Path,
	tags: &[String],
	type_filter: Option<&[String]>,
) -> HashMap<DocRef, f32> {
	let mut seeds: HashMap<DocRef, (f32, f32)> = HashMap::new(); // (overlap, prior)
	for tag in tags {
		if let Some(eid) = tag.strip_prefix("__entity__:") {
			let dref = DocRef { doc_type: "entities".into(), id: eid.to_string() };
			if !type_allowed(&dref.doc_type, type_filter) { continue; }
			let prior = type_prior(&dref.doc_type);
			let entry = seeds.entry(dref).or_insert((0.0, prior));
			entry.0 += 1.0;
			continue;
		}
		for dref in cache::tag_index_lookup(root, tag) {
			if !type_allowed(&dref.doc_type, type_filter) { continue; }
			let prior = type_prior(&dref.doc_type);
			let entry = seeds.entry(dref).or_insert((0.0, prior));
			entry.0 += 1.0;
		}
	}
	seeds.into_iter()
		.map(|(d, (overlap, prior))| (d, overlap * prior))
		.collect()
}

fn type_allowed(dt: &str, filter: Option<&[String]>) -> bool {
	match filter {
		Some(f) => f.iter().any(|s| s == dt),
		None => true,
	}
}

/// Lookup reason kind from the reason doc. Falls back to title parse.
fn reason_kind(root: &Path, reason_id: &str) -> Option<String> {
	let r = store::get_document(root, "reasons", reason_id).ok()?;
	let t = &r.title;
	let start = t.find("-[")? + 2;
	let end = t[start..].find("]->")? + start;
	Some(t[start..end].to_string())
}

/// Lookup reason `to_id` and `from_id` via title parse.
fn reason_endpoints(root: &Path, reason_id: &str) -> Option<(String, String)> {
	let r = store::get_document(root, "reasons", reason_id).ok()?;
	let t = &r.title;
	let kind_open = t.find(" -[")?;
	let from = t[..kind_open].trim().to_string();
	let to = t.split("]-> ").nth(1)?.trim().to_string();
	Some((from, to))
}

fn doc_type_of(root: &Path, id: &str) -> Option<String> {
	for dt in &["conclusions", "thoughts", "entities", "questions", "reasons"] {
		if store::get_document(root, dt, id).is_ok() {
			return Some((*dt).to_string());
		}
	}
	None
}

pub fn tag_walk(root: &Path, query: &str, opts: WalkOpts) -> Result<Vec<WalkHit>> {
	let tags = resolve_candidate_tags(root, query);
	if tags.is_empty() {
		return Ok(Vec::new());
	}

	let allowed_kinds: HashSet<String> = opts
		.kinds
		.clone()
		.unwrap_or_else(|| DEFAULT_KINDS.iter().map(|s| (*s).to_string()).collect())
		.into_iter()
		.collect();

	let seeds = collect_seeds(root, &tags, opts.types.as_deref());
	if seeds.is_empty() {
		return Ok(Vec::new());
	}

	// Per-doc score and best path so far.
	let mut score: HashMap<String, f32> = HashMap::new();
	let mut path_of: HashMap<String, Vec<String>> = HashMap::new();
	let mut dt_of: HashMap<String, String> = HashMap::new();

	for (dref, seed_score) in &seeds {
		let prev = score.get(&dref.id).copied().unwrap_or(f32::MIN);
		if *seed_score > prev {
			score.insert(dref.id.clone(), *seed_score);
			path_of.insert(dref.id.clone(), vec![dref.id.clone()]);
			dt_of.insert(dref.id.clone(), dref.doc_type.clone());
		}
	}

	// BFS from each seed with depth/fanout caps.
	let mut queue: VecDeque<(String, u8, Vec<String>)> = VecDeque::new();
	for dref in seeds.keys() {
		queue.push_back((dref.id.clone(), 0, vec![dref.id.clone()]));
	}
	let mut visited: HashSet<(String, u8)> = HashSet::new();

	while let Some((node_id, depth, path)) = queue.pop_front() {
		if depth >= opts.depth { continue; }
		if !visited.insert((node_id.clone(), depth)) { continue; }

		let adj = cache::reason_index_lookup(root, &node_id);
		// Combine outgoing+incoming for traversal.
		let mut reason_ids: Vec<String> = adj.from.into_iter().chain(adj.to).collect();
		reason_ids.sort();
		reason_ids.dedup();

		let mut taken = 0u8;
		for rid in reason_ids {
			if taken >= opts.fanout { break; }
			let Some(kind) = reason_kind(root, &rid) else { continue };
			if !allowed_kinds.contains(&kind) { continue; }
			let Some((from, to)) = reason_endpoints(root, &rid) else { continue };
			let neighbor = if from == node_id { to } else { from };
			if neighbor == node_id { continue; }
			let n_dt = match dt_of.get(&neighbor).cloned() {
				Some(dt) => dt,
				None => match doc_type_of(root, &neighbor) {
					Some(dt) => dt,
					None => continue,
				},
			};
			if !type_allowed(&n_dt, opts.types.as_deref()) { continue; }

			let new_depth = depth + 1;
			let contrib = edge_weight(&kind) * DECAY.powi(new_depth as i32);
			let base = score.get(&node_id).copied().unwrap_or(0.0);
			let candidate = base + contrib;

			let prev = score.get(&neighbor).copied().unwrap_or(f32::MIN);
			if candidate > prev {
				score.insert(neighbor.clone(), candidate);
				let mut new_path = path.clone();
				new_path.push(neighbor.clone());
				path_of.insert(neighbor.clone(), new_path.clone());
				dt_of.insert(neighbor.clone(), n_dt.clone());
				queue.push_back((neighbor.clone(), new_depth, new_path));
			}
			taken += 1;
		}
	}

	let mut hits: Vec<WalkHit> = score
		.into_iter()
		.map(|(id, s)| WalkHit {
			doc_type: dt_of.get(&id).cloned().unwrap_or_else(|| "unknown".into()),
			path: path_of.remove(&id).unwrap_or_else(|| vec![id.clone()]),
			doc_id: id,
			score: s,
		})
		.collect();
	hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
	hits.truncate(opts.k);
	Ok(hits)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::store::{create_document, create_purpose, create_reason, ensure_wiki_layout};
	use tempfile::TempDir;

	fn fresh_root() -> TempDir {
		cache::invalidate_indexes();
		cache::invalidate_entities();
		let dir = TempDir::new().unwrap();
		ensure_wiki_layout(dir.path()).unwrap();
		dir
	}

	#[test]
	fn tag_walk_finds_seed_via_purpose_tag() {
		let dir = fresh_root();
		let root = dir.path();
		create_purpose(root, "phyons", "Phyons", "topic phyons").unwrap();
		let d = create_document(
			root, "thoughts", "Phyon note", "body about phyon stuff",
			vec!["thought".into(), "phyons".into()],
			Some("phyons"), None,
		).unwrap();
		let hits = tag_walk(root, "phyons", WalkOpts::default()).unwrap();
		assert!(hits.iter().any(|h| h.doc_id == d.id), "missing seed: {:?}", hits);
	}

	#[test]
	fn tag_walk_walks_one_hop() {
		let dir = fresh_root();
		let root = dir.path();
		create_purpose(root, "topicx", "TopicX", "x").unwrap();
		let a = create_document(
			root, "thoughts", "Seed A", "a body", vec!["thought".into(), "topicx".into()],
			Some("topicx"), None,
		).unwrap();
		let b = create_document(
			root, "thoughts", "Target B", "b body", vec!["thought".into()],
			None, None,
		).unwrap();
		create_reason(root, &a.id, &b.id, "Answers", "a answers b", None).unwrap();

		let opts = WalkOpts { depth: 1, fanout: 8, kinds: None, types: None, k: 20 };
		let hits = tag_walk(root, "topicx", opts).unwrap();
		assert!(hits.iter().any(|h| h.doc_id == a.id), "seed A missing");
		let bh = hits.iter().find(|h| h.doc_id == b.id).expect("B missing");
		assert_eq!(bh.path.first().unwrap(), &a.id, "path must start at seed");
		assert_eq!(bh.path.last().unwrap(), &b.id);
		assert_eq!(bh.path.len(), 2);
	}

	#[test]
	fn tag_walk_kinds_filter() {
		let dir = fresh_root();
		let root = dir.path();
		create_purpose(root, "tkx", "TKX", "x").unwrap();
		let a = create_document(
			root, "thoughts", "A", "body a", vec!["thought".into(), "tkx".into()],
			Some("tkx"), None,
		).unwrap();
		let b = create_document(
			root, "thoughts", "B", "body b", vec!["thought".into()],
			None, None,
		).unwrap();
		create_reason(root, &a.id, &b.id, "References", "ref only", None).unwrap();

		// Default kinds exclude References.
		let hits = tag_walk(root, "tkx", WalkOpts::default()).unwrap();
		assert!(hits.iter().any(|h| h.doc_id == a.id));
		assert!(!hits.iter().any(|h| h.doc_id == b.id),
			"B reached via References should be filtered: {:?}", hits);

		// Explicitly include References → reachable.
		let opts = WalkOpts {
			depth: 1, fanout: 8,
			kinds: Some(vec!["References".into()]),
			types: None, k: 20,
		};
		let hits = tag_walk(root, "tkx", opts).unwrap();
		assert!(hits.iter().any(|h| h.doc_id == b.id));
	}

	#[test]
	fn tag_walk_score_ordering() {
		let dir = fresh_root();
		let root = dir.path();
		create_purpose(root, "ord", "Ord", "x").unwrap();
		let answers_seed = create_document(
			root, "thoughts", "Ans Seed", "as", vec!["thought".into(), "ord".into()],
			Some("ord"), None,
		).unwrap();
		let refs_seed = create_document(
			root, "thoughts", "Ref Seed", "rs", vec!["thought".into(), "ord".into()],
			Some("ord"), None,
		).unwrap();
		for i in 0..3 {
			let t = create_document(
				root, "thoughts", &format!("ans-target-{}", i), "x",
				vec!["thought".into()], None, None,
			).unwrap();
			create_reason(root, &answers_seed.id, &t.id, "Answers", "a", None).unwrap();
		}
		for i in 0..3 {
			let t = create_document(
				root, "thoughts", &format!("ref-target-{}", i), "x",
				vec!["thought".into()], None, None,
			).unwrap();
			create_reason(root, &refs_seed.id, &t.id, "References", "r", None).unwrap();
		}

		let opts = WalkOpts {
			depth: 1, fanout: 8,
			kinds: Some(vec!["Answers".into(), "References".into()]),
			types: None, k: 50,
		};
		let hits = tag_walk(root, "ord", opts).unwrap();
		let ans = hits.iter().find(|h| h.doc_id == answers_seed.id).unwrap();
		let refs = hits.iter().find(|h| h.doc_id == refs_seed.id).unwrap();
		// Seeds themselves carry only the seed score; but Answers seed accumulates
		// nothing on itself — score lives on neighbors. Check neighbor scores.
		// Compare best Answers-target vs best References-target.
		let best_ans_n = hits.iter()
			.filter(|h| h.path.first() == Some(&answers_seed.id) && h.doc_id != answers_seed.id)
			.map(|h| h.score)
			.fold(f32::MIN, f32::max);
		let best_ref_n = hits.iter()
			.filter(|h| h.path.first() == Some(&refs_seed.id) && h.doc_id != refs_seed.id)
			.map(|h| h.score)
			.fold(f32::MIN, f32::max);
		assert!(best_ans_n > best_ref_n,
			"Answers neighbor score {} should exceed References neighbor score {}",
			best_ans_n, best_ref_n);
		// Sanity: both seeds present.
		assert_eq!(ans.doc_id, answers_seed.id);
		assert_eq!(refs.doc_id, refs_seed.id);
	}
}
