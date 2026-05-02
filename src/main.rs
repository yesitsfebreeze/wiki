use rmcp::{transport::stdio, ServiceExt};
use std::path::{Path, PathBuf};

mod cache;
mod chunker;
mod classifier;
mod code;
mod config;
mod docs;
mod http;
mod io;
mod learn;
mod sanitize;
mod search;
mod smart;
mod store;
mod tools;
mod walk;
mod weight;

fn emit_empty_hook() -> anyhow::Result<()> {
    println!("{{}}");
    Ok(())
}

fn find_dotenv_key(start: &Path, key: &str) -> Option<String> {
    let mut dir = Some(start.to_path_buf());
    let prefix = format!("{}=", key);
    while let Some(d) = dir {
        let p = d.join(".env");
        if p.exists() {
            if let Ok(text) = std::fs::read_to_string(&p) {
                for line in text.lines() {
                    if let Some(rest) = line.trim().strip_prefix(&prefix) {
                        let v = rest.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
                        if !v.is_empty() {
                            return Some(v);
                        }
                    }
                }
            }
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    None
}

fn find_wiki_vault(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        let p = d.join(".wiki");
        if p.is_dir() {
            return Some(p);
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    None
}

fn read_stdin_json() -> Option<serde_json::Value> {
    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&buf).ok()
}

fn cwd_from_payload(payload: &serde_json::Value) -> PathBuf {
    let cwd = payload.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    if cwd.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(cwd)
    }
}

/// Resolve wiki vault dir + ensure OPENAI_API_KEY is set. Returns `None` (and
/// the caller should `emit_empty_hook()`) if either is missing.
fn resolve_wiki_and_key(cwd_path: &Path) -> Option<PathBuf> {
    let wiki_path = std::env::var("WIKI_PATH").ok().map(PathBuf::from)
        .or_else(|| find_wiki_vault(cwd_path))?;
    if !wiki_path.is_dir() { return None; }
    std::env::set_var("WIKI_PATH", &wiki_path);

    if std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()).is_none() {
        if let Some(k) = find_dotenv_key(cwd_path, "OPENAI_API_KEY") {
            std::env::set_var("OPENAI_API_KEY", k);
        }
    }
    std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty())?;
    Some(wiki_path)
}

async fn run_hook() -> anyhow::Result<()> {
    config::load();
    let Some(payload) = read_stdin_json() else { return emit_empty_hook(); };
    let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if prompt.len() < 8 || prompt.starts_with('/') || prompt.starts_with('!') {
        return emit_empty_hook();
    }

    let cwd_path = cwd_from_payload(&payload);
    let Some(wiki_path) = resolve_wiki_and_key(&cwd_path) else { return emit_empty_hook(); };

    let _ = store::bootstrap(&wiki_path);

    // Persist prompt for the Stop hook to pick up after the turn ends.
    let state_dir = wiki_path.join(".hook_state");
    let _ = std::fs::create_dir_all(&state_dir);
    let _ = std::fs::write(state_dir.join("pending_prompt.txt"), &prompt);

    // 1. Expand prompt into diverse sub-queries (LLM, falls back to prompt-only on error).
    let n_sub: usize = std::env::var("WIKI_HOOK_SUBQUERIES")
        .ok().and_then(|s| s.parse().ok()).unwrap_or(4);
    let mut queries: Vec<String> = vec![prompt.clone()];
    if n_sub > 0 {
        if let Ok(extra) = smart::expand_questions(&wiki_path, &prompt, n_sub).await {
            for q in extra {
                if !queries.iter().any(|x| x.eq_ignore_ascii_case(&q)) {
                    queries.push(q);
                }
            }
        }
    }

    // 2. Batch-embed all sub-queries in one OpenAI call, then fan out
    //    parallel BM25+vector searches sharing the cached embedding pool.
    use futures::future::join_all;
    let q_embs = http::embed_batch(&queries).await.unwrap_or_default();
    let search_futs = queries.iter().enumerate().map(|(i, q)| {
        let wiki_path = wiki_path.clone();
        let q = q.clone();
        let emb = q_embs.get(i).cloned();
        async move {
            match emb {
                Some(e) => smart::query_with_qemb(&wiki_path, &q, None, 10, 3, &e).await,
                None => smart::query(&wiki_path, &q, None, 10, 3).await,
            }
        }
    });
    let results = join_all(search_futs).await;

    use std::collections::{HashMap, HashSet};
    let mut best: HashMap<String, (f64, serde_json::Value)> = HashMap::new();
    let empty: Vec<serde_json::Value> = Vec::new();
    for res in results.into_iter().flatten() {
        for r in res.get("results").and_then(|v| v.as_array()).unwrap_or(&empty) {
            let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if id.is_empty() { continue; }
            let s = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            best.entry(id)
                .and_modify(|e| { if s > e.0 { *e = (s, r.clone()); } })
                .or_insert((s, r.clone()));
        }
    }
    if best.is_empty() { return emit_empty_hook(); }

    let mut hits: Vec<(f64, serde_json::Value)> = best.into_values().collect();
    hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(10);

    // 3. Walk depth-1 wikilinks from top hit bodies.
    let link_re = regex::Regex::new(r"\[\[([^\]|#]+?)(?:[#|][^\]]*)?\]\]").ok();
    let doc_types = ["entities", "thoughts", "conclusions", "reasons", "questions", "purposes"];
    let primary_ids: HashSet<String> = hits.iter()
        .filter_map(|(_, r)| r.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect();
    let mut linked: Vec<(String, store::Document)> = Vec::new();
    let mut seen_links: HashSet<String> = HashSet::new();
    if let Some(re) = &link_re {
        'outer: for (_, r) in &hits {
            let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
            for cap in re.captures_iter(content) {
                let target = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
                if target.is_empty() { continue; }
                let (dt_opt, id) = match target.split_once('/') {
                    Some((a, b)) => (Some(a), b),
                    None => (None, target),
                };
                let try_types: Vec<&str> = match dt_opt {
                    Some(t) => vec![t],
                    None => doc_types.to_vec(),
                };
                for dt in try_types {
                    if let Ok(doc) = store::get_document(&wiki_path, dt, id) {
                        if !primary_ids.contains(&doc.id) && seen_links.insert(doc.id.clone()) {
                            linked.push((dt.to_string(), doc));
                        }
                        break;
                    }
                }
                if linked.len() >= 8 { break 'outer; }
            }
        }
    }

    // 4. Build markdown.
    let mut md = String::from("## Wiki context (auto-injected)\n\n**Sub-queries:**\n");
    for q in &queries { md.push_str(&format!("- {}\n", q)); }
    md.push_str("\n### Top hits\n");
    for (score, r) in &hits {
        let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let reason = r.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        let tags: Vec<String> = r.get("tags").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let snippet = take_chars(content, 600);
        md.push_str(&format!(
            "\n#### [{}] ({:.2}) — {}\n_id: {} — {}_\n\n{}\n",
            title, score, tags.join(", "), id, reason, snippet
        ));
    }
    if !linked.is_empty() {
        md.push_str("\n### Linked context (depth-1 wikilinks)\n");
        for (dt, d) in &linked {
            let snip = take_chars(&d.content, 400);
            md.push_str(&format!("\n#### [[{}/{}]] — {}\n\n{}\n", dt, d.id, d.title, snip));
        }
    }

    let out = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": md,
        }
    });
    println!("{}", out);
    Ok(())
}

fn take_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max { return s.to_string(); }
    let truncated: String = s.chars().take(max).collect();
    format!("{}…", truncated)
}

fn run_code_read_hook() -> anyhow::Result<()> {
    config::load();
    let Some(payload) = read_stdin_json() else { return emit_empty_hook(); };

    let file_path = payload
        .get("tool_input").and_then(|v| v.get("file_path")).and_then(|v| v.as_str())
        .unwrap_or("");
    if file_path.is_empty() { return emit_empty_hook(); }

    let cwd_path = cwd_from_payload(&payload);
    let abs_path = {
        let p = PathBuf::from(file_path);
        if p.is_absolute() { p } else { cwd_path.join(p) }
    };

    let Some(ext) = abs_path.extension().and_then(|e| e.to_str()).map(str::to_string) else {
        return emit_empty_hook();
    };

    if code::language::load(&ext).is_none() {
        return emit_empty_hook();
    }

    if std::env::var("CODE_INDEX_DIR").is_err() && std::env::var("SPLIT_INDEX_DIR").is_err() {
        let wiki = store::wiki_root();
        std::env::set_var("CODE_INDEX_DIR", wiki.join("code").to_string_lossy().as_ref());
    }

    match code::open_source(&abs_path, &ext) {
        Ok(result) if !result.trim().is_empty() => {
            let out = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "additionalContext": format!("Wiki code index result for `{}`:\n\n{}", file_path, result),
                },
            });
            println!("{}", out);
            Ok(())
        }
        _ => emit_empty_hook(),
    }
}

fn normalize_question(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

async fn run_stop_hook() -> anyhow::Result<()> {
    config::load();
    let payload = read_stdin_json().unwrap_or(serde_json::Value::Null);

    if payload.get("stop_hook_active").and_then(|v| v.as_bool()).unwrap_or(false) {
        return emit_empty_hook();
    }

    let cwd_path = cwd_from_payload(&payload);
    let Some(wiki_path) = resolve_wiki_and_key(&cwd_path) else { return emit_empty_hook(); };

    let pending_path = wiki_path.join(".hook_state").join("pending_prompt.txt");
    let prompt = match std::fs::read_to_string(&pending_path) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return emit_empty_hook(),
    };
    let _ = std::fs::remove_file(&pending_path);
    if prompt.len() < 16 { return emit_empty_hook(); }

    let pn = normalize_question(&prompt);
    if let Ok(existing) = store::list_documents(&wiki_path, "questions") {
        if existing.iter().any(|d| normalize_question(&d.content) == pn) {
            return emit_empty_hook();
        }
    }

    let tags = vec!["question".to_string()];
    let doc = match store::create_document(
        &wiki_path, "questions", "question", &prompt, tags, None, None,
    ) {
        Ok(d) => d,
        Err(_) => return emit_empty_hook(),
    };
    let _ = store::log_ingest(&wiki_path, "questions", &doc.id, &doc.title);
    let _ = learn::link_doc(&wiki_path, "questions", &doc.id, false).await;

    emit_empty_hook()
}

enum CliCmd {
    Search { query: String, tag: Option<String>, k: usize, top_n: usize },
    Hook,
    CodeReadHook,
    StopHook,
    LearnFeedback { limit: usize, dry_run: bool },
    MigrateTemplatedQuestions { dry_run: bool },
    RecomputeWeights { dry_run: bool },
    PurposeCreate { tag: String, title: String, description: String },
    PurposeDelete { tag: String },
    PurposeList,
    PurposeReembed,
    LearnPass { qa: bool, force: bool, limit: usize },
    Link { doc_type: String, id: String },
    IndexCode { src_dir: String, ext: String },
    IndexValidate { fix: bool },
    Sanitize { dry_run: bool },
}

fn parse_cli() -> Option<CliCmd> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 { return None; }
    match args[1].as_str() {
        "search" => {
            if args.len() < 3 {
                eprintln!("usage: wiki search <query> [--tag T] [--k N] [--top-n N]");
                std::process::exit(2);
            }
            let query = args[2].clone();
            let mut tag: Option<String> = None;
            let mut k: usize = 20;
            let mut top_n: usize = 5;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--tag" => { tag = args.get(i + 1).cloned(); i += 2; }
                    "--k" => { k = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(20); i += 2; }
                    "--top-n" => { top_n = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(5); i += 2; }
                    _ => { i += 1; }
                }
            }
            Some(CliCmd::Search { query, tag, k, top_n })
        }
        "hook" => Some(CliCmd::Hook),
        "code-read-hook" => Some(CliCmd::CodeReadHook),
        "stop-hook" => Some(CliCmd::StopHook),
        "learn-feedback" => {
            let mut limit: usize = 25;
            let mut dry_run = false;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--limit" => { limit = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(25); i += 2; }
                    "--dry-run" => { dry_run = true; i += 1; }
                    _ => { i += 1; }
                }
            }
            Some(CliCmd::LearnFeedback { limit, dry_run })
        }
        "migrate-templated-questions" => {
            let mut dry_run = false;
            let mut i = 2;
            while i < args.len() {
                if args[i].as_str() == "--dry-run" { dry_run = true; }
                i += 1;
            }
            Some(CliCmd::MigrateTemplatedQuestions { dry_run })
        }
        "recompute-weights" => {
            let dry_run = args.iter().skip(2).any(|a| a == "--dry-run");
            Some(CliCmd::RecomputeWeights { dry_run })
        }
        "purpose" => {
            let sub = args.get(2).map(String::as_str).unwrap_or("");
            match sub {
                "create" => {
                    let tag = args.get(3).cloned().unwrap_or_default();
                    let title = args.get(4).cloned().unwrap_or_default();
                    let description = args.get(5).cloned().unwrap_or_default();
                    if tag.is_empty() {
                        eprintln!("usage: wiki purpose create <tag> <title> <description>");
                        std::process::exit(2);
                    }
                    Some(CliCmd::PurposeCreate { tag, title, description })
                }
                "delete" => {
                    let tag = args.get(3).cloned().unwrap_or_default();
                    if tag.is_empty() {
                        eprintln!("usage: wiki purpose delete <tag>");
                        std::process::exit(2);
                    }
                    Some(CliCmd::PurposeDelete { tag })
                }
                "list" => Some(CliCmd::PurposeList),
                "reembed" => Some(CliCmd::PurposeReembed),
                _ => {
                    eprintln!("usage: wiki purpose create|delete|list|reembed");
                    std::process::exit(2);
                }
            }
        }
        "learn" => {
            let sub = args.get(2).map(String::as_str).unwrap_or("");
            match sub {
                "pass" => {
                    let mut qa = false;
                    let mut force = false;
                    let mut limit: usize = 25;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--qa" => { qa = true; i += 1; }
                            "--force" => { force = true; i += 1; }
                            "--limit" => { limit = args.get(i+1).and_then(|s| s.parse().ok()).unwrap_or(25); i += 2; }
                            _ => i += 1,
                        }
                    }
                    Some(CliCmd::LearnPass { qa, force, limit })
                }
                "feedback" => {
                    let mut limit: usize = 25;
                    let mut dry_run = false;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--limit" => { limit = args.get(i+1).and_then(|s| s.parse().ok()).unwrap_or(25); i += 2; }
                            "--dry-run" => { dry_run = true; i += 1; }
                            _ => i += 1,
                        }
                    }
                    Some(CliCmd::LearnFeedback { limit, dry_run })
                }
                _ => {
                    eprintln!("usage: wiki learn pass|feedback [...]");
                    std::process::exit(2);
                }
            }
        }
        "link" => {
            let doc_type = args.get(2).cloned().unwrap_or_default();
            let id = args.get(3).cloned().unwrap_or_default();
            if doc_type.is_empty() || id.is_empty() {
                eprintln!("usage: wiki link <doc_type> <id>");
                std::process::exit(2);
            }
            Some(CliCmd::Link { doc_type, id })
        }
        "index" => {
            let sub = args.get(2).map(String::as_str).unwrap_or("");
            match sub {
                "code" => {
                    let src_dir = args.get(3).cloned().unwrap_or_default();
                    if src_dir.is_empty() {
                        eprintln!("usage: wiki index code <src_dir> [--ext rs]");
                        std::process::exit(2);
                    }
                    let mut ext = "rs".to_string();
                    let mut i = 4;
                    while i < args.len() {
                        if args[i] == "--ext" { ext = args.get(i+1).cloned().unwrap_or(ext); i += 2; }
                        else { i += 1; }
                    }
                    Some(CliCmd::IndexCode { src_dir, ext })
                }
                "validate" => {
                    let fix = args.iter().skip(3).any(|a| a == "--fix");
                    Some(CliCmd::IndexValidate { fix })
                }
                _ => {
                    eprintln!("usage: wiki index code|validate [...]");
                    std::process::exit(2);
                }
            }
        }
        "sanitize" => {
            let dry_run = args.iter().skip(2).any(|a| a == "--dry-run");
            Some(CliCmd::Sanitize { dry_run })
        }
        _ => None,
    }
}

async fn dispatch_cli(cmd: CliCmd) -> anyhow::Result<()> {
    match cmd {
        CliCmd::Search { query, tag, k, top_n } => {
            let root = store::wiki_root();
            store::bootstrap(&root)?;
            match smart::query(&root, &query, tag.as_deref(), k, top_n).await {
                Ok(v) => { println!("{}", v); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::Hook => run_hook().await,
        CliCmd::CodeReadHook => run_code_read_hook(),
        CliCmd::StopHook => run_stop_hook().await,
        CliCmd::LearnFeedback { limit, dry_run } => {
            let root = store::wiki_root();
            store::bootstrap(&root)?;
            match learn::run_feedback_pass(&root, limit, dry_run).await {
                Ok(v) => { println!("{}", v); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::MigrateTemplatedQuestions { dry_run } => {
            let root = store::wiki_root();
            store::bootstrap(&root)?;
            match learn::migrate_templated_questions(&root, dry_run) {
                Ok(report) => {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                    eprintln!(
                        "migrate-templated-questions: scanned={} templated={} deleted={} dry_run={}",
                        report.scanned, report.templated, report.deleted, dry_run,
                    );
                    Ok(())
                }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::RecomputeWeights { dry_run } => {
            let root = store::wiki_root();
            store::bootstrap(&root)?;
            match weight::run_cli(&root, dry_run) {
                Ok(n) => {
                    eprintln!("{} doc(s) {}", n, if dry_run { "would update" } else { "updated" });
                    Ok(())
                }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::PurposeCreate { tag, title, description } => {
            let root = store::wiki_root();
            store::bootstrap(&root)?;
            match store::create_purpose(&root, &tag, &title, &description) {
                Ok(p) => { println!("{}", serde_json::to_string_pretty(&p)?); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::PurposeDelete { tag } => {
            let root = store::wiki_root();
            store::bootstrap(&root)?;
            match store::delete_purpose(&root, &tag) {
                Ok(_) => { eprintln!("Purpose '{}' deleted", tag); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::PurposeList => {
            let root = store::wiki_root();
            store::bootstrap(&root)?;
            match store::list_purposes(&root) {
                Ok(p) => { println!("{}", serde_json::to_string_pretty(&p)?); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::PurposeReembed => {
            let root = store::wiki_root();
            store::bootstrap(&root)?;
            if let Ok(purposes) = store::list_purposes(&root) {
                for p in &purposes {
                    let _ = std::fs::remove_file(p.path.with_extension("vec"));
                }
            }
            match classifier::ensure_purpose_embeddings(&root).await {
                Ok(v) => { eprintln!("Re-embedded {} purposes", v.len()); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::LearnPass { qa, force, limit } => {
            let root = store::wiki_root();
            store::bootstrap(&root)?;
            let cfg = learn::PassConfig::default();
            match learn::run_pass(&root, limit, None, false, qa, force, &cfg).await {
                Ok(v) => { println!("{}", v); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::Link { doc_type, id } => {
            let root = store::wiki_root();
            store::bootstrap(&root)?;
            match learn::link_doc(&root, &doc_type, &id, false).await {
                Ok(v) => { println!("{}", v); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::IndexCode { src_dir, ext } => {
            match code::index_dir(&PathBuf::from(src_dir), &ext) {
                Ok(s) => { println!("{}", s); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::IndexValidate { fix } => {
            match code::validate(fix) {
                Ok(s) => { println!("{}", s); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
        CliCmd::Sanitize { dry_run } => {
            let root = store::wiki_root();
            store::bootstrap(&root)?;
            match sanitize::sanitize_vault(&root, dry_run) {
                Ok(report) => {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                    let total_links: usize = report.link_rewrites.iter().map(|(_, n)| n).sum();
                    eprintln!(
                        "sanitize: renamed={} link_rewrites={} (across {} file(s)) skipped={} dry_run={}",
                        report.renamed.len(),
                        total_links,
                        report.link_rewrites.len(),
                        report.skipped.len(),
                        dry_run,
                    );
                    Ok(())
                }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            }
        }
    }
}

fn spawn_code_watcher() {
    let mut src_dirs: Vec<PathBuf> = std::env::var("SPLIT_SRC_DIRS")
        .into_iter()
        .flat_map(|s| s.split(';').map(|p| PathBuf::from(p.trim())).collect::<Vec<_>>())
        .chain(std::env::var("SPLIT_SRC_DIR").ok().map(PathBuf::from))
        .filter(|p| p.exists())
        .collect();
    src_dirs.dedup();

    let exts: Vec<String> = std::env::var("SPLIT_EXTS")
        .ok()
        .map(|s| s.split(',').map(|e| e.trim().to_string()).filter(|e| !e.is_empty()).collect())
        .or_else(|| std::env::var("SPLIT_EXT").ok().map(|e| vec![e]))
        .unwrap_or_else(|| code::language::list().into_iter().map(|(ext, _)| ext).collect());

    let index = code::default_index_dir();
    if src_dirs.is_empty() || exts.is_empty() { return; }
    std::thread::spawn(move || {
        if let Err(e) = code::watcher::watch(&src_dirs, &index, &exts) {
            eprintln!("watcher: {e}");
        }
    });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    config::load();
    if let Some(cmd) = parse_cli() {
        return dispatch_cli(cmd).await;
    }
    spawn_code_watcher();

    let service = tools::WikiService::new()?;
    let server: rmcp::service::RunningService<_, _> = service.serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}
