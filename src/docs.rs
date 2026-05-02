use include_dir::{include_dir, Dir, DirEntry};

static DOCS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/docs");

pub fn list() -> Vec<String> {
	let mut out = Vec::new();
	collect(&DOCS, &mut out);
	out.sort();
	out
}

fn collect(dir: &Dir<'_>, out: &mut Vec<String>) {
	for entry in dir.entries() {
		match entry {
			DirEntry::Dir(d) => collect(d, out),
			DirEntry::File(f) => {
				if let Some(name) = name_for(f.path()) {
					out.push(name);
				}
			}
		}
	}
}

fn name_for(path: &std::path::Path) -> Option<String> {
	let s = path.to_str()?;
	let stripped = s.strip_suffix(".md")?;
	Some(stripped.replace('\\', "/"))
}

pub fn read(name: &str) -> Option<&'static str> {
	let key = name.trim_start_matches('/').replace('\\', "/");
	let candidates = [
		format!("{key}.md"),
		format!("tools/{key}.md"),
		format!("concepts/{key}.md"),
	];
	for c in &candidates {
		if let Some(file) = DOCS.get_file(c) {
			return file.contents_utf8();
		}
	}
	None
}
