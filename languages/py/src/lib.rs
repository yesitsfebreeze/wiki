use std::alloc::{alloc, dealloc, Layout};
use std::path::Path;

#[derive(serde::Deserialize)]
struct Input {
    source: String,
    source_path: String,
    #[serde(alias = "split_dir", alias = "index_dir")]
    #[allow(dead_code)]
    index_dir: String,
}

#[derive(serde::Serialize)]
struct Output {
    bodies: Vec<Body>,
}

#[derive(serde::Serialize)]
struct Body {
    name: String,
    signature: String,
    raw: String,
    line_start: usize,
    line_end: usize,
}

static META_JSON: &[u8] = b"{\"comment\":\"#\",\"proto\":2}";
static mut OUT: Vec<u8> = Vec::new();

#[no_mangle]
pub extern "C" fn wasm_alloc(size: i32) -> i32 {
    unsafe {
        let layout = Layout::from_size_align(size as usize, 1).unwrap();
        alloc(layout) as i32
    }
}

#[no_mangle]
pub extern "C" fn wasm_dealloc(ptr: i32, size: i32) {
    unsafe {
        let layout = Layout::from_size_align(size as usize, 1).unwrap();
        dealloc(ptr as *mut u8, layout);
    }
}

#[no_mangle]
pub extern "C" fn language_meta_ptr() -> i32 {
    META_JSON.as_ptr() as i32
}

#[no_mangle]
pub extern "C" fn language_meta_len() -> i32 {
    META_JSON.len() as i32
}

#[no_mangle]
pub extern "C" fn language_split(ptr: i32, len: i32) -> i32 {
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let result = do_split(input);
    unsafe {
        OUT = result;
        OUT.len() as i32
    }
}

#[no_mangle]
pub extern "C" fn language_result_ptr() -> i32 {
    unsafe { OUT.as_ptr() as i32 }
}

fn do_split(input: &[u8]) -> Vec<u8> {
    let Ok(inp) = serde_json::from_slice::<Input>(input) else {
        return b"{\"bodies\":[]}".to_vec();
    };
    let source_path = Path::new(&inp.source_path);
    let out = split_py(&inp.source, source_path);
    serde_json::to_vec(&out).unwrap_or_default()
}

fn split_py(source: &str, _source_path: &Path) -> Output {
    let funcs = find_defs(source);
    let mut bodies = Vec::new();

    for f in funcs {
        let raw_body = strip_body_edges(&source[f.body_start..f.body_end]);
        // sig spans from decl_start (line content_start) up to body_block_start (after `:` newline).
        let sig_slice = &source[f.decl_start..f.body_start];
        let signature = sig_slice
            .trim_end_matches(|c: char| c == '\n' || c == '\r')
            .trim()
            .to_string();
        bodies.push(Body {
            name: f.name,
            signature,
            raw: raw_body,
            line_start: f.line_start,
            line_end: f.line_end,
        });
    }

    Output { bodies }
}

fn line_of(line_starts: &[usize], byte_offset: usize) -> usize {
    line_index_at(line_starts, byte_offset) + 1
}

fn strip_body_edges(s: &str) -> String {
    let s = s.strip_prefix("\r\n").or_else(|| s.strip_prefix('\n')).unwrap_or(s);
    s.trim_end().to_string()
}

struct DefLoc {
    name: String,
    decl_start: usize,
    body_start: usize,
    body_end: usize,
    #[allow(dead_code)]
    body_indent: usize,
    line_start: usize,
    line_end: usize,
}

#[derive(Clone)]
struct Scope {
    indent: usize,
    name: String,
    is_def: bool,
}

fn find_defs(source: &str) -> Vec<DefLoc> {
    let bytes = source.as_bytes();
    let line_starts = compute_line_starts(bytes);
    let mut result = Vec::new();
    let mut scopes: Vec<Scope> = Vec::new();
    let mut i = 0usize;

    while i < line_starts.len() {
        let line_start = line_starts[i];
        let line_end = line_end_at(bytes, line_start);
        let (indent, content_start) = leading_indent(bytes, line_start, line_end);

        if content_start >= line_end || bytes[content_start] == b'#' {
            i += 1;
            continue;
        }

        while let Some(s) = scopes.last() {
            if s.indent >= indent {
                scopes.pop();
            } else {
                break;
            }
        }

        if let Some(parsed) = parse_def_or_class(bytes, content_start, line_end) {
            let sig_end = find_signature_colon(bytes, parsed.after_name);
            let body_block_start = match sig_end {
                Some(off) => skip_to_next_line(bytes, off),
                None => {
                    i += 1;
                    continue;
                }
            };

            let (body_end, body_indent, lines_consumed) =
                find_body_extent(bytes, &line_starts, body_block_start, indent);

            let nested_in_def = scopes.iter().any(|s| s.is_def);

            let qualified = if scopes.is_empty() {
                parsed.name.clone()
            } else {
                let path: Vec<&str> = scopes.iter().map(|s| s.name.as_str()).collect();
                format!("{}.{}", path.join("."), parsed.name)
            };

            if parsed.is_def && !nested_in_def && body_end > body_block_start {
                let ls = i + 1;
                let le = line_of(&line_starts, body_end.saturating_sub(1));
                result.push(DefLoc {
                    name: qualified,
                    decl_start: content_start,
                    body_start: body_block_start,
                    body_end,
                    body_indent,
                    line_start: ls,
                    line_end: le,
                });
            }

            scopes.push(Scope {
                indent,
                name: parsed.name,
                is_def: parsed.is_def,
            });

            if parsed.is_def {
                i = line_index_at(&line_starts, body_block_start) + lines_consumed;
            } else {
                i = line_index_at(&line_starts, body_block_start);
            }
            continue;
        }

        i += 1;
    }

    result
}

struct Parsed {
    name: String,
    is_def: bool,
    after_name: usize,
}

fn parse_def_or_class(bytes: &[u8], start: usize, line_end: usize) -> Option<Parsed> {
    let slice = &bytes[start..line_end];
    let (kw_len, is_def) = if slice.starts_with(b"async def ") || slice.starts_with(b"async\tdef ") {
        (10, true)
    } else if slice.starts_with(b"def ") {
        (4, true)
    } else if slice.starts_with(b"class ") {
        (6, false)
    } else {
        return None;
    };
    let name_start = skip_inline_ws(bytes, start + kw_len);
    if name_start >= line_end || !is_ident_start(bytes[name_start]) {
        return None;
    }
    let name_end = ident_end(bytes, name_start);
    let name = String::from_utf8_lossy(&bytes[name_start..name_end]).to_string();
    Some(Parsed {
        name,
        is_def,
        after_name: name_end,
    })
}

fn find_signature_colon(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    let mut paren = 0i32;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'#' if paren == 0 => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'\\' if i + 1 < bytes.len() && bytes[i + 1] == b'\n' => {
                i += 2;
                continue;
            }
            b'\\' if i + 2 < bytes.len() && bytes[i + 1] == b'\r' && bytes[i + 2] == b'\n' => {
                i += 3;
                continue;
            }
            b'\n' if paren == 0 => return None,
            b'(' | b'[' | b'{' => paren += 1,
            b')' | b']' | b'}' => paren -= 1,
            b':' if paren == 0 => return Some(i + 1),
            b'"' | b'\'' => {
                if let Some(j) = skip_string(bytes, i) {
                    i = j;
                    continue;
                } else {
                    return None;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn skip_string(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    let mut prefix_end = i;
    while prefix_end < bytes.len() && prefix_end - i < 3 {
        let c = bytes[prefix_end];
        if matches!(c, b'r' | b'R' | b'b' | b'B' | b'f' | b'F' | b'u' | b'U') {
            prefix_end += 1;
        } else {
            break;
        }
    }
    if prefix_end >= bytes.len() {
        return None;
    }
    let q = bytes[prefix_end];
    if q != b'"' && q != b'\'' {
        return None;
    }
    let is_raw = bytes[i..prefix_end]
        .iter()
        .any(|c| matches!(c, b'r' | b'R'));
    let triple = prefix_end + 2 < bytes.len()
        && bytes[prefix_end + 1] == q
        && bytes[prefix_end + 2] == q;
    let mut j = if triple { prefix_end + 3 } else { prefix_end + 1 };
    if triple {
        while j + 2 < bytes.len() {
            if bytes[j] == q && bytes[j + 1] == q && bytes[j + 2] == q {
                return Some(j + 3);
            }
            if !is_raw && bytes[j] == b'\\' {
                j += 2;
                continue;
            }
            j += 1;
        }
        Some(bytes.len())
    } else {
        while j < bytes.len() {
            let c = bytes[j];
            if c == b'\n' {
                return Some(j);
            }
            if !is_raw && c == b'\\' && j + 1 < bytes.len() {
                j += 2;
                continue;
            }
            if c == q {
                return Some(j + 1);
            }
            j += 1;
        }
        Some(bytes.len())
    }
}

fn find_body_extent(
    bytes: &[u8],
    line_starts: &[usize],
    body_block_start: usize,
    def_indent: usize,
) -> (usize, usize, usize) {
    let start_line_idx = line_index_at(line_starts, body_block_start);
    let mut last_content_end = body_block_start;
    let mut body_indent_opt: Option<usize> = None;
    let mut idx = start_line_idx;

    while idx < line_starts.len() {
        let ls = line_starts[idx];
        let le = line_end_at(bytes, ls);
        let (indent, content_start) = leading_indent(bytes, ls, le);
        let blank_or_comment = content_start >= le || bytes[content_start] == b'#';

        if blank_or_comment {
            if body_indent_opt.is_some() {
                last_content_end = le;
            }
            idx += 1;
            continue;
        }

        if indent <= def_indent {
            break;
        }

        if body_indent_opt.is_none() {
            body_indent_opt = Some(indent);
        }
        last_content_end = le;
        idx += 1;
    }

    let body_indent = body_indent_opt.unwrap_or(def_indent + 4);
    let lines_consumed = idx - start_line_idx;
    (last_content_end, body_indent, lines_consumed.max(1))
}

fn compute_line_starts(bytes: &[u8]) -> Vec<usize> {
    let mut v = vec![0];
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' && i + 1 < bytes.len() {
            v.push(i + 1);
        }
    }
    v
}

fn line_end_at(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

fn leading_indent(bytes: &[u8], start: usize, end: usize) -> (usize, usize) {
    let mut i = start;
    let mut indent = 0usize;
    while i < end {
        match bytes[i] {
            b' ' => indent += 1,
            b'\t' => indent += 8 - (indent % 8),
            _ => break,
        }
        i += 1;
    }
    (indent, i)
}

fn skip_inline_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    i
}

fn skip_to_next_line(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    if i < bytes.len() {
        i + 1
    } else {
        i
    }
}

fn line_index_at(line_starts: &[usize], offset: usize) -> usize {
    match line_starts.binary_search(&offset) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn ident_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && is_ident_char(bytes[i]) {
        i += 1;
    }
    i
}
