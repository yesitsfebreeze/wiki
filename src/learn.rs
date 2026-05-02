use crate::{classifier, search, smart, store};
use anyhow::Result;
use regex::Regex;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Clone)]
pub struct EntityRef {
	pub id: String,
	pub title: String,
	pub aliases: Vec<String>,
	pub slug: String,
	pub body_embedding: Option<Vec<f32>>,
}

fn dedupe_threshold() -> f32 {
	std::env::var("WIKI_DEDUPE_THRESHOLD")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(0.85)
}

fn fnv64(s: &str) -> u64 {
	let mut h: u64 = 0xcbf29ce484222325;
	for b in s.bytes() {
		h ^= b as u64;
		h = h.wrapping_mul(0x100000001b3);
	}
	h
}

fn read_entity_meta(root: &Path, id: &str) -> Option<(Vec<String>, String)> {
	let dir = root.join("entities");
	let entries = std::fs::read_dir(&dir).ok()?;
	for entry in entries.flatten() {
		let path = entry.path();
		if path.extension().and_then(|s| s.to_str()) != Some("md") {
			continue;
		}
		let Ok(raw) = std::fs::read_to_string(&path) else {
			continue;
		};
		let Ok((fm, _)) = store::parse_frontmatter(&raw) else {
			continue;
		};
		if fm.get("id").and_then(|v| v.as_str()) == Some(id) {
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
			return Some((aliases, slug));
		}
	}
	None
}

pub fn build_entity_index(root: &Path) -> Result<Vec<EntityRef>> {
	let entities = store::list_documents(root, "entities")?;
	let mut refs: Vec<EntityRef> = Vec::new();
	let mut texts: Vec<String> = Vec::new();
	for e in entities {
		let (aliases, slug) = read_entity_meta(root, &e.id).unwrap_or((Vec::new(), e.id.clone()));
		texts.push(e.content.clone());
		refs.push(EntityRef {
			id: e.id.clone(),
			title: e.title.clone(),
			aliases,
			slug,
			body_embedding: None,
		});
	}
	if !texts.is_empty() {
		if let Ok(embs) = classifier::embed_batch(&texts) {
			for (r, emb) in refs.iter_mut().zip(embs.into_iter()) {
				r.body_embedding = Some(emb);
			}
		}
	}
	Ok(refs)
}

fn protected_ranges(text: &str) -> Vec<(usize, usize)> {
	let mut ranges = Vec::new();
	let patterns = [
		r"(?ms)^```.*?^```",
		r"`[^`\n]+`",
		r"\[\[[^\]]*\]\]",
		r"\[[^\]]*\]\([^)]*\)",
	];
	for p in patterns {
		if let Ok(re) = Regex::new(p) {
			for m in re.find_iter(text) {
				ranges.push((m.start(), m.end()));
			}
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
			// Collect as alias candidate if surface is a new name variant for this entity.
			let surface_lc = surface.to_lowercase();
			let already_known = e.title.to_lowercase() == surface_lc
				|| e.aliases.iter().any(|a| a.to_lowercase() == surface_lc);
			if !already_known {
				alias_candidates.push((e.id.clone(), surface.clone()));
			}
			let link = format!("[[{}|{}]]", e.slug, surface);
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

pub fn find_near_duplicate_entity(root: &Path, title: &str, content: &str) -> Result<Option<EntityRef>> {
	let threshold = std::env::var("WIKI_ALIAS_THRESHOLD")
		.ok()
		.and_then(|s| s.parse::<f32>().ok())
		.unwrap_or(0.92);

	let entities = build_entity_index(root)?;
	let title_lc = title.trim().to_lowercase();

	// Exact title or alias match first (cheap).
	for e in &entities {
		if e.title.to_lowercase() == title_lc {
			return Ok(Some(e.clone()));
		}
		if e.aliases.iter().any(|a| a.to_lowercase() == title_lc) {
			return Ok(Some(e.clone()));
		}
	}

	// Embedding similarity (requires OpenAI key).
	if !content.is_empty() {
		if let Ok(embs) = classifier::embed_batch(&[content.to_string()]) {
			if let Some(content_emb) = embs.into_iter().next() {
				for e in &entities {
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

fn dedupe_paragraphs(
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
	let embs = match classifier::embed_batch(&texts) {
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
			let Some(ev) = &e.body_embedding else {
				continue;
			};
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

pub fn link_doc_internal(
	root: &Path,
	doc_type: &str,
	id: &str,
	entities: &[EntityRef],
	dry_run: bool,
) -> Result<serde_json::Value> {
	let doc = store::get_document(root, doc_type, id)?;
	let original = doc.content.clone();
	let (linked, link_count, alias_candidates) = rewrite_links(&original, entities, id);
	let (deduped, merges) = dedupe_paragraphs(&linked, entities, id);
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

	Ok(serde_json::json!({
		"doc_id": id,
		"doc_type": doc_type,
		"links_added": link_count,
		"aliases_added": aliases_added,
		"paragraphs_merged": merges.len(),
		"modified": !dry_run && modified,
		"dry_run": dry_run,
	}))
}

pub fn link_doc(root: &Path, doc_type: &str, id: &str, dry_run: bool) -> Result<serde_json::Value> {
	let entities = build_entity_index(root)?;
	link_doc_internal(root, doc_type, id, &entities, dry_run)
}

pub fn run_pass(
	root: &Path,
	limit: usize,
	purpose: Option<&str>,
	dry_run: bool,
) -> Result<serde_json::Value> {
	let entities = build_entity_index(root)?;
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
	let mut details = Vec::new();
	for (dt, id) in &targets {
		match link_doc_internal(root, dt, id, &entities, dry_run) {
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
	}

	let report = serde_json::json!({
		"pass_id": chrono::Utc::now().to_rfc3339(),
		"docs_scanned": targets.len(),
		"docs_modified": docs_modified,
		"links_added": links_added,
		"paragraphs_merged": merges_total,
		"entity_count": entities.len(),
		"purpose_filter": purpose,
		"dry_run": dry_run,
		"details": details,
	});

	let log_dir = root.join("ingest_log");
	let _ = std::fs::create_dir_all(&log_dir);
	let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
	let _ = std::fs::write(
		log_dir.join(format!("learn-{}.json", ts)),
		serde_json::to_string_pretty(&report).unwrap_or_default(),
	);

	if !dry_run {
		let index_path = root.join(".search");
		if let Ok(index) = search::create_index(&index_path) {
			for (dt, id) in &targets {
				if let Ok(doc) = store::get_document(root, dt, id) {
					let _ = search::index_document(&index, &doc);
				}
			}
		}
	}

	Ok(report)
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

fn write_cursor(root: &Path, off: u64) {
	let p = root.join(".feedback.cursor");
	let _ = std::fs::write(&p, off.to_string());
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

fn decide_via_llm(entry: &FeedbackEntry, picks_ctx: &str) -> Result<LlmDecision> {
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
	let content = smart::chat_json(sys, &user)?;
	let parsed: LlmDecision = serde_json::from_str(&content)
		.map_err(|e| anyhow::anyhow!("decision parse: {} body: {}", e, content))?;
	Ok(parsed)
}

fn process_entry(
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
		for dt in &[
			"entities",
			"thoughts",
			"conclusions",
			"reasons",
			"questions",
		] {
			if let Ok(d) = store::get_document(root, dt, pid) {
				doc_lookup = Some(((*dt).to_string(), d));
				break;
			}
		}
		let Some((dt, doc)) = doc_lookup else {
			continue;
		};
		found_any = true;
		let snippet = if doc.content.len() > 400 {
			format!("{}…", &doc.content[..400])
		} else {
			doc.content.clone()
		};
		let r = reason_map.get(pid.as_str()).copied().unwrap_or("");
		picks_ctx.push_str(&format!(
			"- id={} type={} title={:?} reason={:?}\n  snippet={}\n",
			pid, dt, doc.title, r, snippet
		));
		// link wikilinks for picked docs (covers thoughts/conclusions/entities)
		if !dry_run {
			let _ = link_doc_internal(root, &dt, pid, entities, false);
			linked_ids.push(pid.clone());
		}
	}
	if !found_any {
		return Ok(serde_json::json!({"skipped": "no picks resolved"}));
	}

	let decision = match decide_via_llm(entry, &picks_ctx) {
		Ok(d) => d,
		Err(e) => return Ok(serde_json::json!({"error": e.to_string()})),
	};

	let hash_id = fnv_question_id(&entry.question);
	let mut question_id: Option<String> = find_question_by_hash(root, &hash_id);
	let mut created_question = false;

	if decision.keep_question && question_id.is_none() && !dry_run {
		let purpose = decision.purpose.clone().unwrap_or_else(|| {
			entry
				.tag_filter
				.clone()
				.unwrap_or_else(|| "general".to_string())
		});
		let body = decision
			.question_body
			.clone()
			.unwrap_or_else(|| entry.question.clone());
		let mut tags = vec!["question".to_string(), purpose.clone(), hash_id.clone()];
		if decision.resolved {
			tags.push("resolved".to_string());
		}
		if let Ok(qdoc) = store::create_document(
			root,
			"questions",
			decision
				.question_title
				.as_deref()
				.unwrap_or(entry.question.as_str()),
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
				if let Ok(_) = store::create_reason(
					root,
					qid,
					&edge.picked_id,
					kind,
					&edge.body,
					decision.purpose.as_deref(),
				) {
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

pub fn run_feedback_pass(root: &Path, limit: usize, dry_run: bool) -> Result<serde_json::Value> {
	let path = root.join("feedback.jsonl");
	if !path.exists() {
		return Ok(serde_json::json!({"processed": 0, "note": "no feedback.jsonl"}));
	}
	let raw = std::fs::read(&path)?;
	let cursor = read_cursor(root) as usize;
	let cursor = cursor.min(raw.len());
	let slice = &raw[cursor..];
	let text = std::str::from_utf8(slice).map_err(|e| anyhow::anyhow!("utf8: {}", e))?;

	let entities = build_entity_index(root)?;
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
			// don't advance past unprocessed
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
		match process_entry(root, &entry, &entities, dry_run) {
			Ok(v) => details.push(v),
			Err(e) => details.push(serde_json::json!({"error": e.to_string()})),
		}
		processed += 1;
	}

	if !dry_run {
		write_cursor(root, consumed_bytes);
	}

	let report = serde_json::json!({
		"pass_id": chrono::Utc::now().to_rfc3339(),
		"processed": processed,
		"cursor": consumed_bytes,
		"total_bytes": raw.len(),
		"dry_run": dry_run,
		"details": details,
	});

	let log_dir = root.join("ingest_log");
	let _ = std::fs::create_dir_all(&log_dir);
	let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
	let _ = std::fs::write(
		log_dir.join(format!("learn-feedback-{}.json", ts)),
		serde_json::to_string_pretty(&report).unwrap_or_default(),
	);

	Ok(report)
}

#[cfg(test)]
mod tests {
	use super::*;

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
	#[ignore]
	fn e2e_smart_and_feedback() {
		// Requires OPENAI_API_KEY. Run with:
		// cargo test --release e2e_smart_and_feedback -- --ignored --nocapture
		use crate::{search, smart, store};
		let tmp = tempfile::tempdir().expect("tmpdir");
		let root = tmp.path();
		store::ensure_wiki_layout(root).unwrap();
		store::create_purpose(root, "rust", "Rust", "Rust language topics").unwrap();
		store::create_purpose(root, "search", "Search", "Search infrastructure").unwrap();

		let docs_input = [
			(
				"Tantivy",
				"Tantivy is a Rust full-text search engine library similar to Lucene. \
				It uses an inverted index, BM25 scoring, and segment-based storage.",
				vec!["entity".into(), "search".into()],
				"search",
			),
			(
				"BM25 scoring",
				"BM25 is a probabilistic ranking function used by full-text search engines \
				to estimate the relevance of documents to a given query.",
				vec!["entity".into(), "search".into()],
				"search",
			),
			(
				"Kittens",
				"Kittens are juvenile cats. They sleep a lot and chase string.",
				vec!["entity".into(), "rust".into()],
				"rust",
			),
		];

		{
			let index_path = root.join(".search");
			let index = search::create_index(&index_path).unwrap();
			for (title, body, tags, purpose) in &docs_input {
				let d = store::create_document(
					root,
					"entities",
					title,
					body,
					tags.clone(),
					Some(purpose),
					None,
				)
				.unwrap();
				search::index_document(&index, &d).unwrap();
			}
		}

		let res = smart::smart_search(
			root,
			"which library does fulltext indexing in Rust?",
			None,
			10,
			3,
		)
		.expect("smart_search ok");
		println!(
			"--- smart_search result ---\n{}",
			serde_json::to_string_pretty(&res).unwrap()
		);
		let results = res["results"].as_array().expect("results array");
		assert!(!results.is_empty(), "expected non-empty results");
		let titles: Vec<String> = results
			.iter()
			.map(|r| r["title"].as_str().unwrap_or("").to_string())
			.collect();
		println!("retrieved titles: {:?}", titles);
		assert!(
			titles.iter().any(|t| t == "BM25 scoring"),
			"hybrid recall should surface BM25 scoring doc via embeddings"
		);

		let feedback = std::fs::read_to_string(root.join("feedback.jsonl")).expect("feedback");
		assert!(feedback.lines().count() >= 1);

		let report = run_feedback_pass(root, 10, false).expect("feedback pass");
		println!(
			"--- feedback pass report ---\n{}",
			serde_json::to_string_pretty(&report).unwrap()
		);
		assert_eq!(report["processed"].as_u64().unwrap(), 1);

		let questions = store::list_documents(root, "questions").unwrap();
		println!("questions created: {}", questions.len());
		let reasons = store::list_documents(root, "reasons").unwrap();
		println!("reasons created: {}", reasons.len());
		assert!(
			!reasons.is_empty(),
			"expected reason edges from question to picks"
		);
	}
}
