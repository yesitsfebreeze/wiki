#![allow(dead_code)]
use crate::{cache, chunker, classifier, code, io as wiki_io, learn, sanitize, search, smart, store};
use anyhow::Result;
use rmcp::{handler::server::wrapper::Parameters, tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct WikiService {
	wiki_path: PathBuf,
}

/// Pull a clean ≤60-char title from a doc body. Strips leading markdown
/// (`#`, `*`, `-`, `>`), wikilinks (`[[...]]`), and bracketed tags
/// (`[gap]`, `[established]`). Cuts at first sentence terminator or word
/// boundary near the 60-char budget. Returns `fallback` if nothing usable.
pub(crate) fn derive_title(body: &str, fallback: &str) -> String {
	const MAX: usize = 60;
	let mut s = body.trim_start();
	loop {
		let prev = s;
		s = s.trim_start_matches(|c: char| matches!(c, '#' | '*' | '-' | '>' | ' ' | '\t' | '\r' | '\n'));
		// Strip leading [[wikilink]]
		if let Some(rest) = s.strip_prefix("[[") {
			if let Some(end) = rest.find("]]") {
				s = &rest[end + 2..];
				s = s.trim_start();
				continue;
			}
		}
		// Strip leading [bracket-tag] (no nested brackets, no spaces inside).
		if let Some(rest) = s.strip_prefix('[') {
			if let Some(end) = rest.find(']') {
				let inner = &rest[..end];
				if !inner.contains('[') && inner.len() <= 32 {
					s = &rest[end + 1..];
					s = s.trim_start();
					continue;
				}
			}
		}
		if s == prev {
			break;
		}
	}
	let cut = s
		.find(|c: char| matches!(c, '.' | '?' | '!' | '\n'))
		.unwrap_or(s.len());
	let candidate = s[..cut].trim();
	if candidate.is_empty() {
		return fallback.to_string();
	}
	if candidate.chars().count() <= MAX {
		return candidate.to_string();
	}
	let mut chars: Vec<char> = candidate.chars().take(MAX).collect();
	while let Some(&last) = chars.last() {
		if last.is_whitespace() {
			chars.pop();
		} else {
			break;
		}
	}
	if let Some(last_space) = chars.iter().rposition(|c| c.is_whitespace()) {
		if last_space >= MAX / 2 {
			chars.truncate(last_space);
		}
	}
	let mut out: String = chars.into_iter().collect();
	out.push('…');
	out
}

// ── Param structs ────────────────────────────────────────────────────────────

#[derive(Deserialize, JsonSchema)]
struct DocsParams {
	#[serde(default)]
	name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct DocRefParams {
	doc_type: String,
	id: String,
}

#[derive(Deserialize, JsonSchema)]
struct ListLogParams {
	limit: Option<u64>,
	cursor: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct UpdateDocParams {
	doc_type: String,
	id: String,
	content: Option<String>,
	tags: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
struct MarkQuestionParams {
	question_id: String,
	status: String,
}

#[derive(Deserialize, JsonSchema)]
struct SuggestConclusionParams {
	entity_id: String,
}

#[derive(Deserialize, JsonSchema)]
struct PurposeParams {
	/// list | create | delete | reembed
	action: String,
	tag: Option<String>,
	title: Option<String>,
	description: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct CodeParams {
	/// index | search | read | refs | validate
	action: String,
	// index
	src_dir: Option<String>,
	ext: Option<String>,
	// search
	query: Option<String>,
	regex: Option<bool>,
	scope: Option<String>,
	// read
	path: Option<String>,
	symbol: Option<String>,
	granularity: Option<String>,
	// refs
	direction: Option<String>,
	depth: Option<u64>,
	// validate
	fix: Option<bool>,
	// shared pagination
	cursor: Option<u64>,
	limit: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct AdminParams {
	/// recompute | sanitize | migrate | feedback
	action: String,
	dry_run: Option<bool>,
	limit: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct CodeListBodiesParams {
	dir: String,
	glob: Option<String>,
	min_loc: Option<u64>,
	max_loc: Option<u64>,
	sort: Option<String>,
	cursor: Option<u64>,
	limit: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct CodeFindLargeParams {
	max_loc: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct LearnPassParams {
	limit: Option<u64>,
	purpose: Option<String>,
	dry_run: Option<bool>,
	qa: Option<bool>,
	force: Option<bool>,
	/// Enable LLM question raising during the pass. Default `false`.
	raise_questions: Option<bool>,
	/// Cosine ≥ this → `Answers` edge + mark question resolved. Default `0.8`.
	answer_threshold: Option<f32>,
	/// Cosine ≥ this and < `answer_threshold` → `Supports` edge. Default `0.3`.
	support_threshold: Option<f32>,
	/// Connect-step: emit typed edge for any LLM-scored neighbor ≥ this. Default `0.7`.
	edge_threshold: Option<f32>,
	/// Connect-step: number of semantic neighbors per doc. Default `5`.
	connect_k: Option<u64>,
	/// Hard cap on LLM calls per pass. Default `50`.
	qa_max_per_pass: Option<u64>,
	/// Merge into existing conclusion if cosine ≥ this. Default `0.92`.
	conclusion_merge_threshold: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
struct SearchParams {
	query: String,
	/// smart | fts | tag | qa | list
	mode: Option<String>,
	k: Option<u64>,
	include_bodies: Option<bool>,
	include_reasons: Option<bool>,
	edges_depth: Option<u64>,
	/// Required when mode="list". One of: thoughts|entities|questions|conclusions|reasons.
	doc_type: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct GetParams {
	id: String,
	doc_type: Option<String>,
	depth: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct IngestParams {
	kind: String,
	title: Option<String>,
	body: String,
	tags: Option<Vec<String>>,
	refs: Option<Vec<String>>,
	purpose_hint: Option<String>,
	from_id: Option<String>,
	to_id: Option<String>,
	reason_kind: Option<String>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Resolve a `symbol` argument for `code_refs` to an indexed file path.
///
/// Accepts (in priority order):
/// 1. An existing path on disk → use as-is.
/// 2. A bare fn name (`my_fn`, `module::my_fn`) → walk the code index for a
///    body file whose stem matches. Prefer exact stem match; fall back to a
///    trailing-segment match (`module::my_fn` → `.../my_fn.md`).
/// 3. A path-shaped string that doesn't exist → return as-is and let `ref_graph`
///    surface the I/O error.
///
/// Returns `(path, resolution_kind)` where resolution_kind is one of
/// `"path"` (direct), `"symbol_exact"`, `"symbol_suffix"`, or `"unresolved"`.
fn resolve_symbol_path(symbol: &str) -> (PathBuf, &'static str) {
	let direct = PathBuf::from(symbol);
	if direct.exists() {
		return (direct, "path");
	}
	let stem = symbol.rsplit("::").next().unwrap_or(symbol).trim();
	if stem.is_empty() {
		return (direct, "unresolved");
	}
	let target_filename = format!("{stem}.md");
	let index = crate::code::default_index_dir();
	let Ok(rd) = std::fs::read_dir(&index) else {
		return (direct, "unresolved");
	};
	let mut best: Option<PathBuf> = None;
	for ext_entry in rd.filter_map(|e| e.ok()) {
		let fns_dir = ext_entry.path().join("functions");
		if !fns_dir.is_dir() {
			continue;
		}
		if let Some(found) = find_body_by_filename(&fns_dir, &target_filename) {
			return (found, "symbol_exact");
		}
		// Fallback: any file whose stem ends with the target stem (e.g. nested modules).
		if best.is_none() {
			best = find_body_with_suffix(&fns_dir, stem);
		}
	}
	match best {
		Some(p) => (p, "symbol_suffix"),
		None => (direct, "unresolved"),
	}
}

fn find_body_by_filename(dir: &Path, target: &str) -> Option<PathBuf> {
	let rd = std::fs::read_dir(dir).ok()?;
	for entry in rd.filter_map(|e| e.ok()) {
		let p = entry.path();
		if p.is_dir() {
			if let Some(hit) = find_body_by_filename(&p, target) {
				return Some(hit);
			}
		} else if p.file_name().and_then(|s| s.to_str()) == Some(target) {
			return Some(p);
		}
	}
	None
}

fn find_body_with_suffix(dir: &Path, stem: &str) -> Option<PathBuf> {
	let rd = std::fs::read_dir(dir).ok()?;
	for entry in rd.filter_map(|e| e.ok()) {
		let p = entry.path();
		if p.is_dir() {
			if let Some(hit) = find_body_with_suffix(&p, stem) {
				return Some(hit);
			}
		} else if p.extension().and_then(|s| s.to_str()) == Some("md") {
			if let Some(s) = p.file_stem().and_then(|s| s.to_str()) {
				if s == stem || s.ends_with(stem) {
					return Some(p);
				}
			}
		}
	}
	None
}

fn auto_invariants_enabled() -> bool {
	std::env::var("WIKI_AUTO_INVARIANTS").map(|v| v != "0").unwrap_or(true)
}

const KNOWN_DOC_TYPES: &[&str] = &["thoughts", "entities", "questions", "conclusions", "reasons"];

fn doc_type_from_base_tag(tag: &str) -> String {
	match tag {
		"thought" => "thoughts",
		"entity" => "entities",
		"question" => "questions",
		"conclusion" => "conclusions",
		"reason" => "reasons",
		other => other,
	}.to_string()
}


const DEFAULT_LIST_LIMIT: usize = 25;
const SNIPPET_CHARS: usize = 600;

fn json_err(e: impl std::fmt::Display) -> String {
	serde_json::json!({ "error": e.to_string() }).to_string()
}

fn to_json<T: serde::Serialize>(v: &T) -> String {
	serde_json::to_string(v).unwrap_or_else(json_err)
}

/// Render a doc as a paginated-list entry: snippet, not full content.
/// Callers should use `get` to fetch full bodies.
fn doc_preview(d: &store::Document) -> serde_json::Value {
	serde_json::json!({
		"id": d.id,
		"title": d.title,
		"tags": d.tags,
		"purpose": d.purpose,
		"snippet": wiki_io::truncate_chars(&d.content, SNIPPET_CHARS),
		"len": d.content.len(),
	})
}

/// Slice + envelope: applies cursor + limit and returns a JSON object with
/// a `next_cursor` (only present when more remains). Keeps tool output
/// deterministically small.
fn paginate<T: serde::Serialize>(
	items: Vec<T>,
	cursor: Option<u64>,
	limit: Option<u64>,
) -> serde_json::Value {
	let total = items.len();
	let cur = cursor.unwrap_or(0) as usize;
	let lim = limit.map(|n| n as usize).unwrap_or(DEFAULT_LIST_LIMIT).max(1);
	let cur = cur.min(total);
	let end = (cur + lim).min(total);
	let mut iter = items.into_iter();
	for _ in 0..cur { iter.next(); }
	let page: Vec<T> = iter.take(end - cur).collect();
	let mut out = serde_json::json!({
		"total": total,
		"cursor": cur,
		"returned": page.len(),
		"items": page,
	});
	if end < total {
		out["next_cursor"] = serde_json::json!(end);
	}
	out
}

impl WikiService {
	fn root(&self) -> &std::path::Path {
		&self.wiki_path
	}

	fn try_index_doc(&self, doc: &store::Document) {
		if let Ok(index) = cache::search_index(self.root()) {
			let _ = search::index_document(&index, doc);
		}
	}

	async fn classify_or_hint(&self, hint: Option<&str>, content: &str) -> String {
		if let Some(h) = hint {
			return h.to_string();
		}
		match classifier::classify(self.root(), &[content.to_string()]).await {
			Ok(mut v) => v.pop().unwrap_or_else(|| "general".to_string()),
			Err(_) => "general".to_string(),
		}
	}

	fn resolve_doc_type(&self, hint: Option<&str>, id: &str) -> Option<String> {
		if let Some(h) = hint {
			if !h.is_empty() { return Some(h.to_string()); }
		}
		for dt in KNOWN_DOC_TYPES {
			if store::get_document(self.root(), dt, id).is_ok() {
				return Some((*dt).to_string());
			}
		}
		None
	}

	/// Walk an `ingest_chunked` JSON result, collect any doc IDs it created,
	/// then run `learn::link_doc` on each (best-effort). Returns the per-doc
	/// auto-link summary array.
	async fn collect_auto_link_ids(&self, ingested: &serde_json::Value) -> serde_json::Value {
		if !auto_invariants_enabled() {
			return serde_json::json!([]);
		}
		let mut entries: Vec<(String, String)> = Vec::new(); // (doc_type, id)
		// Single-doc shape: {id, tags:[base_tag,...], ...}
		if let Some(id) = ingested.get("id").and_then(|v| v.as_str()) {
			let dt = ingested.get("tags").and_then(|t| t.as_array())
				.and_then(|a| a.first()).and_then(|v| v.as_str())
				.map(doc_type_from_base_tag)
				.unwrap_or_else(|| "thoughts".to_string());
			entries.push((dt, id.to_string()));
		}
		// Multi-chunk shape: {parent: {id, tags}, chunks: [{id,...}]}
		if let Some(parent) = ingested.get("parent") {
			if let Some(id) = parent.get("id").and_then(|v| v.as_str()) {
				let dt = parent.get("tags").and_then(|t| t.as_array())
					.and_then(|a| a.first()).and_then(|v| v.as_str())
					.map(doc_type_from_base_tag)
					.unwrap_or_else(|| "thoughts".to_string());
				entries.push((dt, id.to_string()));
			}
		}
		if let Some(chunks) = ingested.get("chunks").and_then(|v| v.as_array()) {
			for c in chunks {
				if let Some(id) = c.get("id").and_then(|v| v.as_str()) {
					entries.push(("thoughts".to_string(), id.to_string()));
				}
			}
		}
		let mut out = Vec::new();
		for (dt, id) in entries {
			match learn::link_doc(self.root(), &dt, &id, false).await {
				Ok(v) => out.push(serde_json::json!({"id": id, "doc_type": dt, "result": v})),
				Err(e) => out.push(serde_json::json!({"id": id, "doc_type": dt, "error": e.to_string()})),
			}
		}
		serde_json::json!(out)
	}

	async fn ingest_chunked(
		&self,
		doc_type: &str,
		title: &str,
		content: &str,
		base_tag: &str,
		hint: Option<&str>,
	) -> serde_json::Value {
		let chunks = if let Some(h) = hint {
			vec![chunker::Chunk {
				purpose: h.to_string(),
				content: content.to_string(),
			}]
		} else {
			match chunker::chunk_by_purpose(self.root(), content).await {
				Ok(c) if !c.is_empty() => c,
				Ok(_) => vec![chunker::Chunk {
					purpose: "general".to_string(),
					content: content.to_string(),
				}],
				Err(e) => return serde_json::json!({ "error": e.to_string() }),
			}
		};

		if chunks.iter().any(|c| c.purpose == "general") {
			let _ = classifier::ensure_general_purpose(self.root());
		}

		if chunks.len() == 1 {
			let c = &chunks[0];
			let tags = vec![base_tag.to_string(), c.purpose.clone()];
			match store::create_document(
				self.root(), doc_type, title, &c.content, tags, Some(&c.purpose), None,
			) {
				Ok(doc) => {
					self.try_index_doc(&doc);
					let _ = store::log_ingest(self.root(), doc_type, &doc.id, &doc.title);
					return serde_json::json!(doc);
				}
				Err(e) => return serde_json::json!({ "error": e.to_string() }),
			}
		}

		let parent_tags = vec![base_tag.to_string(), "multi-purpose".to_string()];
		let parent = match store::create_document(
			self.root(), doc_type, title, content, parent_tags, None, None,
		) {
			Ok(d) => d,
			Err(e) => return serde_json::json!({ "error": e.to_string() }),
		};
		self.try_index_doc(&parent);
		let _ = store::log_ingest(self.root(), doc_type, &parent.id, &parent.title);

		let mut children = Vec::new();
		for (i, chunk) in chunks.iter().enumerate() {
			let child_title = format!("{} [{}#{}]", title, chunk.purpose, i + 1);
			let tags = vec![base_tag.to_string(), chunk.purpose.clone()];
			if let Ok(child) = store::create_document(
				self.root(),
				doc_type,
				&child_title,
				&chunk.content,
				tags,
				Some(&chunk.purpose),
				Some(&parent.id),
			) {
				self.try_index_doc(&child);
				let _ = store::log_ingest(self.root(), doc_type, &child.id, &child.title);
				let _ = store::create_reason(
					self.root(),
					&child.id,
					&parent.id,
					"PartOf",
					&format!("Chunk {} of '{}' (purpose: {})", i + 1, title, chunk.purpose),
					Some(&chunk.purpose),
				);
				children.push(serde_json::json!({
					"id": child.id,
					"purpose": chunk.purpose,
					"title": child_title,
				}));
			}
		}

		serde_json::json!({
			"parent": parent,
			"chunks": children,
			"chunk_count": chunks.len(),
		})
	}
}

// ── Tool implementations ─────────────────────────────────────────────────────

#[tool_router]
impl WikiService {
	#[tool(description = "Fetch markdown docs. No arg → list available names. With name → body. Resolves tools/<name>.md then concepts/<name>.md. Args: name?.")]
	fn docs(&self, params: Parameters<DocsParams>) -> String {
		match params.0.name {
			None => serde_json::json!({ "docs": crate::docs::list() }).to_string(),
			Some(name) => match crate::docs::read(&name) {
				Some(body) => body.to_string(),
				None => json_err(format!("Doc not found: {name}. Call `docs` with no name for index.")),
			},
		}
	}

	#[tool(description = "Manage purposes. action: list | create | delete | reembed. Docs: docs(\"purpose\"). Args: action, tag?, title?, description?.")]
	async fn purpose(&self, params: Parameters<PurposeParams>) -> String {
		let PurposeParams { action, tag, title, description } = params.0;
		match action.as_str() {
			"list" => match store::list_purposes(self.root()) {
				Ok(p) => to_json(&p),
				Err(e) => json_err(e),
			},
			"create" => {
				let (Some(tag), Some(title), Some(description)) = (tag, title, description) else {
					return json_err("create requires tag, title, description");
				};
				match store::create_purpose(self.root(), &tag, &title, &description) {
					Ok(p) => to_json(&p),
					Err(e) => json_err(e),
				}
			},
			"delete" => {
				let Some(tag) = tag else { return json_err("delete requires tag"); };
				match store::delete_purpose(self.root(), &tag) {
					Ok(_) => format!("Purpose '{}' deleted", tag),
					Err(e) => json_err(e),
				}
			},
			"reembed" => {
				if let Ok(purposes) = store::list_purposes(self.root()) {
					for p in &purposes {
						let _ = std::fs::remove_file(p.path.with_extension("vec"));
					}
				}
				match classifier::ensure_purpose_embeddings(self.root()).await {
					Ok(v) => format!("Re-embedded {} purposes", v.len()),
					Err(e) => json_err(e),
				}
			},
			other => json_err(format!("Unknown action: {} (list|create|delete|reembed)", other)),
		}
	}

	#[tool(description = "Update doc body, title, or tags. Re-embeds + re-links body. Docs: docs(\"update\"). Args: id, doc_type?, body?, title?, tags?, edges?.")]
	async fn update(&self, params: Parameters<UpdateDocParams>) -> String {
		let UpdateDocParams { doc_type, id, content, tags } = params.0;
		match store::update_document(self.root(), &doc_type, &id, content.as_deref(), tags) {
			Ok(doc) => {
				self.try_index_doc(&doc);
				let mut out = serde_json::json!(doc);
				if auto_invariants_enabled() && content.is_some() {
					if let Ok(v) = learn::link_doc(self.root(), &doc_type, &id, false).await {
						out["auto_linked"] = v;
					}
				}
				out.to_string()
			}
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Delete a doc and cascade edge cleanup. Docs: docs(\"delete_doc\"). Args: id, doc_type?.")]
	fn delete_doc(&self, params: Parameters<DocRefParams>) -> String {
		let DocRefParams { doc_type, id } = params.0;
		match store::delete_document(self.root(), &doc_type, &id) {
			Ok(_) => format!("Deleted {} '{}'", doc_type, id),
			Err(e) => json_err(e),
		}
	}

	fn list_ingest_log(&self, params: Parameters<ListLogParams>) -> String {
		let ListLogParams { limit, cursor } = params.0;
		let log_dir = self.root().join("ingest_log");
		if !log_dir.exists() {
			return paginate::<serde_json::Value>(vec![], cursor, limit).to_string();
		}
		let entries: Vec<serde_json::Value> = std::fs::read_dir(&log_dir)
			.into_iter()
			.flatten()
			.filter_map(|e| e.ok())
			.filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
			.filter_map(|e| std::fs::read_to_string(e.path()).ok())
			.filter_map(|s| serde_json::from_str(&s).ok())
			.collect();
		paginate(entries, cursor, limit).to_string()
	}

	#[tool(description = "List unresolved questions. Docs: docs(\"list_open_questions\"). Args: purpose?, limit?, cursor?.")]
	fn list_open_questions(&self, params: Parameters<ListLogParams>) -> String {
		let ListLogParams { limit, cursor } = params.0;
		match store::list_documents(self.root(), "questions") {
			Ok(docs) => {
				let open: Vec<serde_json::Value> = docs.iter()
					.filter(|d| !d.tags.iter().any(|t| t == "answered" || t == "dropped"))
					.map(doc_preview)
					.collect();
				paginate(open, cursor, limit).to_string()
			}
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Manually set question state. Override only — learn_pass auto-marks. Docs: docs(\"mark_question\"). Args: id, state (answered|dropped).")]
	fn mark_question(&self, params: Parameters<MarkQuestionParams>) -> String {
		let MarkQuestionParams { question_id, status } = params.0;
		const VALID: &[&str] = &["answered", "dropped"];
		if !VALID.contains(&status.as_str()) {
			return json_err(format!("Invalid status: {}", status));
		}
		match store::get_document(self.root(), "questions", &question_id) {
			Ok(mut doc) => {
				doc.tags.retain(|t| !VALID.contains(&t.as_str()));
				doc.tags.push(status.clone());
				match store::update_document(self.root(), "questions", &question_id, None, Some(doc.tags.clone())) {
					Ok(_) => {
						if status == "answered" {
							let _ = learn::move_to_answered(self.root(), &question_id);
						} else if status == "dropped" {
							let _ = learn::move_to_dropped(self.root(), &question_id);
						}
						serde_json::json!({
							"question_id": question_id,
							"status": status,
						}).to_string()
					}
					Err(e) => json_err(e),
				}
			}
			Err(e) => json_err(e),
		}
	}

	fn suggest_conclusion(&self, params: Parameters<SuggestConclusionParams>) -> String {
		let entity_id = &params.0.entity_id;
		let entity = match store::get_document(self.root(), "entities", entity_id) {
			Ok(d) => d,
			Err(e) => return json_err(e),
		};

		let reasons = store::search_reasons_for(self.root(), entity_id, "both").unwrap_or_default();
		let questions = store::list_documents(self.root(), "questions").unwrap_or_default();
		let related: Vec<_> = questions.iter()
			.filter(|q| q.content.contains(&entity.title) || q.title.contains(&entity.title))
			.cloned()
			.collect();
		let answered_count = related.iter()
			.filter(|q| q.tags.iter().any(|t| t == "answered"))
			.count();

		let can_conclude = !reasons.is_empty() && answered_count >= 2;
		serde_json::json!({
			"entity_id": entity_id,
			"entity_title": entity.title,
			"supporting_reasons": reasons.len(),
			"related_questions": related.len(),
			"answered_questions": answered_count,
			"can_conclude": can_conclude,
		}).to_string()
	}

	// ── Code tool ───────────────────────────────────────────────────────────

	#[tool(description = "Read docs(\"code\") first. Code index ops. action: index | search | read | refs | validate. Docs: docs(\"code\"). Args: action, src_dir?, ext?, query?, regex?, scope?, path?, symbol?, granularity?, direction?, depth?, fix?, cursor?, limit?.")]
	fn code(&self, params: Parameters<CodeParams>) -> String {
		let CodeParams { action, src_dir, ext, query, regex, scope, path, symbol, granularity, direction, depth, fix, cursor, limit } = params.0;
		match action.as_str() {
			"index" => {
				let Some(src_dir) = src_dir else { return json_err("index requires src_dir"); };
				let ext = ext.unwrap_or_else(|| "rs".to_string());
				match code::index_dir(&PathBuf::from(src_dir), &ext) {
					Ok(s) => s,
					Err(e) => format!("Error: {e}"),
				}
			},
			"search" => {
				let Some(query) = query else { return json_err("search requires query"); };
				let scope = scope.unwrap_or_else(|| "body".to_string());
				let cursor = cursor.unwrap_or(0) as usize;
				let limit = limit.map(|n| n as usize).unwrap_or(100);
				match code::search_bodies(&query, regex.unwrap_or(false), &scope, cursor, limit) {
					Ok(s) => s,
					Err(e) => format!("Error: {e}"),
				}
			},
			"read" => {
				let g = granularity.unwrap_or_else(|| "outline".to_string());
				let p = match path.as_deref().or(symbol.as_deref()) {
					Some(s) => PathBuf::from(s),
					None => return json_err("read requires path or symbol"),
				};
				let result = match g.as_str() {
					"outline" => code::outline(&p),
					"file" => {
						let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("rs").to_string();
						code::open_source(&p, &ext)
					},
					"fn" => code::read_body(&p, 1, None),
					other => return json_err(format!("Unknown granularity: {} (outline|file|fn)", other)),
				};
				match result {
					Ok(s) => s,
					Err(e) => json_err(e),
				}
			},
			"refs" => {
				let Some(symbol) = symbol.or(path) else { return json_err("refs requires symbol"); };
				let dir = direction.unwrap_or_else(|| "both".to_string());
				let (p, resolution) = resolve_symbol_path(&symbol);
				let mut out = serde_json::Map::new();
				out.insert("resolved_path".into(), serde_json::json!(p.display().to_string().replace('\\', "/")));
				out.insert("resolution".into(), serde_json::json!(resolution));
				match code::ref_graph(&p, &dir) {
					Ok(s) => { out.insert("ref_graph".into(), serde_json::json!(s)); },
					Err(e) => return json_err(e),
				}
				let d = depth.unwrap_or(1);
				if d > 1 {
					match code::fn_tree(&p, d as usize) {
						Ok(s) => { out.insert("fn_tree".into(), serde_json::json!(s)); },
						Err(e) => { out.insert("fn_tree_error".into(), serde_json::json!(e.to_string())); },
					}
				}
				serde_json::Value::Object(out).to_string()
			},
			"validate" => match code::validate(fix.unwrap_or(false)) {
				Ok(s) => s,
				Err(e) => format!("Error: {e}"),
			},
			other => json_err(format!("Unknown action: {} (index|search|read|refs|validate)", other)),
		}
	}

	fn code_list_bodies(&self, params: Parameters<CodeListBodiesParams>) -> String {
		let CodeListBodiesParams { dir, glob, min_loc, max_loc, sort, cursor, limit } = params.0;
		let sort = sort.unwrap_or_else(|| "size".to_string());
		let cursor = cursor.unwrap_or(0) as usize;
		match code::list_bodies(
			&PathBuf::from(dir),
			glob.as_deref(),
			min_loc.map(|n| n as usize),
			max_loc.map(|n| n as usize),
			&sort,
			cursor,
			limit.map(|n| n as usize),
		) {
			Ok(s) => s,
			Err(e) => format!("Error: {e}"),
		}
	}

	fn code_find_large(&self, params: Parameters<CodeFindLargeParams>) -> String {
		match code::find_large(params.0.max_loc.map(|n| n as usize)) {
			Ok(s) => s,
			Err(e) => format!("Error: {e}"),
		}
	}

	fn code_list_languages(&self) -> String {
		code::list_languages()
	}


	#[tool(description = "Read docs(\"learn\") first. Run wiki sensemaker: link/dedupe → connect → raise/answer questions → promote conclusions. Returns report JSON. Args: limit?, purpose?, dry_run?, qa?, force?, raise_questions?, edge_threshold?, connect_k?, answer_threshold?, support_threshold?, qa_max_per_pass?, conclusion_merge_threshold?.")]
	async fn learn_pass(&self, params: Parameters<LearnPassParams>) -> String {
		let LearnPassParams {
			limit, purpose, dry_run, qa, force, raise_questions,
			answer_threshold, support_threshold, edge_threshold, connect_k,
			qa_max_per_pass, conclusion_merge_threshold,
		} = params.0;
		let limit = limit.map(|n| n as usize).unwrap_or(25);
		let defaults = learn::PassConfig::default();
		let cfg = learn::PassConfig {
			answer_threshold: answer_threshold.unwrap_or(defaults.answer_threshold),
			support_threshold: support_threshold.unwrap_or(defaults.support_threshold),
			qa_max_per_pass: qa_max_per_pass.map(|n| n as usize).unwrap_or(defaults.qa_max_per_pass),
			conclusion_merge_threshold: conclusion_merge_threshold.unwrap_or(defaults.conclusion_merge_threshold),
			edge_threshold: edge_threshold.unwrap_or(defaults.edge_threshold),
			connect_k: connect_k.map(|n| n as usize).unwrap_or(defaults.connect_k),
			raise_questions: raise_questions.unwrap_or(defaults.raise_questions),
		};
		match learn::run_pass(self.root(), limit, purpose.as_deref(), dry_run.unwrap_or(false), qa.unwrap_or(true), force.unwrap_or(false), &cfg).await {
			Ok(v) => v.to_string(),
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Vault maintenance. action: recompute | sanitize | migrate | feedback. Docs: docs(\"admin\"). Args: action, dry_run?, limit?.")]
	async fn admin(&self, params: Parameters<AdminParams>) -> String {
		let AdminParams { action, dry_run, limit } = params.0;
		let dry = dry_run.unwrap_or(false);
		match action.as_str() {
			"recompute" => match crate::weight::run_cli(self.root(), dry) {
				Ok(n) => serde_json::json!({ "recomputed": n, "dry_run": dry }).to_string(),
				Err(e) => json_err(e),
			},
			"sanitize" => match sanitize::sanitize_vault(self.root(), false) {
				Ok(report) => to_json(&report),
				Err(e) => json_err(e),
			},
			"migrate" => match learn::migrate_templated_questions(self.root(), dry) {
				Ok(rep) => to_json(&rep),
				Err(e) => json_err(e),
			},
			"feedback" => {
				let limit = limit.map(|n| n as usize).unwrap_or(25);
				match learn::run_feedback_pass(self.root(), limit, dry).await {
					Ok(v) => v.to_string(),
					Err(e) => json_err(e),
				}
			},
			other => json_err(format!("Unknown action: {} (recompute|sanitize|migrate|feedback)", other)),
		}
	}

	// ── New consolidated tools ──────────────────────────────────────────────

	#[tool(description = "Read docs(\"search\") first. Hybrid knowledge search + doc listing. mode: smart (conclusions-first, returns top-10 ranked hits with per-hit snippet + full body + edges — use these 10 hits to synthesize a conclusion) | fts (BM25) | tag | qa | list (paginate by doc_type). Args: query, mode?, k? (default 10), include_bodies?, include_reasons?, edges_depth?, doc_type?.")]
	async fn search(&self, params: Parameters<SearchParams>) -> String {
		let SearchParams { query, mode, k, include_bodies, include_reasons, edges_depth, doc_type } = params.0;
		let mode = mode.unwrap_or_else(|| "smart".to_string());

		// mode="list" — paginate all docs of a given type, no search needed
		if mode == "list" {
			let dt = doc_type.unwrap_or_else(|| query.clone());
			return match store::list_documents(self.root(), &dt) {
				Ok(docs) => {
					let previews: Vec<serde_json::Value> = docs.iter().map(doc_preview).collect();
					paginate(previews, None, k).to_string()
				}
				Err(e) => json_err(e),
			};
		}

		let k = k.map(|n| n as usize).unwrap_or(10);
		let want_bodies = include_bodies.unwrap_or(true);
		let want_reasons = include_reasons.unwrap_or(true);
		let depth = edges_depth.unwrap_or(1) as usize;

		// (id, doc_type, title, score, snippet)
		let raw_hits: Vec<(String, String, String, f64, String)> = match mode.as_str() {
			"smart" | "qa" => {
				let opts = smart::QueryOpts::default();
				let v = match smart::query_with_opts(self.root(), &query, None, k.max(20), k, &opts).await {
					Ok(v) => v,
					Err(e) => return json_err(e),
				};
				let empty: Vec<serde_json::Value> = Vec::new();
				v.get("results").and_then(|r| r.as_array()).unwrap_or(&empty).iter()
					.filter_map(|r| {
						let id = r.get("id").and_then(|v| v.as_str())?.to_string();
						let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
						let dt = r.get("tags").and_then(|t| t.as_array())
							.and_then(|a| a.first()).and_then(|v| v.as_str())
							.map(doc_type_from_base_tag)
							.unwrap_or_else(|| "thoughts".to_string());
						let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
						let snippet = r.get("snippet").and_then(|v| v.as_str()).unwrap_or("").to_string();
						if mode == "qa" && dt != "questions" && dt != "conclusions" { return None; }
						Some((id, dt, title, score, snippet))
					}).collect()
			}
			"fts" => {
				let index = match cache::search_index(self.root()) {
					Ok(i) => i,
					Err(e) => return json_err(e),
				};
				match search::search_topk(&index, &query, None, k) {
					Ok(hits) => hits.into_iter().map(|(d, s)| {
						let dt = d.tags.first().map(|t| doc_type_from_base_tag(t)).unwrap_or_else(|| "thoughts".to_string());
						(d.id, dt, d.title, s as f64, String::new())
					}).collect(),
					Err(e) => return json_err(e),
				}
			}
			"tag" => {
				match store::search_by_tag(self.root(), &query) {
					Ok(docs) => docs.into_iter().take(k).map(|d| {
						let dt = d.tags.first().map(|t| doc_type_from_base_tag(t)).unwrap_or_else(|| "thoughts".to_string());
						(d.id, dt, d.title, 0.0, String::new())
					}).collect(),
					Err(e) => return json_err(e),
				}
			}
			other => return json_err(format!("Unknown mode: {} (use smart|fts|tag|qa)", other)),
		};

		let mut hits = Vec::new();
		let mut suggested = Vec::new();
		for (id, dt, title, score, snippet) in &raw_hits {
			let mut hit = serde_json::json!({
				"id": id, "type": dt, "title": title, "score": score,
				"snippet": snippet,
			});
			if want_bodies {
				if let Ok(doc) = store::get_document(self.root(), dt, id) {
					hit["body"] = serde_json::json!(doc.content);
					hit["tags"] = serde_json::json!(doc.tags);
					hit["purpose"] = serde_json::json!(doc.purpose);
				}
			}
			if want_reasons {
				if let Ok(reasons) = store::search_reasons_for(self.root(), id, "both") {
					let r: Vec<serde_json::Value> = reasons.iter().map(doc_preview).collect();
					hit["reasons"] = serde_json::json!(r);
				}
			}
			if depth >= 1 {
				let edges = store::search_reasons_for(self.root(), id, "both").unwrap_or_default();
				hit["edges"] = serde_json::json!(edges.iter().map(doc_preview).collect::<Vec<_>>());
			}
			// Suggest conclusion banner
			if dt == "entities" {
				let s = self.suggest_conclusion(Parameters(SuggestConclusionParams { entity_id: id.clone() }));
				if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
					if v.get("can_conclude").and_then(|b| b.as_bool()).unwrap_or(false) {
						suggested.push(v);
					}
				}
			}
			hits.push(hit);
		}

		let mut out = serde_json::json!({
			"query": query,
			"mode": mode,
			"hits": hits,
		});
		if !suggested.is_empty() {
			out["suggested_conclusions"] = serde_json::json!(suggested);
		}
		// Knowledge-gap question raise: if a smart/qa search found nothing,
		// the agent is seeking info we don't have — raise it as an open
		// question. Idempotent (deduped by query hash).
		if raw_hits.is_empty() && (mode == "smart" || mode == "qa") {
			if let Some(qid) = learn::raise_question_from_search_miss(self.root(), &query, None).await {
				out["raised_question_id"] = serde_json::json!(qid);
			}
		}
		out.to_string()
	}

	#[tool(description = "Fetch doc by id with reasons + edges. Auto-detects type. Docs: docs(\"get\"). Args: id, doc_type?, depth? (default 1).")]
	fn get(&self, params: Parameters<GetParams>) -> String {
		let GetParams { id, doc_type, depth } = params.0;
		let depth = depth.unwrap_or(1) as usize;
		let dt = match self.resolve_doc_type(doc_type.as_deref(), &id) {
			Some(d) => d,
			None => return json_err(format!("Doc not found: {}", id)),
		};
		let doc = match store::get_document(self.root(), &dt, &id) {
			Ok(d) => d,
			Err(e) => return json_err(e),
		};
		let reasons = store::search_reasons_for(self.root(), &id, "both").unwrap_or_default();
		let mut edges_by_depth = serde_json::Map::new();
		let mut frontier: Vec<String> = vec![id.clone()];
		let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
		seen.insert(id.clone());
		for d in 1..=depth {
			let mut next = Vec::new();
			let mut layer = Vec::new();
			for nid in &frontier {
				if let Ok(neighbors) = store::search_reasons_for(self.root(), nid, "both") {
					for n in neighbors {
						if seen.insert(n.id.clone()) {
							next.push(n.id.clone());
							layer.push(doc_preview(&n));
						}
					}
				}
			}
			edges_by_depth.insert(d.to_string(), serde_json::json!(layer));
			frontier = next;
			if frontier.is_empty() { break; }
		}
		serde_json::json!({
			"doc": doc,
			"reasons": reasons.iter().map(doc_preview).collect::<Vec<_>>(),
			"edges_by_depth": edges_by_depth,
		}).to_string()
	}

	#[tool(description = "Read docs(\"ingest\") and docs(\"learn\") first. Write a doc: thought|entity|question|reason|conclusion. Auto-links + auto-marks-answered. refs= creates References edges (NOT Answers). For Answers: reason_kind=Answers or wikilink in body. Args: kind, body, title?, tags?, refs?, purpose_hint?, from_id?, to_id?, reason_kind?.")]
	async fn ingest(&self, params: Parameters<IngestParams>) -> String {
		let IngestParams { kind, title, body, tags: _, refs, purpose_hint, from_id, to_id, reason_kind } = params.0;
		let title = title.unwrap_or_else(|| match kind.as_str() {
			"reason" => "reason".to_string(),
			"question" => derive_title(&body, "question"),
			_ => derive_title(&body, "untitled"),
		});
		let ingested = match kind.as_str() {
			"thought" => self.ingest_chunked("thoughts", &title, &body, "thought", purpose_hint.as_deref()).await,
			"entity" => {
				match learn::find_near_duplicate_entity(self.root(), &title, &body).await {
					Ok(Some(existing)) => {
						let added = if existing.title.to_lowercase() != title.to_lowercase() {
							store::add_alias_to_entity(self.root(), &existing.id, &title).unwrap_or(false)
						} else { false };
						serde_json::json!({
							"merged_into": existing.id,
							"existing_title": existing.title,
							"alias_added": if added { Some(title.as_str()) } else { None },
							"note": "near-duplicate found — merged as alias, no new doc created"
						})
					}
					_ => self.ingest_chunked("entities", &title, &body, "entity", purpose_hint.as_deref()).await,
				}
			}
			"question" => {
				let purpose = self.classify_or_hint(purpose_hint.as_deref(), &body).await;
				let tags = vec!["question".to_string(), purpose.clone()];
				match store::create_document(self.root(), "questions", &title, &body, tags, Some(&purpose), None) {
					Ok(doc) => {
						self.try_index_doc(&doc);
						let _ = store::log_ingest(self.root(), "questions", &doc.id, &doc.title);
						serde_json::json!(doc)
					}
					Err(e) => return json_err(e),
				}
			}
			"reason" => {
				let from = match from_id { Some(s) => s, None => return json_err("reason requires from_id") };
				let to = match to_id { Some(s) => s, None => return json_err("reason requires to_id") };
				let rk = reason_kind.unwrap_or_else(|| "References".to_string());
				let purpose = self.classify_or_hint(purpose_hint.as_deref(), &body).await;
				match store::create_reason(self.root(), &from, &to, &rk, &body, Some(&purpose)) {
					Ok(doc) => {
						self.try_index_doc(&doc);
						let _ = store::log_ingest(self.root(), "reasons", &doc.id, &doc.title);
						serde_json::json!(doc)
					}
					Err(e) => return json_err(e),
				}
			}
			"conclusion" => self.ingest_chunked("conclusions", &title, &body, "conclusion", purpose_hint.as_deref()).await,
			other => return json_err(format!("Unknown kind: {} (thought|entity|question|reason|conclusion)", other)),
		};

		let auto_linked = self.collect_auto_link_ids(&ingested).await;

		// Explicit refs → create `References` edges from the new doc to each ref id.
		let ingested_id = ingested.get("id").and_then(|v| v.as_str())
			.or_else(|| ingested.get("parent").and_then(|p| p.get("id")).and_then(|v| v.as_str()))
			.or_else(|| ingested.get("merged_into").and_then(|v| v.as_str()))
			.map(|s| s.to_string());
		let mut explicit_refs = Vec::new();
		if let (Some(src_id), Some(ref_ids)) = (ingested_id.as_deref(), refs.as_ref()) {
			let purpose = ingested.get("purpose").and_then(|v| v.as_str()).map(|s| s.to_string());
			for rid in ref_ids {
				match store::create_reason(
					self.root(),
					src_id,
					rid,
					"References",
					"explicit ref provided at ingest",
					purpose.as_deref(),
				) {
					Ok(edge) => {
						self.try_index_doc(&edge);
						let _ = store::log_ingest(self.root(), "reasons", &edge.id, &edge.title);
						explicit_refs.push(serde_json::json!({
							"to_id": rid,
							"reason_id": edge.id,
							"kind": "References",
						}));
					}
					Err(e) => explicit_refs.push(serde_json::json!({
						"to_id": rid,
						"error": e.to_string(),
					})),
				}
			}
		}

		// Auto-mark-answered + cross-link for question↔conclusion pairs.
		let mut promoted: Option<serde_json::Value> = None;
		if auto_invariants_enabled() {
			if kind == "conclusion" {
				if let Some(cid) = ingested.get("id").and_then(|v| v.as_str())
					.or_else(|| ingested.get("parent").and_then(|p| p.get("id")).and_then(|v| v.as_str())) {
					promoted = self.try_match_open_questions(cid, &body).await;
				}
			} else if kind == "question" {
				if let Some(qid) = ingested.get("id").and_then(|v| v.as_str()) {
					promoted = self.try_match_existing_conclusions(qid, &body).await;
				}
			}
		}

		let mut out = serde_json::json!({
			"ingested": ingested,
			"auto_linked": auto_linked,
		});
		if !explicit_refs.is_empty() { out["explicit_refs"] = serde_json::json!(explicit_refs); }
		if let Some(p) = promoted { out["promoted"] = p; }
		out.to_string()
	}

}

impl WikiService {
	async fn try_match_open_questions(&self, conclusion_id: &str, body: &str) -> Option<serde_json::Value> {
		const SIM_THRESHOLD: f32 = 0.85;
		let questions = store::list_documents(self.root(), "questions").ok()?;
		let open: Vec<store::Document> = questions.into_iter()
			.filter(|q| !q.tags.iter().any(|t| t == "answered" || t == "dropped"))
			.collect();
		if open.is_empty() { return None; }
		let body_emb = crate::http::embed_batch(&[body.to_string()]).await.ok()?.into_iter().next()?;
		let texts: Vec<String> = open.iter().map(|q| q.content.clone()).collect();
		let embs = crate::http::embed_batch(&texts).await.ok()?;
		let mut best: Option<(String, f32)> = None;
		for (q, emb) in open.iter().zip(embs.iter()) {
			let s = classifier::cosine(&body_emb, emb);
			if s >= SIM_THRESHOLD && best.as_ref().map(|b| s > b.1).unwrap_or(true) {
				best = Some((q.id.clone(), s));
			}
		}
		let (qid, score) = best?;
		// Mark answered
		let mut tags_opt = None;
		if let Ok(mut q) = store::get_document(self.root(), "questions", &qid) {
			q.tags.retain(|t| t != "answered" && t != "dropped");
			q.tags.push("answered".to_string());
			tags_opt = Some(q.tags);
		}
		let _ = store::update_document(self.root(), "questions", &qid, None, tags_opt);
		let _ = store::create_reason(self.root(), conclusion_id, &qid, "Answers", "auto-linked from ingest invariant", None);
		Some(serde_json::json!({"question_id": qid, "score": score, "marked": "answered"}))
	}

	async fn try_match_existing_conclusions(&self, question_id: &str, body: &str) -> Option<serde_json::Value> {
		const SIM_THRESHOLD: f32 = 0.85;
		let conclusions = store::list_documents(self.root(), "conclusions").ok()?;
		if conclusions.is_empty() { return None; }
		let body_emb = crate::http::embed_batch(&[body.to_string()]).await.ok()?.into_iter().next()?;
		let mut best: Option<(String, f32)> = None;
		let texts: Vec<String> = conclusions.iter().map(|c| c.content.clone()).collect();
		let embs = crate::http::embed_batch(&texts).await.ok()?;
		for (c, emb) in conclusions.iter().zip(embs.iter()) {
			let s = classifier::cosine(&body_emb, emb);
			if s >= SIM_THRESHOLD && best.as_ref().map(|b| s > b.1).unwrap_or(true) {
				best = Some((c.id.clone(), s));
			}
		}
		let (cid, score) = best?;
		let mut tags_opt = None;
		if let Ok(mut q) = store::get_document(self.root(), "questions", question_id) {
			q.tags.retain(|t| t != "answered" && t != "dropped");
			q.tags.push("answered".to_string());
			tags_opt = Some(q.tags);
		}
		let _ = store::update_document(self.root(), "questions", question_id, None, tags_opt);
		let _ = store::create_reason(self.root(), &cid, question_id, "Answers", "auto-linked from ingest invariant", None);
		Some(serde_json::json!({"conclusion_id": cid, "score": score, "marked": "answered"}))
	}
}

#[tool_handler(instructions = "WIKI - Single-store Obsidian Knowledge Base. Each doc tagged with 1 type tag + 1 purpose tag. Ingest auto-classifies content into purpose chunks via OpenAI embeddings.")]
impl ServerHandler for WikiService {}

impl WikiService {
	pub fn new() -> Result<Self> {
		let path = store::wiki_root();
		store::bootstrap(&path)?;
		Ok(Self { wiki_path: path })
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn derive_title_short_body() {
		assert_eq!(derive_title("hello world", "untitled"), "hello world");
	}

	#[test]
	fn derive_title_strips_markdown_prefix() {
		assert_eq!(derive_title("## My heading\nbody", "untitled"), "My heading");
		assert_eq!(derive_title("- bullet item", "untitled"), "bullet item");
		assert_eq!(derive_title("> quote text", "untitled"), "quote text");
	}

	#[test]
	fn derive_title_strips_leading_wikilink() {
		assert_eq!(
			derive_title("[[abc-123]] core claim about widgets", "untitled"),
			"core claim about widgets",
		);
	}

	#[test]
	fn derive_title_strips_bracket_tag() {
		assert_eq!(derive_title("[gap] need to verify X", "untitled"), "need to verify X");
		assert_eq!(
			derive_title("[[abc]] [established] foo bar", "untitled"),
			"foo bar",
		);
	}

	#[test]
	fn derive_title_cuts_at_sentence_terminator() {
		assert_eq!(
			derive_title("First sentence here. Second sentence.", "untitled"),
			"First sentence here",
		);
		assert_eq!(derive_title("A question? Yes.", "untitled"), "A question");
	}

	#[test]
	fn derive_title_cuts_at_word_boundary_near_60() {
		let long = "this is a very long title that exceeds the sixty character budget by a lot indeed";
		let out = derive_title(long, "untitled");
		assert!(out.chars().count() <= 61, "got {} chars: {:?}", out.chars().count(), out);
		assert!(out.ends_with('…'));
		assert!(!out.contains("budget by a lot indeed"));
	}

	#[test]
	fn derive_title_falls_back_when_empty() {
		assert_eq!(derive_title("", "fallback"), "fallback");
		assert_eq!(derive_title("###   \n", "fallback"), "fallback");
		assert_eq!(derive_title("[[only-wikilink]]", "fallback"), "fallback");
	}

	#[test]
	fn derive_title_does_not_strip_inline_brackets_with_text() {
		// Real wiki content like "[gap]" at start strips, but tags inside should stay.
		assert_eq!(
			derive_title("real content [aside] here", "untitled"),
			"real content [aside] here",
		);
	}
}
