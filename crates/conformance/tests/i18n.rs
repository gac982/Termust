use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn parse_po(path: &Path) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| panic!("falta {}", path.display()));
    let mut entries = BTreeMap::new();
    let mut msgid: Option<String> = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("msgid ") {
            msgid = Some(value.trim().trim_matches('"').replace("\\\"", "\""));
        } else if let Some(value) = line.strip_prefix("msgstr ") {
            if let Some(id) = msgid.take() {
                if !id.is_empty() {
                    entries.insert(id, value.trim().trim_matches('"').replace("\\\"", "\""));
                }
            }
        }
    }
    entries
}

#[test]
fn i18n_catalog_has_spanish_translations() {
    let root = repo_root();
    let es = parse_po(&root.join("data/i18n/es.po"));
    assert!(es.len() >= 116, "catálogo es demasiado chico: {}", es.len());
    assert_eq!(
        es.get("Preferences").map(String::as_str),
        Some("Preferencias")
    );
    assert_eq!(
        es.get("File panel").map(String::as_str),
        Some("Panel de archivos")
    );
    for (id, value) in &es {
        assert!(!value.is_empty(), "traducción vacía para {id}");
        if id != "Español" {
            assert!(
                !id.chars().any(|c| "áéíóúñÁÉÍÓÚÑ¿¡".contains(c)),
                "msgid con acento español: {id}"
            );
        }
    }
    assert!(
        !root.join("data/i18n/en.po").exists(),
        "en.po ya no debe existir: el inglés es el msgid"
    );
}

#[test]
fn gsettings_has_language_key() {
    let root = repo_root();
    let schema =
        std::fs::read_to_string(root.join("data/io.github.gac982.Termust.gschema.xml")).unwrap();
    assert!(schema.contains("<key name=\"language\" type=\"s\">"));
}
