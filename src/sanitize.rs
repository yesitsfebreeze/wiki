//! Vault filename sanitization. Obsidian wikilinks break on filenames with
//! special characters (spaces, punctuation, mixed case). This module enforces
//! slug-clean stems vault-wide and rewrites `[[wikilinks]]` + relative
//! markdown links when files are renamed.

use crate::io::write_atomic_str;
use crate::store::{slugify, walk_md_paths};
use anyhow::Result;
use regex::Regex;
use std::path::{Path, PathBuf};

/// Doc-type subdirs that hold user-edited Obsidian files. Other dirs
/// (`.obsidian`, `.search`, `code`, `assets`, `ingest_log`) are internal.
const DOC_DIRS: &[&str] = &[
	"thoughts", "entities", "reasons", "questions", "conclusions", "purposes",
];

/// True if `stem` is byte-identical to its slugified form.
pub fn is_clean_stem(stem: &str) -> bool {
	!stem.is_empty() && stem.len() <= 200 && slugify(stem) == stem
}

/// Slugify a stem. Thin wrapper over `store::slugify`.
pub fn clean_stem(stem: &str) -> String {
	slugify(stem)
}

#[derive(Default, Debug, serde::Serialize)]
pub struct SanitizeReport {
	/// (from, to) for each renamed file.
	pub renamed: Vec<(PathBuf, PathBuf)>,
	/// (file, count) of link rewrites applied per file.
	pub link_rewrites: Vec<(PathBuf, usize)>,
	/// (path, reason) for skipped files.
	pub skipped: Vec<(PathBuf, String)>,
}

/// Pick a non-colliding target path in `dir` for `slug`, mirroring
/// `store::unique_path`. Used when renaming a dirty file would clobber.
fn unique_path(dir: &Path, slug: &str) -> PathBuf {
	let mut p = dir.join(format!("{}.md", slug));
	let mut n = 1;
	while p.exists() {
		p = dir.join(format!("{}-{}.md", slug, n));
		n += 1;
	}
	p
}

/// Walk vault doc dirs, rename dirty `.md` files, then rewrite all
/// wikilinks/relative links in remaining `.md` files. Idempotent.
pub fn sanitize_vault(root: &Path, dry_run: bool) -> Result<SanitizeReport> {
	let mut report = SanitizeReport::default();
	// Map from old stem (lowercased) -> new stem, for link rewriting.
	let mut renames: Vec<(String, String)> = Vec::new();

	// Pass 1: rename files.
	for sub in DOC_DIRS {
		let base = root.join(sub);
		if !base.exists() {
			continue;
		}
		for path in walk_md_paths(&base) {
			let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
				report.skipped.push((path.clone(), "non-utf8 stem".into()));
				continue;
			};
			if is_clean_stem(stem) {
				continue;
			}
			let new_stem = clean_stem(stem);
			if new_stem.is_empty() {
				report.skipped.push((path.clone(), "empty slug".into()));
				continue;
			}
			let parent = path.parent().unwrap_or(&base);
			let new_path = unique_path(parent, &new_stem);
			let final_stem = new_path
				.file_stem()
				.and_then(|s| s.to_str())
				.unwrap_or(&new_stem)
				.to_string();
			if !dry_run {
				if let Err(e) = std::fs::rename(&path, &new_path) {
					report
						.skipped
						.push((path.clone(), format!("rename failed: {}", e)));
					continue;
				}
			}
			renames.push((stem.to_lowercase(), final_stem));
			report.renamed.push((path, new_path));
		}
	}

	if renames.is_empty() {
		return Ok(report);
	}

	report.link_rewrites = rewrite_vault_links(root, &renames, dry_run)?;
	Ok(report)
}

/// Walk `DOC_DIRS` under `root`, rewriting `[[wikilinks]]` and relative
/// `.md` links whose stem appears in `renames` (lowercased old → new).
/// Returns per-file rewrite counts. Skips writes if `dry_run`.
fn rewrite_vault_links(
	root: &Path,
	renames: &[(String, String)],
	dry_run: bool,
) -> Result<Vec<(PathBuf, usize)>> {
	let wiki_re = Regex::new(r"\[\[([^\]\|#]+)((?:#[^\]\|]*)?)((?:\|[^\]]*)?)\]\]")?;
	let md_re = Regex::new(r"\]\(([^)#\s]+?)\.md(#[^)\s]*)?\)")?;
	let mut out = Vec::new();

	for sub in DOC_DIRS {
		let base = root.join(sub);
		if !base.exists() {
			continue;
		}
		for path in walk_md_paths(&base) {
			let Ok(content) = std::fs::read_to_string(&path) else { continue };
			let mut count = 0usize;
			let after_wiki = wiki_re.replace_all(&content, |caps: &regex::Captures| {
				let target = caps.get(1).map(|m| m.as_str()).unwrap_or("");
				let anchor = caps.get(2).map(|m| m.as_str()).unwrap_or("");
				let alias = caps.get(3).map(|m| m.as_str()).unwrap_or("");
				let key = target.trim().to_lowercase();
				if let Some((_, new)) = renames.iter().find(|(old, _)| old == &key) {
					count += 1;
					format!("[[{}{}{}]]", new, anchor, alias)
				} else {
					caps.get(0).unwrap().as_str().to_string()
				}
			});
			let after_md = md_re.replace_all(&after_wiki, |caps: &regex::Captures| {
				let stem = caps.get(1).map(|m| m.as_str()).unwrap_or("");
				let anchor = caps.get(2).map(|m| m.as_str()).unwrap_or("");
				let (prefix, base_name) = match stem.rsplit_once('/') {
					Some((p, b)) => (format!("{}/", p), b),
					None => (String::new(), stem),
				};
				let key = base_name.to_lowercase();
				if let Some((_, new)) = renames.iter().find(|(old, _)| old == &key) {
					count += 1;
					format!("]({}{}.md{})", prefix, new, anchor)
				} else {
					caps.get(0).unwrap().as_str().to_string()
				}
			});
			if count > 0 {
				if !dry_run {
					write_atomic_str(&path, &after_md)?;
				}
				out.push((path, count));
			}
		}
	}
	Ok(out)
}

/// Per-operation hook. If `path`'s stem isn't slug-clean, rename it and
/// rewrite vault links to match. Returns the (possibly new) path. No-op if
/// path doesn't exist or is already clean.
pub fn ensure_sanitized(root: &Path, path: &Path) -> Result<PathBuf> {
	if !path.exists() {
		return Ok(path.to_path_buf());
	}
	let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
		return Ok(path.to_path_buf());
	};
	if is_clean_stem(stem) {
		return Ok(path.to_path_buf());
	}
	let new_stem = clean_stem(stem);
	if new_stem.is_empty() {
		return Ok(path.to_path_buf());
	}
	let parent = path.parent().unwrap_or(root);
	let new_path = unique_path(parent, &new_stem);
	std::fs::rename(path, &new_path)?;

	// Rewrite vault-wide links. We pre-built a single-entry rename list;
	// reuse the regex pass from sanitize_vault by inlining (avoid full walk
	// re-rename overhead).
	let final_stem = new_path
		.file_stem()
		.and_then(|s| s.to_str())
		.unwrap_or(&new_stem)
		.to_string();
	let renames = vec![(stem.to_lowercase(), final_stem)];
	rewrite_vault_links(root, &renames, false)?;
	Ok(new_path)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_is_clean_stem() {
		assert!(is_clean_stem("foo-bar"));
		assert!(!is_clean_stem("Foo Bar"));
		assert!(is_clean_stem("foo_bar"));
		assert!(is_clean_stem("foo-bar-1"));
		assert!(!is_clean_stem("hello!"));
		assert!(!is_clean_stem(""));
		assert!(!is_clean_stem("-foo"));
		assert!(!is_clean_stem("foo-"));
		assert!(!is_clean_stem("foo--bar"));
		assert!(is_clean_stem("abc123"));
	}

	#[test]
	fn test_clean_stem_idempotent() {
		for x in &["Foo Bar", "hello!", "weird   spacing", "MiXeD-CaSe", "a"] {
			let once = clean_stem(x);
			let twice = clean_stem(&once);
			assert_eq!(once, twice, "not idempotent for {:?}", x);
		}
	}
}
