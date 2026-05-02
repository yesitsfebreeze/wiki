use crate::cache;
use crate::io::write_atomic_str;
use chrono::Utc;
use include_dir::{include_dir, Dir, DirEntry};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

static OBSIDIAN_TEMPLATE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/obsidian");

const DOC_TYPES: &[&str] = &["thoughts", "entities", "reasons", "questions", "conclusions"];

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Purpose {
	pub id: String,
	pub tag: String,
	pub title: String,
	pub description: String,
	pub path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Document {
	pub id: String,
	pub title: String,
	pub tags: Vec<String>,
	pub purpose: Option<String>,
	pub source_doc_id: Option<String>,
	pub created_at: String,
	pub updated_at: String,
	pub content: String,
}

pub fn wiki_root() -> PathBuf {
	let raw = std::env::var("WIKI_PATH").unwrap_or_else(|_| ".wiki".to_string());
	let expanded = if raw.contains("${") {
		let mut s = raw.clone();
		for (key, val) in std::env::vars() {
			s = s.replace(&format!("${{{}}}", key), &val);
		}
		if s.contains("${") { ".wiki".to_string() } else { s }
	} else {
		raw
	};
	let p = PathBuf::from(&expanded);
	if p.is_absolute() {
		p
	} else {
		std::env::current_dir().ok().map(|d| d.join(&p)).unwrap_or(p)
	}
}

/// Initialize wiki vault: create required subdirs and seed `.obsidian/` from
/// the embedded template (only files that don't already exist, so user edits
/// survive). Idempotent — safe to call on every entry point.
pub fn bootstrap(root: &Path) -> anyhow::Result<()> {
	for dir in &[
		"purposes", "thoughts", "entities", "reasons", "questions",
		"conclusions", "ingest_log", "assets", ".search",
	] {
		std::fs::create_dir_all(root.join(dir))?;
	}
	seed_obsidian(&root.join(".obsidian"))?;
	Ok(())
}

fn seed_obsidian(target: &Path) -> anyhow::Result<()> {
	std::fs::create_dir_all(target)?;
	seed_dir(&OBSIDIAN_TEMPLATE, target)
}

fn seed_dir(dir: &Dir<'_>, target: &Path) -> anyhow::Result<()> {
	for entry in dir.entries() {
		match entry {
			DirEntry::Dir(d) => {
				let sub = target.join(d.path().file_name().unwrap_or_default());
				std::fs::create_dir_all(&sub)?;
				seed_dir(d, &sub)?;
			}
			DirEntry::File(f) => {
				let dest = target.join(f.path().file_name().unwrap_or_default());
				if dest.exists() {
					continue;
				}
				if let Some(parent) = dest.parent() {
					std::fs::create_dir_all(parent)?;
				}
				std::fs::write(&dest, f.contents())?;
			}
		}
	}
	Ok(())
}

pub fn list_purposes(root: &Path) -> anyhow::Result<Vec<Purpose>> {
	let dir = root.join("purposes");
	if !dir.exists() {
		return Ok(vec![]);
	}
	let mut out = Vec::new();
	for entry in std::fs::read_dir(&dir)? {
		let entry = entry?;
		let path = entry.path();
		if path.extension().and_then(|s| s.to_str()) != Some("md") {
			continue;
		}
		let raw = std::fs::read_to_string(&path)?;
		let (fm, body) = parse_frontmatter(&raw)?;
		let tag = fm["tag"]
			.as_str()
			.unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or(""))
			.to_string();
		out.push(Purpose {
			id: fm["id"].as_str().unwrap_or("").to_string(),
			tag,
			title: fm["title"].as_str().unwrap_or("").to_string(),
			description: body,
			path,
		});
	}
	Ok(out)
}

pub fn create_purpose(root: &Path, tag: &str, title: &str, description: &str) -> anyhow::Result<Purpose> {
	let dir = root.join("purposes");
	std::fs::create_dir_all(&dir)?;
	let slug = slugify(tag);
	let path = dir.join(format!("{}.md", slug));
	if path.exists() {
		return Err(anyhow::anyhow!("Purpose '{}' already exists", tag));
	}
	let id = Uuid::new_v4().to_string();
	let now = Utc::now().to_rfc3339();
	let fm = serde_yaml::to_string(&serde_json::json!({
		"id": id,
		"tag": tag,
		"title": title,
		"created_at": now,
		"updated_at": now,
		"tags": ["purpose"],
	}))?;
	write_atomic_str(&path, &format!("---\n{}---\n\n{}", fm, description))?;
	Ok(Purpose {
		id,
		tag: tag.to_string(),
		title: title.to_string(),
		description: description.to_string(),
		path,
	})
}

pub fn delete_purpose(root: &Path, tag: &str) -> anyhow::Result<()> {
	let slug = slugify(tag);
	let path = root.join("purposes").join(format!("{}.md", slug));
	if !path.exists() {
		return Err(anyhow::anyhow!("Purpose '{}' not found", tag));
	}
	std::fs::remove_file(&path)?;
	let _ = std::fs::remove_file(root.join("purposes").join(format!("{}.vec", slug)));
	Ok(())
}

pub fn slugify(title: &str) -> String {
	title
		.to_lowercase()
		.chars()
		.map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
		.collect::<String>()
		.split('-')
		.filter(|s| !s.is_empty())
		.collect::<Vec<_>>()
		.join("-")
		.chars()
		.take(200)
		.collect()
}

/// Recursively collect every `.md` file under `dir`. Returns empty if missing.
pub fn walk_md_paths(dir: &Path) -> Vec<PathBuf> {
	fn rec(d: &Path, out: &mut Vec<PathBuf>) {
		if !d.exists() { return; }
		let Ok(rd) = std::fs::read_dir(d) else { return };
		for entry in rd.flatten() {
			let p = entry.path();
			if p.is_dir() {
				rec(&p, out);
			} else if p.extension().and_then(|s| s.to_str()) == Some("md") {
				out.push(p);
			}
		}
	}
	let mut out = Vec::new();
	rec(dir, &mut out);
	out
}

fn unique_path(dir: &Path, slug: &str) -> PathBuf {
	let mut p = dir.join(format!("{}.md", slug));
	let mut n = 1;
	while p.exists() {
		p = dir.join(format!("{}-{}.md", slug, n));
		n += 1;
	}
	p
}

pub fn find_document_path_by_id(dir: &Path, id: &str) -> anyhow::Result<PathBuf> {
	if !dir.exists() {
		return Err(anyhow::anyhow!("Directory not found"));
	}
	for path in walk_md_paths(dir) {
		let Ok(content) = std::fs::read_to_string(&path) else { continue };
		let Ok((fm, _)) = parse_frontmatter(&content) else { continue };
		if fm.get("id").and_then(|v| v.as_str()) == Some(id) {
			return Ok(path);
		}
	}
	Err(anyhow::anyhow!("Document not found: {}", id))
}

fn update_link_index(root: &Path) -> anyhow::Result<()> {
	let mut link_map: HashMap<String, String> = HashMap::new();
	for doc_type in DOC_TYPES {
		let dir = root.join(doc_type);
		for path in walk_md_paths(&dir) {
			let Ok(content) = std::fs::read_to_string(&path) else { continue };
			let Ok((fm, _)) = parse_frontmatter(&content) else { continue };
			if let Some(id) = fm.get("id").and_then(|v| v.as_str()) {
				let rel = path.strip_prefix(&dir).unwrap_or(&path);
				link_map.insert(id.to_string(), rel.to_string_lossy().to_string());
			}
		}
	}
	write_atomic_str(&root.join("link.json"), &serde_json::to_string_pretty(&link_map)?)
}

fn doc_from_fm(fm: &serde_json::Value, body: String, fallback_id: &str) -> Document {
	Document {
		id: fm["id"].as_str().unwrap_or(fallback_id).to_string(),
		title: fm["title"].as_str().unwrap_or("").to_string(),
		tags: fm["tags"]
			.as_array()
			.map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
			.unwrap_or_default(),
		purpose: fm["purpose"].as_str().map(String::from),
		source_doc_id: fm["source_doc_id"].as_str().map(String::from),
		created_at: fm["created_at"].as_str().unwrap_or("").to_string(),
		updated_at: fm["updated_at"].as_str().unwrap_or("").to_string(),
		content: body,
	}
}

pub fn create_document(
	root: &Path,
	doc_type: &str,
	title: &str,
	content: &str,
	tags: Vec<String>,
	purpose: Option<&str>,
	source_doc_id: Option<&str>,
) -> anyhow::Result<Document> {
	let id = Uuid::new_v4().to_string();
	let now = Utc::now().to_rfc3339();
	let slug = slugify(title);
	debug_assert!(crate::sanitize::is_clean_stem(&slug) || slug.is_empty());

	let mut fm_obj = serde_json::json!({
		"id": id,
		"title": title,
		"tags": tags,
		"created_at": now,
		"updated_at": now,
	});
	if let Some(p) = purpose {
		fm_obj["purpose"] = serde_json::Value::String(p.to_string());
	}
	if let Some(s) = source_doc_id {
		fm_obj["source_doc_id"] = serde_json::Value::String(s.to_string());
	}
	let fm = serde_yaml::to_string(&fm_obj)?;

	let purpose_dir = purpose.unwrap_or("uncategorized");
	let dir = root.join(doc_type).join(purpose_dir);
	std::fs::create_dir_all(&dir)?;
	let file_path = unique_path(&dir, &slug);
	write_atomic_str(&file_path, &format!("---\n{}---\n\n{}", fm, content))?;
	let _ = update_link_index(root);
	cache::on_doc_changed(root, &id, doc_type);

	Ok(Document {
		id,
		title: title.to_string(),
		tags,
		purpose: purpose.map(String::from),
		source_doc_id: source_doc_id.map(String::from),
		created_at: now.clone(),
		updated_at: now,
		content: content.to_string(),
	})
}

pub fn get_document(root: &Path, doc_type: &str, id: &str) -> anyhow::Result<Document> {
	let dir = root.join(doc_type);
	// Fast path: `id` is a relative path inside the type dir (`<purpose>/<name>`).
	let rel_path = dir.join(format!("{}.md", id));
	if rel_path.is_file() {
		let rel_path = crate::sanitize::ensure_sanitized(root, &rel_path)?;
		let content = std::fs::read_to_string(&rel_path)?;
		let (fm, body) = parse_frontmatter(&content)?;
		return Ok(doc_from_fm(&fm, body, id));
	}
	let file_path = find_document_path_by_id(&dir, id)?;
	let file_path = crate::sanitize::ensure_sanitized(root, &file_path)?;
	let content = std::fs::read_to_string(&file_path)?;
	let (fm, body) = parse_frontmatter(&content)?;
	Ok(doc_from_fm(&fm, body, id))
}

pub fn list_documents(root: &Path, doc_type: &str) -> anyhow::Result<Vec<Document>> {
	let dir = root.join(doc_type);
	let mut out = Vec::new();
	for path in walk_md_paths(&dir) {
		let content = std::fs::read_to_string(&path)?;
		let (fm, body) = parse_frontmatter(&content)?;
		out.push(doc_from_fm(&fm, body, ""));
	}
	Ok(out)
}

pub fn update_document(
	root: &Path,
	doc_type: &str,
	id: &str,
	content: Option<&str>,
	tags: Option<Vec<String>>,
) -> anyhow::Result<Document> {
	let dir = root.join(doc_type);
	let file_path = find_document_path_by_id(&dir, id)?;
	let file_path = crate::sanitize::ensure_sanitized(root, &file_path)?;

	let raw = std::fs::read_to_string(&file_path)?;
	let (mut fm, mut body) = parse_frontmatter(&raw)?;
	let now = Utc::now().to_rfc3339();

	if let Some(new_content) = content {
		body = new_content.to_string();
	}
	if let Some(obj) = fm.as_object_mut() {
		if let Some(new_tags) = tags {
			obj.insert("tags".to_string(), serde_json::json!(new_tags));
		}
		obj.insert("updated_at".to_string(), serde_json::json!(now));
	}

	let fm_str = serde_yaml::to_string(&fm)?;
	write_atomic_str(&file_path, &format!("---\n{}---\n\n{}", fm_str, body))?;
	cache::on_doc_changed(root, id, doc_type);

	Ok(doc_from_fm(&fm, body, id))
}

pub fn add_alias_to_entity(root: &Path, entity_id: &str, alias: &str) -> anyhow::Result<bool> {
	let dir = root.join("entities");
	let file_path = find_document_path_by_id(&dir, entity_id)?;
	let raw = std::fs::read_to_string(&file_path)?;
	let (mut fm, body) = parse_frontmatter(&raw)?;

	let existing: Vec<String> = fm
		.get("aliases")
		.and_then(|v| v.as_array())
		.map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
		.unwrap_or_default();

	let alias_lc = alias.trim().to_lowercase();
	let title_lc = fm.get("title").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
	if alias_lc == title_lc || existing.iter().any(|a| a.to_lowercase() == alias_lc) {
		return Ok(false);
	}

	let mut new_aliases = existing;
	new_aliases.push(alias.trim().to_string());
	if let Some(obj) = fm.as_object_mut() {
		obj.insert("aliases".to_string(), serde_json::json!(new_aliases));
		obj.insert("updated_at".to_string(), serde_json::json!(Utc::now().to_rfc3339()));
	}
	let fm_str = serde_yaml::to_string(&fm)?;
	write_atomic_str(&file_path, &format!("---\n{}---\n\n{}", fm_str, body))?;
	cache::on_doc_changed(root, entity_id, "entities");
	Ok(true)
}

/// Lower-level helper: round-trip frontmatter and set a single field.
/// Does not bump `updated_at`. Used for derived/computed fields like
/// `node_size` where touching the timestamp would dirty the doc.
pub fn set_frontmatter_field(
	root: &Path,
	doc_type: &str,
	id: &str,
	key: &str,
	value: serde_json::Value,
) -> anyhow::Result<()> {
	let dir = root.join(doc_type);
	let file_path = find_document_path_by_id(&dir, id)?;
	let raw = std::fs::read_to_string(&file_path)?;
	let (mut fm, body) = parse_frontmatter(&raw)?;
	if let Some(obj) = fm.as_object_mut() {
		obj.insert(key.to_string(), value);
	} else {
		let mut m = serde_json::Map::new();
		m.insert(key.to_string(), value);
		fm = serde_json::Value::Object(m);
	}
	let fm_str = serde_yaml::to_string(&fm)?;
	write_atomic_str(&file_path, &format!("---\n{}---\n\n{}", fm_str, body))?;
	Ok(())
}

pub fn parse_frontmatter(content: &str) -> anyhow::Result<(serde_json::Value, String)> {
	if !content.starts_with("---") {
		return Ok((serde_json::Value::Null, content.to_string()));
	}
	let end = content[3..]
		.find("---")
		.ok_or_else(|| anyhow::anyhow!("Invalid frontmatter"))?
		+ 3;
	let fm = &content[3..end];
	let body = &content[end + 3..];
	let value: serde_json::Value = serde_yaml::from_str(fm)?;
	Ok((value, body.trim().to_string()))
}

pub fn create_reason(
	root: &Path,
	from_id: &str,
	to_id: &str,
	kind: &str,
	body: &str,
	purpose: Option<&str>,
) -> anyhow::Result<Document> {
	let id = Uuid::new_v4().to_string();
	let now = Utc::now().to_rfc3339();
	let title = format!("{} -[{}]-> {}", from_id, kind, to_id);
	let slug = slugify(&format!("{}-{}-{}", from_id, kind, to_id));
	debug_assert!(crate::sanitize::is_clean_stem(&slug) || slug.is_empty());

	let mut fm_obj = serde_json::json!({
		"id": id,
		"title": title,
		"tags": ["reason"],
		"from_id": from_id,
		"to_id": to_id,
		"kind": kind,
		"created_at": now,
		"updated_at": now,
	});
	if let Some(p) = purpose {
		fm_obj["purpose"] = serde_json::Value::String(p.to_string());
		if let Some(arr) = fm_obj["tags"].as_array_mut() {
			arr.push(serde_json::Value::String(p.to_string()));
		}
	}

	let fm = serde_yaml::to_string(&fm_obj)?;
	let purpose_dir = purpose.unwrap_or("uncategorized");
	let dir = root.join("reasons").join(purpose_dir);
	std::fs::create_dir_all(&dir)?;
	let file_path = unique_path(&dir, &slug);
	write_atomic_str(&file_path, &format!("---\n{}---\n\n{}", fm, body))?;
	let _ = update_link_index(root);
	cache::on_doc_changed(root, &id, "reasons");

	let mut tags = vec!["reason".to_string()];
	if let Some(p) = purpose {
		tags.push(p.to_string());
	}

	Ok(Document {
		id,
		title,
		tags,
		purpose: purpose.map(String::from),
		source_doc_id: None,
		created_at: now.clone(),
		updated_at: now,
		content: body.to_string(),
	})
}

pub fn delete_document(root: &Path, doc_type: &str, id: &str) -> anyhow::Result<()> {
	let dir = root.join(doc_type);
	let file_path = find_document_path_by_id(&dir, id)?;
	std::fs::remove_file(&file_path)?;
	let _ = update_link_index(root);
	cache::on_doc_deleted(root, id, doc_type);
	if let Ok(idx) = cache::search_index(root) {
		let _ = crate::search::delete_by_id(&idx, id);
	}
	Ok(())
}

pub fn log_ingest(root: &Path, doc_type: &str, doc_id: &str, title: &str) -> anyhow::Result<()> {
	let now = Utc::now().to_rfc3339();
	let log_id = Uuid::new_v4().to_string();
	let entry = serde_json::json!({
		"id": log_id,
		"timestamp": now,
		"doc_type": doc_type,
		"doc_id": doc_id,
		"title": title,
	});
	let log_dir = root.join("ingest_log");
	std::fs::create_dir_all(&log_dir)?;
	write_atomic_str(
		&log_dir.join(format!("{}.json", log_id)),
		&serde_json::to_string_pretty(&entry)?,
	)
}

pub fn search_by_tag(root: &Path, tag: &str) -> anyhow::Result<Vec<Document>> {
	let refs = cache::tag_index_lookup(root, tag);
	let mut results = Vec::with_capacity(refs.len());
	for r in refs {
		if let Ok(doc) = get_document(root, &r.doc_type, &r.id) {
			results.push(doc);
		}
	}
	results.sort_by(|a, b| a.id.cmp(&b.id));
	Ok(results)
}

pub fn search_reasons_for(root: &Path, node_id: &str, direction: &str) -> anyhow::Result<Vec<Document>> {
	let adj = cache::reason_index_lookup(root, node_id);
	let mut ids: Vec<String> = match direction {
		"from" => adj.from,
		"to" => adj.to,
		_ => {
			let mut v = adj.from;
			v.extend(adj.to);
			v.sort();
			v.dedup();
			v
		}
	};
	ids.sort();
	ids.dedup();
	let mut results = Vec::with_capacity(ids.len());
	for id in ids {
		if let Ok(doc) = get_document(root, "reasons", &id) {
			results.push(doc);
		}
	}
	Ok(results)
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	#[test]
	fn test_create_purpose_and_list() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		bootstrap(root).unwrap();
		create_purpose(root, "topic-a", "Topic A", "Sample topic").unwrap();
		let purposes = list_purposes(root).unwrap();
		assert_eq!(purposes.len(), 1);
		assert_eq!(purposes[0].tag, "topic-a");
	}

	#[test]
	fn test_delete_purpose() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		bootstrap(root).unwrap();
		create_purpose(root, "x", "X", "desc").unwrap();
		delete_purpose(root, "x").unwrap();
		assert!(list_purposes(root).unwrap().is_empty());
	}
}
