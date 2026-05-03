//! Enrich wiki docs with `code_refs` frontmatter: keyword-search the code
//! index during learn pass, record matching source paths in the wiki doc.
//! Code files are never modified — the link is stored wiki-side only.

use crate::{code, store};
use std::collections::HashSet;
use std::path::Path;

const STOPWORDS: &[&str] = &[
    "also", "about", "are", "been", "but", "can", "could", "does", "each",
    "for", "from", "have", "how", "into", "its", "more", "not", "should",
    "some", "such", "than", "that", "the", "their", "then", "there", "these",
    "they", "this", "those", "used", "use", "was", "what", "when", "where",
    "which", "will", "with", "would", "why",
];

fn extract_tokens(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 4)
        .filter(|t| !STOPWORDS.contains(&t.to_lowercase().as_str()))
        .map(|t| t.to_lowercase())
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// Convert a code body index path back to `src/file.ext::fn_name`.
/// Body path structure: `<index>/<ext>/functions/<src_path>/<fn>.md`
fn body_path_to_ref(body_path: &Path, index: &Path) -> Option<String> {
    let rel = body_path.strip_prefix(index).ok()?;
    let mut comps = rel.components();
    let ext = comps.next()?.as_os_str().to_str()?;
    let _functions = comps.next()?; // skip "functions" segment
    let rest: std::path::PathBuf = comps.collect();
    let fn_name = rest.file_stem()?.to_str()?;
    let src_dir = rest.parent()?;
    let source = format!(
        "{}.{}",
        src_dir.display().to_string().replace('\\', "/"),
        ext
    );
    Some(format!("{}::{}", source, fn_name))
}

fn parse_refs_from_result(result: &str, index: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for line in result.lines() {
        if line.starts_with("no matches") || line.starts_with("--") {
            continue;
        }
        // Line format: `<path>.md:<lineno>: <content>`
        // Use ".md:" as the split boundary to handle Windows drive-letter colons.
        if let Some(pos) = line.find(".md:") {
            let hit_path = Path::new(&line[..pos + 3]);
            if let Some(r) = body_path_to_ref(hit_path, index) {
                out.push(r);
            }
        }
    }
    out
}

/// Run keyword search over the code index for `doc`'s title + body.
/// Writes `code_refs` frontmatter if any matches found. No-op when no
/// code index exists.
pub fn enrich_with_code_refs(root: &Path, doc_type: &str, doc: &store::Document) {
    let index = code::default_index_dir();
    if !index.exists() {
        return;
    }

    // Title tokens weighted first, then body tokens up to a cap.
    let mut tokens = extract_tokens(&doc.title);
    let body_tokens = extract_tokens(&doc.content);
    for t in body_tokens {
        if !tokens.contains(&t) {
            tokens.push(t);
        }
        if tokens.len() >= 20 {
            break;
        }
    }

    let mut refs: Vec<String> = Vec::new();
    let mut seen_sources: HashSet<String> = HashSet::new();

    'outer: for token in &tokens {
        let Ok(result) = code::search_bodies(token, false, "body", 0, 8) else {
            continue;
        };
        for code_ref in parse_refs_from_result(&result, &index) {
            let source = code_ref.split("::").next().unwrap_or("").to_string();
            if seen_sources.insert(source) {
                refs.push(code_ref);
                if refs.len() >= 10 {
                    break 'outer;
                }
            }
        }
    }

    if refs.is_empty() {
        return;
    }

    let _ = store::set_frontmatter_field(
        root,
        doc_type,
        &doc.id,
        "code_refs",
        serde_json::json!(refs),
    );
}
