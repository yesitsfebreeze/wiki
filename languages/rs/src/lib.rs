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

static META_JSON: &[u8] = b"{\"comment\":\"//\",\"proto\":2}";

#[no_mangle]
pub extern "C" fn language_meta_ptr() -> i32 {
    META_JSON.as_ptr() as i32
}

#[no_mangle]
pub extern "C" fn language_meta_len() -> i32 {
    META_JSON.len() as i32
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
    let out = split_rs(&inp.source, source_path);
    serde_json::to_vec(&out).unwrap_or_default()
}

fn split_rs(source: &str, _source_path: &Path) -> Output {
    let funcs = find_fns(source);
    let mut bodies = Vec::new();

    for f in funcs {
        let raw_body = strip_body_edges(&source[f.body_start..f.body_end]);
        // body_start = open + 1, so open = body_start - 1. Sig = decl_start..open.
        let open = f.body_start.saturating_sub(1);
        let signature = source[f.decl_start..open]
            .trim()
            .replace('\n', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        let line_start = line_of(source, f.decl_start);
        let line_end = line_of(source, f.body_close);

        bodies.push(Body {
            name: f.name,
            signature,
            raw: raw_body,
            line_start,
            line_end,
        });
    }

    Output { bodies }
}

fn line_of(source: &str, byte_offset: usize) -> usize {
    let end = byte_offset.min(source.len());
    source.as_bytes()[..end].iter().filter(|&&b| b == b'\n').count() + 1
}

fn strip_body_edges(s: &str) -> String {
    let s = s.strip_prefix("\r\n").or_else(|| s.strip_prefix('\n')).unwrap_or(s);
    s.trim_end().to_string()
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
        if bytes[i] == b'r' && i + 1 < bytes.len() && (bytes[i + 1] == b'#' || bytes[i + 1] == b'"')
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
