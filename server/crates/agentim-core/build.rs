//! Embeds the built console (`server/console/dist`, docs/12 §4) and demo
//! (`server/demo/dist`, docs/12 §3.4) when they exist; otherwise the
//! server shows a page saying how to get them. The Rust build never needs
//! Node.

use std::path::{Path, PathBuf};

fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk(&p, root, out);
        } else {
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, p));
        }
    }
}

/// Writes `$OUT_DIR/<name>.rs` with `FILES`: every file under `dist`.
fn embed(dist: &Path, name: &str) {
    println!("cargo:rerun-if-changed={}", dist.display());
    let mut files = vec![];
    if dist.join("index.html").is_file() {
        walk(dist, dist, &mut files);
    }
    let mut code = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for (rel, p) in &files {
        println!("cargo:rerun-if-changed={}", p.display());
        code.push_str(&format!(
            "    ({rel:?}, include_bytes!({:?})),\n",
            p.canonicalize().unwrap()
        ));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join(format!("{name}.rs"));
    std::fs::write(out, code).unwrap();
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    embed(&manifest.join("../../console/dist"), "console");
    embed(&manifest.join("../../demo/dist"), "demo_web");
}
