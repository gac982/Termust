use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

const APP_ID: &str = "io.github.termust.Termust";

#[test]
fn app_id_is_consistent_everywhere() {
    let root = repo_root();

    let core = std::fs::read_to_string(root.join("crates/core/src/lib.rs")).expect("core lib.rs");
    assert!(
        core.contains(&format!("pub const APP_ID: &str = \"{APP_ID}\";")),
        "termust-core must define APP_ID = {APP_ID}"
    );

    for rel in [
        "data/io.github.termust.Termust.desktop",
        "data/io.github.termust.Termust.metainfo.xml",
        "data/io.github.termust.Termust.gschema.xml",
        "flatpak/io.github.termust.Termust.yml",
    ] {
        let text =
            std::fs::read_to_string(root.join(rel)).unwrap_or_else(|_| panic!("missing {rel}"));
        assert!(text.contains(APP_ID), "{rel} must contain {APP_ID}");
    }
}

#[test]
fn desktop_file_launches_termust() {
    let root = repo_root();
    let desktop =
        std::fs::read_to_string(root.join("data/io.github.termust.Termust.desktop")).unwrap();
    assert!(desktop.lines().any(|l| l == "Exec=termust"));
    assert!(desktop
        .lines()
        .any(|l| l == "Icon=io.github.termust.Termust"));
    assert!(desktop.lines().any(|l| l == "Terminal=false"));
}

#[test]
fn manifest_uses_gnome_51_and_installs_binary() {
    let root = repo_root();
    let manifest =
        std::fs::read_to_string(root.join("flatpak/io.github.termust.Termust.yml")).unwrap();
    assert!(manifest.contains("runtime-version: \"51\""));
    assert!(manifest.contains("command: termust"));
    assert!(manifest.contains("target/release/termust"));
    assert!(manifest.contains("--filesystem=/tmp"));
}
