//! Process-global caches for retrieval hot paths.
//!
//! All caches are keyed by a single wiki root (the binary serves one vault
//! per process). Mutations in `store` call `invalidate_*` to keep state
//! coherent without TTLs or generation counters.

use crate::search::SearchIndex;
use crate::store::Document;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

const EXPAND_TTL: Duration = Duration::from_secs(300);
const HYDE_TTL: Duration = Duration::from_secs(300);

pub struct PoolEntry {
	pub doc_type: String,
	pub doc: Document,
	pub content_hash: String,
	pub vec: Vec<f32>,
}

#[derive(Default)]
struct Cache {
	search: Mutex<Option<Arc<SearchIndex>>>,
	pool: RwLock<HashMap<String, Arc<PoolEntry>>>,
	pool_seeded: Mutex<bool>,
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
	if doc_type == "entities" {
		invalidate_entities();
	}
}

pub fn on_doc_deleted(id: &str, doc_type: &str) {
	pool_remove(id);
	if doc_type == "entities" {
		invalidate_entities();
	}
}
