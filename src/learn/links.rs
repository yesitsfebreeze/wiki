//! Wikilink rewriting, paragraph dedupe orchestration, link_doc entry points,
//! crosstopic move, inbound link rewrite.

use super::dedup::dedupe_paragraphs;
use super::infra::{build_entity_index, EntityRef};
use crate::store;
use anyhow::Result;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

pub(crate) fn protected_re() -> &'static [Regex] {
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

fn code_protected_re() -> &'static [Regex] {
	static RE: OnceLock<Vec<Regex>> = OnceLock::new();
	RE.get_or_init(|| {
		[r"(?ms)^```.*?^```", r"`[^`\n]+`"]
			.iter()
			.filter_map(|p| Regex::new(p).ok())
			.collect()
	})
}

fn code_protected_ranges(text: &str) -> Vec<(usize, usize)> {
	let mut ranges = Vec::new();
	for re in code_protected_re() {
		for m in re.find_iter(text) {
			ranges.push((m.start(), m.end()));
		}
	}
	ranges.sort();
	ranges
}

pub(crate) fn link_target_re() -> &'static Regex {
	static RE: OnceLock<Regex> = OnceLock::new();
	RE.get_or_init(|| Regex::new(r"\[\[([^\]|#]+?)(?:[#|][^\]]*)?\]\]").unwrap())
}

pub(crate) fn protected_ranges(text: &str) -> Vec<(usize, usize)> {
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

/// Walk wikilinks in `body` and return the set of distinct purposes the
/// targets belong to.
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

const DOC_TYPES: &[&str] = &["thoughts", "entities", "conclusions", "reasons", "questions"];

/// Resolve a wikilink target string to `(doc_type, id)` if it points at an
/// existing doc. Accepts:
/// - 1-part: raw uuid — searched across all doc types.
/// - 2-part `<doc_type>/<id>`: direct lookup.
/// - 3-part `entities/<purpose>/<slug>`: slug→id via entity index.
fn resolve_wikilink_target(
	root: &Path,
	target: &str,
	entities: &[EntityRef],
) -> Option<(&'static str, String)> {
	let parts: Vec<&str> = target.split('/').filter(|p| !p.is_empty()).collect();
	match parts.len() {
		1 => {
			let id = parts[0];
			for dt in DOC_TYPES {
				if store::get_document(root, dt, id).is_ok() {
					return Some((dt, id.to_string()));
				}
			}
			None
		}
		2 => {
			let dt_in = parts[0];
			let dt = DOC_TYPES.iter().find(|d| **d == dt_in)?;
			if let Ok(d) = store::get_document(root, dt, parts[1]) {
				return Some((dt, d.id));
			}
			None
		}
		3 => {
			if parts[0] != "entities" { return None; }
			let slug = parts[2];
			let e = entities.iter().find(|e| e.slug == slug)?;
			Some(("entities", e.id.clone()))
		}
		_ => None,
	}
}

/// Scan body for `[[...]]` wikilinks (skipping those inside code blocks) and
/// mint a reason edge from `self_id` → each resolved target. Edge kind is
/// classified by the link's position and the relationship between source and
/// target docs:
///
/// - **Body-start** wikilink (first non-whitespace token in `body`) targeting a
///   doc under `questions/` → `Answers` edge, and the target question is
///   moved to `questions/answered/...` with the `answered` tag set.
/// - **Body-start** wikilink targeting a non-question doc whose purpose
///   matches `purpose` (the source doc's purpose) → `Supports`.
/// - All other resolved wikilinks (mid-body, cross-purpose, or unknown
///   purpose) → `References`.
///
/// Idempotent: skips emission if an edge of the desired kind already exists
/// from `self_id` → target. Returns the number of new edges created.
fn emit_wikilink_edges(
	root: &Path,
	self_id: &str,
	body: &str,
	purpose: Option<&str>,
	entities: &[EntityRef],
) -> usize {
	let code_ranges = code_protected_ranges(body);
	let in_code = |pos: usize| code_ranges.iter().any(|(s, e)| pos >= *s && pos < *e);

	// Offset of the first non-whitespace byte in `body`. A wikilink whose
	// match start equals this offset is "body-start".
	let body_start_offset: Option<usize> = body
		.char_indices()
		.find(|(_, c)| !c.is_whitespace())
		.map(|(i, _)| i);

	// Existing edges keyed by (target_id, kind) — supports per-kind dedupe so
	// a later `Supports`/`Answers` upgrade is allowed even if `References`
	// was minted earlier (and vice versa: same kind never duplicates).
	let existing: HashSet<(String, String)> = match store::search_reasons_for(root, self_id, "from") {
		Ok(reasons) => reasons
			.into_iter()
			.filter_map(|r| {
				let (_from, to, kind, _p) = super::infra::read_reason_meta(root, &r.id)?;
				Some((to, kind))
			})
			.collect(),
		Err(_) => HashSet::new(),
	};

	let mut seen_this_pass: HashSet<(String, String)> = HashSet::new();
	let mut emitted = 0usize;
	for cap in link_target_re().captures_iter(body) {
		let m = match cap.get(0) { Some(m) => m, None => continue };
		if in_code(m.start()) { continue; }
		let target = match cap.get(1).map(|m| m.as_str().trim()) {
			Some(t) if !t.is_empty() => t,
			_ => continue,
		};
		let Some((target_dt, target_id)) = resolve_wikilink_target(root, target, entities) else { continue };
		if target_id == self_id { continue; }

		let is_body_start = body_start_offset == Some(m.start());
		let kind = classify_edge_kind(root, is_body_start, target_dt, &target_id, purpose);

		let key = (target_id.clone(), kind.to_string());
		if existing.contains(&key) { continue; }
		if !seen_this_pass.insert(key) { continue; }

		if store::create_reason(root, self_id, &target_id, kind, None, purpose).is_ok() {
			emitted += 1;
		}
	}
	emitted
}

/// Classify the edge kind for a single resolved wikilink. See
/// [`emit_wikilink_edges`] for the rules.
fn classify_edge_kind(
	root: &Path,
	is_body_start: bool,
	target_dt: &'static str,
	target_id: &str,
	self_purpose: Option<&str>,
) -> &'static str {
	if is_body_start && target_dt == "questions" {
		// Body-start link to a question is *evidence*, not a final answer.
		// Mint `Supports` so multiple thoughts can accumulate; learn_pass
		// promotes a conclusion + flips the question to answered once the
		// support floor is met or one candidate clears `answer_threshold`.
		// Use explicit `ingest({kind:"reason", reason_kind:"Answers"})` for
		// a direct answer.
		return "Supports";
	}
	if is_body_start && target_dt != "questions" {
		if let Some(sp) = self_purpose {
			if let Ok(target_doc) = store::get_document(root, target_dt, target_id) {
				if target_doc.purpose.as_deref() == Some(sp) {
					return "Supports";
				}
			}
		}
	}
	"References"
}

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

fn move_question_to(root: &Path, question_id: &str, subfolder: &str) -> Result<bool> {
	let dir = root.join("questions");
	let old_path = store::find_document_path_by_id(&dir, question_id)?;

	// Guard: already under a resolved subfolder (answered/ or dropped/)
	if old_path.components().any(|c| c.as_os_str() == "answered" || c.as_os_str() == "dropped") {
		return Ok(false);
	}

	let parent_name = old_path
		.parent()
		.and_then(|p| p.file_name())
		.and_then(|s| s.to_str())
		.unwrap_or("")
		.to_string();
	let stem = old_path
		.file_stem()
		.and_then(|s| s.to_str())
		.ok_or_else(|| anyhow::anyhow!("missing file stem"))?
		.to_string();

	let new_dir = dir.join(subfolder).join(&parent_name);
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

	if !parent_name.is_empty() {
		let old_target = format!("questions/{}/{}", parent_name, stem);
		let new_target = format!("questions/{}/{}/{}", subfolder, parent_name, new_stem);
		let _ = rewrite_inbound_links(root, &old_target, &new_target);
	}
	Ok(true)
}

/// Move an answered question to `questions/answered/<purpose>/<stem>.md`,
/// preserving the frontmatter `id` so all doc lookups remain valid.
/// Rewrites inbound wikilinks. No-op if already under a resolved subfolder.
pub fn move_to_answered(root: &Path, question_id: &str) -> Result<bool> {
	move_question_to(root, question_id, "answered")
}

/// Move a dropped question to `questions/dropped/<purpose>/<stem>.md`.
/// Rewrites inbound wikilinks. No-op if already under a resolved subfolder.
pub fn move_to_dropped(root: &Path, question_id: &str) -> Result<bool> {
	move_question_to(root, question_id, "dropped")
}

pub(crate) async fn link_doc_internal(
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
			for (entity_id, _hash) in &merges {
				let _ = store::create_reason(
					root,
					entity_id,
					id,
					"Consolidates",
					None,
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

	let mut references_added = 0usize;
	if !dry_run {
		references_added = emit_wikilink_edges(
			root,
			id,
			&deduped,
			doc.purpose.as_deref(),
			entities,
		);
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
		"references_added": references_added,
		"modified": !dry_run && modified,
		"moved_to_crosstopic": moved_to_crosstopic,
		"dry_run": dry_run,
	}))
}

pub async fn link_doc(root: &Path, doc_type: &str, id: &str, dry_run: bool) -> Result<serde_json::Value> {
	let entities = build_entity_index(root).await?;
	link_doc_internal(root, doc_type, id, entities.as_slice(), dry_run).await
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
	fn code_ranges_exclude_wikilinks() {
		let text = "see `[[a]]` and [[b]]";
		let r = code_protected_ranges(text);
		// wikilink [[b]] is not in any code range
		let b_pos = text.find("[[b]]").unwrap();
		assert!(!r.iter().any(|(s, e)| b_pos >= *s && b_pos < *e));
		// inline-code wikilink is in a code range
		let a_pos = text.find("[[a]]").unwrap();
		assert!(r.iter().any(|(s, e)| a_pos >= *s && a_pos < *e));
	}

	#[tokio::test]
	async fn mid_body_wikilink_emits_references_for_thought_target() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let target = store::create_document(
			root, "thoughts", "Target", "target body",
			vec!["thought".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();
		// Mid-body link: leading "see " ensures wikilink is not body-start.
		let body = format!("see [[{}]] for context.", target.id);
		let src = store::create_document(
			root, "thoughts", "Source", &body,
			vec!["thought".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();

		let res = link_doc_internal(root, "thoughts", &src.id, &[], false).await.unwrap();
		assert_eq!(res["references_added"].as_u64(), Some(1));

		let from_src = store::search_reasons_for(root, &src.id, "from").unwrap();
		assert!(
			from_src.iter().any(|r| r.title.contains("-[References]->") && r.title.ends_with(&target.id)),
			"expected References edge src→target, got {:?}",
			from_src.iter().map(|r| r.title.clone()).collect::<Vec<_>>()
		);
	}

	#[tokio::test]
	async fn body_start_wikilink_to_question_emits_supports_no_automark() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let q = store::create_document(
			root, "questions", "Why?", "question body",
			vec!["question".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();
		// Body starts with the wikilink. Should mint Supports (evidence),
		// not Answers — synthesis path is owned by learn_pass.
		let body = format!("[[{}]] because the answer is foo.", q.id);
		let src = store::create_document(
			root, "thoughts", "Answer", &body,
			vec!["thought".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();

		let res = link_doc_internal(root, "thoughts", &src.id, &[], false).await.unwrap();
		assert_eq!(res["references_added"].as_u64(), Some(1));

		let from_src = store::search_reasons_for(root, &src.id, "from").unwrap();
		assert!(
			from_src.iter().any(|r| r.title.contains("-[Supports]->") && r.title.ends_with(&q.id)),
			"expected Supports edge src→question, got {:?}",
			from_src.iter().map(|r| r.title.clone()).collect::<Vec<_>>()
		);
		assert!(
			!from_src.iter().any(|r| r.title.contains("-[Answers]->")),
			"body-start wikilink must NOT mint Answers — that's reserved for learn_pass synthesis or explicit reason ingest"
		);

		let q_after = store::get_document(root, "questions", &q.id).unwrap();
		assert!(
			!q_after.tags.iter().any(|t| t == "answered"),
			"question must NOT be auto-marked answered by a single supporting wikilink"
		);
	}

	#[tokio::test]
	async fn body_start_wikilink_same_purpose_emits_supports() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let target = store::create_document(
			root, "thoughts", "Tgt", "tbody",
			vec!["thought".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();
		let body = format!("[[{}]] reinforces the prior claim.", target.id);
		let src = store::create_document(
			root, "thoughts", "Src", &body,
			vec!["thought".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();

		let _ = link_doc_internal(root, "thoughts", &src.id, &[], false).await.unwrap();

		let from_src = store::search_reasons_for(root, &src.id, "from").unwrap();
		assert!(
			from_src.iter().any(|r| r.title.contains("-[Supports]->") && r.title.ends_with(&target.id)),
			"expected Supports edge src→target, got {:?}",
			from_src.iter().map(|r| r.title.clone()).collect::<Vec<_>>()
		);
	}

	#[tokio::test]
	async fn body_start_wikilink_cross_purpose_emits_references() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let target = store::create_document(
			root, "thoughts", "Tgt", "tbody",
			vec!["thought".to_string(), "alpha".to_string()],
			Some("alpha"), None,
		).unwrap();
		let body = format!("[[{}]] mentioned in cross-purpose context.", target.id);
		let src = store::create_document(
			root, "thoughts", "Src", &body,
			vec!["thought".to_string(), "beta".to_string()],
			Some("beta"), None,
		).unwrap();

		let _ = link_doc_internal(root, "thoughts", &src.id, &[], false).await.unwrap();

		let from_src = store::search_reasons_for(root, &src.id, "from").unwrap();
		assert!(
			from_src.iter().any(|r| r.title.contains("-[References]->") && r.title.ends_with(&target.id)),
			"expected References edge src→target, got {:?}",
			from_src.iter().map(|r| r.title.clone()).collect::<Vec<_>>()
		);
	}

	#[tokio::test]
	async fn mid_body_wikilink_emits_references_even_for_question() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let q = store::create_document(
			root, "questions", "Q?", "qbody",
			vec!["question".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();
		// Wikilink appears mid-body; must NOT promote to Answers.
		let body = format!("a thought referencing [[{}]] casually.", q.id);
		let src = store::create_document(
			root, "thoughts", "S", &body,
			vec!["thought".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();

		let _ = link_doc_internal(root, "thoughts", &src.id, &[], false).await.unwrap();

		let from_src = store::search_reasons_for(root, &src.id, "from").unwrap();
		assert!(
			from_src.iter().any(|r| r.title.contains("-[References]->") && r.title.ends_with(&q.id)),
			"expected References edge for mid-body link to question"
		);
		assert!(
			!from_src.iter().any(|r| r.title.contains("-[Answers]->")),
			"mid-body link must not mint an Answers edge"
		);

		let q_after = store::get_document(root, "questions", &q.id).unwrap();
		assert!(
			!q_after.tags.iter().any(|t| t == "answered"),
			"question must NOT be marked answered for a mid-body reference"
		);
	}

	#[tokio::test]
	async fn wikilink_idempotent() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let target = store::create_document(
			root, "thoughts", "T", "tbody",
			vec!["thought".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();
		let body = format!("ref [[{}]]", target.id);
		let src = store::create_document(
			root, "thoughts", "S", &body,
			vec!["thought".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();

		let r1 = link_doc_internal(root, "thoughts", &src.id, &[], false).await.unwrap();
		let r2 = link_doc_internal(root, "thoughts", &src.id, &[], false).await.unwrap();
		assert_eq!(r1["references_added"].as_u64(), Some(1));
		assert_eq!(r2["references_added"].as_u64(), Some(0), "second run must not duplicate");
	}

	#[tokio::test]
	async fn wikilink_skips_inside_code_block() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let target = store::create_document(
			root, "thoughts", "T2", "x",
			vec!["thought".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();
		// Wikilink only appears inside inline code → must not mint an edge.
		let body = format!("example: `[[{}]]`", target.id);
		let src = store::create_document(
			root, "thoughts", "S2", &body,
			vec!["thought".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();

		let res = link_doc_internal(root, "thoughts", &src.id, &[], false).await.unwrap();
		assert_eq!(res["references_added"].as_u64(), Some(0));
	}

	#[tokio::test]
	async fn wikilink_resolves_two_part_form() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let target = store::create_document(
			root, "questions", "Q?", "qbody",
			vec!["question".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();
		let body = format!("see [[questions/{}]]", target.id);
		let src = store::create_document(
			root, "thoughts", "S3", &body,
			vec!["thought".to_string(), "general".to_string()],
			Some("general"), None,
		).unwrap();

		let res = link_doc_internal(root, "thoughts", &src.id, &[], false).await.unwrap();
		assert_eq!(res["references_added"].as_u64(), Some(1));
		let from_src = store::search_reasons_for(root, &src.id, "from").unwrap();
		assert!(from_src.iter().any(|r| r.title.ends_with(&target.id)));
	}
}
