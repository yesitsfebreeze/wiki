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
    let res = match smart::smart_search(&wiki_path, &prompt, None, 15, 3) {
        Ok(v) => v,
        Err(_) => return emit_empty_hook(),
    };

    let empty: Vec<serde_json::Value> = Vec::new();
    let items = res.get("results").and_then(|v| v.as_array()).unwrap_or(&empty);
    if items.is_empty() { return emit_empty_hook(); }

    let mut md = String::from("## Wiki context (auto-injected, top hits for this prompt)\n");
    for r in items {
        let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let reason = r.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        let tags: Vec<String> = r.get("tags").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let snippet = if content.len() > 600 {
            format!("{}…", &content[..600])
        } else {
            content.to_string()
        };
        md.push_str(&format!(
            "\n### [{}] ({:.2}) — {}\n_id: {} — {}_\n\n{}\n",
            title, score, tags.join(", "), id, reason, snippet
        ));
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
                    "additionalContext": format!("Wiki code index result for `{}`:\n\n{}", file_path, result),
                },
            });
            println!("{}", out);
            Ok(())
        }
        _ => emit_empty_hook(),
    }
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
    if let Ok(src_dir) = std::env::var("SPLIT_SRC_DIR") {
        let src = PathBuf::from(&src_dir);
        let index = code::default_index_dir();
        let ext = std::env::var("SPLIT_EXT").unwrap_or_else(|_| "rs".to_string());
        if src.exists() && index.exists() {
            std::thread::spawn(move || {
                if let Err(e) = code::watcher::watch(&src, &index, &ext) {
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

