use anyhow::Result;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::code::splitter;

pub fn watch(src_dirs: &[PathBuf], index_dir: &Path, exts: &[String]) -> Result<()> {
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = RecommendedWatcher::new(move |res| { let _ = tx.send(res); }, Config::default())?;

    for src_dir in src_dirs {
        watcher.watch(src_dir, RecursiveMode::Recursive)?;
        eprintln!("split: watching {} ({})", src_dir.display(), exts.join(","));
    }

    let index_dir = index_dir.to_path_buf();
    let ext_set: HashSet<String> = exts.iter().cloned().collect();

    for res in rx {
        match res {
            Ok(event) => {
                let is_write = matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_));
                let is_remove = matches!(event.kind, EventKind::Remove(_));
                if !is_write && !is_remove {
                    continue;
                }
                for path in event.paths {
                    let path_ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
                    let s = path.to_string_lossy();
                    if !ext_set.contains(&path_ext) || s.contains(".wiki/") || s.contains(".wiki\\") {
                        continue;
                    }
                    if is_remove {
                        if let Err(e) = on_source_delete(&path, &index_dir) {
                            eprintln!("split delete error: {e}");
                        }
                    } else if let Err(e) = on_source_change(&path, &index_dir, &path_ext) {
                        eprintln!("split error: {e}");
                    }
                }
            }
            Err(e) => eprintln!("watch error: {e}"),
        }
    }

    Ok(())
}

fn on_source_change(src_path: &Path, index_dir: &Path, ext: &str) -> Result<()> {
    let struct_path = splitter::structure_path(src_path, index_dir);
    let (structure, bodies) = splitter::split_for_ext(src_path, index_dir, ext)?;
    if let Some(p) = struct_path.parent() { std::fs::create_dir_all(p)?; }
    std::fs::write(&struct_path, &structure)?;
    for b in &bodies {
        if let Some(p) = b.path.parent() { std::fs::create_dir_all(p).ok(); }
        std::fs::write(&b.path, &b.content)?;
    }
    eprintln!("re-split <- {}", src_path.display());
    Ok(())
}

fn on_source_delete(src_path: &Path, index_dir: &Path) -> Result<()> {
    let struct_path = splitter::structure_path(src_path, index_dir);
    if struct_path.exists() {
        std::fs::remove_file(&struct_path)?;
    }
    let ext = src_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let key = splitter::source_key_path(src_path).with_extension("");
    let body_dir = index_dir.join(ext).join("functions").join(&key);
    if body_dir.exists() {
        std::fs::remove_dir_all(&body_dir)?;
    }
    eprintln!("deleted index <- {}", src_path.display());
    Ok(())
}
