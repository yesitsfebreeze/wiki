use crate::{cache, chunker, classifier, code, io as wiki_io, learn, search, smart, store};
use anyhow::Result;
use rmcp::{handler::server::wrapper::Parameters, tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone)]
pub struct WikiService {
	wiki_path: PathBuf,
}

// ── Param structs ────────────────────────────────────────────────────────────

#[derive(Deserialize, JsonSchema)]
struct CreatePurposeParams {
	tag: String,
	title: String,
	description: String,
}

#[derive(Deserialize, JsonSchema)]
struct PurposeTagParams {
	tag: String,
}

#[derive(Deserialize, JsonSchema)]
struct IngestThoughtParams {
	title: String,
	content: String,
	purpose_hint: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct IngestEntityParams {
	title: String,
	content: String,
	purpose_hint: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct IngestReasonParams {
	from_id: String,
	to_id: String,
	kind: String,
	body: String,
	purpose_hint: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct IngestQuestionParams {
	body: String,
	purpose_hint: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct IngestConclusionParams {
	title: String,
	body: String,
	purpose_hint: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct QueryParams {
	query: String,
	limit: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct TagParams {
	tag: String,
	limit: Option<u64>,
	cursor: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct ReasonsForParams {
	node_id: String,
	direction: String,
	limit: Option<u64>,
	cursor: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct DocRefParams {
	doc_type: String,
	id: String,
}

#[derive(Deserialize, JsonSchema)]
struct DocTypeParams {
	doc_type: String,
	limit: Option<u64>,
	cursor: Option<u64>,
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
struct ExtractPdfParams {
	paths: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ExtractYouTubeParams {
	ids: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
struct AnswerQuestionParams {
	question_id: String,
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
struct CodeIndexParams {
	src_dir: String,
	ext: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct CodeOpenParams {
	source_path: String,
	ext: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct CodeReadBodyParams {
	path: String,
	start: Option<u64>,
	limit: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct CodeSearchParams {
	query: String,
	regex: Option<bool>,
	scope: Option<String>,
	cursor: Option<u64>,
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
struct CodeRefGraphParams {
	path: String,
	direction: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct CodeOutlineParams {
	path: String,
}

#[derive(Deserialize, JsonSchema)]
struct CodeValidateParams {
	fix: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
struct CodeFnTreeParams {
	fn_id: String,
	depth: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct LearnPassParams {
	limit: Option<u64>,
	purpose: Option<String>,
	dry_run: Option<bool>,
	qa: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
struct LearnFeedbackParams {
	limit: Option<u64>,
	dry_run: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
struct SmartSearchParams {
	question: String,
	tag: Option<String>,
	k: Option<u64>,
	top_n: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct LinkDocParams {
	doc_type: String,
	id: String,
	dry_run: Option<bool>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

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
	#[tool(description = "List all purposes with tag, title, and description")]
	fn list_purposes(&self) -> String {
		match store::list_purposes(self.root()) {
			Ok(p) => to_json(&p),
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Create a new purpose. Args: tag, title, description")]
	fn create_purpose(&self, params: Parameters<CreatePurposeParams>) -> String {
		let CreatePurposeParams { tag, title, description } = params.0;
		match store::create_purpose(self.root(), &tag, &title, &description) {
			Ok(p) => to_json(&p),
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Delete a purpose (does not delete tagged docs). Args: tag")]
	fn delete_purpose(&self, params: Parameters<PurposeTagParams>) -> String {
		match store::delete_purpose(self.root(), &params.0.tag) {
			Ok(_) => format!("Purpose '{}' deleted", params.0.tag),
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Force-rebuild OpenAI embeddings for all purposes")]
	async fn reembed_purposes(&self) -> String {
		if let Ok(purposes) = store::list_purposes(self.root()) {
			for p in &purposes {
				let _ = std::fs::remove_file(p.path.with_extension("vec"));
			}
		}
		match classifier::ensure_purpose_embeddings(self.root()).await {
			Ok(v) => format!("Re-embedded {} purposes", v.len()),
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Ingest a thought (raw fact). Auto-chunks by purpose via OpenAI embeddings; pass purpose_hint to skip classification. Args: title, content, purpose_hint?")]
	async fn ingest_thought(&self, params: Parameters<IngestThoughtParams>) -> String {
		let IngestThoughtParams { title, content, purpose_hint } = params.0;
		self.ingest_chunked("thoughts", &title, &content, "thought", purpose_hint.as_deref())
			.await
			.to_string()
	}

	#[tool(description = "Ingest an entity (consolidated concept). Before calling, check if the concept already exists under a different name — if a near-duplicate is found (by title match or embedding similarity >= WIKI_ALIAS_THRESHOLD, default 0.92), the new title is added as an alias to the existing entity instead of creating a duplicate. Returns {merged_into, existing_title, alias_added} when merged, or the new Document when created. Args: title, content, purpose_hint?")]
	async fn ingest_entity(&self, params: Parameters<IngestEntityParams>) -> String {
		let IngestEntityParams { title, content, purpose_hint } = params.0;
		match learn::find_near_duplicate_entity(self.root(), &title, &content).await {
			Ok(Some(existing)) => {
				let added = if existing.title.to_lowercase() != title.to_lowercase() {
					store::add_alias_to_entity(self.root(), &existing.id, &title).unwrap_or(false)
				} else {
					false
				};
				serde_json::json!({
					"merged_into": existing.id,
					"existing_title": existing.title,
					"alias_added": if added { Some(title.as_str()) } else { None },
					"note": "near-duplicate found — merged as alias, no new doc created"
				}).to_string()
			}
			_ => self.ingest_chunked("entities", &title, &content, "entity", purpose_hint.as_deref()).await.to_string(),
		}
	}

	#[tool(description = "Ingest a reason (directed edge). kind: supports|contradicts|extends|requires|references|derives|instances|PartOf. Args: from_id, to_id, kind, body, purpose_hint?")]
	async fn ingest_reason(&self, params: Parameters<IngestReasonParams>) -> String {
		let IngestReasonParams { from_id, to_id, kind, body, purpose_hint } = params.0;
		let purpose = self.classify_or_hint(purpose_hint.as_deref(), &body).await;
		match store::create_reason(self.root(), &from_id, &to_id, &kind, &body, Some(&purpose)) {
			Ok(doc) => {
				self.try_index_doc(&doc);
				let _ = store::log_ingest(self.root(), "reasons", &doc.id, &doc.title);
				to_json(&doc)
			}
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Ingest an open question. Args: body, purpose_hint?")]
	async fn ingest_question(&self, params: Parameters<IngestQuestionParams>) -> String {
		let IngestQuestionParams { body, purpose_hint } = params.0;
		let purpose = self.classify_or_hint(purpose_hint.as_deref(), &body).await;
		let tags = vec!["question".to_string(), purpose.clone()];
		match store::create_document(
			self.root(), "questions", "question", &body, tags, Some(&purpose), None,
		) {
			Ok(doc) => {
				self.try_index_doc(&doc);
				let _ = store::log_ingest(self.root(), "questions", &doc.id, &doc.title);
				to_json(&doc)
			}
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Ingest a synthesized conclusion. Args: title, body, purpose_hint?")]
	async fn ingest_conclusion(&self, params: Parameters<IngestConclusionParams>) -> String {
		let IngestConclusionParams { title, body, purpose_hint } = params.0;
		self.ingest_chunked("conclusions", &title, &body, "conclusion", purpose_hint.as_deref())
			.await
			.to_string()
	}

	#[tool(description = "Full-text search across all docs. Returns paginated previews (snippets, not full bodies). Use `get` to fetch a full body. Args: query, limit? (default 25)")]
	fn search_fulltext(&self, params: Parameters<QueryParams>) -> String {
		let QueryParams { query, limit } = params.0;
		let lim = limit.map(|n| n as usize).unwrap_or(DEFAULT_LIST_LIMIT);
		let index = match cache::search_index(self.root()) {
			Ok(i) => i,
			Err(e) => return json_err(e),
		};
		match search::search_topk(&index, &query, None, lim) {
			Ok(hits) => {
				let items: Vec<serde_json::Value> = hits
					.into_iter()
					.map(|(d, score)| {
						let mut v = doc_preview(&d);
						v["score"] = serde_json::json!(score);
						v
					})
					.collect();
				serde_json::json!({
					"query": query,
					"returned": items.len(),
					"items": items,
				}).to_string()
			}
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Search by tag (purpose tag, type tag, or sub-tag). Returns paginated previews. Args: tag, limit? (default 25), cursor? (default 0)")]
	fn search_by_tag(&self, params: Parameters<TagParams>) -> String {
		let TagParams { tag, limit, cursor } = params.0;
		match store::search_by_tag(self.root(), &tag) {
			Ok(docs) => {
				let previews: Vec<serde_json::Value> = docs.iter().map(doc_preview).collect();
				paginate(previews, cursor, limit).to_string()
			}
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Get reasons connected to a node. direction: from|to|both. Args: node_id, direction, limit? (default 25), cursor? (default 0)")]
	fn search_reasons_for(&self, params: Parameters<ReasonsForParams>) -> String {
		let ReasonsForParams { node_id, direction, limit, cursor } = params.0;
		match store::search_reasons_for(self.root(), &node_id, &direction) {
			Ok(docs) => {
				let previews: Vec<serde_json::Value> = docs.iter().map(doc_preview).collect();
				paginate(previews, cursor, limit).to_string()
			}
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Get a document by type and ID. doc_type: thoughts|entities|reasons|questions|conclusions. Args: doc_type, id")]
	fn get(&self, params: Parameters<DocRefParams>) -> String {
		let DocRefParams { doc_type, id } = params.0;
		match store::get_document(self.root(), &doc_type, &id) {
			Ok(doc) => to_json(&doc),
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "List documents by type. Returns paginated previews. Args: doc_type, limit? (default 25), cursor? (default 0)")]
	fn list(&self, params: Parameters<DocTypeParams>) -> String {
		let DocTypeParams { doc_type, limit, cursor } = params.0;
		match store::list_documents(self.root(), &doc_type) {
			Ok(docs) => {
				let previews: Vec<serde_json::Value> = docs.iter().map(doc_preview).collect();
				paginate(previews, cursor, limit).to_string()
			}
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Update a document. Args: doc_type, id, content?, tags?")]
	fn update(&self, params: Parameters<UpdateDocParams>) -> String {
		let UpdateDocParams { doc_type, id, content, tags } = params.0;
		match store::update_document(self.root(), &doc_type, &id, content.as_deref(), tags) {
			Ok(doc) => to_json(&doc),
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Delete a document by type and ID. Args: doc_type, id")]
	fn delete_doc(&self, params: Parameters<DocRefParams>) -> String {
		let DocRefParams { doc_type, id } = params.0;
		match store::delete_document(self.root(), &doc_type, &id) {
			Ok(_) => format!("Deleted {} '{}'", doc_type, id),
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "List ingest log entries. Returns paginated entries. Args: limit? (default 25), cursor? (default 0)")]
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

	#[tool(description = "Extract text and assets from PDFs (batch). Args: paths")]
	fn extract_pdfs(&self, params: Parameters<ExtractPdfParams>) -> String {
		let mut results = Vec::new();
		for path_str in &params.0.paths {
			let path = std::path::Path::new(path_str);
			match crate::extract::extract_pdf(path) {
				Ok((text, assets)) => results.push(serde_json::json!({
					"path": path_str,
					"status": "extracted",
					"text_length": text.len(),
					"assets": assets.len()
				})),
				Err(e) => results.push(serde_json::json!({
					"path": path_str,
					"status": "error",
					"error": e.to_string()
				})),
			}
		}
		serde_json::json!({ "extracted": results.len(), "results": results }).to_string()
	}

	#[tool(description = "Extract transcripts from YouTube videos. Args: ids (IDs or URLs)")]
	fn extract_youtube(&self, params: Parameters<ExtractYouTubeParams>) -> String {
		let mut results = Vec::new();
		for input in &params.0.ids {
			let video_id = if input.contains("youtube.com/watch?v=") {
				input.split("v=").nth(1).unwrap_or(input).split('&').next().unwrap_or(input).to_string()
			} else if input.contains("youtu.be/") {
				input.split("youtu.be/").nth(1).unwrap_or(input).split('?').next().unwrap_or(input).to_string()
			} else {
				input.clone()
			};
			let url = format!("https://www.youtube.com/watch?v={}", video_id);
			match crate::extract::extract_youtube(&url) {
				Ok(text) => results.push(serde_json::json!({
					"id": video_id,
					"status": "extracted",
					"text_length": text.len()
				})),
				Err(e) => results.push(serde_json::json!({
					"id": video_id,
					"status": "error",
					"error": e.to_string()
				})),
			}
		}
		serde_json::json!({ "extracted": results.len(), "results": results }).to_string()
	}

	#[tool(description = "List all open (unresolved) questions. Returns paginated previews. Args: limit? (default 25), cursor? (default 0)")]
	fn list_open_questions(&self, params: Parameters<ListLogParams>) -> String {
		let ListLogParams { limit, cursor } = params.0;
		match store::list_documents(self.root(), "questions") {
			Ok(docs) => {
				let open: Vec<serde_json::Value> = docs.iter()
					.filter(|d| !d.tags.iter().any(|t| t == "resolved"))
					.map(doc_preview)
					.collect();
				paginate(open, cursor, limit).to_string()
			}
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Find candidate answers for a question via fulltext search. Args: question_id")]
	fn find_answers(&self, params: Parameters<AnswerQuestionParams>) -> String {
		let qid = &params.0.question_id;
		let question = match store::get_document(self.root(), "questions", qid) {
			Ok(d) => d,
			Err(e) => return json_err(e),
		};
		let mut candidates = Vec::new();
		if let Ok(index) = cache::search_index(self.root()) {
			if let Ok(results) = search::search_documents(&index, &question.content) {
				for doc in results.iter().take(5) {
					let doc_type = doc.tags.first().map(|s| s.as_str()).unwrap_or("unknown");
					let kind = if doc.title.contains('?') { "Answers" } else { "Supports" };
					candidates.push(serde_json::json!({
						"id": doc.id,
						"title": doc.title,
						"doc_type": doc_type,
						"suggested_reason_kind": kind,
					}));
				}
			}
		}
		serde_json::json!({
			"question_id": qid,
			"question": question.content,
			"candidates": candidates,
		}).to_string()
	}

	#[tool(description = "Mark a question status. status: resolved|unanswerable|partial_answer. Args: question_id, status")]
	fn mark_question(&self, params: Parameters<MarkQuestionParams>) -> String {
		let MarkQuestionParams { question_id, status } = params.0;
		const VALID: &[&str] = &["resolved", "unanswerable", "partial_answer"];
		if !VALID.contains(&status.as_str()) {
			return json_err(format!("Invalid status: {}", status));
		}
		match store::get_document(self.root(), "questions", &question_id) {
			Ok(mut doc) => {
				doc.tags.retain(|t| !VALID.contains(&t.as_str()));
				doc.tags.push(status.clone());
				match store::update_document(self.root(), "questions", &question_id, None, Some(doc.tags.clone())) {
					Ok(_) => serde_json::json!({
						"question_id": question_id,
						"status": status,
					}).to_string(),
					Err(e) => json_err(e),
				}
			}
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Suggest a conclusion for an entity based on linked reasons + resolved questions. Args: entity_id")]
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
		let resolved_count = related.iter()
			.filter(|q| q.tags.iter().any(|t| t == "resolved"))
			.count();

		let can_conclude = !reasons.is_empty() && resolved_count >= 2;
		serde_json::json!({
			"entity_id": entity_id,
			"entity_title": entity.title,
			"supporting_reasons": reasons.len(),
			"related_questions": related.len(),
			"resolved_questions": resolved_count,
			"can_conclude": can_conclude,
		}).to_string()
	}

	// ── Code tools ──────────────────────────────────────────────────────────

	#[tool(description = "Bootstrap fn-level code index. Args: src_dir, ext?")]
	fn code_index(&self, params: Parameters<CodeIndexParams>) -> String {
		let CodeIndexParams { src_dir, ext } = params.0;
		let ext = ext.unwrap_or_else(|| "rs".to_string());
		match code::index_dir(&PathBuf::from(src_dir), &ext) {
			Ok(s) => s,
			Err(e) => format!("Error: {e}"),
		}
	}

	#[tool(description = "Open a source file via the index. Args: source_path, ext?")]
	fn code_open(&self, params: Parameters<CodeOpenParams>) -> String {
		let CodeOpenParams { source_path, ext } = params.0;
		let ext = ext.unwrap_or_else(|| "rs".to_string());
		match code::open_source(&PathBuf::from(source_path), &ext) {
			Ok(s) => s,
			Err(e) => format!("Error: {e}"),
		}
	}

	#[tool(description = "Read one fn body. Args: path, start?, limit?")]
	fn code_read_body(&self, params: Parameters<CodeReadBodyParams>) -> String {
		let CodeReadBodyParams { path, start, limit } = params.0;
		let start = start.map(|n| n as usize).unwrap_or(1);
		let limit = limit.map(|n| n as usize);
		match code::read_body(&PathBuf::from(path), start, limit) {
			Ok(s) => s,
			Err(e) => format!("Error: {e}"),
		}
	}

	#[tool(description = "Grep across indexed fn bodies/skeletons. scope: all|skel|body. Args: query, regex?, scope?, cursor?, limit?")]
	fn code_search(&self, params: Parameters<CodeSearchParams>) -> String {
		let CodeSearchParams { query, regex, scope, cursor, limit } = params.0;
		let scope = scope.unwrap_or_else(|| "body".to_string());
		let cursor = cursor.unwrap_or(0) as usize;
		let limit = limit.map(|n| n as usize).unwrap_or(100);
		match code::search_bodies(&query, regex.unwrap_or(false), &scope, cursor, limit) {
			Ok(s) => s,
			Err(e) => format!("Error: {e}"),
		}
	}

	#[tool(description = "List fn body files. Args: dir, glob?, min_loc?, max_loc?, sort?, cursor?, limit?")]
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

	#[tool(description = "Find fns exceeding max_loc lines. Args: max_loc?")]
	fn code_find_large(&self, params: Parameters<CodeFindLargeParams>) -> String {
		match code::find_large(params.0.max_loc.map(|n| n as usize)) {
			Ok(s) => s,
			Err(e) => format!("Error: {e}"),
		}
	}

	#[tool(description = "List installed code languages")]
	fn code_list_languages(&self) -> String {
		code::list_languages()
	}

	#[tool(description = "Reverse-lookup which sources reference a body. Args: path, direction?")]
	fn code_ref_graph(&self, params: Parameters<CodeRefGraphParams>) -> String {
		let CodeRefGraphParams { path, direction } = params.0;
		let direction = direction.unwrap_or_else(|| "both".to_string());
		match code::ref_graph(&PathBuf::from(path), &direction) {
			Ok(s) => s,
			Err(e) => format!("Error: {e}"),
		}
	}

	#[tool(description = "Symbol map of body/skeleton. Args: path")]
	fn code_outline(&self, params: Parameters<CodeOutlineParams>) -> String {
		match code::outline(&PathBuf::from(params.0.path)) {
			Ok(s) => s,
			Err(e) => format!("Error: {e}"),
		}
	}

	#[tool(description = "Check index integrity. Args: fix?")]
	fn code_validate(&self, params: Parameters<CodeValidateParams>) -> String {
		match code::validate(params.0.fix.unwrap_or(false)) {
			Ok(s) => s,
			Err(e) => format!("Error: {e}"),
		}
	}

	#[tool(description = "Walk fn call tree. Args: fn_id, depth?")]
	fn code_fn_tree(&self, params: Parameters<CodeFnTreeParams>) -> String {
		let CodeFnTreeParams { fn_id, depth } = params.0;
		match code::fn_tree(&PathBuf::from(fn_id), depth.unwrap_or(2) as usize) {
			Ok(s) => s,
			Err(e) => format!("Error: {e}"),
		}
	}

	#[tool(description = "Run a learn pass over the vault: (1) rewrite bare entity mentions as [[wikilinks]], (2) whenever a surface text variant differs from the entity canonical title and all known aliases it is automatically added as an alias to that entity's frontmatter — so recurring alternate names become first-class aliases rather than one-off links, (3) fold paragraphs >=WIKI_DEDUPE_THRESHOLD (default 0.85) cosine-similar to an entity body into that entity (emits Consolidates reasons), (4) write a report to ingest_log/. Prefer alias merging over wikilink-only when two names refer to the same concept. Args: limit? (default 25), purpose? (filter doc set), dry_run? (default false)")]
	async fn learn_pass(&self, params: Parameters<LearnPassParams>) -> String {
		let LearnPassParams { limit, purpose, dry_run, qa } = params.0;
		let limit = limit.map(|n| n as usize).unwrap_or(25);
		match learn::run_pass(self.root(), limit, purpose.as_deref(), dry_run.unwrap_or(false), qa.unwrap_or(true)).await {
			Ok(v) => v.to_string(),
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Consume feedback.jsonl: for each entry, relink picked docs (wikilinks), call OpenAI to decide if question is keepable, what reason kind/body links question→picks, mark resolved if a strong Answers exists. Idempotent via .feedback.cursor. Args: limit? (default 25), dry_run? (default false)")]
	async fn learn_from_feedback(&self, params: Parameters<LearnFeedbackParams>) -> String {
		let LearnFeedbackParams { limit, dry_run } = params.0;
		let limit = limit.map(|n| n as usize).unwrap_or(25);
		match learn::run_feedback_pass(self.root(), limit, dry_run.unwrap_or(false)).await {
			Ok(v) => v.to_string(),
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Smart search: BM25 top-K + OpenAI rerank. Returns ranked context with reasons. Logs (question, picked, reasons) to feedback.jsonl for the learn loop. Args: question, tag? (filter), k? (BM25 pool, default 20), top_n? (final results, default 5)")]
	async fn smart_search(&self, params: Parameters<SmartSearchParams>) -> String {
		let SmartSearchParams { question, tag, k, top_n } = params.0;
		let k = k.map(|n| n as usize).unwrap_or(20);
		let top_n = top_n.map(|n| n as usize).unwrap_or(5);
		match smart::smart_search(self.root(), &question, tag.as_deref(), k, top_n).await {
			Ok(v) => v.to_string(),
			Err(e) => json_err(e),
		}
	}

	#[tool(description = "Single-doc learn variant. Rewrite entity mentions as [[wikilinks]] and auto-add surface text variants as entity aliases (if the matched text differs from all known titles/aliases, it is persisted to the entity's frontmatter aliases list). Fold near-duplicate paragraphs into matching entities. Use as ingest-time hook on a freshly created doc to immediately wire it into the entity graph. Returns aliases_added count. Args: doc_type, id, dry_run?")]
	async fn link_doc(&self, params: Parameters<LinkDocParams>) -> String {
		let LinkDocParams { doc_type, id, dry_run } = params.0;
		match learn::link_doc(self.root(), &doc_type, &id, dry_run.unwrap_or(false)).await {
			Ok(v) => v.to_string(),
			Err(e) => json_err(e),
		}
	}
}

#[tool_handler(instructions = "WIKI - Single-store Obsidian Knowledge Base. Each doc tagged with 1 type tag + 1 purpose tag. Ingest auto-classifies content into purpose chunks via OpenAI embeddings.")]
impl ServerHandler for WikiService {}

impl WikiService {
	pub fn new() -> Result<Self> {
		let path = store::wiki_root();
		store::ensure_wiki_layout(&path)?;
		Ok(Self { wiki_path: path })
	}
}
