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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn protected_ranges_skip_code() {
		let text = "hello `code` world\n\n```\nfoo bar\n```\n\ntail";
		let r = protected_ranges(text);
		assert!(r.iter().any(|(s, _)| text[*s..].starts_with('`')));
	}
}
