use anyhow::{Context, Result};
use std::path::{Component, Path, PathBuf};

pub struct BodyFile {
	pub path: PathBuf,
	pub content: String,
}

#[derive(Clone)]
pub struct FnMeta {
	pub name: String,
	pub signature: String,
	pub raw: String,
	pub line_start: usize,
	pub line_end: usize,
}

pub fn split_for_ext(
	source_path: &Path,
	index_dir: &Path,
	ext: &str,
) -> Result<(String, Vec<BodyFile>)> {
	if let Some(wasm) = crate::code::language::load(ext) {
		if let Ok(result) = crate::code::language::split(&wasm, ext, source_path, index_dir) {
			return Ok(result);
		}
	}
	if ext == "rs" {
		split_rs(source_path, index_dir)
	} else {
		split_generic(source_path, index_dir)
	}
}

pub fn split_generic(source_path: &Path, index_dir: &Path) -> Result<(String, Vec<BodyFile>)> {
	let source = std::fs::read_to_string(source_path)
		.with_context(|| format!("read {}", source_path.display()))?;
	let source_key = source_key_path(source_path);
	let ext = source_path
		.extension()
		.and_then(|e| e.to_str())
		.unwrap_or("");
	let src_display = to_slash(&source_key);
	let total = source.lines().count().max(1);
	let meta = FnMeta {
		name: "_body".to_string(),
		signature: format!("(whole file: {})", src_display),
		raw: source.trim_end().to_string(),
		line_start: 1,
		line_end: total,
	};
	let body_path = body_path(&source_key, ext, &meta.name, index_dir);
	let body_content = render_body_md(ext, &src_display, &meta);
	let structure = render_structure_md(&source, &source_key, ext, &[meta]);
	Ok((
		structure,
		vec![BodyFile {
			path: body_path,
			content: body_content,
		}],
	))
}

pub fn split_rs(source_path: &Path, index_dir: &Path) -> Result<(String, Vec<BodyFile>)> {
	let source = std::fs::read_to_string(source_path)
		.with_context(|| format!("read {}", source_path.display()))?;
	let source_key = source_key_path(source_path);
	let src_display = to_slash(&source_key);
	let funcs = find_fns(&source);

	let mut metas = Vec::with_capacity(funcs.len());
	for f in funcs {
		let raw = strip_body_edges(&source[f.body_start..f.body_end]);
		let open = f.body_start.saturating_sub(1);
		let signature = source[f.decl_start..open]
			.split_whitespace()
			.collect::<Vec<_>>()
			.join(" ");
		metas.push(FnMeta {
			name: f.name,
			signature,
			raw,
			line_start: line_of(&source, f.decl_start),
			line_end: line_of(&source, f.body_close),
		});
	}

	let bodies: Vec<BodyFile> = metas
		.iter()
		.map(|m| BodyFile {
			path: body_path(&source_key, "rs", &m.name, index_dir),
			content: render_body_md("rs", &src_display, m),
		})
		.collect();
	let structure = render_structure_md(&source, &source_key, "rs", &metas);
	Ok((structure, bodies))
}

// ── Path scheme ──────────────────────────────────────────────────────────────
// index_dir/<ext>/structure/<source_key>.md
// index_dir/<ext>/functions/<source_key_no_ext>/<fn_name>.md

pub fn structure_path(src: &Path, index_dir: &Path) -> PathBuf {
	let key = source_key_path(src);
	let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("rs");
	index_dir
		.join(ext)
		.join("structure")
		.join(key.with_extension("md"))
}

pub fn body_path(source_key: &Path, ext: &str, fn_name: &str, index_dir: &Path) -> PathBuf {
	let stem_dir = source_key.with_extension("");
	let safe_name = sanitize_fn_name(fn_name);
	index_dir
		.join(ext)
		.join("functions")
		.join(stem_dir)
		.join(format!("{safe_name}.md"))
}

fn sanitize_fn_name(name: &str) -> String {
	// Replace path-hostile chars (e.g. `Class.method` → `Class.method` is fine; `<T>` not).
	name.chars()
		.map(|c| if matches!(c, '<' | '>' | '/' | '\\' | ':' | '*' | '?' | '"' | '|') { '_' } else { c })
		.collect()
}

// ── Markdown rendering ───────────────────────────────────────────────────────

pub fn render_body_md(ext: &str, src_display: &str, m: &FnMeta) -> String {
	let mut out = String::new();
	out.push_str("---\n");
	out.push_str("tags: [code, code-function]\n");
	out.push_str(&format!("fn: {}\n", yaml_escape(&m.name)));
	out.push_str(&format!("source: {}\n", yaml_escape(src_display)));
	out.push_str(&format!("lines: {}-{}\n", m.line_start, m.line_end));
	out.push_str(&format!("language: {}\n", ext));
	out.push_str(&format!("signature: {}\n", yaml_escape(&m.signature)));
	out.push_str("---\n\n");
	out.push_str(&format!("# {}\n\n", m.name));
	if !m.signature.is_empty() {
		out.push_str(&format!("`{}`\n\n", m.signature));
	}
	out.push_str(&format!("```{ext}\n{}\n```\n\n", m.raw));
	out.push_str(&format!("← [[{}]]\n", structure_link(src_display)));
	out
}

pub fn render_structure_md(
	source: &str,
	source_key: &Path,
	ext: &str,
	fns: &[FnMeta],
) -> String {
	let src_display = to_slash(source_key);
	let mut out = String::new();
	out.push_str("---\n");
	out.push_str("tags: [code, code-structure]\n");
	out.push_str(&format!("source: {}\n", yaml_escape(&src_display)));
	out.push_str(&format!("language: {}\n", ext));
	out.push_str("fns:\n");
	for f in fns {
		out.push_str(&format!("  - {}\n", yaml_escape(&f.name)));
	}
	out.push_str("---\n\n");
	out.push_str(&format!("# {}\n\n", src_display));

	// Top-level: source minus fn body ranges (by line numbers).
	let top = top_level_text(source, fns);
	if !top.trim().is_empty() {
		out.push_str("## Top-level\n\n");
		out.push_str(&format!("```{ext}\n{}\n```\n\n", top.trim_end()));
	}

	out.push_str("## Functions\n\n");
	for f in fns {
		let stem = source_key.with_extension("");
		let link = format!(
			"{}/functions/{}/{}",
			ext,
			to_slash(&stem),
			sanitize_fn_name(&f.name)
		);
		out.push_str(&format!(
			"- [[{link}|{name}]] — lines {ls}-{le}\n",
			name = f.name,
			ls = f.line_start,
			le = f.line_end
		));
		if !f.signature.is_empty() {
			out.push_str(&format!("  - `{}`\n", f.signature));
		}
	}
	out
}

fn top_level_text(source: &str, fns: &[FnMeta]) -> String {
	if fns.is_empty() {
		return source.to_string();
	}
	let lines: Vec<&str> = source.lines().collect();
	let mut keep = vec![true; lines.len()];
	for f in fns {
		let lo = f.line_start.saturating_sub(1);
		let hi = f.line_end.min(lines.len());
		for k in &mut keep[lo..hi] {
			*k = false;
		}
	}
	lines
		.iter()
		.zip(keep)
		.filter_map(|(l, k)| if k { Some(*l) } else { None })
		.collect::<Vec<_>>()
		.join("\n")
}

fn structure_link(src_display: &str) -> String {
	// Produce wiki-style structure link target relative to vault root.
	// Caller passes ext via src_display extension.
	let ext = std::path::Path::new(src_display)
		.extension()
		.and_then(|e| e.to_str())
		.unwrap_or("rs");
	let key = std::path::Path::new(src_display).with_extension("");
	format!("{ext}/structure/{}", to_slash(&key))
}

fn yaml_escape(s: &str) -> String {
	if s.is_empty() {
		return "\"\"".to_string();
	}
	if s.contains(':') || s.contains('#') || s.contains('"') || s.contains('\'') || s.contains('\n') {
		let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
		return format!("\"{}\"", escaped);
	}
	s.to_string()
}

// ── Frontmatter parsing (for ref_graph / validate / fn_tree) ─────────────────

pub fn parse_frontmatter(content: &str) -> Option<std::collections::BTreeMap<String, String>> {
	let mut lines = content.lines();
	if lines.next()? != "---" {
		return None;
	}
	let mut map = std::collections::BTreeMap::new();
	let mut current_list_key: Option<String> = None;
	let mut current_list: Vec<String> = Vec::new();
	for line in lines {
		if line == "---" {
			if let Some(k) = current_list_key.take() {
				map.insert(k, current_list.join(","));
				current_list.clear();
			}
			return Some(map);
		}
		if let Some(item) = line.strip_prefix("  - ") {
			if current_list_key.is_some() {
				current_list.push(item.trim().trim_matches('"').to_string());
				continue;
			}
		}
		if let Some(k) = current_list_key.take() {
			map.insert(k, current_list.join(","));
			current_list.clear();
		}
		if let Some((k, v)) = line.split_once(':') {
			let k = k.trim().to_string();
			let v = v.trim().trim_matches('"');
			if v.is_empty() {
				current_list_key = Some(k);
			} else {
				map.insert(k, v.to_string());
			}
		}
	}
	None
}

// ── Helpers (RS native splitter) ─────────────────────────────────────────────

fn line_of(source: &str, byte_offset: usize) -> usize {
	let end = byte_offset.min(source.len());
	source.as_bytes()[..end].iter().filter(|&&b| b == b'\n').count() + 1
}

struct FnLoc {
	name: String,
	decl_start: usize,
	body_start: usize,
	body_end: usize,
	body_close: usize,
}

fn find_fns(source: &str) -> Vec<FnLoc> {
	let bytes = source.as_bytes();
	let mut result = Vec::new();
	let mut i = 0;

	while i < bytes.len() {
		if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
			while i < bytes.len() && bytes[i] != b'\n' {
				i += 1;
			}
			continue;
		}
		if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
			i += 2;
			while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
				i += 1;
			}
			i += 2;
			continue;
		}
		if bytes[i] == b'"' {
			i = skip_string(bytes, i + 1);
			continue;
		}
		if bytes[i] == b'r'
			&& i + 1 < bytes.len()
			&& (bytes[i + 1] == b'#' || bytes[i + 1] == b'"')
		{
			if let Some(j) = skip_raw_string(bytes, i) {
				i = j;
				continue;
			}
		}

		if i + 2 <= bytes.len() && &bytes[i..i + 2] == b"fn" {
			let pre_ok = i == 0 || !is_ident_char(bytes[i - 1]);
			let post_ok = i + 2 >= bytes.len() || !is_ident_char(bytes[i + 2]);
			if pre_ok && post_ok {
				let name_start = skip_ws(bytes, i + 2);
				if name_start < bytes.len() && is_ident_start(bytes[name_start]) {
					let name_end = ident_end(bytes, name_start);
					let name = String::from_utf8_lossy(&bytes[name_start..name_end]).to_string();
					if let Some(open) = find_open_brace(bytes, name_end) {
						if let Some(close) = find_close_brace(bytes, open) {
							result.push(FnLoc {
								name,
								decl_start: i,
								body_start: open + 1,
								body_end: close,
								body_close: close,
							});
							i = close + 1;
							continue;
						}
					}
				}
			}
		}
		i += 1;
	}
	result
}

fn find_open_brace(bytes: &[u8], from: usize) -> Option<usize> {
	let mut i = from;
	let mut paren = 0i32;
	let mut angle = 0i32;
	while i < bytes.len() {
		match bytes[i] {
			b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
				while i < bytes.len() && bytes[i] != b'\n' {
					i += 1;
				}
			}
			b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
				i += 2;
				while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
					i += 1;
				}
				i += 2;
				continue;
			}
			b'(' => paren += 1,
			b')' => paren -= 1,
			b'<' if paren == 0 => angle += 1,
			b'>' if paren == 0 && angle > 0 => angle -= 1,
			b';' if paren == 0 && angle == 0 => return None,
			b'{' if paren == 0 && angle == 0 => return Some(i),
			_ => {}
		}
		i += 1;
	}
	None
}

fn find_close_brace(bytes: &[u8], open: usize) -> Option<usize> {
	let mut depth = 1i32;
	let mut i = open + 1;
	while i < bytes.len() {
		match bytes[i] {
			b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
				while i < bytes.len() && bytes[i] != b'\n' {
					i += 1;
				}
				continue;
			}
			b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
				i += 2;
				while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
					i += 1;
				}
				i += 2;
				continue;
			}
			b'"' => {
				i = skip_string(bytes, i + 1);
				continue;
			}
			b'r' if i + 1 < bytes.len() && (bytes[i + 1] == b'#' || bytes[i + 1] == b'"') => {
				if let Some(j) = skip_raw_string(bytes, i) {
					i = j;
					continue;
				}
			}
			b'\'' if i + 2 < bytes.len() => {
				let next = bytes[i + 1];
				if next == b'\\' {
					i += 3;
					if i < bytes.len() && bytes[i] == b'\'' {
						i += 1;
					}
					continue;
				} else if i + 2 < bytes.len() && bytes[i + 2] == b'\'' {
					i += 3;
					continue;
				}
			}
			b'{' => depth += 1,
			b'}' => {
				depth -= 1;
				if depth == 0 {
					return Some(i);
				}
			}
			_ => {}
		}
		i += 1;
	}
	None
}

fn skip_string(bytes: &[u8], mut i: usize) -> usize {
	while i < bytes.len() {
		if bytes[i] == b'\\' {
			i += 2;
			continue;
		}
		if bytes[i] == b'"' {
			return i + 1;
		}
		i += 1;
	}
	i
}

fn skip_raw_string(bytes: &[u8], start: usize) -> Option<usize> {
	let mut i = start + 1;
	let h0 = i;
	while i < bytes.len() && bytes[i] == b'#' {
		i += 1;
	}
	let hashes = i - h0;
	if i >= bytes.len() || bytes[i] != b'"' {
		return None;
	}
	i += 1;
	loop {
		if i >= bytes.len() {
			return Some(i);
		}
		if bytes[i] == b'"' {
			let mut j = i + 1;
			let mut count = 0;
			while j < bytes.len() && bytes[j] == b'#' {
				count += 1;
				j += 1;
			}
			if count >= hashes {
				return Some(j);
			}
		}
		i += 1;
	}
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
	while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
		i += 1;
	}
	i
}

fn is_ident_char(b: u8) -> bool {
	b.is_ascii_alphanumeric() || b == b'_'
}
fn is_ident_start(b: u8) -> bool {
	b.is_ascii_alphabetic() || b == b'_'
}

fn ident_end(bytes: &[u8], start: usize) -> usize {
	let mut i = start;
	while i < bytes.len() && is_ident_char(bytes[i]) {
		i += 1;
	}
	i
}

fn strip_body_edges(s: &str) -> String {
	let s = s
		.strip_prefix("\r\n")
		.or_else(|| s.strip_prefix('\n'))
		.unwrap_or(s);
	s.trim_end().to_string()
}

pub fn to_slash(p: &Path) -> String {
	p.to_string_lossy().replace('\\', "/")
}

pub fn source_key_path(source_path: &Path) -> PathBuf {
	if !source_path.is_absolute() {
		return source_path.to_path_buf();
	}
	if let Ok(cwd) = std::env::current_dir() {
		if let Ok(rel) = source_path.strip_prefix(&cwd) {
			return rel.to_path_buf();
		}
	}
	let mut key = PathBuf::new();
	for comp in source_path.components() {
		match comp {
			Component::Normal(seg) => key.push(seg),
			Component::Prefix(prefix) => {
				let mut drive = prefix.as_os_str().to_string_lossy().to_string();
				drive.retain(|c| c != ':' && c != '\\' && c != '/');
				if !drive.is_empty() {
					key.push(drive);
				}
			}
			Component::RootDir | Component::CurDir | Component::ParentDir => {}
		}
	}
	if key.as_os_str().is_empty() {
		source_path.to_path_buf()
	} else {
		key
	}
}

