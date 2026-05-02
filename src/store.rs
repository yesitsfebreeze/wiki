use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

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
	// Expand ${VAR} placeholders; discard if any remain unexpanded.
	// Claude Code does not expand ${} in MCP env values (only in hooks).
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

pub fn ensure_wiki_layout(root: &Path) -> anyhow::Result<()> {
	for dir in &[
		"purposes",
		"thoughts",
		"entities",
		"reasons",
		"questions",
		"conclusions",
		"ingest_log",
		"auto_links",
		"assets",
		".search",
	] {
		std::fs::create_dir_all(root.join(dir))?;
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
	let path = dir.join(format!("{}.md", tag));
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
	let full = format!("---\n{}---\n\n{}", fm, description);
	std::fs::write(&path, full)?;
	Ok(Purpose {
		id,
		tag: tag.to_string(),
		title: title.to_string(),
		description: description.to_string(),
		path,
	})
}

pub fn delete_purpose(root: &Path, tag: &str) -> anyhow::Result<()> {
	let path = root.join("purposes").join(format!("{}.md", tag));
	if !path.exists() {
		return Err(anyhow::anyhow!("Purpose '{}' not found", tag));
	}
	std::fs::remove_file(&path)?;
	let _ = std::fs::remove_file(root.join("purposes").join(format!("{}.vec", tag)));
	Ok(())
}

fn slugify(title: &str) -> String {
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
		let rd = match std::fs::read_dir(d) { Ok(r) => r, Err(_) => return };
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

/// Resolve a unique target path by appending numeric suffix on collision.
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
		if let Ok(content) = std::fs::read_to_string(&path) {
			if let Ok((fm, _)) = parse_frontmatter(&content) {
				if let Some(doc_id) = fm.get("id").and_then(|v| v.as_str()) {
					if doc_id == id {
						return Ok(path);
					}
				}
			}
		}
	}
	Err(anyhow::anyhow!("Document not found: {}", id))
}

fn update_link_index(root: &Path) -> anyhow::Result<()> {
	let mut link_map: HashMap<String, String> = HashMap::new();
	let doc_types = ["thoughts", "entities", "reasons", "questions", "conclusions"];
	for doc_type in &doc_types {
		let dir = root.join(doc_type);
		for path in walk_md_paths(&dir) {
			if let Ok(content) = std::fs::read_to_string(&path) {
				if let Ok((fm, _)) = parse_frontmatter(&content) {
					if let Some(id) = fm.get("id").and_then(|v| v.as_str()) {
						// Store path relative to the doc_type dir so callers can locate
						// files under their purpose subfolder.
						let rel = path.strip_prefix(&dir).unwrap_or(&path);
						link_map.insert(id.to_string(), rel.to_string_lossy().to_string());
					}
				}
			}
		}
	}
	std::fs::write(root.join("link.json"), serde_json::to_string_pretty(&link_map)?)?;
	Ok(())
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
	let full = format!("---\n{}---\n\n{}", fm, content);

	let purpose_dir = purpose.unwrap_or("uncategorized");
	let dir = root.join(doc_type).join(purpose_dir);
	std::fs::create_dir_all(&dir)?;
	let file_path = unique_path(&dir, &slug);
	std::fs::write(&file_path, full)?;
	let _ = update_link_index(root);

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
	// First try `id` as a relative path inside the type dir (new layout: `<purpose>/<name>`).
	// This makes `[[type/purpose/name]]` wikilinks resolve cheaply without scanning.
	let rel_path = dir.join(format!("{}.md", id));
	if rel_path.is_file() {
		let content = std::fs::read_to_string(&rel_path)?;
		let (fm, body) = parse_frontmatter(&content)?;
		return Ok(doc_from_fm(&fm, body, id));
	}
	// Fall back to a UUID/id frontmatter scan (graph linkage, legacy callers).
	let file_path = find_document_path_by_id(&dir, id)?;
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
	let full = format!("---\n{}---\n\n{}", fm_str, body);
	std::fs::write(&file_path, full)?;

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
	std::fs::write(&file_path, format!("---\n{}---\n\n{}", fm_str, body))?;
	Ok(true)
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
	let full = format!("---\n{}---\n\n{}", fm, body);
	let purpose_dir = purpose.unwrap_or("uncategorized");
	let dir = root.join("reasons").join(purpose_dir);
	std::fs::create_dir_all(&dir)?;
	let file_path = unique_path(&dir, &slug);
	std::fs::write(&file_path, full)?;
	let _ = update_link_index(root);

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
	std::fs::write(
		log_dir.join(format!("{}.json", log_id)),
		serde_json::to_string_pretty(&entry)?,
	)?;
	Ok(())
}

pub fn search_by_tag(root: &Path, tag: &str) -> anyhow::Result<Vec<Document>> {
	let doc_types = ["thoughts", "entities", "reasons", "questions", "conclusions"];
	let mut results = Vec::new();
	for doc_type in &doc_types {
		let dir = root.join(doc_type);
		for path in walk_md_paths(&dir) {
			let raw = std::fs::read_to_string(&path)?;
			let (fm, body) = parse_frontmatter(&raw)?;
			let tags: Vec<String> = fm["tags"]
				.as_array()
				.map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
				.unwrap_or_default();
			if tags.iter().any(|t| t == tag) {
				results.push(doc_from_fm(&fm, body, ""));
			}
		}
	}
	Ok(results)
}

pub fn search_reasons_for(root: &Path, node_id: &str, direction: &str) -> anyhow::Result<Vec<Document>> {
	let dir = root.join("reasons");
	let mut results = Vec::new();
	for path in walk_md_paths(&dir) {
		let raw = std::fs::read_to_string(&path)?;
		let (fm, body) = parse_frontmatter(&raw)?;
		let from_id = fm["from_id"].as_str().unwrap_or("");
		let to_id = fm["to_id"].as_str().unwrap_or("");
		let matches = match direction {
			"from" => from_id == node_id,
			"to" => to_id == node_id,
			_ => from_id == node_id || to_id == node_id,
		};
		if matches {
			results.push(doc_from_fm(&fm, body, ""));
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
		ensure_wiki_layout(root).unwrap();
		create_purpose(root, "topic-a", "Topic A", "Sample topic").unwrap();
		let purposes = list_purposes(root).unwrap();
		assert_eq!(purposes.len(), 1);
		assert_eq!(purposes[0].tag, "topic-a");
	}

	#[test]
	fn test_delete_purpose() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		ensure_wiki_layout(root).unwrap();
		create_purpose(root, "x", "X", "desc").unwrap();
		delete_purpose(root, "x").unwrap();
		assert!(list_purposes(root).unwrap().is_empty());
	}
}

