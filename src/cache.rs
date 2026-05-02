//! Per-root caches for retrieval hot paths.
//!
//! Each wiki root (canonicalized `&Path`) gets its own `RootCache`. A
//! process-wide registry maps roots to caches, so multiple roots in the same
//! process (e.g. parallel tests with `TempDir`s) do not corrupt each other's
//! state. Mutations in `store` call `invalidate_*` to keep state coherent
//! without TTLs or generation counters.

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
pub struct RootCache {
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

static REGISTRY: OnceLock<RwLock<HashMap<PathBuf, Arc<RootCache>>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<PathBuf, Arc<RootCache>>> {
	REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

fn canonical(root: &Path) -> PathBuf {
	// Normalize to an absolute path. We deliberately avoid `canonicalize`
	// because (a) it requires the path to exist, and (b) transient I/O
	// failures (e.g. Windows lock contention on a TempDir under heavy
	// parallel test load) would shard the registry across multiple keys
	// for the same logical root and silently lose dirty-set entries.
	if root.is_absolute() {
		root.to_path_buf()
	} else {
		std::env::current_dir()
			.map(|cwd| cwd.join(root))
			.unwrap_or_else(|_| root.to_path_buf())
	}
}

/// Resolve (or lazily create) the cache for `root`.
pub fn root_cache(root: &Path) -> Arc<RootCache> {
	let key = canonical(root);
	{
		let g = registry().read().unwrap();
		if let Some(c) = g.get(&key) {
			return c.clone();
		}
	}
	let mut g = registry().write().unwrap();
	if let Some(c) = g.get(&key) {
		return c.clone();
	}
	let c = Arc::new(RootCache::default());
	g.insert(key, c.clone());
	c
}

// ── SearchIndex ──────────────────────────────────────────────────────────────

pub fn search_index(root: &std::path::Path) -> anyhow::Result<Arc<SearchIndex>> {
	let c = root_cache(root);
	let mut g = c.search.lock().unwrap();
	if let Some(idx) = g.as_ref() {
		return Ok(idx.clone());
	}
	let idx = Arc::new(crate::search::create_index(&root.join(".search"))?);
	*g = Some(idx.clone());
	Ok(idx)
}

// ── Embedding pool ───────────────────────────────────────────────────────────

pub fn pool_seeded(root: &Path) -> bool {
	*root_cache(root).pool_seeded.lock().unwrap()
}

pub fn mark_pool_seeded(root: &Path) {
	*root_cache(root).pool_seeded.lock().unwrap() = true;
}

pub fn pool_get(root: &Path, id: &str) -> Option<Arc<PoolEntry>> {
	root_cache(root).pool.read().unwrap().get(id).cloned()
}

pub fn pool_snapshot(root: &Path) -> Vec<Arc<PoolEntry>> {
	root_cache(root).pool.read().unwrap().values().cloned().collect()
}

pub fn pool_insert(root: &Path, entry: PoolEntry) {
	root_cache(root)
		.pool
		.write()
		.unwrap()
		.insert(entry.doc.id.clone(), Arc::new(entry));
}

pub fn pool_remove(root: &Path, id: &str) {
	root_cache(root).pool.write().unwrap().remove(id);
	mark_pool_dirty(root, id);
}

/// Mark a doc id as needing re-embedding on the next `refresh_pool`.
pub fn mark_pool_dirty(root: &Path, id: &str) {
	let c = root_cache(root);
	dbg_log(&format!("mark_pool_dirty root={:?} canon={:?} ptr={:p} id={}", root, canonical(root), Arc::as_ptr(&c), id));
	c.dirty_pool_ids.write().unwrap().insert(id.to_string());
}

fn dbg_log(msg: &str) {
	use std::io::Write;
	if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("C:/Users/sayhe/dbg_wiki.log") {
		let _ = writeln!(f, "[{:?}] {}", std::thread::current().id(), msg);
	}
}

/// Snapshot + clear the dirty set. Returned ids should be re-checked /
/// re-embedded by the caller.
pub fn drain_dirty_pool(root: &Path) -> Vec<String> {
	let c = root_cache(root);
	let g_data: Vec<String> = c.dirty_pool_ids.read().unwrap().iter().cloned().collect();
	dbg_log(&format!("drain_dirty_pool root={:?} canon={:?} ptr={:p} contains={:?}", root, canonical(root), Arc::as_ptr(&c), g_data));
	let mut g = c.dirty_pool_ids.write().unwrap();
	let out: Vec<String> = g.iter().cloned().collect();
	g.clear();
	out
}

// ── Entity index ─────────────────────────────────────────────────────────────

pub fn entity_index_get(root: &Path) -> Option<Arc<Vec<crate::learn::EntityRef>>> {
	root_cache(root).entity_index.read().unwrap().clone()
}

pub fn entity_index_set(root: &Path, v: Vec<crate::learn::EntityRef>) -> Arc<Vec<crate::learn::EntityRef>> {
	let arc = Arc::new(v);
	*root_cache(root).entity_index.write().unwrap() = Some(arc.clone());
	arc
}

pub fn invalidate_entities(root: &Path) {
	*root_cache(root).entity_index.write().unwrap() = None;
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
	let c = root_cache(root);
	let need_build = c.tag_index.read().unwrap().is_none()
		|| c.reason_index.read().unwrap().is_none()
		|| c.hash_index.read().unwrap().is_none();
	if !need_build {
		return;
	}
	let (tag_idx, reason_idx, hash_idx) = build_indexes(root);
	*c.tag_index.write().unwrap() = Some(Arc::new(tag_idx));
	*c.reason_index.write().unwrap() = Some(Arc::new(reason_idx));
	*c.hash_index.write().unwrap() = Some(Arc::new(hash_idx));
}

pub fn tag_index_lookup(root: &Path, tag: &str) -> Vec<DocRef> {
	ensure_indexes(root);
	root_cache(root)
		.tag_index
		.read()
		.unwrap()
		.as_ref()
		.and_then(|m| m.get(tag).cloned())
		.unwrap_or_default()
}

pub fn reason_index_lookup(root: &Path, node_id: &str) -> ReasonAdjacency {
	ensure_indexes(root);
	root_cache(root)
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
	root_cache(root)
		.hash_index
		.read()
		.unwrap()
		.as_ref()
		.and_then(|m| m.get(hash_tag).cloned())
		.unwrap_or_default()
}

pub fn invalidate_indexes(root: &Path) {
	let c = root_cache(root);
	*c.tag_index.write().unwrap() = None;
	*c.reason_index.write().unwrap() = None;
	*c.hash_index.write().unwrap() = None;
}

// ── expand_questions ─────────────────────────────────────────────────────────

pub fn expand_get(root: &Path, prompt: &str) -> Option<Vec<String>> {
	let c = root_cache(root);
	let g = c.expand.read().unwrap();
	g.get(prompt).and_then(|(t, v)| {
		if t.elapsed() < EXPAND_TTL { Some(v.clone()) } else { None }
	})
}

pub fn expand_set(root: &Path, prompt: &str, queries: Vec<String>) {
	let c = root_cache(root);
	let mut g = c.expand.write().unwrap();
	// Lazy GC: evict expired entries when we touch the map.
	g.retain(|_, (t, _)| t.elapsed() < EXPAND_TTL);
	g.insert(prompt.to_string(), (Instant::now(), queries));
}

// ── HyDE ─────────────────────────────────────────────────────────────────────

pub fn hyde_get(root: &Path, prompt: &str) -> Option<Vec<f32>> {
	let c = root_cache(root);
	let g = c.hyde.read().unwrap();
	g.get(prompt).and_then(|(t, v)| {
		if t.elapsed() < HYDE_TTL { Some(v.clone()) } else { None }
	})
}

pub fn hyde_set(root: &Path, prompt: &str, emb: Vec<f32>) {
	let c = root_cache(root);
	let mut g = c.hyde.write().unwrap();
	g.retain(|_, (t, _)| t.elapsed() < HYDE_TTL);
	g.insert(prompt.to_string(), (Instant::now(), emb));
}

// ── Bulk invalidations triggered by store mutations ──────────────────────────

/// Doc was created/updated. Invalidate dependent caches and signal that the
/// embedding pool entry (if any) is stale.
pub fn on_doc_changed(root: &Path, id: &str, doc_type: &str) {
	pool_remove(root, id);
	mark_pool_dirty(root, id);
	if doc_type == "entities" {
		invalidate_entities(root);
	}
	invalidate_indexes(root);
}

pub fn on_doc_deleted(root: &Path, id: &str, doc_type: &str) {
	pool_remove(root, id);
	mark_pool_dirty(root, id);
	if doc_type == "entities" {
		invalidate_entities(root);
	}
	invalidate_indexes(root);
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::store;
	use tempfile::TempDir;

	fn drain_only(root: &Path, id: &str) -> bool {
		// Drain everything, then check whether `id` was present.
		let drained = drain_dirty_pool(root);
		drained.iter().any(|s| s == id)
	}

	#[test]
	fn mark_pool_dirty_inserts() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		let id = "test-dirty-mark-x-unique-1";
		eprintln!("DEBUG mark_pool_dirty_inserts root={:?} canon={:?}", root, canonical(root));
		mark_pool_dirty(root, id);
		let drained = drain_dirty_pool(root);
		eprintln!("DEBUG drained={:?}", drained);
		assert!(drained.iter().any(|s| s == id), "marked id should appear in drain");
	}

	#[test]
	fn drain_clears_set() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		let id = "test-dirty-clear-y-unique-2";
		mark_pool_dirty(root, id);
		let first = drain_dirty_pool(root);
		assert!(first.iter().any(|s| s == id));
		// Second drain must not contain this id (set was cleared).
		let second = drain_dirty_pool(root);
		assert!(!second.iter().any(|s| s == id), "second drain leaked id");
	}

	#[test]
	fn on_doc_changed_marks_dirty() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		let id = "test-on-changed-z-unique-3";
		on_doc_changed(root, id, "thoughts");
		assert!(drain_only(root, id), "on_doc_changed must mark dirty");
	}

	#[test]
	fn on_doc_deleted_marks_dirty() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		let id = "test-on-deleted-w-unique-4";
		on_doc_deleted(root, id, "thoughts");
		assert!(drain_only(root, id), "on_doc_deleted must mark dirty");
	}

	#[test]
	fn tag_index_returns_docs_with_tag() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		store::create_document(root, "thoughts", "A", "a", vec!["x".into()], None, None).unwrap();
		store::create_document(root, "thoughts", "B", "b", vec!["x".into()], None, None).unwrap();
		store::create_document(root, "thoughts", "C", "c", vec!["y".into()], None, None).unwrap();
		let hits = tag_index_lookup(root, "x");
		assert_eq!(hits.len(), 2);
		assert!(hits.iter().all(|d| d.doc_type == "thoughts"));
	}

	#[test]
	fn tag_index_invalidated_on_create() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		store::create_document(root, "thoughts", "A", "a", vec!["t".into()], None, None).unwrap();
		assert_eq!(tag_index_lookup(root, "t").len(), 1);
		store::create_document(root, "thoughts", "B", "b", vec!["t".into()], None, None).unwrap();
		assert_eq!(tag_index_lookup(root, "t").len(), 2);
	}

	#[test]
	fn tag_index_invalidated_on_delete() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let d = store::create_document(root, "thoughts", "A", "a", vec!["k".into()], None, None).unwrap();
		assert_eq!(tag_index_lookup(root, "k").len(), 1);
		store::delete_document(root, "thoughts", &d.id).unwrap();
		assert_eq!(tag_index_lookup(root, "k").len(), 0);
	}

	#[test]
	fn reason_index_from_and_to() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
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
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
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
