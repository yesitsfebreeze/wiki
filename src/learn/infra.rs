//! Shared types, thresholds, cursors, qa-timestamps, hash helpers, kind mapping,
//! and the entity index used across the learn passes.

use crate::cache;
use crate::io::fnv64;
use crate::{http, store};
use anyhow::Result;
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct RaisedQuestion {
	pub question_id: String,
	pub title: String,
	pub purpose: Option<String>,
	pub created: bool,
}

#[derive(Debug, Clone)]
pub struct AnswerCandidate {
	pub doc_id: String,
	pub doc_type: String,
	pub score: f32,
	pub kind: String,
	pub body: String,
}

/// Tunable knobs for `run_pass`. Defaults preserve prior env-driven behavior.
/// Pass `&PassConfig::default()` for the legacy values.
#[derive(Debug, Clone, Copy)]
pub struct PassConfig {
	/// Cosine ≥ this → `Answers` edge + mark question resolved.
	pub answer_threshold: f32,
	/// Cosine ≥ this and < `answer_threshold` → `Supports` edge / weak link floor.
	pub support_threshold: f32,
	/// Hard cap on LLM calls per `run_pass` invocation.
	pub qa_max_per_pass: usize,
	/// Merge into existing conclusion if cosine ≥ this.
	pub conclusion_merge_threshold: f32,
	/// Connect-step: emit typed edge for any LLM-scored neighbor ≥ this.
	pub edge_threshold: f32,
	/// Connect-step: number of semantic neighbors to fetch per doc.
	pub connect_k: usize,
	/// Enable LLM question raising during the pass. Off by default so ingest-
	/// driven passes stay quiet; enable for deliberate `/learn --raise` runs.
	pub raise_questions: bool,
}

impl Default for PassConfig {
	fn default() -> Self {
		Self {
			answer_threshold: 0.6,
			support_threshold: 0.3,
			qa_max_per_pass: 50,
			conclusion_merge_threshold: 0.92,
			edge_threshold: 0.7,
			connect_k: 5,
			raise_questions: false,
		}
	}
}

const DEFAULT_QUESTION_DEDUPE_THRESHOLD: f32 = 0.88;

pub(crate) fn question_dedupe_threshold() -> f32 {
	std::env::var("WIKI_QUESTION_DEDUPE_THRESHOLD")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(DEFAULT_QUESTION_DEDUPE_THRESHOLD)
}

#[derive(Deserialize, Debug, Clone)]
pub struct RaisedQItem {
	#[serde(default)]
	pub title: String,
	#[serde(default)]
	pub body: String,
}

#[derive(Deserialize, Debug)]
pub(crate) struct RaisedQResp {
	#[serde(default)]
	pub questions: Vec<RaisedQItem>,
}

#[derive(Clone)]
pub struct EntityRef {
	pub id: String,
	pub title: String,
	pub aliases: Vec<String>,
	pub slug: String,
	pub purpose: Option<String>,
	pub body_embedding: Option<Vec<f32>>,
}

pub(crate) fn dedupe_threshold() -> f32 {
	std::env::var("WIKI_DEDUPE_THRESHOLD")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(0.85)
}

pub(crate) fn alias_threshold() -> f32 {
	std::env::var("WIKI_ALIAS_THRESHOLD")
		.ok()
		.and_then(|s| s.parse::<f32>().ok())
		.unwrap_or(0.92)
}

pub(crate) fn read_entity_meta(root: &Path, id: &str) -> Option<(Vec<String>, String, Option<String>)> {
	let dir = root.join("entities");
	for path in store::walk_md_paths(&dir) {
		let Ok(raw) = std::fs::read_to_string(&path) else { continue };
		let Ok((fm, _)) = store::parse_frontmatter(&raw) else { continue };
		if fm.get("id").and_then(|v| v.as_str()) != Some(id) {
			continue;
		}
		let aliases = fm
			.get("aliases")
			.and_then(|v| v.as_array())
			.map(|a| {
				a.iter()
					.filter_map(|v| v.as_str().map(String::from))
					.collect()
			})
			.unwrap_or_default();
		let slug = path
			.file_stem()
			.and_then(|s| s.to_str())
			.map(String::from)
			.unwrap_or_else(|| id.to_string());
		let purpose = fm.get("purpose").and_then(|v| v.as_str()).map(String::from);
		return Some((aliases, slug, purpose));
	}
	None
}

pub async fn build_entity_index(root: &Path) -> Result<Arc<Vec<EntityRef>>> {
	if let Some(cached) = cache::entity_index_get(root) {
		return Ok(cached);
	}
	let entities = store::list_documents(root, "entities")?;
	let mut refs: Vec<EntityRef> = Vec::new();
	let mut texts: Vec<String> = Vec::new();
	for e in entities {
		let (aliases, slug, purpose) = read_entity_meta(root, &e.id)
			.unwrap_or_else(|| (Vec::new(), e.id.clone(), e.purpose.clone()));
		texts.push(e.content.clone());
		refs.push(EntityRef {
			id: e.id.clone(),
			title: e.title.clone(),
			aliases,
			slug,
			purpose,
			body_embedding: None,
		});
	}
	if !texts.is_empty() {
		if let Ok(embs) = http::embed_batch(&texts).await {
			for (r, emb) in refs.iter_mut().zip(embs.into_iter()) {
				r.body_embedding = Some(emb);
			}
		}
	}
	Ok(cache::entity_index_set(root, refs))
}

// ── Feedback structs (used by feedback module) ───────────────────────────────

#[derive(Deserialize, Debug)]
pub(crate) struct FeedbackEntry {
	pub question: String,
	#[serde(default)]
	pub tag_filter: Option<String>,
	#[serde(default)]
	pub picked: Vec<String>,
	#[serde(default)]
	pub reasons: Vec<(String, String)>,
}

#[derive(Debug)]
pub(crate) struct LlmEdge {
	pub picked_id: String,
	pub score: f32,
	pub kind: String,
	pub body: String,
}

#[derive(Debug)]
pub(crate) struct LlmDecision {
	pub keep_question: bool,
	pub question_title: Option<String>,
	pub question_body: Option<String>,
	pub purpose: Option<String>,
	pub answered: bool,
	pub edges: Vec<LlmEdge>,
}

// ── Cursors ──────────────────────────────────────────────────────────────────

pub(crate) fn read_cursor(root: &Path) -> u64 {
	let p = root.join(".feedback.cursor");
	std::fs::read_to_string(&p)
		.ok()
		.and_then(|s| s.trim().parse().ok())
		.unwrap_or(0)
}

pub(crate) fn write_cursor(root: &Path, off: u64) -> Result<()> {
	crate::io::write_atomic_str(&root.join(".feedback.cursor"), &off.to_string())
}

fn pass_cursor_path(root: &Path, key: &str) -> std::path::PathBuf {
	let safe: String = key.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '<' || c == '>' { c } else { '_' }).collect();
	root.join(format!(".learn.cursor.{}", safe))
}

/// Read per-purpose pass cursor. Returns Some((doc_type, id)) if present.
/// Cursor is now diagnostic-only (sampling is random, not sequential).
#[allow(dead_code)]
pub(crate) fn read_pass_cursor(root: &Path, key: &str) -> Option<(String, String)> {
	let raw = std::fs::read_to_string(pass_cursor_path(root, key)).ok()?;
	let mut lines = raw.lines();
	let first = lines.next()?.trim();
	let (dt, id) = first.split_once('/')?;
	if dt.is_empty() || id.is_empty() {
		return None;
	}
	Some((dt.to_string(), id.to_string()))
}

pub(crate) fn write_pass_cursor(root: &Path, key: &str, doc_type: &str, id: &str) -> Result<()> {
	let body = format!("{}/{}\n{}\n", doc_type, id, chrono::Utc::now().to_rfc3339());
	crate::io::write_atomic_str(&pass_cursor_path(root, key), &body)
}

/// Returns true if the doc has a `last_qa_at` frontmatter field within the last 24h.
pub(crate) fn doc_qa_is_recent(root: &Path, doc_type: &str, id: &str) -> bool {
	let dir = root.join(doc_type);
	let Ok(path) = store::find_document_path_by_id(&dir, id) else { return false };
	let Ok(raw) = std::fs::read_to_string(&path) else { return false };
	let Ok((fm, _)) = store::parse_frontmatter(&raw) else { return false };
	let Some(ts) = fm.get("last_qa_at").and_then(|v| v.as_str()) else { return false };
	let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) else { return false };
	let age = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
	age.num_hours() < 24 && age.num_seconds() >= 0
}

/// Write `last_qa_at: <now-rfc3339>` into the doc's frontmatter (preserves body + other fm).
pub(crate) fn stamp_last_qa_at(root: &Path, doc_type: &str, id: &str) -> Result<()> {
	let dir = root.join(doc_type);
	let path = store::find_document_path_by_id(&dir, id)?;
	let raw = std::fs::read_to_string(&path)?;
	let (mut fm, body) = store::parse_frontmatter(&raw)?;
	let now = chrono::Utc::now().to_rfc3339();
	if let Some(obj) = fm.as_object_mut() {
		obj.insert("last_qa_at".to_string(), serde_json::json!(now));
	} else {
		let mut m = serde_json::Map::new();
		m.insert("id".to_string(), serde_json::json!(id));
		m.insert("last_qa_at".to_string(), serde_json::json!(now));
		fm = serde_json::Value::Object(m);
	}
	let fm_str = serde_yaml::to_string(&fm)?;
	crate::io::write_atomic_str(&path, &format!("---\n{}---\n\n{}", fm_str, body))?;
	Ok(())
}

pub(crate) fn fnv_question_id(q: &str) -> String {
	format!("q-{:x}", fnv64(q.trim()))
}

pub(crate) fn find_question_by_hash(root: &Path, hash_id: &str) -> Option<String> {
	if let Some(id) = cache::hash_index_lookup(root, hash_id)
		.into_iter()
		.find(|d| d.doc_type == "questions")
		.map(|d| d.id)
	{
		return Some(id);
	}
	store::list_documents(root, "questions")
		.ok()?
		.into_iter()
		.find(|d| d.tags.iter().any(|t| t == hash_id))
		.map(|d| d.id)
}

pub(crate) fn find_conclusion_by_hash(root: &Path, hash_id: &str) -> Option<String> {
	if let Some(id) = cache::hash_index_lookup(root, hash_id)
		.into_iter()
		.find(|d| d.doc_type == "conclusions")
		.map(|d| d.id)
	{
		return Some(id);
	}
	store::list_documents(root, "conclusions")
		.ok()?
		.into_iter()
		.find(|d| d.tags.iter().any(|t| t == hash_id))
		.map(|d| d.id)
}

pub(crate) fn allowed_kind(k: &str) -> &'static str {
	match k.to_lowercase().as_str() {
		"answers" => "Answers",
		"supports" => "Supports",
		"contradicts" => "Contradicts",
		"extends" => "Extends",
		"requires" => "Requires",
		"references" => "References",
		"derives" => "Derives",
		"instances" => "Instances",
		_ => "References",
	}
}

/// Read a reason doc's frontmatter and return `(from_id, to_id, kind, purpose)`.
pub(crate) fn read_reason_meta(root: &Path, reason_id: &str) -> Option<(String, String, String, Option<String>)> {
	let dir = root.join("reasons");
	let path = store::find_document_path_by_id(&dir, reason_id).ok()?;
	let raw = std::fs::read_to_string(&path).ok()?;
	let (fm, _) = store::parse_frontmatter(&raw).ok()?;
	let from_id = fm.get("from_id").and_then(|v| v.as_str())?.to_string();
	let to_id = fm.get("to_id").and_then(|v| v.as_str())?.to_string();
	let kind = fm.get("kind").and_then(|v| v.as_str())?.to_string();
	let purpose = fm.get("purpose").and_then(|v| v.as_str()).map(String::from);
	Some((from_id, to_id, kind, purpose))
}

pub(crate) fn write_pass_log(root: &Path, kind: &str, report: &serde_json::Value) -> Result<()> {
	let log_dir = root.join("ingest_log");
	std::fs::create_dir_all(&log_dir)?;
	let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
	let json = serde_json::to_string_pretty(report)?;
	crate::io::write_atomic_str(&log_dir.join(format!("{}-{}.json", kind, ts)), &json)?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	#[test]
	fn fnv64_stable() {
		assert_eq!(fnv64("abc"), fnv64("abc"));
		assert_ne!(fnv64("abc"), fnv64("abd"));
	}

	#[test]
	fn fnv_question_id_stable() {
		assert_eq!(fnv_question_id("Why is the sky blue?"), fnv_question_id("Why is the sky blue?"));
		assert_eq!(fnv_question_id(" Why is the sky blue? "), fnv_question_id("Why is the sky blue?"));
		assert_ne!(fnv_question_id("Why?"), fnv_question_id("How?"));
		assert!(fnv_question_id("Q").starts_with("q-"));
	}

	#[test]
	fn allowed_kind_extends_answers() {
		assert_eq!(allowed_kind("answers"), "Answers");
		assert_eq!(allowed_kind("Answers"), "Answers");
		assert_eq!(allowed_kind("supports"), "Supports");
		assert_eq!(allowed_kind("contradicts"), "Contradicts");
		assert_eq!(allowed_kind("extends"), "Extends");
		assert_eq!(allowed_kind("garbage"), "References");
	}

	#[test]
	fn raise_questions_dedup_via_hash() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let title = "Does borrowing prevent data races?";
		let hash = fnv_question_id(title);
		let tags = vec!["question".to_string(), "general".to_string(), hash.clone()];
		let q = store::create_document(root, "questions", title, "body", tags, Some("general"), None).unwrap();
		let found = find_question_by_hash(root, &hash);
		assert_eq!(found, Some(q.id));
		assert!(find_question_by_hash(root, "q-deadbeef").is_none());
	}
}
