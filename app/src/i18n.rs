//! Traducciones de la UI (es fuente, en traducido).

use gettextrs::{bindtextdomain, gettext, setlocale, textdomain, LocaleCategory};
use std::path::PathBuf;

pub fn init() {
    // SAFETY: se llama una sola vez al arrancar, antes de crear hilos.
    let _ = unsafe { setlocale(LocaleCategory::LcAll, "") };
    let dir = std::env::var_os("TERMUST_LOCALE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/locale");
            local.is_dir().then_some(local)
        })
        .unwrap_or_else(|| PathBuf::from("/app/share/locale"));
    let _ = bindtextdomain("termust", dir);
    let _ = textdomain("termust");
}

pub fn tr(message: &str) -> String {
    gettext(message)
}
