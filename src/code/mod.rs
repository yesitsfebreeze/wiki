pub mod language;
pub mod splitter;
pub mod watcher;

use anyhow::{anyhow, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub fn default_index_dir() -> PathBuf {
	let raw = std::env::var("CODE_INDEX_DIR")
		.or_else(|_| std::env::var("SPLIT_INDEX_DIR"))
		.unwrap_or_else(|_| ".wiki/code".to_string());
	let p = PathBuf::from(&raw);
	if p.is_absolute() {
		p
	} else {
		std::env::current_dir()
			.ok()
			.map(|d| d.join(&p))
			.unwrap_or(p)
	}
}

pub fn max_loc_threshold() -> usize {
	std::env::var("SPLIT_MAX_LOC")
		.ok()
		.and_then(|v| v.parse().ok())
		.unwrap_or(256)
}

pub fn index_dir(src_dir: &Path, ext: &str) -> Result<String> {
	let index = default_index_dir();
	std::fs::create_dir_all(&index)?;
	let mut files_indexed = 0u32;
	let mut files_skipped = 0u32;
	let mut bodies_total = 0u32;
	for src in walk_files(src_dir, ext) {
		let struct_path = splitter::structure_path(&src, &index);
		if struct_path.exists() {
			files_skipped += 1;
			continue;
		}
		match splitter::split_for_ext(&src, &index, ext) {
			Ok((structure, bodies)) => {
				if let Some(p) = struct_path.parent() {
					std::fs::create_dir_all(p)?;
				}
				std::fs::write(&struct_path, &structure)?;
				for b in &bodies {
					if let Some(p) = b.path.parent() {
						std::fs::create_dir_all(p).ok();
					}
					std::fs::write(&b.path, &b.content)?;
				}
				bodies_total += bodies.len() as u32;
				files_indexed += 1;
			}
			Err(e) => eprintln!("skip {}: {e}", src.display()),
		}
	}
	Ok(format!(
		"indexed {files_indexed} files ({bodies_total} functions); {files_skipped} skipped"
	))
}

pub fn open_source(src: &Path, ext: &str) -> Result<String> {
	let index = default_index_dir();
	let struct_path = splitter::structure_path(src, &index);
	if !struct_path.exists() {
		let (structure, bodies) = splitter::split_for_ext(src, &index, ext)?;
		if let Some(p) = struct_path.parent() {
			std::fs::create_dir_all(p)?;
		}
		std::fs::write(&struct_path, &structure)?;
		for b in &bodies {
			if let Some(p) = b.path.parent() {
				std::fs::create_dir_all(p).ok();
			}
			std::fs::write(&b.path, &b.content)?;
		}
	}
	let body_dir = index
		.join(ext)
		.join("functions")
		.join(splitter::source_key_path(src).with_extension(""));
	let mut entries: Vec<(u64, PathBuf)> = if body_dir.exists() {
		walk_md_files(&body_dir)
			.into_iter()
			.filter_map(|p| Some((std::fs::metadata(&p).ok()?.len(), p)))
			.collect()
	} else {
		Vec::new()
	};
	if entries.is_empty() {
		return Ok(format!("structure: {} (no fns)", struct_path.display()));
	}
	entries.sort_by(|a, b| b.0.cmp(&a.0));
	let max_loc = max_loc_threshold();
	let mut out = format!(
		"structure: {}\nbodies:    {}\n",
		struct_path.display(),
		body_dir.display()
	);
	for (_, p) in &entries {
		let name = p.file_stem().unwrap_or_default().to_string_lossy();
		let loc = count_body_loc(p);
		let flag = if loc > max_loc { " ⚠" } else { "" };
		out.push_str(&format!("{loc:6} loc  {name}{flag}\n"));
	}
	Ok(out.trim_end().to_string())
}

pub fn read_body(path: &Path, start: usize, limit: Option<usize>) -> Result<String> {
	let content = std::fs::read_to_string(path)?;
	let start = start.max(1);
	if start == 1 && limit.is_none() {
		return Ok(content);
	}
	let lines: Vec<&str> = content.lines().collect();
	let total = lines.len();
	let begin = (start - 1).min(total);
	let end = match limit {
		Some(l) => (begin + l).min(total),
		None => total,
	};
	let mut out = lines[begin..end].join("\n");
	out.push_str(&format!("\n-- lines {}-{} of {}", begin + 1, end, total));
	Ok(out)
}

pub fn search_bodies(
	query: &str,
	regex: bool,
	scope: &str,
	cursor: usize,
	limit: usize,
) -> Result<String> {
	let index = default_index_dir();
	let matcher = build_matcher(query, regex)?;
	let mut paths: Vec<PathBuf> = Vec::new();
	match scope {
		"structure" | "skel" => paths.extend(walk_structure_files(&index)),
		"body" | "bodies" => paths.extend(walk_body_files(&index)),
		_ => {
			paths.extend(walk_body_files(&index));
			paths.extend(walk_structure_files(&index));
		}
	}
	paths.sort();
	let results = grep_paths(&paths, &matcher)?;
	Ok(format_grep(&results, cursor, limit, query))
}

pub fn list_bodies(
	dir: &Path,
	glob_pat: Option<&str>,
	min_loc: Option<usize>,
	max_loc: Option<usize>,
	sort: &str,
	cursor: usize,
	limit: Option<usize>,
) -> Result<String> {
	let pattern = glob_pat
		.map(glob::Pattern::new)
		.transpose()
		.map_err(|e| anyhow!("invalid glob: {e}"))?;
	let mut entries: Vec<(u64, usize, std::time::SystemTime, PathBuf)> = walk_md_files(dir)
		.into_iter()
		.filter_map(|p| {
			let md = std::fs::metadata(&p).ok()?;
			Some((md.len(), 0usize, md.modified().ok()?, p))
		})
		.filter(|(_, _, _, p)| {
			if let Some(pat) = &pattern {
				let stem = p.file_stem().unwrap_or_default().to_string_lossy();
				pat.matches(&stem)
			} else {
				true
			}
		})
		.collect();
	let need_loc = min_loc.is_some() || max_loc.is_some() || sort == "loc";
	if need_loc {
		for e in &mut entries {
			e.1 = count_body_loc(&e.3);
		}
	}
	if let Some(mn) = min_loc {
		entries.retain(|e| e.1 >= mn);
	}
	if let Some(mx) = max_loc {
		entries.retain(|e| e.1 <= mx);
	}
	match sort {
		"loc" => entries.sort_by(|a, b| b.1.cmp(&a.1)),
		"mtime" => entries.sort_by(|a, b| b.2.cmp(&a.2)),
		"name" => entries.sort_by(|a, b| {
			a.3.file_stem()
				.unwrap_or_default()
				.cmp(b.3.file_stem().unwrap_or_default())
		}),
		_ => entries.sort_by(|a, b| b.0.cmp(&a.0)),
	}
	let total = entries.len();
	let sliced: Vec<_> = entries.into_iter().skip(cursor).collect();
	let sliced: Vec<_> = if let Some(l) = limit {
		sliced.into_iter().take(l).collect()
	} else {
		sliced
	};
	if sliced.is_empty() {
		return Ok(format!("no .md (total={total}, cursor={cursor})"));
	}
	let shown = sliced.len();
	let lines: Vec<String> = sliced
		.iter()
		.map(|(sz, loc, _, p)| {
			let name = p.file_stem().unwrap_or_default().to_string_lossy();
			if need_loc {
				format!("{sz:8}  {loc:6} loc  {name}")
			} else {
				format!("{sz:8}  {name}")
			}
		})
		.collect();
	let next = cursor + shown;
	let footer = if next < total {
		format!("\n-- {shown}/{total} (next cursor: {next})")
	} else {
		format!("\n-- {shown}/{total}")
	};
	Ok(lines.join("\n") + &footer)
}

pub fn find_large(max_loc: Option<usize>) -> Result<String> {
	let index = default_index_dir();
	let max_loc = max_loc.unwrap_or_else(max_loc_threshold);
	let mut hits: Vec<(usize, PathBuf)> = walk_body_files(&index)
		.into_iter()
		.filter_map(|p| {
			let loc = count_body_loc(&p);
			if loc > max_loc {
				Some((loc, p))
			} else {
				None
			}
		})
		.collect();
	hits.sort_by(|a, b| b.0.cmp(&a.0));
	if hits.is_empty() {
		return Ok(format!("no fns exceed {max_loc} loc"));
	}
	Ok(hits
		.iter()
		.map(|(loc, p)| {
			let name = p.file_stem().unwrap_or_default().to_string_lossy();
			let rel = p.strip_prefix(&index).unwrap_or(p);
			format!(
				"⚠ {loc:6} loc  {}/{}",
				rel.with_extension("")
					.display()
					.to_string()
					.replace('\\', "/"),
				name
			)
		})
		.collect::<Vec<_>>()
		.join("\n"))
}

pub fn list_languages() -> String {
	let langs = language::list();
	let arr: Vec<serde_json::Value> = langs
		.into_iter()
		.map(|(ext, source)| {
			let meta = language::meta_for_ext(&ext);
			serde_json::json!({"ext": ext, "source": source, "comment": meta.comment})
		})
		.collect();
	serde_json::to_string_pretty(&serde_json::json!({"languages": arr}))
		.unwrap_or_else(|_| "[]".to_string())
}

pub fn ref_graph(path: &Path, direction: &str) -> Result<String> {
	let index = default_index_dir();
	let content = std::fs::read_to_string(path)?;
	let fm = splitter::parse_frontmatter(&content).unwrap_or_default();
	let mut out = String::new();
	out.push_str(&format!(
		"file: {}\n",
		path.display().to_string().replace('\\', "/")
	));
	let is_body = fm.contains_key("fn");
	if is_body {
		// in: structure file referencing this body (read source: from fm).
		if direction == "in" || direction == "both" {
			let src = fm.get("source").cloned().unwrap_or_default();
			let lang = fm.get("language").cloned().unwrap_or_default();
			let ext = std::path::Path::new(&src)
				.extension()
				.and_then(|e| e.to_str())
				.unwrap_or(&lang);
			let key = std::path::Path::new(&src).with_extension("");
			let struct_p = index
				.join(ext)
				.join("structure")
				.join(format!("{}.md", splitter::to_slash(&key)));
			out.push_str(&format!("in: {}\n", struct_p.display()));
		}
		out.push_str(&format!("source: {}\n", fm.get("source").map(|s| s.as_str()).unwrap_or("?")));
	} else {
		// structure file: list referenced fns from frontmatter.
		if direction == "out" || direction == "both" {
			let fns = fm.get("fns").cloned().unwrap_or_default();
			let names: Vec<&str> = fns.split(',').filter(|s| !s.is_empty()).collect();
			out.push_str(&format!("out ({}):\n", names.len()));
			for n in names {
				out.push_str(&format!("  {n}\n"));
			}
		}
	}
	Ok(out.trim_end().to_string())
}

pub fn outline(path: &Path) -> Result<String> {
	let content = std::fs::read_to_string(path)?;
	// If markdown structure file, list fns from frontmatter.
	if let Some(fm) = splitter::parse_frontmatter(&content) {
		if let Some(fns) = fm.get("fns") {
			let mut out = format!(
				"outline: {}\n",
				path.display().to_string().replace('\\', "/")
			);
			for n in fns.split(',').filter(|s| !s.is_empty()) {
				out.push_str(&format!("fn {n}\n"));
			}
			return Ok(out.trim_end().to_string());
		}
	}
	// Else fall back to source file scan.
	let kinds = ["fn", "impl", "mod", "struct", "enum", "trait"];
	let mut out = format!(
		"outline: {}\n",
		path.display().to_string().replace('\\', "/")
	);
	for (i, line) in content.lines().enumerate() {
		let trimmed = line.trim_start();
		if trimmed.starts_with("//") {
			continue;
		}
		let indent = line.len() - trimmed.len();
		let mut rest = trimmed;
		for _ in 0..2 {
			for prefix in [
				"pub(crate) ",
				"pub(super) ",
				"pub ",
				"async ",
				"unsafe ",
				"default ",
			] {
				if rest.starts_with(prefix) {
					rest = &rest[prefix.len()..];
				}
			}
		}
		for k in &kinds {
			let kw = format!("{} ", k);
			if rest.starts_with(&kw) {
				let after = &rest[kw.len()..];
				let name: String = after
					.chars()
					.take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '<' || *c == ':' || *c == ' ')
					.collect();
				let name = name.split_whitespace().next().unwrap_or("").to_string();
				if !name.is_empty() {
					out.push_str(&format!(
						"{:width$}{} {}  (line {})\n",
						"",
						k,
						name,
						i + 1,
						width = indent
					));
				}
				break;
			}
		}
	}
	Ok(out.trim_end().to_string())
}

pub fn validate(fix: bool) -> Result<String> {
	let index = default_index_dir();
	let bodies: BTreeSet<PathBuf> = walk_body_files(&index).into_iter().collect();
	let structures: Vec<PathBuf> = walk_structure_files(&index);

	let mut referenced: BTreeSet<PathBuf> = BTreeSet::new();
	let mut dead_refs: Vec<(PathBuf, String)> = Vec::new();

	for sp in &structures {
		let content = std::fs::read_to_string(sp).unwrap_or_default();
		let fm = match splitter::parse_frontmatter(&content) {
			Some(m) => m,
			None => continue,
		};
		let src = fm.get("source").cloned().unwrap_or_default();
		let lang = fm.get("language").cloned().unwrap_or_default();
		let ext = std::path::Path::new(&src)
			.extension()
			.and_then(|e| e.to_str())
			.unwrap_or(&lang);
		let key = std::path::Path::new(&src).with_extension("");
		let body_root = index.join(ext).join("functions").join(&key);
		for n in fm
			.get("fns")
			.cloned()
			.unwrap_or_default()
			.split(',')
			.filter(|s| !s.is_empty())
		{
			let bp = body_root.join(format!("{n}.md"));
			if bodies.contains(&bp) {
				referenced.insert(bp);
			} else {
				dead_refs.push((sp.clone(), n.to_string()));
			}
		}
	}

	let orphans: Vec<&PathBuf> = bodies.iter().filter(|b| !referenced.contains(*b)).collect();

	let mut out = String::new();
	out.push_str(&format!("structures: {}\n", structures.len()));
	out.push_str(&format!("bodies:     {}\n", bodies.len()));
	out.push_str(&format!("orphans:    {}\n", orphans.len()));
	for o in &orphans {
		out.push_str(&format!("  - {}\n", o.display()));
	}
	out.push_str(&format!("dead refs:  {}\n", dead_refs.len()));
	for (s, r) in &dead_refs {
		out.push_str(&format!("  - {} -> {r}\n", s.display()));
	}

	if fix {
		let mut deleted = 0u32;
		for o in &orphans {
			if std::fs::remove_file(o).is_ok() {
				deleted += 1;
			}
		}
		out.push_str(&format!("\nfixed: deleted {deleted} orphans\n"));
	}
	Ok(out.trim_end().to_string())
}

fn extract_fenced_code(content: &str) -> String {
	let mut in_fence = false;
	let mut out = Vec::new();
	for line in content.lines() {
		if line.starts_with("```") {
			in_fence = !in_fence;
			continue;
		}
		if in_fence {
			out.push(line);
		}
	}
	out.join("\n")
}

pub fn fn_tree(fn_id: &Path, depth: usize) -> Result<String> {
	let index = default_index_dir();
	let body_paths: Vec<PathBuf> = walk_body_files(&index);
	let known: BTreeSet<String> = body_paths
		.iter()
		.filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
		.collect();
	let by_name: BTreeMap<String, Vec<PathBuf>> = body_paths.iter().fold(BTreeMap::new(), |mut m, p| {
		if let Some(n) = p.file_stem().map(|s| s.to_string_lossy().to_string()) {
			m.entry(n).or_default().push(p.clone());
		}
		m
	});
	let root_name = fn_id
		.file_stem()
		.map(|s| s.to_string_lossy().to_string())
		.ok_or_else(|| anyhow!("bad fn id"))?;
	let mut out = String::new();
	let mut visited: BTreeSet<String> = BTreeSet::new();
	walk_calls(&root_name, &by_name, &known, depth, 0, &mut visited, &mut out)?;
	Ok(out.trim_end().to_string())
}

fn walk_calls(
	name: &str,
	by_name: &BTreeMap<String, Vec<PathBuf>>,
	known: &BTreeSet<String>,
	max_depth: usize,
	depth: usize,
	visited: &mut BTreeSet<String>,
	out: &mut String,
) -> Result<()> {
	let indent = "  ".repeat(depth);
	out.push_str(&format!("{indent}{name}\n"));
	if depth >= max_depth || !visited.insert(name.to_string()) {
		return Ok(());
	}
	let Some(paths) = by_name.get(name) else {
		return Ok(());
	};
	let raw = std::fs::read_to_string(&paths[0]).unwrap_or_default();
	let body = extract_fenced_code(&raw);
	let ident = regex::Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(")?;
	let mut callees: BTreeSet<String> = BTreeSet::new();
	for cap in ident.captures_iter(&body) {
		let n = cap.get(1).unwrap().as_str().to_string();
		if known.contains(&n) && n != name {
			callees.insert(n);
		}
	}
	for c in callees {
		walk_calls(&c, by_name, known, max_depth, depth + 1, visited, out)?;
	}
	Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn count_body_loc(path: &Path) -> usize {
	let Ok(content) = std::fs::read_to_string(path) else {
		return 0;
	};
	// Count code-fence lines only.
	let mut in_fence = false;
	let mut n = 0usize;
	for line in content.lines() {
		if line.starts_with("```") {
			in_fence = !in_fence;
			continue;
		}
		if in_fence {
			n += 1;
		}
	}
	n
}

fn walk_files(dir: &Path, ext: &str) -> Vec<PathBuf> {
	let mut out = Vec::new();
	let Ok(rd) = std::fs::read_dir(dir) else {
		return out;
	};
	for entry in rd.filter_map(|e| e.ok()) {
		let path = entry.path();
		if path.is_dir() {
			out.extend(walk_files(&path, ext));
		} else if path.extension().is_some_and(|e| e == ext) {
			out.push(path);
		}
	}
	out
}

fn walk_md_files(dir: &Path) -> Vec<PathBuf> {
	walk_files(dir, "md")
}

fn walk_body_files(index: &Path) -> Vec<PathBuf> {
	let mut out = Vec::new();
	let Ok(rd) = std::fs::read_dir(index) else {
		return out;
	};
	for entry in rd.filter_map(|e| e.ok()) {
		let p = entry.path();
		if p.is_dir() {
			let fns = p.join("functions");
			if fns.exists() {
				out.extend(walk_md_files(&fns));
			}
		}
	}
	out
}

fn walk_structure_files(index: &Path) -> Vec<PathBuf> {
	let mut out = Vec::new();
	let Ok(rd) = std::fs::read_dir(index) else {
		return out;
	};
	for entry in rd.filter_map(|e| e.ok()) {
		let p = entry.path();
		if p.is_dir() {
			let s = p.join("structure");
			if s.exists() {
				out.extend(walk_md_files(&s));
			}
		}
	}
	out
}

enum Matcher {
	Substring(String),
	Regex(regex::Regex),
}

fn build_matcher(query: &str, use_regex: bool) -> Result<Matcher> {
	if use_regex {
		Ok(Matcher::Regex(
			regex::Regex::new(query).map_err(|e| anyhow!("invalid regex: {e}"))?,
		))
	} else {
		Ok(Matcher::Substring(query.to_lowercase()))
	}
}

fn matcher_hits(m: &Matcher, line: &str) -> bool {
	match m {
		Matcher::Substring(q) => line.to_lowercase().contains(q),
		Matcher::Regex(re) => re.is_match(line),
	}
}

fn grep_paths(paths: &[PathBuf], m: &Matcher) -> Result<Vec<String>> {
	let mut results = Vec::new();
	for path in paths {
		let Ok(content) = std::fs::read_to_string(path) else {
			continue;
		};
		// Skip frontmatter (between leading --- pair).
		let mut in_fm = false;
		let mut fm_done = false;
		for (i, line) in content.lines().enumerate() {
			if !fm_done {
				if i == 0 && line == "---" {
					in_fm = true;
					continue;
				}
				if in_fm {
					if line == "---" {
						in_fm = false;
						fm_done = true;
					}
					continue;
				}
				fm_done = true;
			}
			if matcher_hits(m, line) {
				results.push(format!("{}:{}: {}", path.display(), i + 1, line));
			}
		}
	}
	Ok(results)
}

fn format_grep(results: &[String], cursor: usize, limit: usize, query: &str) -> String {
	let total = results.len();
	if total == 0 {
		return format!("no matches for {query:?}");
	}
	let end = (cursor + limit).min(total);
	let slice = &results[cursor.min(total)..end];
	let shown = slice.len();
	let footer = if end < total {
		format!("\n-- {shown}/{total} (next cursor: {end})")
	} else {
		format!("\n-- {shown}/{total}")
	};
	slice.join("\n") + &footer
}
