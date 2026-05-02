use crate::cache;
use crate::io::fnv64;
use crate::{classifier, http, search, smart, store};
use std::sync::Arc;
use anyhow::Result;
use regex::Regex;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

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

fn answer_threshold() -> f32 {
	std::env::var("WIKI_ANSWER_THRESHOLD").ok().and_then(|s| s.parse().ok()).unwrap_or(0.8)
}

fn support_threshold() -> f32 {
	std::env::var("WIKI_SUPPORT_THRESHOLD").ok().and_then(|s| s.parse().ok()).unwrap_or(0.3)
}

fn qa_max_per_pass() -> usize {
	std::env::var("WIKI_QA_MAX_PER_PASS").ok().and_then(|s| s.parse().ok()).unwrap_or(50)
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

fn dedupe_threshold() -> f32 {
	std::env::var("WIKI_DEDUPE_THRESHOLD")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(0.85)
}

fn alias_threshold() -> f32 {
	std::env::var("WIKI_ALIAS_THRESHOLD")
		.ok()
		.and_then(|s| s.parse::<f32>().ok())
		.unwrap_or(0.92)
}

fn read_entity_meta(root: &Path, id: &str) -> Option<(Vec<String>, String, Option<String>)> {
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
	if let Some(cached) = cache::entity_index_get() {
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
	Ok(cache::entity_index_set(refs))
}

fn protected_re() -> &'static [Regex] {
	static RE: OnceLock<Vec<Regex>> = OnceLock::new();
	RE.get_or_init(|| {
		[
			r"(?ms)^```.*?^```",
			r"`[^`\n]+`",
			r"\[\[[^\]]*\]\]",
			r"\[[^\]]*\]\([^)]*\)",
		]
		.iter()
		.filter_map(|p| Regex::new(p).ok())
		.collect()
	})
}

fn link_target_re() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| Regex::new(r"\[\[([^\]|#]+?)(?:[#|][^\]]*)?\]\]").unwrap())
}

fn protected_ranges(text: &str) -> Vec<(usize, usize)> {
	let mut ranges = Vec::new();
	for re in protected_re() {
		for m in re.find_iter(text) {
			ranges.push((m.start(), m.end()));
		}
	}
	ranges.sort();
	ranges
}

fn in_protected(pos: usize, ranges: &[(usize, usize)]) -> bool {
	ranges.iter().any(|(s, e)| pos >= *s && pos < *e)
}

fn rewrite_links(body: &str, entities: &[EntityRef], self_id: &str) -> (String, usize, Vec<(String, String)>) {
	let mut out = body.to_string();
	let mut count = 0usize;
	let mut linked_ids: HashSet<String> = HashSet::new();
	let mut alias_candidates: Vec<(String, String)> = Vec::new();

	let mut all: Vec<(&EntityRef, String)> = Vec::new();
	for e in entities {
		if e.id == self_id {
			continue;
		}
		let mut names = vec![e.title.clone()];
		names.extend(e.aliases.iter().cloned());
		for n in names {
			let n = n.trim().to_string();
			if n.len() >= 3 {
				all.push((e, n));
			}
		}
	}
	all.sort_by_key(|(_, n)| std::cmp::Reverse(n.len()));

	for (e, name) in all {
		if linked_ids.contains(&e.id) {
			continue;
		}
		let escaped = regex::escape(&name);
		let re = match Regex::new(&format!(r"(?i)\b{}\b", escaped)) {
			Ok(r) => r,
			Err(_) => continue,
		};
		let prot = protected_ranges(&out);
		if let Some(m) = re.find(&out) {
			if in_protected(m.start(), &prot) {
				continue;
			}
			let surface = out[m.start()..m.end()].to_string();
			let surface_lc = surface.to_lowercase();
			let already_known = e.title.to_lowercase() == surface_lc
				|| e.aliases.iter().any(|a| a.to_lowercase() == surface_lc);
			if !already_known {
				alias_candidates.push((e.id.clone(), surface.clone()));
			}
			let purpose_seg = e.purpose.as_deref().unwrap_or("uncategorized");
			let link = format!("[[entities/{}/{}|{}]]", purpose_seg, e.slug, surface);
			let mut new = String::with_capacity(out.len() + link.len());
			new.push_str(&out[..m.start()]);
			new.push_str(&link);
			new.push_str(&out[m.end()..]);
			out = new;
			count += 1;
			linked_ids.insert(e.id.clone());
		}
	}
	(out, count, alias_candidates)
}

pub async fn find_near_duplicate_entity(root: &Path, title: &str, content: &str) -> Result<Option<EntityRef>> {
	let threshold = alias_threshold();
	let entities = build_entity_index(root).await?;
	let title_lc = title.trim().to_lowercase();

	for e in entities.iter() {
		if e.title.to_lowercase() == title_lc
			|| e.aliases.iter().any(|a| a.to_lowercase() == title_lc)
		{
			return Ok(Some(e.clone()));
		}
	}

	if !content.is_empty() {
		if let Ok(embs) = http::embed_batch(&[content.to_string()]).await {
			if let Some(content_emb) = embs.into_iter().next() {
				for e in entities.iter() {
					if let Some(ev) = &e.body_embedding {
						if classifier::cosine(&content_emb, ev) >= threshold {
							return Ok(Some(e.clone()));
						}
					}
				}
			}
		}
	}

	Ok(None)
}

async fn dedupe_paragraphs(
	body: &str,
	entities: &[EntityRef],
	self_id: &str,
) -> (String, Vec<(String, String)>) {
	let paragraphs: Vec<String> = body.split("\n\n").map(|s| s.to_string()).collect();
	let nonempty: Vec<(usize, String)> = paragraphs
		.iter()
		.enumerate()
		.map(|(i, p)| (i, p.trim().to_string()))
		.filter(|(_, p)| !p.is_empty() && p.len() >= 40)
		.collect();
	if nonempty.is_empty() {
		return (body.to_string(), Vec::new());
	}
	let texts: Vec<String> = nonempty.iter().map(|(_, p)| p.clone()).collect();
	let embs = match http::embed_batch(&texts).await {
		Ok(v) => v,
		Err(_) => return (body.to_string(), Vec::new()),
	};
	let threshold = dedupe_threshold();
	let mut drop_idx: HashMap<usize, (String, String)> = HashMap::new();
	for ((idx, para), emb) in nonempty.iter().zip(embs.iter()) {
		for e in entities {
			if e.id == self_id {
				continue;
			}
			let Some(ev) = &e.body_embedding else { continue };
			if classifier::cosine(emb, ev) >= threshold {
				drop_idx.insert(*idx, (e.id.clone(), format!("{:x}", fnv64(para))));
				break;
			}
		}
	}
	let mut merges = Vec::new();
	let mut kept = Vec::new();
	for (i, p) in paragraphs.iter().enumerate() {
		match drop_idx.remove(&i) {
			Some(merge) => merges.push(merge),
			None => kept.push(p.clone()),
		}
	}
	(kept.join("\n\n"), merges)
}

/// Walk wikilinks in `body` and return the set of distinct purposes the
/// targets belong to. Recognises both new-style `[[type/purpose/name]]`
/// (purpose read from path) and legacy `[[type/id]]` (purpose looked up via
/// frontmatter). Bare `[[slug]]` links are skipped — too ambiguous post-migration.
fn collect_link_purposes(root: &Path, body: &str) -> HashSet<String> {
	let mut out: HashSet<String> = HashSet::new();
	for cap in link_target_re().captures_iter(body) {
		let target = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
		if target.is_empty() { continue; }
		let parts: Vec<&str> = target.split('/').collect();
		match parts.len() {
			3 => { out.insert(parts[1].to_string()); }
			2 => {
				if let Ok(doc) = store::get_document(root, parts[0], parts[1]) {
					if let Some(p) = doc.purpose { out.insert(p); }
				}
			}
			_ => {}
		}
	}
	out
}

/// Replace every `[[<old_target>(#|)|...]]` occurrence with `[[<new_target>...]]`
/// across all known doc-type directories in the vault.
fn rewrite_inbound_links(root: &Path, old_target: &str, new_target: &str) -> Result<()> {
	let doc_types = ["thoughts", "entities", "reasons", "questions", "conclusions"];
	let escaped = regex::escape(old_target);
	let re = Regex::new(&format!(r"\[\[{}(?P<rest>[\]|#])", escaped))?;
	let replacement = format!("[[{}$rest", new_target);
	for dt in &doc_types {
		let dir = root.join(dt);
		for path in store::walk_md_paths(&dir) {
			let Ok(raw) = std::fs::read_to_string(&path) else { continue };
			if !raw.contains(old_target) { continue; }
			let new = re.replace_all(&raw, replacement.as_str()).to_string();
			if new != raw {
				crate::io::write_atomic_str(&path, &new)?;
			}
		}
	}
	Ok(())
}

/// Move `<doc_type>/<old_purpose>/<name>.md` to `<doc_type>/crosstopic/<name>.md`
/// and rewrite every inbound `[[<doc_type>/<old_purpose>/<name>]]` link in the
/// vault to the new location.
fn move_to_crosstopic(root: &Path, doc_type: &str, id: &str) -> Result<bool> {
	let dir = root.join(doc_type);
	let old_path = store::find_document_path_by_id(&dir, id)?;
	let parent_name = old_path
		.parent()
		.and_then(|p| p.file_name())
		.and_then(|s| s.to_str())
		.unwrap_or("");
	if parent_name == "crosstopic" {
		return Ok(false);
	}
	let stem = old_path
		.file_stem()
		.and_then(|s| s.to_str())
		.ok_or_else(|| anyhow::anyhow!("missing file stem"))?
		.to_string();
	let old_purpose = parent_name.to_string();

	let new_dir = dir.join("crosstopic");
	std::fs::create_dir_all(&new_dir)?;
	let mut new_path = new_dir.join(format!("{}.md", stem));
	let mut suffix = 1;
	while new_path.exists() {
		new_path = new_dir.join(format!("{}-{}.md", stem, suffix));
		suffix += 1;
	}
	let new_stem = new_path
		.file_stem()
		.and_then(|s| s.to_str())
		.unwrap_or(&stem)
		.to_string();

	std::fs::rename(&old_path, &new_path)?;

	if !old_purpose.is_empty() {
		let old_target = format!("{}/{}/{}", doc_type, old_purpose, stem);
		let new_target = format!("{}/crosstopic/{}", doc_type, new_stem);
		let _ = rewrite_inbound_links(root, &old_target, &new_target);
	}
	Ok(true)
}

pub async fn link_doc_internal(
	root: &Path,
	doc_type: &str,
	id: &str,
	entities: &[EntityRef],
	dry_run: bool,
) -> Result<serde_json::Value> {
	let doc = store::get_document(root, doc_type, id)?;
	let original = doc.content.clone();
	let (linked, link_count, alias_candidates) = rewrite_links(&original, entities, id);
	let (deduped, merges) = dedupe_paragraphs(&linked, entities, id).await;
	let modified = deduped != original;

	let mut aliases_added = 0usize;
	if !dry_run {
		if modified {
			store::update_document(root, doc_type, id, Some(&deduped), None)?;
			for (entity_id, hash) in &merges {
				let _ = store::create_reason(
					root,
					entity_id,
					id,
					"Consolidates",
					&format!("absorbed paragraph hash:{}", hash),
					doc.purpose.as_deref(),
				);
			}
		}
		for (entity_id, surface) in &alias_candidates {
			if let Ok(true) = store::add_alias_to_entity(root, entity_id, surface) {
				aliases_added += 1;
			}
		}
	}

	let mut moved_to_crosstopic = false;
	if !dry_run {
		let mut purposes = collect_link_purposes(root, &deduped);
		if let Some(p) = doc.purpose.as_deref() {
			if !p.is_empty() { purposes.insert(p.to_string()); }
		}
		purposes.remove("uncategorized");
		purposes.remove("crosstopic");
		if purposes.len() >= 2 && doc.purpose.as_deref() != Some("crosstopic") {
			if let Ok(true) = move_to_crosstopic(root, doc_type, id) {
				moved_to_crosstopic = true;
			}
		}
	}

	Ok(serde_json::json!({
		"doc_id": id,
		"doc_type": doc_type,
		"links_added": link_count,
		"aliases_added": aliases_added,
		"paragraphs_merged": merges.len(),
		"modified": !dry_run && modified,
		"moved_to_crosstopic": moved_to_crosstopic,
		"dry_run": dry_run,
	}))
}

pub async fn link_doc(root: &Path, doc_type: &str, id: &str, dry_run: bool) -> Result<serde_json::Value> {
	let entities = build_entity_index(root).await?;
	link_doc_internal(root, doc_type, id, entities.as_slice(), dry_run).await
}

/// QA loop for a single doc. Returns (raised, answered, promoted) counts.
/// Decrements `llm_budget` by approximate LLM call count.
async fn qa_for_doc(
	root: &Path,
	doc: &store::Document,
	llm_budget: &mut usize,
) -> Result<(u64, u64, u64)> {
	if *llm_budget == 0 {
		return Ok((0, 0, 0));
	}
	*llm_budget = llm_budget.saturating_sub(1);
	let raised = raise_questions_for_doc(root, doc, false).await.unwrap_or_default();

	// Plus any pre-existing OPEN question with a "References" reason → this doc.
	let mut q_targets: Vec<(String, String, Option<String>)> = raised
		.iter()
		.map(|r| (r.question_id.clone(), r.title.clone(), r.purpose.clone()))
		.collect();
	if let Ok(reasons) = store::search_reasons_for(root, &doc.id, "to") {
		for r in reasons {
			// Heuristic: question→doc edges. We only know `from_id` via frontmatter — skip
			// scanning frontmatter again; instead enumerate questions and check their tags.
			let _ = r; // unused; we walk questions directly below
		}
	}
	if let Ok(questions) = store::list_documents(root, "questions") {
		for q in questions {
			if q.tags.iter().any(|t| t == "resolved") { continue; }
			if q_targets.iter().any(|(id, _, _)| id == &q.id) { continue; }
			// Linked-to-this-doc check: scan reasons from q.id with to_id == doc.id.
			let linked = store::search_reasons_for(root, &q.id, "from")
				.ok()
				.map(|rs| rs.iter().any(|r| {
					// reason title format: "<from> -[<kind>]-> <to>"
					r.title.ends_with(&doc.id)
				}))
				.unwrap_or(false);
			if linked {
				q_targets.push((q.id, q.title, q.purpose));
			}
		}
	}

	let mut answered = 0u64;
	let mut promoted = 0u64;
	let strong = answer_threshold();

	for (qid, qtitle, qpurpose) in q_targets {
		if *llm_budget == 0 { break; }
		*llm_budget = llm_budget.saturating_sub(1);
		let cands = match cross_reference_question(root, &qtitle, qpurpose.as_deref()).await {
			Ok(v) => v,
			Err(_) => continue,
		};
		let mut strong_edges: Vec<AnswerCandidate> = Vec::new();
		let mut got_answer = false;
		for c in &cands {
			let kind = if c.score >= strong { "Answers" } else { "Supports" };
			if c.score >= strong { got_answer = true; }
			let _ = store::create_reason(root, &qid, &c.doc_id, kind, &c.body, qpurpose.as_deref());
			if c.score >= strong { strong_edges.push(c.clone()); }
		}
		if got_answer {
			answered += 1;
			if let Ok(mut q) = store::get_document(root, "questions", &qid) {
				if !q.tags.iter().any(|t| t == "resolved") {
					q.tags.push("resolved".to_string());
					let _ = store::update_document(root, "questions", &qid, None, Some(q.tags));
				}
			}
			if *llm_budget == 0 { continue; }
			*llm_budget = llm_budget.saturating_sub(1);
			if let Ok(Some(_)) = promote_to_conclusion(root, &qid, &strong_edges).await {
				promoted += 1;
			}
		}
	}

	Ok((raised.iter().filter(|r| r.created).count() as u64, answered, promoted))
}

pub async fn run_pass(
	root: &Path,
	limit: usize,
	purpose: Option<&str>,
	dry_run: bool,
	qa: bool,
) -> Result<serde_json::Value> {
	let entities = build_entity_index(root).await?;
	let mut targets: Vec<(String, String)> = Vec::new();
	for doc_type in &["thoughts", "conclusions"] {
		let docs = store::list_documents(root, doc_type)?;
		for d in docs {
			if let Some(p) = purpose {
				if d.purpose.as_deref() != Some(p) {
					continue;
				}
			}
			targets.push(((*doc_type).to_string(), d.id));
			if targets.len() >= limit {
				break;
			}
		}
		if targets.len() >= limit {
			break;
		}
	}

	let mut docs_modified = 0u64;
	let mut links_added = 0u64;
	let mut merges_total = 0u64;
	let mut questions_raised = 0u64;
	let mut questions_answered = 0u64;
	let mut conclusions_promoted = 0u64;
	let mut llm_budget = qa_max_per_pass();
	let mut details = Vec::new();
	for (dt, id) in &targets {
		match link_doc_internal(root, dt, id, entities.as_slice(), dry_run).await {
			Ok(v) => {
				if v["modified"].as_bool().unwrap_or(false) {
					docs_modified += 1;
				}
				links_added += v["links_added"].as_u64().unwrap_or(0);
				merges_total += v["paragraphs_merged"].as_u64().unwrap_or(0);
				details.push(v);
			}
			Err(e) => details.push(serde_json::json!({
				"doc_id": id,
				"doc_type": dt,
				"error": e.to_string()
			})),
		}

		if !qa || dry_run || llm_budget == 0 {
			continue;
		}
		let Ok(doc) = store::get_document(root, dt, id) else { continue };
		match qa_for_doc(root, &doc, &mut llm_budget).await {
			Ok((raised, answered, promoted)) => {
				questions_raised += raised;
				questions_answered += answered;
				conclusions_promoted += promoted;
			}
			Err(e) => details.push(serde_json::json!({
				"doc_id": id, "qa_error": e.to_string(),
			})),
		}
	}

	let report = serde_json::json!({
		"pass_id": chrono::Utc::now().to_rfc3339(),
		"docs_scanned": targets.len(),
		"docs_modified": docs_modified,
		"links_added": links_added,
		"paragraphs_merged": merges_total,
		"questions_raised": questions_raised,
		"questions_answered": questions_answered,
		"conclusions_promoted": conclusions_promoted,
		"entity_count": entities.len(),
		"purpose_filter": purpose,
		"dry_run": dry_run,
		"qa": qa,
		"details": details,
	});

	write_pass_log(root, "learn", &report)?;

	if !dry_run {
		if let Ok(index) = cache::search_index(root) {
			let docs: Vec<store::Document> = targets
				.iter()
				.filter_map(|(dt, id)| store::get_document(root, dt, id).ok())
				.collect();
			let _ = search::index_documents(&index, &docs);
		}
	}

	Ok(report)
}

fn write_pass_log(root: &Path, kind: &str, report: &serde_json::Value) -> Result<()> {
	let log_dir = root.join("ingest_log");
	std::fs::create_dir_all(&log_dir)?;
	let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
	let json = serde_json::to_string_pretty(report)?;
	crate::io::write_atomic_str(&log_dir.join(format!("{}-{}.json", kind, ts)), &json)?;
	Ok(())
}

// ── Feedback-driven learning ─────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct FeedbackEntry {
	question: String,
	#[serde(default)]
	tag_filter: Option<String>,
	#[serde(default)]
	picked: Vec<String>,
	#[serde(default)]
	reasons: Vec<(String, String)>,
}

#[derive(Deserialize, Debug)]
struct LlmEdge {
	picked_id: String,
	#[serde(default)]
	score: f32,
	kind: String,
	body: String,
}

#[derive(Deserialize, Debug)]
struct LlmDecision {
	#[serde(default)]
	keep_question: bool,
	#[serde(default)]
	question_title: Option<String>,
	#[serde(default)]
	question_body: Option<String>,
	#[serde(default)]
	purpose: Option<String>,
	#[serde(default)]
	resolved: bool,
	#[serde(default)]
	edges: Vec<LlmEdge>,
}

fn read_cursor(root: &Path) -> u64 {
	let p = root.join(".feedback.cursor");
	std::fs::read_to_string(&p)
		.ok()
		.and_then(|s| s.trim().parse().ok())
		.unwrap_or(0)
}

fn write_cursor(root: &Path, off: u64) -> Result<()> {
	crate::io::write_atomic_str(&root.join(".feedback.cursor"), &off.to_string())
}

fn fnv_question_id(q: &str) -> String {
	format!("q-{:x}", fnv64(q.trim()))
}

fn find_question_by_hash(root: &Path, hash_id: &str) -> Option<String> {
	let docs = store::list_documents(root, "questions").ok()?;
	docs.into_iter().find_map(|d| {
		if d.tags.iter().any(|t| t == hash_id) {
			Some(d.id)
		} else {
			None
		}
	})
}

fn find_conclusion_by_hash(root: &Path, hash_id: &str) -> Option<String> {
	let docs = store::list_documents(root, "conclusions").ok()?;
	docs.into_iter().find_map(|d| {
		if d.tags.iter().any(|t| t == hash_id) { Some(d.id) } else { None }
	})
}

#[derive(Deserialize, Debug)]
struct RaisedQItem {
	#[serde(default)]
	title: String,
	#[serde(default)]
	body: String,
}

#[derive(Deserialize, Debug)]
struct RaisedQResp {
	#[serde(default)]
	questions: Vec<RaisedQItem>,
}

pub async fn raise_questions_for_doc(
	root: &Path,
	doc: &store::Document,
	dry_run: bool,
) -> Result<Vec<RaisedQuestion>> {
	let sys = "You read a wiki doc and produce open questions IT raises (not answers). \
		Return JSON {\"questions\": [{\"title\": string, \"body\": string}]}. \
		Skip if doc already self-explanatory. Max 3.";
	let user = format!("Title: {}\n\nBody:\n{}", doc.title, doc.content);
	let raw = http::chat_json(sys, &user).await?;
	let parsed: RaisedQResp = serde_json::from_str(&raw)
		.map_err(|e| anyhow::anyhow!("raise parse: {} body: {}", e, raw))?;

	let purpose = doc.purpose.clone();
	let mut out = Vec::new();
	for q in parsed.questions.into_iter().take(3) {
		let title = q.title.trim().to_string();
		if title.is_empty() {
			continue;
		}
		let hash = fnv_question_id(&title);
		if let Some(existing_id) = find_question_by_hash(root, &hash) {
			out.push(RaisedQuestion {
				question_id: existing_id,
				title,
				purpose: purpose.clone(),
				created: false,
			});
			continue;
		}
		if dry_run {
			out.push(RaisedQuestion {
				question_id: hash.clone(),
				title,
				purpose: purpose.clone(),
				created: false,
			});
			continue;
		}
		let body = if q.body.trim().is_empty() { title.clone() } else { q.body };
		let purpose_tag = purpose.clone().unwrap_or_else(|| "general".to_string());
		let tags = vec!["question".to_string(), purpose_tag.clone(), hash.clone()];
		let qdoc = store::create_document(
			root, "questions", &title, &body, tags, Some(&purpose_tag), None,
		)?;
		let _ = store::create_reason(root, &qdoc.id, &doc.id, "References", "raised by", purpose.as_deref());
		out.push(RaisedQuestion {
			question_id: qdoc.id,
			title,
			purpose: purpose.clone(),
			created: true,
		});
	}
	Ok(out)
}

#[derive(Deserialize, Debug)]
struct ScoredCand {
	picked_id: String,
	#[serde(default)]
	score: f32,
	#[serde(default)]
	kind: String,
	#[serde(default)]
	body: String,
}

#[derive(Deserialize, Debug)]
struct ScoredResp {
	#[serde(default)]
	scored: Vec<ScoredCand>,
}

pub async fn cross_reference_question(
	root: &Path,
	question: &str,
	purpose: Option<&str>,
) -> Result<Vec<AnswerCandidate>> {
	let res = smart::smart_search(root, question, purpose, 5, 5).await?;
	let results = res.get("results").and_then(|v| v.as_array()).cloned().unwrap_or_default();
	if results.is_empty() {
		return Ok(Vec::new());
	}
	// Build a doc-type map by re-resolving each candidate id.
	let mut id_to_dt: HashMap<String, String> = HashMap::new();
	for r in &results {
		let Some(id) = r.get("id").and_then(|v| v.as_str()) else { continue };
		for dt in &["entities", "thoughts", "conclusions", "reasons", "questions"] {
			if store::get_document(root, dt, id).is_ok() {
				id_to_dt.insert(id.to_string(), (*dt).to_string());
				break;
			}
		}
	}

	let cand_json = serde_json::to_string(&results)?;
	let sys = "Given a question and these candidate docs, score each 0..1 for how well it \
		answers, and pick a kind from Answers|Supports|Contradicts|Extends|References. \
		Return JSON {\"scored\":[{\"picked_id\":string,\"score\":number,\"kind\":string,\"body\":string}]}. \
		`body` is one short sentence on WHY. Include every candidate id.";
	let user = format!("Question: {}\n\nCandidates:\n{}", question, cand_json);
	let raw = http::chat_json(sys, &user).await?;
	let parsed: ScoredResp = serde_json::from_str(&raw)
		.map_err(|e| anyhow::anyhow!("cross-ref parse: {} body: {}", e, raw))?;

	let support = support_threshold();
	let out = parsed.scored.into_iter()
		.filter(|c| c.score >= support)
		.map(|c| AnswerCandidate {
			doc_type: id_to_dt.get(&c.picked_id).cloned().unwrap_or_else(|| "thoughts".to_string()),
			doc_id: c.picked_id,
			score: c.score,
			kind: allowed_kind(&c.kind).to_string(),
			body: c.body,
		})
		.collect();
	Ok(out)
}

pub async fn promote_to_conclusion(
	root: &Path,
	question_id: &str,
	edges: &[AnswerCandidate],
) -> Result<Option<String>> {
	let question = store::get_document(root, "questions", question_id)?;
	let hash = fnv_question_id(&question.title);
	if let Some(existing) = find_conclusion_by_hash(root, &hash) {
		return Ok(Some(existing));
	}

	#[derive(serde::Serialize)]
	struct EdgeView<'a> {
		doc_id: &'a str,
		doc_type: &'a str,
		score: f32,
		kind: &'a str,
		body: &'a str,
	}
	let edges_json: Vec<EdgeView> = edges.iter().map(|e| EdgeView {
		doc_id: &e.doc_id, doc_type: &e.doc_type, score: e.score, kind: &e.kind, body: &e.body,
	}).collect();

	let sys = "Synthesize a 1-paragraph conclusion answering this question, citing the \
		supplied edges. Be concise. Return JSON {\"body\": string}.";
	let user = format!(
		"Question: {}\n\nEdges JSON:\n{}",
		question.title,
		serde_json::to_string(&edges_json)?,
	);
	let raw = http::chat_json(sys, &user).await?;
	#[derive(Deserialize)]
	struct Body { body: String }
	let parsed: Body = serde_json::from_str(&raw)
		.map_err(|e| anyhow::anyhow!("promote parse: {} body: {}", e, raw))?;

	let purpose = question.purpose.clone();
	let purpose_tag = purpose.clone().unwrap_or_else(|| "general".to_string());
	let tags = vec!["conclusion".to_string(), purpose_tag.clone(), hash];
	let cdoc = store::create_document(
		root, "conclusions", &question.title, &parsed.body, tags, Some(&purpose_tag), None,
	)?;

	let _ = store::create_reason(root, question_id, &cdoc.id, "Derives", "promoted from resolved question", purpose.as_deref());
	let strong = answer_threshold();
	for e in edges {
		if e.score >= strong {
			let _ = store::create_reason(root, &cdoc.id, &e.doc_id, "References", &e.body, purpose.as_deref());
		}
	}
	Ok(Some(cdoc.id))
}

fn allowed_kind(k: &str) -> &'static str {
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

async fn decide_via_llm(entry: &FeedbackEntry, picks_ctx: &str) -> Result<LlmDecision> {
	let sys = "You curate a knowledge wiki from search feedback. Given a user question and the \
		documents picked as relevant (with reasons), decide: \
		(a) is the question worth saving as a 'questions' doc? \
		(b) for each picked doc, what reason kind connects question→doc and what is a 1-sentence \
		body summarizing WHY it answers/supports/etc the question? \
		Return JSON: {\"keep_question\":bool, \"question_title\":string|null, \
		\"question_body\":string|null, \"purpose\":string|null, \"resolved\":bool, \
		\"edges\":[{\"picked_id\":string, \"score\":0..1, \"kind\":\"Answers|Supports|Contradicts|Extends|Requires|References|Derives|Instances\", \"body\":string}]}. \
		Set resolved=true only if at least one edge is a strong direct Answers (score>=0.8). \
		Skip edges below score 0.3.";
	let user = format!(
		"Question: {}\nPurpose hint (tag_filter): {:?}\n\nPicks (id, search_reason, snippet):\n{}",
		entry.question, entry.tag_filter, picks_ctx
	);
	let content = http::chat_json(sys, &user).await?;
	let parsed: LlmDecision = serde_json::from_str(&content)
		.map_err(|e| anyhow::anyhow!("decision parse: {} body: {}", e, content))?;
	Ok(parsed)
}

async fn process_entry(
	root: &Path,
	entry: &FeedbackEntry,
	entities: &[EntityRef],
	dry_run: bool,
) -> Result<serde_json::Value> {
	if entry.picked.is_empty() {
		return Ok(serde_json::json!({"skipped": "no picks"}));
	}

	let mut picks_ctx = String::new();
	let reason_map: HashMap<&str, &str> = entry
		.reasons
		.iter()
		.map(|(a, b)| (a.as_str(), b.as_str()))
		.collect();
	let mut found_any = false;
	let mut linked_ids: Vec<String> = Vec::new();
	for pid in &entry.picked {
		let mut doc_lookup: Option<(String, store::Document)> = None;
		for dt in &["entities", "thoughts", "conclusions", "reasons", "questions"] {
			if let Ok(d) = store::get_document(root, dt, pid) {
				doc_lookup = Some(((*dt).to_string(), d));
				break;
			}
		}
		let Some((dt, doc)) = doc_lookup else { continue };
		found_any = true;
		let snippet = if doc.content.len() > 400 {
			let mut end = 400;
			while !doc.content.is_char_boundary(end) && end > 0 { end -= 1; }
			format!("{}…", &doc.content[..end])
		} else {
			doc.content.clone()
		};
		let r = reason_map.get(pid.as_str()).copied().unwrap_or("");
		picks_ctx.push_str(&format!(
			"- id={} type={} title={:?} reason={:?}\n  snippet={}\n",
			pid, dt, doc.title, r, snippet
		));
		if !dry_run {
			let _ = link_doc_internal(root, &dt, pid, entities, false).await;
			linked_ids.push(pid.clone());
		}
	}
	if !found_any {
		return Ok(serde_json::json!({"skipped": "no picks resolved"}));
	}

	let decision = match decide_via_llm(entry, &picks_ctx).await {
		Ok(d) => d,
		Err(e) => return Ok(serde_json::json!({"error": e.to_string()})),
	};

	let hash_id = fnv_question_id(&entry.question);
	let mut question_id: Option<String> = find_question_by_hash(root, &hash_id);
	let mut created_question = false;

	if decision.keep_question && question_id.is_none() && !dry_run {
		let purpose = decision.purpose.clone().unwrap_or_else(|| {
			entry.tag_filter.clone().unwrap_or_else(|| "general".to_string())
		});
		let body = decision.question_body.clone().unwrap_or_else(|| entry.question.clone());
		let mut tags = vec!["question".to_string(), purpose.clone(), hash_id.clone()];
		if decision.resolved {
			tags.push("resolved".to_string());
		}
		if let Ok(qdoc) = store::create_document(
			root,
			"questions",
			decision.question_title.as_deref().unwrap_or(entry.question.as_str()),
			&body,
			tags,
			Some(&purpose),
			None,
		) {
			question_id = Some(qdoc.id);
			created_question = true;
		}
	} else if decision.resolved && !dry_run {
		if let Some(qid) = &question_id {
			if let Ok(mut q) = store::get_document(root, "questions", qid) {
				if !q.tags.iter().any(|t| t == "resolved") {
					q.tags.push("resolved".to_string());
					let _ = store::update_document(root, "questions", qid, None, Some(q.tags.clone()));
				}
			}
		}
	}

	let mut edges_created = 0u64;
	if let Some(qid) = &question_id {
		if !dry_run {
			for edge in &decision.edges {
				if edge.score < 0.3 {
					continue;
				}
				let kind = allowed_kind(&edge.kind);
				if store::create_reason(
					root,
					qid,
					&edge.picked_id,
					kind,
					&edge.body,
					decision.purpose.as_deref(),
				).is_ok() {
					edges_created += 1;
				}
			}
		}
	}

	Ok(serde_json::json!({
		"question": entry.question,
		"question_id": question_id,
		"created_question": created_question,
		"resolved": decision.resolved,
		"edges_created": edges_created,
		"docs_relinked": linked_ids.len(),
		"keep_question": decision.keep_question,
	}))
}

pub async fn run_feedback_pass(root: &Path, limit: usize, dry_run: bool) -> Result<serde_json::Value> {
	let path = root.join("feedback.jsonl");
	if !path.exists() {
		return Ok(serde_json::json!({"processed": 0, "note": "no feedback.jsonl"}));
	}
	let raw = std::fs::read(&path)?;
	let cursor = read_cursor(root) as usize;
	let cursor = cursor.min(raw.len());
	let slice = &raw[cursor..];
	let text = std::str::from_utf8(slice).map_err(|e| anyhow::anyhow!("utf8: {}", e))?;

	let entities = build_entity_index(root).await?;
	let mut details = Vec::new();
	let mut consumed_bytes = cursor as u64;
	let mut processed = 0usize;

	for line in text.split_inclusive('\n') {
		let stripped = line.trim_end_matches('\n').trim();
		consumed_bytes += line.len() as u64;
		if stripped.is_empty() {
			continue;
		}
		if processed >= limit {
			consumed_bytes -= line.len() as u64;
			break;
		}
		let entry: FeedbackEntry = match serde_json::from_str(stripped) {
			Ok(e) => e,
			Err(e) => {
				details.push(serde_json::json!({"parse_error": e.to_string(), "line": stripped}));
				processed += 1;
				continue;
			}
		};
		match process_entry(root, &entry, entities.as_slice(), dry_run).await {
			Ok(v) => details.push(v),
			Err(e) => details.push(serde_json::json!({"error": e.to_string()})),
		}
		processed += 1;
	}

	if !dry_run {
		write_cursor(root, consumed_bytes)?;
	}

	let report = serde_json::json!({
		"pass_id": chrono::Utc::now().to_rfc3339(),
		"processed": processed,
		"cursor": consumed_bytes,
		"total_bytes": raw.len(),
		"dry_run": dry_run,
		"details": details,
	});

	write_pass_log(root, "learn-feedback", &report)?;

	Ok(report)
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	#[test]
	fn protected_ranges_skip_code() {
		let text = "hello `code` world\n\n```\nfoo bar\n```\n\ntail";
		let r = protected_ranges(text);
		assert!(r.iter().any(|(s, _)| text[*s..].starts_with('`')));
	}

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
		store::ensure_wiki_layout(root).unwrap();
		let title = "Does borrowing prevent data races?";
		let hash = fnv_question_id(title);
		let tags = vec!["question".to_string(), "general".to_string(), hash.clone()];
		let q = store::create_document(root, "questions", title, "body", tags, Some("general"), None).unwrap();
		let found = find_question_by_hash(root, &hash);
		assert_eq!(found, Some(q.id));
		// Negative: distinct hash returns None
		assert!(find_question_by_hash(root, "q-deadbeef").is_none());
	}

	#[tokio::test]
	async fn promote_to_conclusion_idempotent() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		let qtitle = "What is ownership?";
		let qhash = fnv_question_id(qtitle);
		let qtags = vec!["question".to_string(), "general".to_string(), qhash.clone()];
		let qdoc = store::create_document(root, "questions", qtitle, "q body", qtags, Some("general"), None).unwrap();

		// Pre-existing conclusion w/ matching hash tag.
		let ctags = vec!["conclusion".to_string(), "general".to_string(), qhash.clone()];
		let cdoc = store::create_document(root, "conclusions", qtitle, "existing", ctags, Some("general"), None).unwrap();

		let result = promote_to_conclusion(root, &qdoc.id, &[]).await.unwrap();
		assert_eq!(result, Some(cdoc.id));
		// No new conclusion was added.
		let concs = store::list_documents(root, "conclusions").unwrap();
		assert_eq!(concs.len(), 1);
	}
}
