//! Process-global caches for retrieval hot paths.
//!
//! All caches are keyed by a single wiki root (the binary serves one vault
//! per process). Mutations in `store` call `invalidate_*` to keep state
//! coherent without TTLs or generation counters.

use crate::search::SearchIndex;
use crate::store::Document;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

const EXPAND_TTL: Duration = Duration::from_secs(300);
const HYDE_TTL: Duration = Duration::from_secs(300);

const DOC_TYPES: &[&str] = &["thoughts", "entities", "reasons", "questions", "conclusions"];

pub struct PoolEntry {
	pub doc_type: String,
	pub doc: Document,
	pub content_hash: String,
	pub vec: Vec<f32>,
}

/// Compact id for a doc resolvable through `store::get_document` -- doc_type +
/// id is sufficient.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocRef {
	pub doc_type: String,
	pub id: String,
}

/// Adjacency record for a node in the reason graph.
#[derive(Clone, Debug, Default)]
pub struct ReasonAdjacency {
	/// Reason ids whose `from_id` equals this node.
	pub from: Vec<String>,
	/// Reason ids whose `to_id` equals this node.
	pub to: Vec<String>,
}

type TagMap = HashMap<String, Vec<DocRef>>;
type ReasonMap = HashMap<String, ReasonAdjacency>;
type HashMap2 = HashMap<String, Vec<DocRef>>;

#[derive(Default)]
struct Cache {
	search: Mutex<Option<Arc<SearchIndex>>>,
	pool: RwLock<HashMap<String, Arc<PoolEntry>>>,
	pool_seeded: Mutex<bool>,
	dirty_pool_ids: RwLock<HashSet<String>>,
	tag_index: RwLock<Option<Arc<TagMap>>>,
	reason_index: RwLock<Option<Arc<ReasonMap>>>,
	hash_index: RwLock<Option<Arc<HashMap2>>>,
	entity_index: RwLock<Option<Arc<Vec<crate::learn::EntityRef>>>>,
	expand: RwLock<HashMap<String, (Instant, Vec<String>)>>,
	hyde: RwLock<HashMap<String, (Instant, Vec<f32>)>>,
}

fn cache() -> &'static Cache {
	static C: OnceLock<Cache> = OnceLock::new();
	C.get_or_init(Cache::default)
}

// ── SearchIndex ──────────────────────────────────────────────────────────────

pub fn search_index(root: &std::path::Path) -> anyhow::Result<Arc<SearchIndex>> {
	let mut g = cache().search.lock().unwrap();
	if let Some(idx) = g.as_ref() {
		return Ok(idx.clone());
	}
	let idx = Arc::new(crate::search::create_index(&root.join(".search"))?);
	*g = Some(idx.clone());
	Ok(idx)
}

// ── Embedding pool ───────────────────────────────────────────────────────────

pub fn pool_seeded() -> bool {
	*cache().pool_seeded.lock().unwrap()
}

pub fn mark_pool_seeded() {
	*cache().pool_seeded.lock().unwrap() = true;
}

pub fn pool_get(id: &str) -> Option<Arc<PoolEntry>> {
	cache().pool.read().unwrap().get(id).cloned()
}

pub fn pool_snapshot() -> Vec<Arc<PoolEntry>> {
	cache().pool.read().unwrap().values().cloned().collect()
}

pub fn pool_insert(entry: PoolEntry) {
	cache()
		.pool
		.write()
		.unwrap()
		.insert(entry.doc.id.clone(), Arc::new(entry));
}

pub fn pool_remove(id: &str) {
	cache().pool.write().unwrap().remove(id);
	mark_pool_dirty(id);
}

/// Mark a doc id as needing re-embedding on the next `refresh_pool`.
pub fn mark_pool_dirty(id: &str) {
	cache().dirty_pool_ids.write().unwrap().insert(id.to_string());
}

/// Snapshot + clear the dirty set. Returned ids should be re-checked /
/// re-embedded by the caller.
pub fn drain_dirty_pool() -> Vec<String> {
	let mut g = cache().dirty_pool_ids.write().unwrap();
	let out: Vec<String> = g.iter().cloned().collect();
	g.clear();
	out
}

// ── Entity index ─────────────────────────────────────────────────────────────

pub fn entity_index_get() -> Option<Arc<Vec<crate::learn::EntityRef>>> {
	cache().entity_index.read().unwrap().clone()
}

pub fn entity_index_set(v: Vec<crate::learn::EntityRef>) -> Arc<Vec<crate::learn::EntityRef>> {
	let arc = Arc::new(v);
	*cache().entity_index.write().unwrap() = Some(arc.clone());
	arc
}

pub fn invalidate_entities() {
	*cache().entity_index.write().unwrap() = None;
}

// ── Tag / reason / hash indexes ──────────────────────────────────────────────

fn collect_md_paths(dir: &Path) -> Vec<PathBuf> {
	fn rec(d: &Path, out: &mut Vec<PathBuf>) {
		if !d.exists() {
			return;
		}
		let Ok(rd) = std::fs::read_dir(d) else { return };
		for entry in rd.flatten() {
			let p = entry.path();
			if p.is_dir() {
				rec(&p, out);
			} else if p.extension().and_then(|s| s.to_str()) == Some("md") {
				out.push(p);
			}
		}
	}
	let mut out = Vec::new();
	rec(dir, &mut out);
	out
}

fn parse_fm(content: &str) -> Option<serde_json::Value> {
	if !content.starts_with("---") {
		return None;
	}
	let end = content[3..].find("---")? + 3;
	serde_yaml::from_str(&content[3..end]).ok()
}

fn build_indexes(root: &Path) -> (TagMap, ReasonMap, HashMap2) {
	let mut tag_idx: HashMap<String, Vec<DocRef>> = HashMap::new();
	let mut reason_idx: HashMap<String, ReasonAdjacency> = HashMap::new();
	let mut hash_idx: HashMap<String, Vec<DocRef>> = HashMap::new();

	for doc_type in DOC_TYPES {
		let dir = root.join(doc_type);
		for path in collect_md_paths(&dir) {
			let Ok(raw) = std::fs::read_to_string(&path) else { continue };
			let Some(fm) = parse_fm(&raw) else { continue };
			let Some(id) = fm.get("id").and_then(|v| v.as_str()) else { continue };
			let dref = DocRef { doc_type: (*doc_type).to_string(), id: id.to_string() };

			if let Some(arr) = fm.get("tags").and_then(|v| v.as_array()) {
				for tag in arr.iter().filter_map(|v| v.as_str()) {
					tag_idx.entry(tag.to_string()).or_default().push(dref.clone());
					if tag.starts_with("q-") {
						hash_idx.entry(tag.to_string()).or_default().push(dref.clone());
					}
				}
			}

			if *doc_type == "reasons" {
				let from_id = fm.get("from_id").and_then(|v| v.as_str()).unwrap_or("");
				let to_id = fm.get("to_id").and_then(|v| v.as_str()).unwrap_or("");
				if !from_id.is_empty() {
					reason_idx.entry(from_id.to_string()).or_default().from.push(id.to_string());
				}
				if !to_id.is_empty() {
					reason_idx.entry(to_id.to_string()).or_default().to.push(id.to_string());
				}
			}
		}
	}

	for v in tag_idx.values_mut() {
		v.sort_by(|a, b| a.id.cmp(&b.id));
		v.dedup();
	}

	(tag_idx, reason_idx, hash_idx)
}

fn ensure_indexes(root: &Path) {
	let need_build = {
		let c = cache();
		c.tag_index.read().unwrap().is_none()
			|| c.reason_index.read().unwrap().is_none()
			|| c.hash_index.read().unwrap().is_none()
	};
	if !need_build {
		return;
	}
	let (tag_idx, reason_idx, hash_idx) = build_indexes(root);
	let c = cache();
	*c.tag_index.write().unwrap() = Some(Arc::new(tag_idx));
	*c.reason_index.write().unwrap() = Some(Arc::new(reason_idx));
	*c.hash_index.write().unwrap() = Some(Arc::new(hash_idx));
}

pub fn tag_index_lookup(root: &Path, tag: &str) -> Vec<DocRef> {
	ensure_indexes(root);
	cache()
		.tag_index
		.read()
		.unwrap()
		.as_ref()
		.and_then(|m| m.get(tag).cloned())
		.unwrap_or_default()
}

pub fn reason_index_lookup(root: &Path, node_id: &str) -> ReasonAdjacency {
	ensure_indexes(root);
	cache()
		.reason_index
		.read()
		.unwrap()
		.as_ref()
		.and_then(|m| m.get(node_id).cloned())
		.unwrap_or_default()
}

/// Returns all docs that carry `hash_tag` as a frontmatter tag. Order is
/// arbitrary across calls; callers needing a specific doc_type must filter.
pub fn hash_index_lookup(root: &Path, hash_tag: &str) -> Vec<DocRef> {
	ensure_indexes(root);
	cache()
		.hash_index
		.read()
		.unwrap()
		.as_ref()
		.and_then(|m| m.get(hash_tag).cloned())
		.unwrap_or_default()
}

pub fn invalidate_indexes() {
	let c = cache();
	*c.tag_index.write().unwrap() = None;
	*c.reason_index.write().unwrap() = None;
	*c.hash_index.write().unwrap() = None;
}

// ── expand_questions ─────────────────────────────────────────────────────────

pub fn expand_get(prompt: &str) -> Option<Vec<String>> {
	let g = cache().expand.read().unwrap();
	g.get(prompt).and_then(|(t, v)| {
		if t.elapsed() < EXPAND_TTL { Some(v.clone()) } else { None }
	})
}

pub fn expand_set(prompt: &str, queries: Vec<String>) {
	let mut g = cache().expand.write().unwrap();
	// Lazy GC: evict expired entries when we touch the map.
	g.retain(|_, (t, _)| t.elapsed() < EXPAND_TTL);
	g.insert(prompt.to_string(), (Instant::now(), queries));
}

// ── HyDE ─────────────────────────────────────────────────────────────────────

pub fn hyde_get(prompt: &str) -> Option<Vec<f32>> {
	let g = cache().hyde.read().unwrap();
	g.get(prompt).and_then(|(t, v)| {
		if t.elapsed() < HYDE_TTL { Some(v.clone()) } else { None }
	})
}

pub fn hyde_set(prompt: &str, emb: Vec<f32>) {
	let mut g = cache().hyde.write().unwrap();
	g.retain(|_, (t, _)| t.elapsed() < HYDE_TTL);
	g.insert(prompt.to_string(), (Instant::now(), emb));
}

// ── Bulk invalidations triggered by store mutations ──────────────────────────

/// Doc was created/updated. Invalidate dependent caches and signal that the
/// embedding pool entry (if any) is stale.
pub fn on_doc_changed(id: &str, doc_type: &str) {
	pool_remove(id);
	mark_pool_dirty(id);
	if doc_type == "entities" {
		invalidate_entities();
	}
	invalidate_indexes();
}

pub fn on_doc_deleted(id: &str, doc_type: &str) {
	pool_remove(id);
	mark_pool_dirty(id);
	if doc_type == "entities" {
		invalidate_entities();
	}
	invalidate_indexes();
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::store;
	use tempfile::TempDir;

	fn drain_only(id: &str) -> bool {
		// Drain everything, then check whether `id` was present.
		let drained = drain_dirty_pool();
		drained.iter().any(|s| s == id)
	}

	#[test]
	fn mark_pool_dirty_inserts() {
		let id = "test-dirty-mark-x-unique-1";
		// Clear baseline
		let _ = drain_dirty_pool();
		mark_pool_dirty(id);
		assert!(drain_only(id), "marked id should appear in drain");
	}

	#[test]
	fn drain_clears_set() {
		let id = "test-dirty-clear-y-unique-2";
		let _ = drain_dirty_pool();
		mark_pool_dirty(id);
		let first = drain_dirty_pool();
		assert!(first.iter().any(|s| s == id));
		// Second drain must not contain this id (set was cleared).
		let second = drain_dirty_pool();
		assert!(!second.iter().any(|s| s == id), "second drain leaked id");
	}

	#[test]
	fn on_doc_changed_marks_dirty() {
		let id = "test-on-changed-z-unique-3";
		let _ = drain_dirty_pool();
		on_doc_changed(id, "thoughts");
		assert!(drain_only(id), "on_doc_changed must mark dirty");
	}

	#[test]
	fn on_doc_deleted_marks_dirty() {
		let id = "test-on-deleted-w-unique-4";
		let _ = drain_dirty_pool();
		on_doc_deleted(id, "thoughts");
		assert!(drain_only(id), "on_doc_deleted must mark dirty");
	}

	#[test]
	fn tag_index_returns_docs_with_tag() {
		invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		store::create_document(root, "thoughts", "A", "a", vec!["x".into()], None, None).unwrap();
		store::create_document(root, "thoughts", "B", "b", vec!["x".into()], None, None).unwrap();
		store::create_document(root, "thoughts", "C", "c", vec!["y".into()], None, None).unwrap();
		let hits = tag_index_lookup(root, "x");
		assert_eq!(hits.len(), 2);
		assert!(hits.iter().all(|d| d.doc_type == "thoughts"));
	}

	#[test]
	fn tag_index_invalidated_on_create() {
		invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		store::create_document(root, "thoughts", "A", "a", vec!["t".into()], None, None).unwrap();
		assert_eq!(tag_index_lookup(root, "t").len(), 1);
		store::create_document(root, "thoughts", "B", "b", vec!["t".into()], None, None).unwrap();
		assert_eq!(tag_index_lookup(root, "t").len(), 2);
	}

	#[test]
	fn tag_index_invalidated_on_delete() {
		invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		let d = store::create_document(root, "thoughts", "A", "a", vec!["k".into()], None, None).unwrap();
		assert_eq!(tag_index_lookup(root, "k").len(), 1);
		store::delete_document(root, "thoughts", &d.id).unwrap();
		assert_eq!(tag_index_lookup(root, "k").len(), 0);
	}

	#[test]
	fn reason_index_from_and_to() {
		invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		let a = store::create_document(root, "entities", "A", "a", vec![], None, None).unwrap();
		let b = store::create_document(root, "entities", "B", "b", vec![], None, None).unwrap();
		let r = store::create_reason(root, &a.id, &b.id, "Answers", "because", None).unwrap();
		let from_a = reason_index_lookup(root, &a.id);
		let to_b = reason_index_lookup(root, &b.id);
		assert!(from_a.from.contains(&r.id), "expected from-edge on A");
		assert!(to_b.to.contains(&r.id), "expected to-edge on B");
	}

	#[test]
	fn hash_index_finds_question_by_q_tag() {
		invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		let qhash = "q-deadbeef".to_string();
		let tags = vec!["question".to_string(), qhash.clone()];
		let q = store::create_document(root, "questions", "Q?", "qb", tags, None, None).unwrap();
		let hits = hash_index_lookup(root, &qhash);
		assert_eq!(hits.len(), 1);
		assert_eq!(hits[0].id, q.id);
		assert_eq!(hits[0].doc_type, "questions");
		assert!(hash_index_lookup(root, "q-nothere").is_empty());
	}
}
