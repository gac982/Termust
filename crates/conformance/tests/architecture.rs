use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const ADAPTERS: &[&str] = &[
    "termust-render-cairo",
    "termust-render-gl",
    "termust-term-engine",
    "termust-pty-unix",
    "termust-fs-local",
    "termust-git-local",
    "termust-git-remote",
    "termust-vault",
    "termust-store",
    "termust-host-exec",
    "termust-agents",
    "termust-syntax",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn metadata() -> Value {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(repo_root())
        .output()
        .expect("cargo metadata");
    assert!(out.status.success(), "cargo metadata failed");
    serde_json::from_slice(&out.stdout).expect("metadata json")
}

fn package<'a>(meta: &'a Value, name: &str) -> &'a Value {
    meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|p| p["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("package {name} not found"))
}

fn normal_deps(meta: &Value, name: &str) -> Vec<String> {
    package(meta, name)["dependencies"]
        .as_array()
        .expect("deps")
        .iter()
        .filter(|d| d["kind"].is_null())
        .map(|d| d["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn core_has_no_gtk_io_or_adapter_deps() {
    let meta = metadata();
    let deps = normal_deps(&meta, "termust-core");
    for d in &deps {
        assert!(
            !matches!(
                d.as_str(),
                "gtk4" | "libadwaita" | "glib" | "gio" | "tokio" | "git2" | "rusqlite"
            ),
            "termust-core must not depend on {d}"
        );
        assert!(
            !d.starts_with("termust-"),
            "termust-core must not depend on adapter {d}"
        );
    }
}

#[test]
fn adapters_do_not_depend_on_ui() {
    let meta = metadata();
    for a in ADAPTERS {
        let deps = normal_deps(&meta, a);
        assert!(
            !deps.iter().any(|d| d == "termust-ui"),
            "{a} must not depend on termust-ui"
        );
    }
}

#[test]
fn ui_depends_only_on_core_text_render_and_gtk() {
    let meta = metadata();
    for d in normal_deps(&meta, "termust-ui") {
        if d.starts_with("termust-") {
            assert!(
                matches!(d.as_str(), "termust-core" | "termust-text-render"),
                "termust-ui must not depend on {d}"
            );
        }
    }
}

#[test]
fn only_app_knows_adapters() {
    let meta = metadata();
    let packages = meta["packages"].as_array().expect("packages");
    for a in ADAPTERS {
        for p in packages {
            let name = p["name"].as_str().unwrap_or_default();
            if name == *a || name == "termust-app" {
                continue;
            }
            let deps = normal_deps(&meta, name);
            assert!(
                !deps.iter().any(|d| d == a),
                "{name} must not depend on adapter {a}; only termust-app may"
            );
        }
    }
}

#[test]
fn adapters_do_not_depend_on_other_adapters() {
    let meta = metadata();
    for a in ADAPTERS {
        for d in normal_deps(&meta, a) {
            assert!(
                !ADAPTERS.contains(&d.as_str()),
                "{a} must not depend on adapter {d}"
            );
        }
    }
}

#[test]
fn pure_crates_forbid_unsafe() {
    let root = repo_root();
    for dir in ["core", "ui", "store", "vault", "agents", "syntax"] {
        let src = root.join("crates").join(dir).join("src/lib.rs");
        let text = std::fs::read_to_string(&src).unwrap_or_default();
        assert!(
            text.contains("#![forbid(unsafe_code)]"),
            "{dir} must contain #![forbid(unsafe_code)]"
        );
    }
}

#[test]
fn no_crate_reads_doc_or_assets() {
    let root = repo_root();
    for entry in walk(&root.join("crates")) {
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        assert!(
            !text.contains("\"doc/") && !text.contains("\"assets/"),
            "{} references doc/ or assets/",
            entry.display()
        );
    }
    for entry in walk(&root.join("app")) {
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        assert!(
            !text.contains("\"doc/") && !text.contains("\"assets/"),
            "{} references doc/ or assets/",
            entry.display()
        );
    }
}

#[test]
fn only_host_exec_spawns_flatpak() {
    let root = repo_root();
    for entry in walk(&root.join("crates"))
        .into_iter()
        .chain(walk(&root.join("app")))
    {
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        if text.contains("flatpak-spawn") {
            let path = entry.to_string_lossy();
            assert!(
                path.contains("host-exec"),
                "{} contains flatpak-spawn; only host-exec may",
                entry.display()
            );
        }
    }
}

#[test]
fn viewer_and_syntax_have_no_write_path() {
    let root = repo_root();
    let mut files = walk(&root.join("crates/syntax/src"));
    files.push(root.join("app/src/viewer.rs"));
    const FORBIDDEN: &[&str] = &[
        "fs::write",
        "File::create",
        "OpenOptions",
        "fs::remove_file",
        "fs::rename",
        "set_len(",
        ".write(true)",
    ];
    for entry in files {
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        let production = text.split("#[cfg(test)]").next().unwrap_or("");
        for token in FORBIDDEN {
            assert!(
                !production.contains(token),
                "{} contiene {token}; el visor es solo lectura",
                entry.display()
            );
        }
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name != "tests" && name != "target" {
                    stack.push(p);
                }
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }
    out
}
