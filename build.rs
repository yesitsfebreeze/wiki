use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=languages/rs/src");
    println!("cargo:rerun-if-changed=languages/rs/Cargo.toml");
    println!("cargo:rerun-if-changed=languages/py/src");
    println!("cargo:rerun-if-changed=languages/py/Cargo.toml");

    build_language("rs", "split_language_rs");
    build_language("py", "split_language_py");
}

fn build_language(lang: &str, crate_name: &str) {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dst = format!("{out_dir}/{crate_name}.wasm");
    let manifest_str = format!("languages/{lang}/Cargo.toml");
    let manifest = Path::new(&manifest_str);

    for target in ["wasm32-wasip1", "wasm32-wasi"] {
        let ok = Command::new("cargo")
            .args(["build", "--target", target, "--release", "--manifest-path"])
            .arg(manifest)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if ok {
            let src = format!(
                "languages/{lang}/target/{target}/release/{crate_name}.wasm"
            );
            if std::fs::copy(&src, &dst).is_ok() {
                return;
            }
        }
    }

    std::fs::write(&dst, b"").unwrap();
    println!(
        "cargo:warning=wasm32-wasip1 target not found; {lang} language module falls back to native splitter"
    );
    println!("cargo:warning=Install with: rustup target add wasm32-wasip1");
}
