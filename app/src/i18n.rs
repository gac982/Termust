//! Resolución del idioma de la app y re-export de las traducciones.

use gtk4 as gtk;
use gtk4::gio::prelude::SettingsExt;

#[allow(unused_imports)]
pub use termust_i18n::{ntr, tr, tr_args};

/// Inicializa gettext. Precedencia: `TERMUST_LANG` > GSettings `language` >
/// locale del sistema.
pub fn init() {
    let locale = std::env::var("TERMUST_LANG")
        .ok()
        .and_then(|value| locale_for_setting(&value).map(str::to_string))
        .or_else(|| {
            stored_language().and_then(|value| locale_for_setting(&value).map(str::to_string))
        });
    termust_i18n::init(locale.as_deref());
}

fn stored_language() -> Option<String> {
    let source = gtk::gio::SettingsSchemaSource::default()?;
    let schema = source.lookup(termust_core::APP_ID, true)?;
    if !schema.has_key("language") {
        return None;
    }
    let settings = gtk::gio::Settings::new(termust_core::APP_ID);
    Some(settings.string("language").to_string())
}

pub(crate) fn locale_for_setting(value: &str) -> Option<&'static str> {
    match value {
        "en" => Some("en"),
        "es" => Some("es"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::locale_for_setting;

    #[test]
    fn maps_language_setting() {
        assert_eq!(locale_for_setting("en"), Some("en"));
        assert_eq!(locale_for_setting("es"), Some("es"));
        assert_eq!(locale_for_setting("system"), None);
        assert_eq!(locale_for_setting("fr"), None);
    }
}
