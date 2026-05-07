use regex::Regex;
use std::sync::OnceLock;

/// Default templated-question patterns. Anything matching is rejected during
/// `raise_questions_for_doc`. Override via `WIKI_TEMPLATE_QUESTION_REGEXES` --
/// `;`-separated list of full-line regexes (case-insensitive).
pub const DEFAULT_TEMPLATE_QUESTION_REGEXES: &[&str] = &[
    r"(?i)^how does .{0,80} relate to or differ from similar concepts\??$",
    r"(?i)^what are the key characteristics of .{0,80}\??$",
    r"(?i)^what are the implications of .{0,80}\??$",
    r"(?i)^what is the (importance|significance|purpose) of .{0,80}\??$",
];

/// Default cap on open (unresolved) questions per purpose. Override via
/// `WIKI_OPEN_QUESTIONS_PER_PURPOSE_CAP`.
pub const DEFAULT_OPEN_QUESTIONS_PER_PURPOSE_CAP: usize = 25;

fn template_regexes() -> &'static Vec<Regex> {
    static CELL: OnceLock<Vec<Regex>> = OnceLock::new();
    CELL.get_or_init(|| {
        let raw = std::env::var("WIKI_TEMPLATE_QUESTION_REGEXES").ok();
        let patterns: Vec<String> = match raw.as_deref() {
            Some(s) if !s.trim().is_empty() => s
                .split(';')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect(),
            _ => DEFAULT_TEMPLATE_QUESTION_REGEXES.iter().map(|s| s.to_string()).collect(),
        };
        patterns.into_iter().filter_map(|p| Regex::new(&p).ok()).collect()
    })
}

/// Returns true when `title` matches any configured template regex.
pub fn is_template_question(title: &str) -> bool {
    let trimmed = title.trim();
    template_regexes().iter().any(|re| re.is_match(trimmed))
}

pub fn open_questions_per_purpose_cap() -> usize {
    std::env::var("WIKI_OPEN_QUESTIONS_PER_PURPOSE_CAP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_OPEN_QUESTIONS_PER_PURPOSE_CAP)
}

pub fn relations_limit(doc_type: &str) -> usize {
    let (env_key, default) = match doc_type {
        "thoughts"    => ("WIKI_RELATIONS_LIMIT_THOUGHTS",    5usize),
        "questions"   => ("WIKI_RELATIONS_LIMIT_QUESTIONS",   3usize),
        "conclusions" => ("WIKI_RELATIONS_LIMIT_CONCLUSIONS", 5usize),
        "entities"    => ("WIKI_RELATIONS_LIMIT_ENTITIES",   10usize),
        _             => ("WIKI_RELATIONS_LIMIT_CODE",        5usize),
    };
    std::env::var(env_key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

const MAPPINGS: &[(&str, &str)] = &[
    ("openai_api_key", "OPENAI_API_KEY"),
    ("wiki_rerank_model", "WIKI_RERANK_MODEL"),
    ("wiki_similarity_threshold", "WIKI_SIMILARITY_THRESHOLD"),
    ("wiki_dedupe_threshold", "WIKI_DEDUPE_THRESHOLD"),
    ("wiki_template_question_regexes", "WIKI_TEMPLATE_QUESTION_REGEXES"),
    ("wiki_open_questions_per_purpose_cap", "WIKI_OPEN_QUESTIONS_PER_PURPOSE_CAP"),
    ("code_dirs", "WIKI_CODE_DIRS"),
    ("split_ext", "SPLIT_EXT"),
    ("split_exts", "SPLIT_EXTS"),
    ("split_index_dir", "SPLIT_INDEX_DIR"),
    ("split_max_loc", "SPLIT_MAX_LOC"),
    ("relations_limit_thoughts", "WIKI_RELATIONS_LIMIT_THOUGHTS"),
    ("relations_limit_questions", "WIKI_RELATIONS_LIMIT_QUESTIONS"),
    ("relations_limit_conclusions", "WIKI_RELATIONS_LIMIT_CONCLUSIONS"),
    ("relations_limit_entities", "WIKI_RELATIONS_LIMIT_ENTITIES"),
    ("relations_limit_code", "WIKI_RELATIONS_LIMIT_CODE"),
];

fn table_to_map(table: &toml::Table) -> std::collections::HashMap<&'static str, String> {
    let mut map = std::collections::HashMap::new();
    for (toml_key, env_key) in MAPPINGS {
        if let Some(val) = table.get(*toml_key) {
            let s = match val {
                toml::Value::String(s) => s.clone(),
                toml::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                other => other.to_string(),
            };
            map.insert(*env_key, s);
        }
    }
    map
}

fn find_local_config() -> Option<std::path::PathBuf> {
    // prefer WIKI_PATH env var (set by MCP server config)
    if let Ok(wiki_path) = std::env::var("WIKI_PATH") {
        let p = std::path::PathBuf::from(&wiki_path).join("config.toml");
        if p.exists() {
            return Some(p);
        }
    }
    // walk up from cwd looking for .wiki/config.toml
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let p = dir.join(".wiki").join("config.toml");
        if p.exists() {
            return Some(p);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

pub fn load() {
    use std::collections::HashMap;

    // snapshot which env vars were already set before we touch anything
    let pre_set: std::collections::HashSet<String> = MAPPINGS
        .iter()
        .filter(|(_, env_key)| std::env::var(env_key).is_ok())
        .map(|(_, env_key)| env_key.to_string())
        .collect();

    let mut merged: HashMap<&'static str, String> = HashMap::new();

    // global config — lowest priority
    if let Some(home) = dirs::home_dir() {
        let path = home.join(".config/wiki/config.toml");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(table) = content.parse::<toml::Table>() {
                merged.extend(table_to_map(&table));
            }
        }
    }

    // local .wiki/config.toml — overrides global
    if let Some(local_path) = find_local_config() {
        if let Ok(content) = std::fs::read_to_string(&local_path) {
            if let Ok(table) = content.parse::<toml::Table>() {
                merged.extend(table_to_map(&table));
            }
        }
    }

    // apply merged values — env vars set before load() always win
    for (env_key, val) in merged {
        if !pre_set.contains(env_key) {
            unsafe { std::env::set_var(env_key, val) };
        }
    }
}
