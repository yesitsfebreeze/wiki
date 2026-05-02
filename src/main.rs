use rmcp::{transport::stdio, ServiceExt};
use std::path::PathBuf;

mod chunker;
mod config;
mod classifier;
mod code;
mod extract;
mod learn;
mod search;
mod smart;
mod store;
mod tools;

fn emit_empty_hook() -> anyhow::Result<()> {
    println!("{{}}");
    Ok(())
}

fn find_dotenv_key(start: &std::path::Path, key: &str) -> Option<String> {
    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        let p = d.join(".env");
        if p.exists() {
            if let Ok(text) = std::fs::read_to_string(&p) {
                for line in text.lines() {
                    let line = line.trim();
                    if let Some(rest) = line.strip_prefix(&format!("{}=", key)) {
                        let v = rest.trim().trim_matches('"').trim_matches('\'').to_string();
                        if !v.is_empty() {
                            return Some(v);
                        }
                    }
                }
            }
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

fn find_wiki_vault(start: &std::path::Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        let p = d.join(".wiki");
        if p.is_dir() {
            return Some(p);
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

fn run_hook() -> anyhow::Result<()> {
    config::load();
    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return emit_empty_hook();
    }
    let payload: serde_json::Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(_) => return emit_empty_hook(),
    };
    let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let cwd = payload.get("cwd").and_then(|v| v.as_str()).unwrap_or("").to_string();

    if prompt.len() < 8 { return emit_empty_hook(); }
    if prompt.starts_with('/') || prompt.starts_with('!') { return emit_empty_hook(); }

    let cwd_path = if cwd.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(&cwd)
    };

    // Resolve wiki path: env override > walk up cwd for .wiki
    let wiki_path = std::env::var("WIKI_PATH").ok().map(PathBuf::from)
        .or_else(|| find_wiki_vault(&cwd_path));
    let Some(wiki_path) = wiki_path else { return emit_empty_hook(); };
    if !wiki_path.is_dir() { return emit_empty_hook(); }
    std::env::set_var("WIKI_PATH", &wiki_path);

    // Resolve OpenAI key: env > .env walk-up
    if std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()).is_none() {
        if let Some(k) = find_dotenv_key(&cwd_path, "OPENAI_API_KEY") {
            std::env::set_var("OPENAI_API_KEY", k);
        }
    }
    if std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()).is_none() {
        return emit_empty_hook();
    }

    let _ = store::ensure_wiki_layout(&wiki_path);

    // Persist prompt for the Stop hook to pick up after the turn ends.
    let state_dir = wiki_path.join(".hook_state");
    let _ = std::fs::create_dir_all(&state_dir);
    let _ = std::fs::write(state_dir.join("pending_prompt.txt"), &prompt);

    // 1. Expand prompt into diverse sub-queries (LLM, falls back to prompt-only on error)
    let n_sub: usize = std::env::var("WIKI_HOOK_SUBQUERIES")
        .ok().and_then(|s| s.parse().ok()).unwrap_or(4);
    let mut queries: Vec<String> = vec![prompt.clone()];
    if n_sub > 0 {
        if let Ok(extra) = smart::expand_questions(&prompt, n_sub) {
            for q in extra {
                if !queries.iter().any(|x| x.eq_ignore_ascii_case(&q)) {
                    queries.push(q);
                }
            }
        }
    }

    // 2. Search each, fuse by best score per doc id
    use std::collections::{HashMap, HashSet};
    let mut best: HashMap<String, (f64, serde_json::Value)> = HashMap::new();
    for q in &queries {
        let res = match smart::smart_search(&wiki_path, q, None, 10, 3) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let empty: Vec<serde_json::Value> = Vec::new();
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

    // 3. Walk depth-1 wikilinks from top hit bodies
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

    // 4. Build markdown
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
    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return emit_empty_hook();
    }
    let payload: serde_json::Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(_) => return emit_empty_hook(),
    };

    let file_path = payload
        .get("tool_input").and_then(|v| v.get("file_path")).and_then(|v| v.as_str())
        .unwrap_or("");
    if file_path.is_empty() { return emit_empty_hook(); }

    let cwd = payload.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    let cwd_path = if cwd.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(cwd)
    };

    let abs_path = {
        let p = PathBuf::from(file_path);
        if p.is_absolute() { p } else { cwd_path.join(p) }
    };

    let ext = match abs_path.extension().and_then(|e| e.to_str()) {
        Some(e) => e.to_string(),
        None => return emit_empty_hook(),
    };

    if code::language::load(&ext).is_none() {
        return emit_empty_hook();
    }

    // Ensure code index dir env is set so open_source finds the index
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

fn run_stop_hook() -> anyhow::Result<()> {
    config::load();
    use std::io::Read;
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    let payload: serde_json::Value = serde_json::from_str(&buf).unwrap_or(serde_json::Value::Null);

    // Avoid recursion: if a previous Stop hook already fired this turn, bail.
    if payload.get("stop_hook_active").and_then(|v| v.as_bool()).unwrap_or(false) {
        return emit_empty_hook();
    }

    let cwd = payload.get("cwd").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let cwd_path = if cwd.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(&cwd)
    };

    let wiki_path = std::env::var("WIKI_PATH").ok().map(PathBuf::from)
        .or_else(|| find_wiki_vault(&cwd_path));
    let Some(wiki_path) = wiki_path else { return emit_empty_hook(); };
    if !wiki_path.is_dir() { return emit_empty_hook(); }
    std::env::set_var("WIKI_PATH", &wiki_path);

    if std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()).is_none() {
        if let Some(k) = find_dotenv_key(&cwd_path, "OPENAI_API_KEY") {
            std::env::set_var("OPENAI_API_KEY", k);
        }
    }
    if std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()).is_none() {
        return emit_empty_hook();
    }

    let pending_path = wiki_path.join(".hook_state").join("pending_prompt.txt");
    let prompt = match std::fs::read_to_string(&pending_path) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return emit_empty_hook(),
    };
    let _ = std::fs::remove_file(&pending_path);
    if prompt.len() < 16 { return emit_empty_hook(); }

    // Dedupe against existing question docs by normalized content.
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

    // Link entity mentions in the new question (replaces bare names with [[wikilinks]]).
    let _ = learn::link_doc(&wiki_path, "questions", &doc.id, false);

    emit_empty_hook()
}

fn run_cli() -> Option<anyhow::Result<()>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        return None;
    }
    match args[1].as_str() {
        "search" => {
            if args.len() < 3 {
                eprintln!("usage: wiki search <query> [--tag T] [--k N] [--top-n N]");
                std::process::exit(2);
            }
            let query = &args[2];
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
            let root = store::wiki_root();
            let _ = store::ensure_wiki_layout(&root);
            return Some(match smart::smart_search(&root, query, tag.as_deref(), k, top_n) {
                Ok(v) => { println!("{}", v); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            });
        }
        "hook" => {
            return Some(run_hook());
        }
        "code-read-hook" => {
            return Some(run_code_read_hook());
        }
        "stop-hook" => {
            return Some(run_stop_hook());
        }
        "migrate-layout" => {
            let root = store::wiki_root();
            let _ = store::ensure_wiki_layout(&root);
            return Some(match store::migrate_layout(&root) {
                Ok(v) => { println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            });
        }
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
            let root = store::wiki_root();
            let _ = store::ensure_wiki_layout(&root);
            return Some(match learn::run_feedback_pass(&root, limit, dry_run) {
                Ok(v) => { println!("{}", v); Ok(()) }
                Err(e) => { eprintln!("error: {}", e); std::process::exit(1); }
            });
        }
        _ => None,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    config::load();
    if let Some(r) = run_cli() {
        return r;
    }
    {
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
        if !src_dirs.is_empty() && !exts.is_empty() {
            std::thread::spawn(move || {
                if let Err(e) = code::watcher::watch(&src_dirs, &index, &exts) {
                    eprintln!("watcher: {e}");
                }
            });
        }
    }

    let service = tools::WikiService::new()?;
    let server: rmcp::service::RunningService<_, _> = service.serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}

