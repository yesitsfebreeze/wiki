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

const DEFAULT_QUESTION_DEDUPE_THRESHOLD: f32 = 0.88;

fn question_dedupe_threshold() -> f32 {
	std::env::var("WIKI_QUESTION_DEDUPE_THRESHOLD")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(DEFAULT_QUESTION_DEDUPE_THRESHOLD)
}

/// Pure: drop candidate indices whose max cosine against any existing
/// embedding meets/exceeds `threshold`. Returns indices kept, in input order.
pub fn dedupe_candidates_by_embedding(
	cand_embs: &[Vec<f32>],
	existing_embs: &[Vec<f32>],
	threshold: f32,
) -> Vec<usize> {
	let mut kept = Vec::with_capacity(cand_embs.len());
	for (i, c) in cand_embs.iter().enumerate() {
		let mut dup = false;
		for e in existing_embs {
			if classifier::cosine(c, e) >= threshold {
				dup = true;
				break;
			}
		}
		if !dup {
			kept.push(i);
		}
	}
	kept
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
		let mut max_score: f32 = 0.0;
		for c in &cands {
			if c.score > max_score { max_score = c.score; }
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
		} else if max_score >= support_threshold() && *llm_budget >= 2 {
			// In-purpose answer fell below threshold but cleared the support
			// floor. Try ONE cross-topic fallback pass (no recursion).
			*llm_budget = llm_budget.saturating_sub(2);
			if let Ok(n) = cross_topic_pass(root, &qid).await {
				if n > 0 {
					answered += 1;
					promoted += 1;
				}
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
	force: bool,
) -> Result<serde_json::Value> {
	let entities = build_entity_index(root).await?;

	// Build the full ordered sequence of (doc_type, id) candidates respecting purpose filter.
	let mut sequence: Vec<(String, String)> = Vec::new();
	for doc_type in &["thoughts", "conclusions"] {
		let docs = store::list_documents(root, doc_type)?;
		for d in docs {
			if let Some(p) = purpose {
				if d.purpose.as_deref() != Some(p) {
					continue;
				}
			}
			sequence.push(((*doc_type).to_string(), d.id));
		}
	}

	// Apply per-purpose cursor: rotate sequence so it starts after the cursor.
	let cursor_key = purpose.unwrap_or("<global>");
	let cursor = read_pass_cursor(root, cursor_key);
	let start_idx = match cursor {
		Some((ref dt, ref id)) => sequence
			.iter()
			.position(|(d, i)| d == dt && i == id)
			.map(|i| (i + 1) % sequence.len().max(1))
			.unwrap_or(0),
		None => 0,
	};

	let mut targets: Vec<(String, String)> = Vec::new();
	if !sequence.is_empty() {
		let n = sequence.len();
		for k in 0..n {
			let idx = (start_idx + k) % n;
			targets.push(sequence[idx].clone());
			if targets.len() >= limit {
				break;
			}
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
	let mut last_processed: Option<(String, String)> = None;
	let mut skipped_recent = 0u64;
	for (dt, id) in &targets {
		last_processed = Some((dt.clone(), id.clone()));

		// Skip if last_qa_at is within 24h and not forced.
		if !force && qa && doc_qa_is_recent(root, dt, id) {
			skipped_recent += 1;
			details.push(serde_json::json!({
				"doc_id": id,
				"doc_type": dt,
				"skipped": "recent_qa",
			}));
			continue;
		}

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
				if !dry_run {
					let _ = stamp_last_qa_at(root, dt, id);
				}
			}
			Err(e) => details.push(serde_json::json!({
				"doc_id": id, "qa_error": e.to_string(),
			})),
		}
	}

	// Auto-trigger: open questions with Supports edges from ≥2 distinct
	// purposes get a cross-topic pass even if their docs were not QA'd above.
	let mut crosstopic_invoked = 0u64;
	if qa && !dry_run {
		let resolved: HashSet<String> = cache::tag_index_lookup(root, "resolved")
			.into_iter()
			.filter(|d| d.doc_type == "questions")
			.map(|d| d.id)
			.collect();
		let candidates: Vec<String> = cache::tag_index_lookup(root, "question")
			.into_iter()
			.filter(|d| d.doc_type == "questions" && !resolved.contains(&d.id))
			.map(|d| d.id)
			.collect();
		for qid in candidates {
			if llm_budget < 2 { break; }
			if !should_invoke_cross_topic(root, &qid) { continue; }
			llm_budget = llm_budget.saturating_sub(2);
			if let Ok(n) = cross_topic_pass(root, &qid).await {
				if n > 0 {
					crosstopic_invoked += 1;
					questions_answered += 1;
					conclusions_promoted += 1;
				}
			}
		}
	}

	// Persist cursor at last-processed doc so the next pass resumes after it.
	if !dry_run {
		if let Some((dt, id)) = &last_processed {
			let _ = write_pass_cursor(root, cursor_key, dt, id);
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
		"force": force,
		"skipped_recent": skipped_recent,
		"crosstopic_invoked": crosstopic_invoked,
		"cursor": last_processed.as_ref().map(|(dt, id)| format!("{}/{}", dt, id)),
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
		// Recompute node_size weights for the whole vault. Cheap because the
		// reason index is cached; min/max normalization needs all docs anyway.
		let _ = crate::weight::recompute_all(root);
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

fn pass_cursor_path(root: &Path, key: &str) -> std::path::PathBuf {
	let safe: String = key.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '<' || c == '>' { c } else { '_' }).collect();
	root.join(format!(".learn.cursor.{}", safe))
}

/// Read per-purpose pass cursor. Returns Some((doc_type, id)) if present.
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
fn doc_qa_is_recent(root: &Path, doc_type: &str, id: &str) -> bool {
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
fn stamp_last_qa_at(root: &Path, doc_type: &str, id: &str) -> Result<()> {
	let dir = root.join(doc_type);
	let path = store::find_document_path_by_id(&dir, id)?;
	let raw = std::fs::read_to_string(&path)?;
	let (mut fm, body) = store::parse_frontmatter(&raw)?;
	let now = chrono::Utc::now().to_rfc3339();
	if let Some(obj) = fm.as_object_mut() {
		obj.insert("last_qa_at".to_string(), serde_json::json!(now));
	} else {
		// fm was Null — synthesize.
		let mut m = serde_json::Map::new();
		m.insert("id".to_string(), serde_json::json!(id));
		m.insert("last_qa_at".to_string(), serde_json::json!(now));
		fm = serde_json::Value::Object(m);
	}
	let fm_str = serde_yaml::to_string(&fm)?;
	crate::io::write_atomic_str(&path, &format!("---\n{}---\n\n{}", fm_str, body))?;
	Ok(())
}

fn fnv_question_id(q: &str) -> String {
	format!("q-{:x}", fnv64(q.trim()))
}

fn find_question_by_hash(root: &Path, hash_id: &str) -> Option<String> {
	if let Some(id) = cache::hash_index_lookup(root, hash_id)
		.into_iter()
		.find(|d| d.doc_type == "questions")
		.map(|d| d.id)
	{
		return Some(id);
	}
	// Fallback FS scan: process-global cache may be stale across parallel
	// tests using different roots.
	store::list_documents(root, "questions")
		.ok()?
		.into_iter()
		.find(|d| d.tags.iter().any(|t| t == hash_id))
		.map(|d| d.id)
}

fn find_conclusion_by_hash(root: &Path, hash_id: &str) -> Option<String> {
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

#[derive(Deserialize, Debug, Clone)]
pub struct RaisedQItem {
	#[serde(default)]
	pub title: String,
	#[serde(default)]
	pub body: String,
}

#[derive(Deserialize, Debug)]
struct RaisedQResp {
	#[serde(default)]
	questions: Vec<RaisedQItem>,
}

/// Collect embeddings for OPEN (not `resolved`) questions in `purpose_tag`.
/// Uses the in-memory pool when present; live-embeds missing titles via a
/// single `embed_batch` call. Returns embeddings (order is arbitrary).
pub async fn gather_open_question_embeddings(
	root: &Path,
	purpose_tag: &str,
) -> Result<Vec<Vec<f32>>> {
	let by_purpose = cache::tag_index_lookup(root, purpose_tag);
	let resolved: HashSet<String> = cache::tag_index_lookup(root, "resolved")
		.into_iter()
		.filter(|d| d.doc_type == "questions")
		.map(|d| d.id)
		.collect();

	let mut from_pool: Vec<Vec<f32>> = Vec::new();
	let mut miss_titles: Vec<String> = Vec::new();
	for dref in by_purpose {
		if dref.doc_type != "questions" || resolved.contains(&dref.id) {
			continue;
		}
		if let Some(entry) = cache::pool_get(&dref.id) {
			from_pool.push(entry.vec.clone());
		} else if let Ok(qd) = store::get_document(root, "questions", &dref.id) {
			miss_titles.push(qd.title);
		}
	}
	if !miss_titles.is_empty() {
		let live = http::embed_batch(&miss_titles).await?;
		from_pool.extend(live);
	}
	Ok(from_pool)
}

/// Counts open (i.e. not yet `resolved`) questions for the given purpose using
/// the cached tag index. `purpose` of `None` falls back to `"general"`.
pub fn count_open_questions_in_purpose(root: &Path, purpose: Option<&str>) -> usize {
	let purpose_tag = purpose.unwrap_or("general");
	let by_purpose = cache::tag_index_lookup(root, purpose_tag);
	let resolved: std::collections::HashSet<String> = cache::tag_index_lookup(root, "resolved")
		.into_iter()
		.filter(|d| d.doc_type == "questions")
		.map(|d| d.id)
		.collect();
	by_purpose
		.into_iter()
		.filter(|d| d.doc_type == "questions" && !resolved.contains(&d.id))
		.count()
}

/// Filter LLM-emitted candidates: drop empties, drop template-shaped titles.
/// Pure -- no I/O. Returns `(kept, skipped_template_titles)`.
pub fn filter_raised_candidates(
	items: Vec<RaisedQItem>,
) -> (Vec<RaisedQItem>, Vec<String>) {
	let mut kept = Vec::new();
	let mut skipped = Vec::new();
	for q in items.into_iter().take(3) {
		let title = q.title.trim().to_string();
		if title.is_empty() {
			continue;
		}
		if crate::config::is_template_question(&title) {
			skipped.push(title);
			continue;
		}
		kept.push(RaisedQItem { title, body: q.body });
	}
	(kept, skipped)
}

pub async fn raise_questions_for_doc(
	root: &Path,
	doc: &store::Document,
	dry_run: bool,
) -> Result<Vec<RaisedQuestion>> {
	// Per-purpose backpressure: refuse to call the LLM if we already have too
	// many open questions in this purpose.
	let cap = crate::config::open_questions_per_purpose_cap();
	let open_now = count_open_questions_in_purpose(root, doc.purpose.as_deref());
	if open_now >= cap {
		eprintln!(
			"raise_questions_for_doc: purpose {:?} at cap ({} >= {}), skipping raise",
			doc.purpose.as_deref().unwrap_or("general"),
			open_now,
			cap,
		);
		return Ok(Vec::new());
	}

	let sys = "You read a wiki doc and produce open questions IT raises (not answers). \
		Return JSON {\"questions\": [{\"title\": string, \"body\": string}]}. \
		Skip if doc already self-explanatory. Max 3. \
		Hard rules: \
		- Reject questions any reasonable reader could answer from the doc body alone. \
		- No templated phrasing. No 'How does X relate to similar concepts'. \
		No 'What are the implications of X'. No 'What are the key characteristics of X'. \
		- Each question must require >=2 sentences of context in the body, not just a title.";
	let user = format!("Title: {}\n\nBody:\n{}", doc.title, doc.content);
	let raw = http::chat_json(sys, &user).await?;
	let parsed: RaisedQResp = serde_json::from_str(&raw)
		.map_err(|e| anyhow::anyhow!("raise parse: {} body: {}", e, raw))?;

	let (kept, skipped) = filter_raised_candidates(parsed.questions);
	for s in &skipped {
		eprintln!("raise_questions_for_doc: skipped templated question {:?}", s);
	}

	let purpose = doc.purpose.clone();
	let purpose_tag_for_dedupe = purpose.clone().unwrap_or_else(|| "general".to_string());
	let templated_dropped = skipped.len();

	// Embedding-cosine dedupe vs existing OPEN questions in same purpose.
	let mut kept = kept;
	let mut semantic_dropped = 0usize;
	if !kept.is_empty() {
		let cand_titles: Vec<String> = kept.iter().map(|q| q.title.clone()).collect();
		let cand_embs = http::embed_batch(&cand_titles).await?;
		let existing_embs = gather_open_question_embeddings(root, &purpose_tag_for_dedupe).await?;
		let threshold = question_dedupe_threshold();
		let keep_idx = dedupe_candidates_by_embedding(&cand_embs, &existing_embs, threshold);
		semantic_dropped = kept.len() - keep_idx.len();
		let kept_set: HashSet<usize> = keep_idx.into_iter().collect();
		kept = kept
			.into_iter()
			.enumerate()
			.filter(|(i, _)| kept_set.contains(i))
			.map(|(_, q)| q)
			.collect();
	}

	// Cap interaction: trim survivors to fit (cap - existing_open).
	let existing_open = count_open_questions_in_purpose(root, doc.purpose.as_deref());
	let slots = cap.saturating_sub(existing_open);
	if kept.len() > slots {
		kept.truncate(slots);
	}

	eprintln!(
		"raise_questions_for_doc: deduped {} templated, {} semantic, {} passed",
		templated_dropped,
		semantic_dropped,
		kept.len(),
	);

	let mut out = Vec::new();
	for q in kept {
		let title = q.title;
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

/// Outcome of [`migrate_templated_questions`].
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TemplateMigrationReport {
	pub scanned: usize,
	pub templated: usize,
	pub deleted: usize,
	/// IDs that matched a template but were spared because they had at least
	/// one inbound `Answers` edge.
	pub kept_with_answers: Vec<String>,
}

/// One-shot cleanup: walk all questions, delete those whose title matches a
/// template regex AND have no inbound `Answers` reason edges. Returns a
/// summary; in `dry_run` mode no docs are deleted.
pub fn migrate_templated_questions(root: &Path, dry_run: bool) -> Result<TemplateMigrationReport> {
	let mut rep = TemplateMigrationReport::default();
	let questions = store::list_documents(root, "questions").unwrap_or_default();
	rep.scanned = questions.len();

	for q in questions {
		if !crate::config::is_template_question(&q.title) {
			continue;
		}
		rep.templated += 1;

		// "Inbound Answers" edges: reasons whose to_id == question.id and
		// kind == "Answers".  reason_index_lookup gives us the reason ids.
		let adj = cache::reason_index_lookup(root, &q.id);
		let mut has_answer = false;
		// reason title format is "{from} -[{kind}]-> {to}" -- cheap kind probe.
		for rid in adj.to.iter().chain(adj.from.iter()) {
			if let Ok(r) = store::get_document(root, "reasons", rid) {
				if r.title.contains("-[Answers]->") {
					has_answer = true;
					break;
				}
			}
		}

		if has_answer {
			rep.kept_with_answers.push(q.id.clone());
			continue;
		}
		if dry_run {
			rep.deleted += 1;
			continue;
		}
		if store::delete_document(root, "questions", &q.id).is_ok() {
			rep.deleted += 1;
		}
	}
	Ok(rep)
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

const BRIDGING_BONUS: f32 = 1.5;

/// Read a reason doc's frontmatter and return `(from_id, to_id, kind, purpose)`.
fn read_reason_meta(root: &Path, reason_id: &str) -> Option<(String, String, String, Option<String>)> {
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

/// Return the set of distinct purposes from which a candidate doc has inbound
/// `Supports` reason edges. The candidate's own purpose is not excluded; the
/// caller decides what counts as "different".
pub fn candidate_support_purposes(root: &Path, doc_id: &str) -> HashSet<String> {
	let mut set = HashSet::new();
	let adj = cache::reason_index_lookup(root, doc_id);
	for rid in &adj.to {
		let Some((from_id, _to, kind, _rp)) = read_reason_meta(root, rid) else { continue };
		if kind != "Supports" { continue; }
		// Resolve the from-doc's purpose by trying common doc types.
		for dt in &["thoughts", "conclusions", "entities", "questions"] {
			if let Ok(d) = store::get_document(root, dt, &from_id) {
				if let Some(p) = d.purpose {
					if !p.is_empty() { set.insert(p); }
				}
				break;
			}
		}
	}
	set
}

/// Apply the bridging bonus to candidate scores: any candidate whose
/// `candidate_support_purposes` set contains a purpose distinct from
/// `question_purpose` gets its score multiplied by `BRIDGING_BONUS`.
pub fn apply_bridging_bonus(
	root: &Path,
	candidates: Vec<AnswerCandidate>,
	question_purpose: Option<&str>,
) -> Vec<AnswerCandidate> {
	candidates
		.into_iter()
		.map(|mut c| {
			let supports = candidate_support_purposes(root, &c.doc_id);
			let bridges = supports
				.iter()
				.any(|p| Some(p.as_str()) != question_purpose);
			if bridges {
				c.score *= BRIDGING_BONUS;
			}
			c
		})
		.collect()
}

/// Distinct purposes among `Supports` edges into `question_id`. Used by the
/// auto-trigger heuristic in `run_pass`.
pub fn question_support_purposes(root: &Path, question_id: &str) -> HashSet<String> {
	let mut set = HashSet::new();
	let adj = cache::reason_index_lookup(root, question_id);
	for rid in &adj.to {
		let Some((from_id, _to, kind, _rp)) = read_reason_meta(root, rid) else { continue };
		if kind != "Supports" { continue; }
		for dt in &["thoughts", "conclusions", "entities", "questions"] {
			if let Ok(d) = store::get_document(root, dt, &from_id) {
				if let Some(p) = d.purpose {
					if !p.is_empty() { set.insert(p); }
				}
				break;
			}
		}
	}
	set
}

/// `true` when the question has Supports edges from at least two distinct
/// purposes — the auto-trigger condition for `cross_topic_pass`.
pub fn should_invoke_cross_topic(root: &Path, question_id: &str) -> bool {
	question_support_purposes(root, question_id).len() >= 2
}

/// Add `bridges-<purpose>` extra tags + `crosstopic` purpose tag to a
/// conclusion doc. Idempotent: existing tags are preserved and new ones are
/// only appended when missing.
fn tag_conclusion_with_bridges(
	root: &Path,
	conclusion_id: &str,
	bridge_purposes: &HashSet<String>,
) -> Result<()> {
	let doc = store::get_document(root, "conclusions", conclusion_id)?;
	let mut tags = doc.tags.clone();
	let crosstopic = "crosstopic".to_string();
	if !tags.iter().any(|t| t == &crosstopic) {
		tags.push(crosstopic);
	}
	for p in bridge_purposes {
		let bt = format!("bridges-{}", p);
		if !tags.iter().any(|t| t == &bt) {
			tags.push(bt);
		}
	}
	if tags != doc.tags {
		store::update_document(root, "conclusions", conclusion_id, None, Some(tags))?;
	}
	Ok(())
}

/// Inner promote step for cross-topic pass. Pure of LLM if `candidates` already
/// have `Answers`-strong scores AND the conclusion can be merged into an
/// existing one (bypassing the synthesis call). Used by tests and by
/// `cross_topic_pass`.
async fn cross_topic_emit_and_promote(
	root: &Path,
	question_id: &str,
	question_purpose: Option<&str>,
	candidates: &[AnswerCandidate],
) -> Result<usize> {
	let strong = answer_threshold();
	let mut strong_edges: Vec<AnswerCandidate> = Vec::new();
	let mut got_answer = false;
	for c in candidates {
		let kind = if c.score >= strong { "Answers" } else { "Supports" };
		if c.score >= strong { got_answer = true; }
		let _ = store::create_reason(
			root, question_id, &c.doc_id, kind, &c.body, question_purpose,
		);
		if c.score >= strong { strong_edges.push(c.clone()); }
	}
	if !got_answer {
		return Ok(0);
	}

	if let Ok(mut q) = store::get_document(root, "questions", question_id) {
		if !q.tags.iter().any(|t| t == "resolved") {
			q.tags.push("resolved".to_string());
			let _ = store::update_document(root, "questions", question_id, None, Some(q.tags));
		}
	}

	// Collect bridging purposes from each strong candidate's existing Supports
	// edges (the purposes being bridged INTO this answer).
	let mut bridges: HashSet<String> = HashSet::new();
	for c in &strong_edges {
		for p in candidate_support_purposes(root, &c.doc_id) {
			if Some(p.as_str()) != question_purpose {
				bridges.insert(p);
			}
		}
		// Also include the candidate doc's own purpose if it differs.
		for dt in &["thoughts", "conclusions", "entities"] {
			if let Ok(d) = store::get_document(root, dt, &c.doc_id) {
				if let Some(p) = d.purpose {
					if !p.is_empty() && Some(p.as_str()) != question_purpose {
						bridges.insert(p);
					}
				}
				break;
			}
		}
	}

	let cid = promote_to_conclusion(root, question_id, &strong_edges).await?;
	if let Some(cid) = cid.as_ref() {
		let _ = tag_conclusion_with_bridges(root, cid, &bridges);
	}
	Ok(strong_edges.len())
}

/// Cross-purpose synthesis pass for a single question.
///
/// Re-runs `cross_reference_question` with no purpose filter (full vault),
/// applies a `BRIDGING_BONUS` multiplier to candidates that already have
/// `Supports` edges from a different purpose than the question's, and on
/// strong matches emits `Answers` edges + marks the question resolved + tags
/// the resulting conclusion as `crosstopic` with `bridges-<purpose>` tags.
///
/// Returns the number of strong (≥ `WIKI_ANSWER_THRESHOLD`) edges emitted.
/// Does NOT recurse — call once per question per pass.
pub async fn cross_topic_pass(root: &Path, question_id: &str) -> Result<usize> {
	let q = store::get_document(root, "questions", question_id)?;
	let cands = cross_reference_question(root, &q.title, None).await?;
	if cands.is_empty() {
		return Ok(0);
	}
	let scored = apply_bridging_bonus(root, cands, q.purpose.as_deref());
	cross_topic_emit_and_promote(root, question_id, q.purpose.as_deref(), &scored).await
}

const DEFAULT_CONCLUSION_MERGE_THRESHOLD: f32 = 0.92;

fn conclusion_merge_threshold() -> f32 {
	std::env::var("WIKI_CONCLUSION_MERGE_THRESHOLD")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(DEFAULT_CONCLUSION_MERGE_THRESHOLD)
}

/// Find an existing conclusion in the same `purpose` whose embedding cosine
/// against `body_emb` meets or exceeds `threshold`. Returns the conclusion id
/// of the best match, or `None`. Uses the in-memory pool; conclusions absent
/// from the pool are silently skipped (they will be considered on the next
/// search-driven pool refresh).
pub fn find_similar_conclusion(
	root: &Path,
	body_emb: &[f32],
	purpose_tag: &str,
	threshold: f32,
) -> Option<(String, f32)> {
	let candidates = cache::tag_index_lookup(root, purpose_tag);
	let mut best: Option<(String, f32)> = None;
	for dref in candidates {
		if dref.doc_type != "conclusions" {
			continue;
		}
		let Some(entry) = cache::pool_get(&dref.id) else { continue };
		let s = classifier::cosine(body_emb, &entry.vec);
		if s >= threshold && best.as_ref().is_none_or(|(_, bs)| s > *bs) {
			best = Some((dref.id.clone(), s));
		}
	}
	best
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

	// Embedding-similarity merge: if the synthesized body is near-identical to
	// an existing conclusion in this purpose, consolidate instead of forking.
	let body_text = format!("{}\n\n{}", question.title, parsed.body);
	let threshold = conclusion_merge_threshold();
	let body_embs = http::embed_batch(&[body_text]).await?;
	if let Some(body_emb) = body_embs.into_iter().next() {
		if let Some((existing_id, _)) =
			find_similar_conclusion(root, &body_emb, &purpose_tag, threshold)
		{
			let _ = store::create_reason(
				root,
				question_id,
				&existing_id,
				"Consolidates",
				"merged into existing conclusion via embedding similarity",
				purpose.as_deref(),
			);
			eprintln!("merged into existing conclusion {}", existing_id);
			return Ok(Some(existing_id));
		}
	}

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

	fn seed_pool_entry(id: &str, doc_type: &str, title: &str, content: &str, vec: Vec<f32>) {
		let doc = store::Document {
			id: id.to_string(),
			title: title.to_string(),
			tags: vec![],
			purpose: None,
			source_doc_id: None,
			created_at: String::new(),
			updated_at: String::new(),
			content: content.to_string(),
		};
		cache::pool_insert(cache::PoolEntry {
			doc_type: doc_type.to_string(),
			doc,
			content_hash: "x".to_string(),
			vec,
		});
	}

	#[test]
	fn promote_creates_new_when_no_similar() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		// Empty vault: no conclusions in purpose => merge check returns None.
		let probe = vec![1.0, 0.0, 0.0];
		let hit = find_similar_conclusion(root, &probe, "general", 0.92);
		assert!(hit.is_none(), "expected no merge candidate in empty vault");
	}

	#[test]
	fn promote_merges_when_similar() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();

		// Existing conclusion in purpose `general` with a known embedding.
		let existing_emb = vec![0.6, 0.8, 0.0];
		let cdoc = store::create_document(
			root,
			"conclusions",
			"X means Y",
			"X means Y because reasons.",
			vec!["conclusion".to_string(), "general".to_string()],
			Some("general"),
			None,
		).unwrap();
		seed_pool_entry(&cdoc.id, "conclusions", &cdoc.title, &cdoc.content, existing_emb.clone());

		// Probe is near-identical (cosine ≈ 1.0) → should merge.
		let probe = vec![0.6, 0.8, 0.0];
		let hit = find_similar_conclusion(root, &probe, "general", 0.92);
		assert_eq!(hit.as_ref().map(|(id, _)| id.clone()), Some(cdoc.id.clone()));

		// A clearly orthogonal probe should not match.
		let ortho = vec![0.0, 0.0, 1.0];
		let no_hit = find_similar_conclusion(root, &ortho, "general", 0.92);
		assert!(no_hit.is_none());

		cache::pool_remove(&cdoc.id);
	}

	#[test]
	fn merge_threshold_respects_env() {
		// Default threshold path.
		std::env::remove_var("WIKI_CONCLUSION_MERGE_THRESHOLD");
		assert!((conclusion_merge_threshold() - 0.92).abs() < 1e-6);

		// Override path: a 0.99 threshold rejects a high-but-not-perfect cosine.
		std::env::set_var("WIKI_CONCLUSION_MERGE_THRESHOLD", "0.99");
		assert!((conclusion_merge_threshold() - 0.99).abs() < 1e-6);

		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		let cdoc = store::create_document(
			root,
			"conclusions",
			"Z",
			"z body",
			vec!["conclusion".to_string(), "general".to_string()],
			Some("general"),
			None,
		).unwrap();
		// Existing has a slightly different embedding (cosine ≈ 0.94).
		let existing = vec![1.0, 0.0, 0.0];
		seed_pool_entry(&cdoc.id, "conclusions", &cdoc.title, &cdoc.content, existing);
		let probe = vec![0.94, 0.34, 0.0]; // cosine with [1,0,0] ≈ 0.94
		let cos = classifier::cosine(&probe, &[1.0, 0.0, 0.0]);
		assert!(cos > 0.92 && cos < 0.99, "cos={}", cos);

		// At 0.99 threshold -> no merge.
		let strict = conclusion_merge_threshold();
		assert!(find_similar_conclusion(root, &probe, "general", strict).is_none());
		// At 0.92 threshold -> merge.
		assert!(find_similar_conclusion(root, &probe, "general", 0.92).is_some());

		cache::pool_remove(&cdoc.id);
		std::env::remove_var("WIKI_CONCLUSION_MERGE_THRESHOLD");
	}

	fn mk_thought(root: &Path, title: &str, purpose: &str) -> String {
		let tags = vec!["thought".to_string(), purpose.to_string()];
		store::create_document(root, "thoughts", title, "body", tags, Some(purpose), None)
			.unwrap()
			.id
	}

	#[tokio::test]
	async fn cursor_advances_after_pass() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		let mut ids = Vec::new();
		for i in 0..5 { ids.push(mk_thought(root, &format!("t{}", i), "p1")); }

		let _ = run_pass(root, 2, Some("p1"), false, false, false).await.unwrap();
		let cur = read_pass_cursor(root, "p1").expect("cursor written");
		assert_eq!(cur.0, "thoughts");
		// Cursor must point at one of the thoughts processed.
		assert!(ids.contains(&cur.1));
	}

	#[tokio::test]
	async fn cursor_resumes_from_position() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		let mut ids = Vec::new();
		for i in 0..5 { ids.push(mk_thought(root, &format!("t{}", i), "p1")); }
		// list_documents is filesystem-ordered; capture true order:
		let docs = store::list_documents(root, "thoughts").unwrap();
		let order: Vec<String> = docs.into_iter().map(|d| d.id).collect();
		assert_eq!(order.len(), 5);

		// Set cursor at order[2] → next pass should start at order[3].
		write_pass_cursor(root, "p1", "thoughts", &order[2]).unwrap();
		let report = run_pass(root, 2, Some("p1"), false, false, false).await.unwrap();
		let details = report["details"].as_array().unwrap();
		let processed_ids: Vec<String> = details
			.iter()
			.filter_map(|d| d.get("doc_id").and_then(|v| v.as_str()).map(String::from))
			.collect();
		assert_eq!(processed_ids, vec![order[3].clone(), order[4].clone()]);
	}

	#[tokio::test]
	async fn cursor_wraps_when_exhausted() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		for i in 0..3 { mk_thought(root, &format!("t{}", i), "p1"); }
		let docs = store::list_documents(root, "thoughts").unwrap();
		let order: Vec<String> = docs.into_iter().map(|d| d.id).collect();

		// Cursor at last → next pass should wrap to start.
		write_pass_cursor(root, "p1", "thoughts", &order[2]).unwrap();
		let report = run_pass(root, 2, Some("p1"), false, false, false).await.unwrap();
		let details = report["details"].as_array().unwrap();
		let processed: Vec<String> = details
			.iter()
			.filter_map(|d| d.get("doc_id").and_then(|v| v.as_str()).map(String::from))
			.collect();
		assert_eq!(processed, vec![order[0].clone(), order[1].clone()]);
	}

	#[test]
	fn last_qa_at_skips_recent() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		let id = mk_thought(root, "t-recent", "p1");
		stamp_last_qa_at(root, "thoughts", &id).unwrap();
		assert!(doc_qa_is_recent(root, "thoughts", &id));

		// Old timestamp → not recent.
		let dir2 = root.join("thoughts");
		let path = store::find_document_path_by_id(&dir2, &id).unwrap();
		let raw = std::fs::read_to_string(&path).unwrap();
		let new = raw.replace(
			&chrono::Utc::now().to_rfc3339()[..4],
			"2000",
		);
		// Just rewrite frontmatter with explicit old ts to be safe:
		let (mut fm, body) = store::parse_frontmatter(&new).unwrap();
		fm.as_object_mut().unwrap().insert(
			"last_qa_at".to_string(),
			serde_json::json!("2000-01-01T00:00:00+00:00"),
		);
		let fm_str = serde_yaml::to_string(&fm).unwrap();
		crate::io::write_atomic_str(&path, &format!("---\n{}---\n\n{}", fm_str, body)).unwrap();
		assert!(!doc_qa_is_recent(root, "thoughts", &id));
	}

	#[test]
	fn force_overrides_last_qa_at() {
		// `force=true` bypasses the recent-skip branch in run_pass.
		// Logic check (no LLM): the run_pass branch is `if !force && qa && doc_qa_is_recent`,
		// so with force=true the doc is processed regardless. We assert this guard directly.
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		let id = mk_thought(root, "t-recent", "p1");
		stamp_last_qa_at(root, "thoughts", &id).unwrap();
		let recent = doc_qa_is_recent(root, "thoughts", &id);
		assert!(recent);
		let force = true;
		let qa = true;
		let should_skip = !force && qa && recent;
		assert!(!should_skip, "force must override recent-skip");
	}

	#[tokio::test]
	async fn promote_to_conclusion_idempotent() {
		cache::invalidate_indexes();
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

	#[test]
	fn template_regex_matches_relate_form() {
		assert!(crate::config::is_template_question(
			"How does 'GPU Pipeline (8-Pass)' relate to or differ from similar concepts?"
		));
		assert!(crate::config::is_template_question(
			"What are the key characteristics of 'XPBD'?"
		));
		assert!(crate::config::is_template_question(
			"What are the implications of 'Visibility Buffer'?"
		));
		assert!(crate::config::is_template_question(
			"What is the importance of 'foo'?"
		));
		// Novel question, not template-shaped.
		assert!(!crate::config::is_template_question(
			"Why does the 8-Pass pipeline tile in 32x32 blocks instead of 16x16?"
		));
	}

	#[test]
	fn raise_questions_skips_templates() {
		// Three candidates: 2 templated, 1 novel. Filter must keep 1.
		let items = vec![
			RaisedQItem {
				title: "How does 'XPBD' relate to or differ from similar concepts?".into(),
				body: "x".into(),
			},
			RaisedQItem {
				title: "Why is the substep count fixed at 8?".into(),
				body: "Body sentence one. Body sentence two.".into(),
			},
			RaisedQItem {
				title: "What are the implications of 'Substep'?".into(),
				body: "y".into(),
			},
		];
		let (kept, skipped) = filter_raised_candidates(items);
		assert_eq!(kept.len(), 1);
		assert_eq!(kept[0].title, "Why is the substep count fixed at 8?");
		assert_eq!(skipped.len(), 2);
	}

	#[test]
	fn purpose_cap_blocks_new_raises() {
		cache::invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		// Force a low cap so we don't have to spam create_document.
		std::env::set_var("WIKI_OPEN_QUESTIONS_PER_PURPOSE_CAP", "3");
		// Three open questions in purpose "phyons".
		for i in 0..3 {
			let title = format!("Open question #{}?", i);
			let hash = fnv_question_id(&title);
			let tags = vec!["question".to_string(), "phyons".to_string(), hash];
			store::create_document(root, "questions", &title, "b", tags, Some("phyons"), None).unwrap();
		}
		assert_eq!(count_open_questions_in_purpose(root, Some("phyons")), 3);
		// Add one resolved question; should not count.
		let title = "Resolved Q?";
		let hash = fnv_question_id(title);
		let tags = vec!["question".to_string(), "phyons".to_string(), hash, "resolved".to_string()];
		store::create_document(root, "questions", title, "b", tags, Some("phyons"), None).unwrap();
		assert_eq!(count_open_questions_in_purpose(root, Some("phyons")), 3);
		std::env::remove_var("WIKI_OPEN_QUESTIONS_PER_PURPOSE_CAP");
	}

	fn seed_open_question(root: &Path, purpose: &str, title: &str, vec: Vec<f32>) -> String {
		let hash = fnv_question_id(title);
		let tags = vec!["question".to_string(), purpose.to_string(), hash];
		let q = store::create_document(root, "questions", title, "body", tags, Some(purpose), None).unwrap();
		seed_pool_entry(&q.id, "questions", title, "body", vec);
		q.id
	}

	#[tokio::test]
	async fn dedupe_drops_near_duplicate() {
		cache::invalidate_indexes();
		std::env::remove_var("WIKI_QUESTION_DEDUPE_THRESHOLD");
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();

		// Existing open question w/ embedding e1.
		let e1 = vec![1.0, 0.0, 0.0];
		let qid = seed_open_question(root, "p1", "What causes X?", e1.clone());

		// Candidate near-identical embedding e2 (cosine ≈ 1.0 with e1).
		let e2 = vec![1.0, 0.0, 0.0];
		let existing = gather_open_question_embeddings(root, "p1").await.unwrap();
		assert_eq!(existing.len(), 1);
		let kept = dedupe_candidates_by_embedding(&[e2], &existing, 0.88);
		assert!(kept.is_empty(), "near-duplicate must be dropped");

		cache::pool_remove(&qid);
	}

	#[tokio::test]
	async fn dedupe_keeps_distinct() {
		cache::invalidate_indexes();
		std::env::remove_var("WIKI_QUESTION_DEDUPE_THRESHOLD");
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();

		let qid = seed_open_question(root, "p1", "What causes X?", vec![1.0, 0.0, 0.0]);
		let cand = vec![0.0, 1.0, 0.0]; // orthogonal -> cosine 0
		let existing = gather_open_question_embeddings(root, "p1").await.unwrap();
		let kept = dedupe_candidates_by_embedding(&[cand], &existing, 0.88);
		assert_eq!(kept, vec![0]);

		cache::pool_remove(&qid);
	}

	#[tokio::test]
	async fn dedupe_threshold_respects_env() {
		cache::invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();

		let qid = seed_open_question(root, "p1", "What causes X?", vec![1.0, 0.0, 0.0]);
		// Near-but-not-perfect cosine ≈ 0.94.
		let cand = vec![0.94, 0.34, 0.0];
		let cos = classifier::cosine(&cand, &[1.0, 0.0, 0.0]);
		assert!(cos > 0.88 && cos < 0.99, "cos={}", cos);

		// Default 0.88 threshold -> dropped.
		std::env::remove_var("WIKI_QUESTION_DEDUPE_THRESHOLD");
		let t_default = question_dedupe_threshold();
		assert!((t_default - 0.88).abs() < 1e-6);
		let existing = gather_open_question_embeddings(root, "p1").await.unwrap();
		assert!(dedupe_candidates_by_embedding(std::slice::from_ref(&cand), &existing, t_default).is_empty());

		// 0.99 threshold -> kept.
		std::env::set_var("WIKI_QUESTION_DEDUPE_THRESHOLD", "0.99");
		let t_strict = question_dedupe_threshold();
		assert!((t_strict - 0.99).abs() < 1e-6);
		assert_eq!(
			dedupe_candidates_by_embedding(&[cand], &existing, t_strict),
			vec![0]
		);

		std::env::remove_var("WIKI_QUESTION_DEDUPE_THRESHOLD");
		cache::pool_remove(&qid);
	}

	#[tokio::test]
	async fn dedupe_runs_after_template_filter() {
		cache::invalidate_indexes();
		std::env::remove_var("WIKI_QUESTION_DEDUPE_THRESHOLD");
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();

		// Existing open Q in purpose w/ known embedding.
		let qid = seed_open_question(root, "p1", "What causes X?", vec![1.0, 0.0, 0.0]);

		// Three LLM-shaped candidates: 1 templated, 1 semantic dupe, 1 novel.
		let raw = vec![
			RaisedQItem {
				title: "How does 'X' relate to or differ from similar concepts?".into(),
				body: "b".into(),
			},
			RaisedQItem {
				title: "What is the cause of X?".into(),
				body: "Body sentence one. Body sentence two.".into(),
			},
			RaisedQItem {
				title: "Why does X happen at high latency?".into(),
				body: "Body sentence one. Body sentence two.".into(),
			},
		];
		let (kept_after_template, skipped) = filter_raised_candidates(raw);
		let templated_dropped = skipped.len();
		assert_eq!(templated_dropped, 1);
		assert_eq!(kept_after_template.len(), 2);

		// Inject candidate embeddings: index 0 = near-dupe of e1; index 1 = orthogonal.
		let cand_embs = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]];
		let existing = gather_open_question_embeddings(root, "p1").await.unwrap();
		let keep_idx = dedupe_candidates_by_embedding(&cand_embs, &existing, 0.88);
		let semantic_dropped = cand_embs.len() - keep_idx.len();
		assert_eq!(semantic_dropped, 1);
		assert_eq!(keep_idx, vec![1]); // novel survives

		// Counts distinguished: 1 templated, 1 semantic, 1 passed.
		assert_eq!(templated_dropped, 1);
		assert_eq!(semantic_dropped, 1);
		assert_eq!(keep_idx.len(), 1);

		cache::pool_remove(&qid);
	}

	#[test]
	fn migration_deletes_unanswered_templates() {
		cache::invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();

		// Anchor doc to point Answers at.
		let anchor = store::create_document(
			root, "thoughts", "anchor", "b", vec!["thought".into()], Some("phyons"), None,
		).unwrap();

		// Three templated questions.
		let mut q_ids = Vec::new();
		for (i, t) in [
			"How does 'A' relate to or differ from similar concepts?",
			"What are the key characteristics of 'B'?",
			"What are the implications of 'C'?",
		].iter().enumerate() {
			let hash = fnv_question_id(t);
			let tags = vec!["question".to_string(), "phyons".to_string(), hash];
			let q = store::create_document(root, "questions", t, "b", tags, Some("phyons"), None).unwrap();
			if i == 0 {
				// Q[0] has an Answers edge -> must be kept.
				store::create_reason(root, &anchor.id, &q.id, "Answers", "answers it", Some("phyons")).unwrap();
			}
			q_ids.push(q.id);
		}
		// One novel question that must never be touched.
		let novel_hash = fnv_question_id("Novel question that survives?");
		let novel_tags = vec!["question".to_string(), "phyons".to_string(), novel_hash];
		let novel = store::create_document(
			root, "questions", "Novel question that survives?", "b", novel_tags, Some("phyons"), None,
		).unwrap();

		let report = migrate_templated_questions(root, false).unwrap();
		assert_eq!(report.scanned, 4);
		assert_eq!(report.templated, 3);
		assert_eq!(report.deleted, 2);
		assert_eq!(report.kept_with_answers.len(), 1);

		// Surviving questions: q[0] (had Answers) + novel.
		let remaining = store::list_documents(root, "questions").unwrap();
		let remaining_ids: std::collections::HashSet<_> =
			remaining.iter().map(|d| d.id.clone()).collect();
		assert!(remaining_ids.contains(&q_ids[0]));
		assert!(remaining_ids.contains(&novel.id));
		assert!(!remaining_ids.contains(&q_ids[1]));
		assert!(!remaining_ids.contains(&q_ids[2]));
	}

	#[test]
	fn migration_dry_run_deletes_nothing() {
		cache::invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();
		let t = "What are the implications of 'X'?";
		let hash = fnv_question_id(t);
		let tags = vec!["question".to_string(), "p".to_string(), hash];
		store::create_document(root, "questions", t, "b", tags, Some("p"), None).unwrap();
		let r = migrate_templated_questions(root, true).unwrap();
		assert_eq!(r.templated, 1);
		assert_eq!(r.deleted, 1); // counted but not actually deleted
		assert_eq!(store::list_documents(root, "questions").unwrap().len(), 1);
	}

	// ── Cross-purpose synthesis (item #4) ────────────────────────────────────

	#[tokio::test]
	async fn cross_topic_finds_bridging_answer() {
		// Question lives in purpose A, candidate doc in purpose B is the actual
		// answer. We exercise the LLM-free inner step
		// (`cross_topic_emit_and_promote`) with a hand-built strong candidate
		// to verify edges + resolved + bridge tags. The pre-seeded conclusion
		// short-circuits `promote_to_conclusion`'s LLM call.
		cache::invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();

		// Question in purpose A.
		let qtitle = "How does Clustered Forward+ relate to deferred shading?";
		let qhash = fnv_question_id(qtitle);
		let qtags = vec!["question".to_string(), "phyons".to_string(), qhash.clone()];
		let qdoc = store::create_document(root, "questions", qtitle, "qbody", qtags, Some("phyons"), None).unwrap();

		// Candidate doc in purpose B (the bridging answer).
		let cand = store::create_document(
			root,
			"thoughts",
			"Forward+ Clustering",
			"Clustered Forward+ partitions the view frustum into 3D clusters.",
			vec!["thought".to_string(), "forward-plus".to_string()],
			Some("forward-plus"),
			None,
		).unwrap();

		// Pre-seed a conclusion with the matching qhash so promote short-circuits.
		let cdoc = store::create_document(
			root,
			"conclusions",
			qtitle,
			"existing",
			vec!["conclusion".to_string(), "phyons".to_string(), qhash.clone()],
			Some("phyons"),
			None,
		).unwrap();

		let cands = vec![AnswerCandidate {
			doc_id: cand.id.clone(),
			doc_type: "thoughts".to_string(),
			score: 0.95,
			kind: "Answers".to_string(),
			body: "directly explains the relation".to_string(),
		}];

		let n = cross_topic_emit_and_promote(root, &qdoc.id, Some("phyons"), &cands).await.unwrap();
		assert_eq!(n, 1, "one strong edge expected");

		// Question is now resolved.
		let q = store::get_document(root, "questions", &qdoc.id).unwrap();
		assert!(q.tags.iter().any(|t| t == "resolved"));

		// Answers edge emitted.
		let from_q = store::search_reasons_for(root, &qdoc.id, "from").unwrap();
		assert!(
			from_q.iter().any(|r| r.title.contains("-[Answers]->") && r.title.ends_with(&cand.id)),
			"expected Answers edge q→cand"
		);

		// Existing conclusion picked up bridge tags.
		let cf = store::get_document(root, "conclusions", &cdoc.id).unwrap();
		assert!(cf.tags.iter().any(|t| t == "crosstopic"));
		assert!(cf.tags.iter().any(|t| t == "bridges-forward-plus"));
	}

	#[test]
	fn bridging_bonus_applied() {
		// Candidate w/ Supports from 2 purposes (one different from question's)
		// should be boosted; candidate w/ Supports only from same-purpose docs
		// should not.
		cache::invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();

		let cand_a = store::create_document(
			root, "thoughts", "A", "a body",
			vec!["thought".to_string(), "phyons".to_string()],
			Some("phyons"), None,
		).unwrap();
		let cand_b = store::create_document(
			root, "thoughts", "B", "b body",
			vec!["thought".to_string(), "phyons".to_string()],
			Some("phyons"), None,
		).unwrap();

		// Source docs that emit Supports edges into the candidates.
		let same_purpose_src = store::create_document(
			root, "thoughts", "src-same", "x",
			vec!["thought".to_string(), "phyons".to_string()],
			Some("phyons"), None,
		).unwrap();
		let other_purpose_src = store::create_document(
			root, "thoughts", "src-other", "y",
			vec!["thought".to_string(), "forward-plus".to_string()],
			Some("forward-plus"), None,
		).unwrap();

		// cand_a gets Supports from BOTH purposes (bridges).
		store::create_reason(root, &same_purpose_src.id, &cand_a.id, "Supports", "z", Some("phyons")).unwrap();
		store::create_reason(root, &other_purpose_src.id, &cand_a.id, "Supports", "z", Some("forward-plus")).unwrap();
		// cand_b gets Supports only from the question's own purpose.
		store::create_reason(root, &same_purpose_src.id, &cand_b.id, "Supports", "z", Some("phyons")).unwrap();

		cache::invalidate_indexes();

		let cands = vec![
			AnswerCandidate { doc_id: cand_a.id.clone(), doc_type: "thoughts".into(), score: 0.5, kind: "Supports".into(), body: "".into() },
			AnswerCandidate { doc_id: cand_b.id.clone(), doc_type: "thoughts".into(), score: 0.5, kind: "Supports".into(), body: "".into() },
		];
		let scored = apply_bridging_bonus(root, cands, Some("phyons"));
		let a = scored.iter().find(|c| c.doc_id == cand_a.id).unwrap();
		let b = scored.iter().find(|c| c.doc_id == cand_b.id).unwrap();
		assert!(a.score > b.score, "bridging candidate should outrank single-purpose");
		assert!((a.score - 0.75).abs() < 1e-5, "expected 1.5× boost, got {}", a.score);
		assert!((b.score - 0.5).abs() < 1e-5, "expected no boost, got {}", b.score);
	}

	#[test]
	fn auto_trigger_on_multi_support_question() {
		// A question with Supports edges from ≥2 distinct purposes triggers
		// `should_invoke_cross_topic` without needing a QA failure.
		cache::invalidate_indexes();
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::ensure_wiki_layout(root).unwrap();

		let qtitle = "Open question?";
		let qhash = fnv_question_id(qtitle);
		let qdoc = store::create_document(
			root, "questions", qtitle, "body",
			vec!["question".to_string(), "phyons".to_string(), qhash],
			Some("phyons"), None,
		).unwrap();

		let src_a = store::create_document(
			root, "thoughts", "src-A", "a",
			vec!["thought".to_string(), "phyons".to_string()],
			Some("phyons"), None,
		).unwrap();
		let src_b = store::create_document(
			root, "thoughts", "src-B", "b",
			vec!["thought".to_string(), "forward-plus".to_string()],
			Some("forward-plus"), None,
		).unwrap();

		store::create_reason(root, &src_a.id, &qdoc.id, "Supports", "z", Some("phyons")).unwrap();
		// Single-purpose so far → must NOT trigger.
		cache::invalidate_indexes();
		assert!(!should_invoke_cross_topic(root, &qdoc.id));

		store::create_reason(root, &src_b.id, &qdoc.id, "Supports", "z", Some("forward-plus")).unwrap();
		cache::invalidate_indexes();
		assert!(should_invoke_cross_topic(root, &qdoc.id));
		let purposes = question_support_purposes(root, &qdoc.id);
		assert!(purposes.contains("phyons"));
		assert!(purposes.contains("forward-plus"));
	}
}
