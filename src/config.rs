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

pub fn load() {
    let Some(home) = dirs::home_dir() else { return };
    let path = home.join(".config/wiki/config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else { return };
    let Ok(table) = content.parse::<toml::Table>() else { return };

    // env var takes precedence over config.toml
    let mappings = [
        ("openai_api_key", "OPENAI_API_KEY"),
        ("wiki_rerank_model", "WIKI_RERANK_MODEL"),
        ("wiki_similarity_threshold", "WIKI_SIMILARITY_THRESHOLD"),
        ("wiki_dedupe_threshold", "WIKI_DEDUPE_THRESHOLD"),
        ("wiki_template_question_regexes", "WIKI_TEMPLATE_QUESTION_REGEXES"),
        ("wiki_open_questions_per_purpose_cap", "WIKI_OPEN_QUESTIONS_PER_PURPOSE_CAP"),
        ("split_src_dir", "SPLIT_SRC_DIR"),
        ("split_src_dirs", "SPLIT_SRC_DIRS"),
        ("split_ext", "SPLIT_EXT"),
        ("split_exts", "SPLIT_EXTS"),
        ("split_index_dir", "SPLIT_INDEX_DIR"),
        ("split_max_loc", "SPLIT_MAX_LOC"),
    ];

    for (toml_key, env_key) in &mappings {
        if std::env::var(env_key).is_ok() {
            continue;
        }
        if let Some(val) = table.get(*toml_key) {
            let s = match val {
                toml::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            unsafe { std::env::set_var(env_key, s) };
        }
    }
}
