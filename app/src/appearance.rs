//! Catálogo de temas, preferencias de apariencia y su aplicación a las vistas.

use gtk::gio;
use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::{Rc, Weak};
use termust_core::appearance::{clamp_font_scale, clamp_opacity, zoom_scale};
use termust_core::terminal::Rgb;
use termust_core::theme::{
    builtin_themes, contrast_ratio, parse_themes, resolve_theme, Theme, ThemeSelection,
    ThemeVariant, AUTO_THEME as CORE_AUTO_THEME,
};
use termust_text_render::{FontSpec, Theme as TextTheme};

pub const AUTO_THEME: &str = CORE_AUTO_THEME;

const KEY_THEME: &str = "theme";
const KEY_THEME_DARK: &str = "theme-dark";
const KEY_THEME_LIGHT: &str = "theme-light";
const KEY_OPACITY: &str = "opacity";
const KEY_FONT_SCALE: &str = "font-scale";

/// Estado en memoria usado cuando el esquema GSettings no está instalado
/// (por ejemplo, ejecutando el binario desde `target/` sin Flatpak).
struct Memory {
    theme: String,
    dark: String,
    light: String,
    opacity: i32,
    font_scale: f64,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            theme: AUTO_THEME.to_string(),
            dark: "termust-dark".to_string(),
            light: "catppuccin-latte".to_string(),
            opacity: 0,
            font_scale: 1.0,
        }
    }
}

pub struct Appearance {
    settings: Option<gio::Settings>,
    memory: RefCell<Memory>,
    themes: Vec<Theme>,
    import_errors: Vec<String>,
    base_font: FontSpec,
    system_dark: Cell<bool>,
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
}

impl Appearance {
    pub fn load(home: &Path, base_font: FontSpec) -> Rc<Self> {
        let mut themes = builtin_themes().unwrap_or_default();
        let mut import_errors = Vec::new();
        let dir = home.join(".config/termust/themes");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut files: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
            files.sort();
            for path in files {
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                match parse_themes(&text) {
                    Ok(mut imported) => {
                        for theme in imported.drain(..) {
                            themes.retain(|existing| existing.id != theme.id);
                            themes.push(theme);
                        }
                    }
                    Err(error) => import_errors.push(format!("{}: {error}", path.display())),
                }
            }
        }
        if themes.is_empty() {
            themes.push(fallback_theme());
        }
        let settings = gio::SettingsSchemaSource::default()
            .filter(|source| source.lookup(termust_core::APP_ID, true).is_some())
            .map(|_| gio::Settings::new(termust_core::APP_ID));
        let appearance = Rc::new(Self {
            settings,
            memory: RefCell::new(Memory::default()),
            themes,
            import_errors,
            base_font,
            system_dark: Cell::new(libadwaita::StyleManager::default().is_dark()),
            listeners: RefCell::new(Vec::new()),
        });
        appearance.normalize();
        appearance.watch();
        appearance
    }

    fn watch(self: &Rc<Self>) {
        let weak: Weak<Self> = Rc::downgrade(self);
        if let Some(settings) = &self.settings {
            for key in [
                KEY_THEME,
                KEY_THEME_DARK,
                KEY_THEME_LIGHT,
                KEY_OPACITY,
                KEY_FONT_SCALE,
            ] {
                let weak = weak.clone();
                settings.connect_changed(Some(key), move |_, _| {
                    if let Some(this) = weak.upgrade() {
                        this.normalize();
                        this.notify();
                    }
                });
            }
        }
        let weak = weak.clone();
        libadwaita::StyleManager::default().connect_dark_notify(move |manager| {
            if let Some(this) = weak.upgrade() {
                this.system_dark.set(manager.is_dark());
                this.notify();
            }
        });
    }

    /// Ajusta ids desconocidos y valores fuera de rango.
    fn normalize(&self) {
        if self.theme_id() != AUTO_THEME && self.theme(&self.theme_id()).is_none() {
            self.write_string(KEY_THEME, AUTO_THEME);
        }
        if self.theme(&self.dark_id()).is_none() {
            self.write_string(KEY_THEME_DARK, self.default_dark().id.as_str());
        }
        if self.theme(&self.light_id()).is_none() {
            self.write_string(KEY_THEME_LIGHT, self.default_light().id.as_str());
        }
        let opacity = clamp_opacity(self.opacity());
        if opacity != self.opacity() {
            self.write_int(KEY_OPACITY, opacity);
        }
        let scale = clamp_font_scale(self.font_scale());
        if (scale - self.font_scale()).abs() > f64::EPSILON {
            self.write_double(KEY_FONT_SCALE, scale);
        }
    }

    pub fn on_change(&self, callback: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Rc::new(callback));
    }

    fn notify(&self) {
        let callbacks: Vec<Rc<dyn Fn()>> = self.listeners.borrow().iter().cloned().collect();
        for callback in callbacks {
            callback();
        }
    }

    fn has_key(&self, key: &str) -> bool {
        self.settings
            .as_ref()
            .and_then(|settings| settings.settings_schema())
            .map(|schema| schema.has_key(key))
            .unwrap_or(false)
    }

    fn read_string(&self, key: &str) -> Option<String> {
        match &self.settings {
            Some(settings) if self.has_key(key) => Some(settings.string(key).to_string()),
            Some(_) => None,
            None => {
                let memory = self.memory.borrow();
                Some(match key {
                    KEY_THEME => memory.theme.clone(),
                    KEY_THEME_DARK => memory.dark.clone(),
                    KEY_THEME_LIGHT => memory.light.clone(),
                    _ => return None,
                })
            }
        }
    }

    fn write_string(&self, key: &str, value: &str) {
        match &self.settings {
            Some(settings) if self.has_key(key) => {
                let _ = settings.set_string(key, value);
            }
            Some(_) => {}
            None => {
                let mut memory = self.memory.borrow_mut();
                match key {
                    KEY_THEME => memory.theme = value.to_string(),
                    KEY_THEME_DARK => memory.dark = value.to_string(),
                    KEY_THEME_LIGHT => memory.light = value.to_string(),
                    _ => {}
                }
            }
        }
    }

    fn read_int(&self, key: &str) -> i32 {
        match &self.settings {
            Some(settings) if self.has_key(key) => settings.int(key),
            Some(_) => 0,
            None => match key {
                KEY_OPACITY => self.memory.borrow().opacity,
                _ => 0,
            },
        }
    }

    fn write_int(&self, key: &str, value: i32) {
        match &self.settings {
            Some(settings) if self.has_key(key) => {
                let _ = settings.set_int(key, value);
            }
            Some(_) => {}
            None => {
                if key == KEY_OPACITY {
                    self.memory.borrow_mut().opacity = value;
                }
            }
        }
    }

    fn read_double(&self, key: &str) -> f64 {
        match &self.settings {
            Some(settings) if self.has_key(key) => settings.double(key),
            Some(_) => 1.0,
            None => match key {
                KEY_FONT_SCALE => self.memory.borrow().font_scale,
                _ => 1.0,
            },
        }
    }

    fn write_double(&self, key: &str, value: f64) {
        match &self.settings {
            Some(settings) if self.has_key(key) => {
                let _ = settings.set_double(key, value);
            }
            Some(_) => {}
            None => {
                if key == KEY_FONT_SCALE {
                    self.memory.borrow_mut().font_scale = value;
                }
            }
        }
    }

    pub fn themes(&self) -> &[Theme] {
        &self.themes
    }

    pub fn theme(&self, id: &str) -> Option<&Theme> {
        self.themes.iter().find(|theme| theme.id == id)
    }

    pub fn import_errors(&self) -> &[String] {
        &self.import_errors
    }

    /// Id global: `auto` o un id explícito.
    pub fn theme_id(&self) -> String {
        self.read_string(KEY_THEME)
            .unwrap_or_else(|| AUTO_THEME.to_string())
    }

    pub fn dark_id(&self) -> String {
        self.read_string(KEY_THEME_DARK)
            .unwrap_or_else(|| self.default_dark().id.clone())
    }

    pub fn light_id(&self) -> String {
        self.read_string(KEY_THEME_LIGHT)
            .unwrap_or_else(|| self.default_light().id.clone())
    }

    /// Tema efectivo del sistema o el forzado por el usuario.
    pub fn resolved_theme(&self) -> Theme {
        let choice = self.theme_id();
        let dark = self.dark_id();
        let light = self.light_id();
        resolve_theme(
            &self.themes,
            ThemeSelection {
                choice: &choice,
                dark_id: &dark,
                light_id: &light,
                system_dark: self.system_dark.get(),
            },
        )
        .clone()
    }

    /// Tema de una pestaña: su override o el global.
    pub fn theme_for_tab(&self, tab_theme: Option<&str>) -> Theme {
        tab_theme
            .and_then(|id| self.theme(id).cloned())
            .unwrap_or_else(|| self.resolved_theme())
    }

    pub fn render_theme(&self, tab_theme: Option<&str>) -> TextTheme {
        TextTheme::from(&self.theme_for_tab(tab_theme))
    }

    pub fn set_theme_id(&self, id: Option<&str>) {
        self.write_string(KEY_THEME, id.unwrap_or(AUTO_THEME));
        self.notify();
    }

    pub fn set_dark_id(&self, id: &str) {
        self.write_string(KEY_THEME_DARK, id);
        self.notify();
    }

    pub fn set_light_id(&self, id: &str) {
        self.write_string(KEY_THEME_LIGHT, id);
        self.notify();
    }

    pub fn opacity(&self) -> i32 {
        clamp_opacity(self.read_int(KEY_OPACITY))
    }

    pub fn set_opacity(&self, percent: i32) {
        self.write_int(KEY_OPACITY, clamp_opacity(percent));
        self.notify();
    }

    pub fn font_scale(&self) -> f64 {
        clamp_font_scale(self.read_double(KEY_FONT_SCALE))
    }

    pub fn set_font_scale(&self, scale: f64) {
        self.write_double(KEY_FONT_SCALE, clamp_font_scale(scale));
        self.notify();
    }

    /// Aplica `steps` pasos de zoom y devuelve la escala resultante.
    pub fn zoom(&self, steps: i32) -> f64 {
        let scale = zoom_scale(self.font_scale(), steps);
        self.set_font_scale(scale);
        scale
    }

    pub fn base_font(&self) -> &FontSpec {
        &self.base_font
    }

    /// Fuente con la escala de zoom aplicada.
    pub fn font(&self) -> FontSpec {
        FontSpec {
            family: self.base_font.family.clone(),
            size: termust_core::appearance::scaled_font_size(
                self.base_font.size,
                self.font_scale(),
            ),
        }
    }

    /// Alto contraste del sistema: desactiva la transparencia (FR-9.2).
    pub fn high_contrast(&self) -> bool {
        settings_bool("org.gnome.desktop.a11y.interface", "high-contrast").unwrap_or(false)
            || settings_string("org.gnome.desktop.interface", "contrast")
                .map(|value| value == "high")
                .unwrap_or(false)
    }

    pub fn bg_alpha(&self) -> f64 {
        termust_core::appearance::effective_bg_alpha(self.opacity(), self.high_contrast())
    }

    /// Contraste efectivo mínimo con la transparencia actual, si es bajo.
    pub fn contrast_warning(&self) -> Option<(f64, bool)> {
        let high = self.high_contrast();
        let theme = self.resolved_theme();
        termust_core::appearance::opacity_contrast_warning(&theme, self.opacity(), high)
            .map(|ratio| (ratio, high))
    }

    /// Contraste del tema sin transparencia, para auditar en preferencias.
    pub fn theme_contrast(&self, theme: &Theme) -> f64 {
        contrast_ratio(theme.fg, theme.bg)
    }

    /// Aplica tema, zoom y opacidad a una vista.
    pub fn apply_to_view(&self, view: &termust_ui::TerminalView, tab_theme: Option<&str>) {
        view.set_theme(self.render_theme(tab_theme));
        view.set_font(self.font());
        view.set_bg_alpha(self.bg_alpha());
    }

    pub fn default_dark(&self) -> &Theme {
        self.themes
            .iter()
            .find(|theme| theme.variant == ThemeVariant::Dark)
            .unwrap_or(&self.themes[0])
    }

    pub fn default_light(&self) -> &Theme {
        self.themes
            .iter()
            .find(|theme| theme.variant == ThemeVariant::Light)
            .unwrap_or_else(|| &self.themes[0])
    }
}

fn settings_bool(schema: &str, key: &str) -> Option<bool> {
    let source = gio::SettingsSchemaSource::default()?;
    let found = source.lookup(schema, true)?;
    if !found.has_key(key) {
        return None;
    }
    Some(gio::Settings::new(schema).boolean(key))
}

fn settings_string(schema: &str, key: &str) -> Option<String> {
    let source = gio::SettingsSchemaSource::default()?;
    let found = source.lookup(schema, true)?;
    if !found.has_key(key) {
        return None;
    }
    Some(gio::Settings::new(schema).string(key).to_string())
}

/// Tema de respaldo si el catálogo embebido no se pudiera leer.
fn fallback_theme() -> Theme {
    Theme {
        id: "termust-dark".into(),
        name: "Termust Dark".into(),
        source: Some("Termust".into()),
        variant: ThemeVariant::Dark,
        fg: Rgb::new(0xe6, 0xe6, 0xe6),
        bg: Rgb::new(0x1b, 0x1e, 0x24),
        cursor: Rgb::new(0xe6, 0xe6, 0xe6),
        selection_fg: Rgb::new(0xe6, 0xe6, 0xe6),
        selection_bg: Rgb::new(0x2d, 0x33, 0x40),
        palette: [Rgb::new(0x1b, 0x1e, 0x24); 16],
    }
}

/// Nombre para mostrar en listas.
pub fn display_name(theme: &Theme) -> String {
    match theme.variant {
        ThemeVariant::Dark => format!("{}  ·  oscuro", theme.name),
        ThemeVariant::Light => format!("{}  ·  claro", theme.name),
    }
}
