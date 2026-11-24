//! Traducciones compartidas de la UI.
//!
//! El inglés es el idioma fuente: los `msgid` son inglés y `data/i18n/es.po`
//! contiene la traducción al español. Cualquier locale sin catálogo cae al
//! `msgid`, es decir, a inglés.

use gettextrs::{bindtextdomain, gettext, ngettext, setlocale, textdomain, LocaleCategory};
use std::path::PathBuf;

/// Inicializa gettext. `locale` fuerza el idioma (`en`, `es`); `None` usa el
/// locale del sistema.
pub fn init(locale: Option<&str>) {
    if let Some(locale) = locale {
        std::env::set_var("LANGUAGE", locale);
    }

    // SAFETY: se llama una sola vez al arrancar, antes de crear la UI.
    let _ = unsafe { setlocale(LocaleCategory::LcAll, "") };

    let dir = std::env::var_os("TERMUST_LOCALE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/locale");
            local.is_dir().then_some(local)
        })
        .unwrap_or_else(|| PathBuf::from("/app/share/locale"));

    let _ = bindtextdomain("termust", dir);
    let _ = textdomain("termust");
}

pub fn tr(message: &str) -> String {
    gettext(message)
}

pub fn ntr(singular: &str, plural: &str, n: u32) -> String {
    ngettext(singular, plural, n)
}

/// Traduce un mensaje con placeholders `{clave}` y los reemplaza.
pub fn tr_args(message: &str, args: &[(&str, &str)]) -> String {
    let mut out = gettext(message);
    for (key, value) in args {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}
