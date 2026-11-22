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
fn i18n_catalog_has_spanish_and_english() {
    let root = repo_root();
    let es = parse_po(&root.join("data/i18n/es.po"));
    let en = parse_po(&root.join("data/i18n/en.po"));
    assert!(es.len() >= 60, "catálogo es demasiado chico: {}", es.len());
    assert_eq!(
        es.keys().collect::<Vec<_>>(),
        en.keys().collect::<Vec<_>>(),
        "msgids distintos entre es y en"
    );
    for (id, value) in &es {
        assert_eq!(id, value, "es debe devolver el msgid");
    }
    let translated = en.iter().filter(|(id, value)| *id != *value).count();
    assert!(
        translated >= 60,
        "solo {translated} cadenas traducidas al inglés"
    );
    assert_eq!(
        en.get("Preferencias").map(String::as_str),
        Some("Preferences")
    );
    assert_eq!(
        en.get("Panel de archivos").map(String::as_str),
        Some("File panel")
    );
}
