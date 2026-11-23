use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn tracked_files() -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(repo_root())
        .output()
        .expect("git ls-files");
    assert!(out.status.success(), "git ls-files failed");
    out.stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).to_string())
        .collect()
}

#[test]
fn no_doc_or_assets_tracked() {
    for f in tracked_files() {
        assert!(
            !f.starts_with("doc/") && !f.starts_with("assets/"),
            "{f} must not be tracked"
        );
    }
}

#[test]
fn markdown_allowlist() {
    let allowed = ["README.md", "README.en.md", "CHANGELOG.md"];
    for f in tracked_files() {
        if f.ends_with(".md") {
            assert!(
                allowed.contains(&f.as_str()),
                "{f} is Markdown outside the allowlist"
            );
        }
    }
}

#[test]
fn no_large_files_tracked() {
    let root = repo_root();
    for f in tracked_files() {
        let p = root.join(&f);
        if let Ok(meta) = std::fs::metadata(&p) {
            assert!(
                meta.len() <= 5 * 1024 * 1024,
                "{f} is larger than 5 MB ({} bytes)",
                meta.len()
            );
        }
    }
}

#[test]
fn cargo_lock_is_tracked() {
    assert!(
        tracked_files().iter().any(|f| f == "Cargo.lock"),
        "Cargo.lock must be committed"
    );
}
