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
