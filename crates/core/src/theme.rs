//! Esquema de temas de terminal (TOML) y validación de contraste.

use crate::terminal::Rgb;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MIN_CONTRAST: f64 = 4.5;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeFile {
    #[serde(default, rename = "theme")]
    pub themes: Vec<ThemeSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeSpec {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub source: Option<String>,
    pub variant: ThemeVariant,
    pub fg: String,
    pub bg: String,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub selection_fg: Option<String>,
    #[serde(default)]
    pub selection_bg: Option<String>,
    pub palette: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeVariant {
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub source: Option<String>,
    pub variant: ThemeVariant,
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub selection_fg: Rgb,
    pub selection_bg: Rgb,
    pub palette: [Rgb; 16],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeError {
    Toml(String),
    Empty,
    InvalidColor(String),
    PaletteSize(usize),
    DuplicateId(String),
    MissingName(String),
}

impl std::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeError::Toml(e) => write!(f, "TOML inválido: {e}"),
            ThemeError::Empty => write!(f, "el archivo no define temas"),
            ThemeError::InvalidColor(c) => write!(f, "color inválido: {c}"),
            ThemeError::PaletteSize(n) => write!(f, "la paleta debe tener 16 colores, tiene {n}"),
            ThemeError::DuplicateId(id) => write!(f, "id de tema duplicado: {id}"),
            ThemeError::MissingName(id) => write!(f, "el tema {id} no tiene nombre"),
        }
    }
}

impl std::error::Error for ThemeError {}

impl ThemeSpec {
    pub fn resolve(&self) -> Result<Theme, ThemeError> {
        if self.id.trim().is_empty() || self.name.trim().is_empty() {
            return Err(ThemeError::MissingName(self.id.clone()));
        }
        if self.palette.len() != 16 {
            return Err(ThemeError::PaletteSize(self.palette.len()));
        }
        let bg = parse_color(&self.bg)?;
        let fg = parse_color(&self.fg)?;
        let cursor = match self.cursor.as_deref() {
            Some(value) => parse_color(value)?,
            None => fg,
        };
        let selection_fg = match self.selection_fg.as_deref() {
            Some(value) => parse_color(value)?,
            None => fg,
        };
        let selection_bg = match self.selection_bg.as_deref() {
            Some(value) => parse_color(value)?,
            None => blend(bg, fg, 0.22),
        };
        let mut palette = [bg; 16];
        for (index, value) in self.palette.iter().enumerate() {
            palette[index] = parse_color(value)?;
        }
        Ok(Theme {
            id: self.id.clone(),
            name: self.name.clone(),
            source: self.source.clone(),
            variant: self.variant,
            fg,
            bg,
            cursor,
            selection_fg,
            selection_bg,
            palette,
        })
    }
}

/// Catálogo versionado que se embebe en el binario (funciona sin red ni
/// archivos externos dentro del Flatpak).
pub const BUILTIN_THEMES: &str = include_str!("../../../data/themes/termust.toml");

pub fn builtin_themes() -> Result<Vec<Theme>, ThemeError> {
    parse_themes(BUILTIN_THEMES)
}

/// Selección de tema global: `auto` sigue el modo del sistema.
#[derive(Clone, Copy, Debug)]
pub struct ThemeSelection<'a> {
    pub choice: &'a str,
    pub dark_id: &'a str,
    pub light_id: &'a str,
    pub system_dark: bool,
}

pub const AUTO_THEME: &str = "auto";

/// Resuelve el tema efectivo sin depender de GTK.
pub fn resolve_theme<'a>(themes: &'a [Theme], selection: ThemeSelection<'_>) -> &'a Theme {
    if selection.choice != AUTO_THEME {
        if let Some(theme) = themes.iter().find(|theme| theme.id == selection.choice) {
            return theme;
        }
    }
    let variant = if selection.system_dark {
        ThemeVariant::Dark
    } else {
        ThemeVariant::Light
    };
    let preferred = if selection.system_dark {
        selection.dark_id
    } else {
        selection.light_id
    };
    themes
        .iter()
        .find(|theme| theme.id == preferred)
        .or_else(|| themes.iter().find(|theme| theme.variant == variant))
        .or_else(|| themes.first())
        .expect("el catálogo nunca está vacío")
}

pub fn parse_themes(text: &str) -> Result<Vec<Theme>, ThemeError> {
    let file: ThemeFile = toml::from_str(text).map_err(|e| ThemeError::Toml(e.to_string()))?;
    if file.themes.is_empty() {
        return Err(ThemeError::Empty);
    }
    let mut ids = HashSet::new();
    let mut out = Vec::with_capacity(file.themes.len());
    for spec in file.themes {
        if !ids.insert(spec.id.clone()) {
            return Err(ThemeError::DuplicateId(spec.id));
        }
        out.push(spec.resolve()?);
    }
    Ok(out)
}

pub fn parse_color(value: &str) -> Result<Rgb, ThemeError> {
    let value = value.trim().trim_start_matches('#');
    if value.len() != 6 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ThemeError::InvalidColor(value.to_string()));
    }
    let number =
        u32::from_str_radix(value, 16).map_err(|_| ThemeError::InvalidColor(value.to_string()))?;
    Ok(Rgb::new(
        ((number >> 16) & 0xff) as u8,
        ((number >> 8) & 0xff) as u8,
        (number & 0xff) as u8,
    ))
}

pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let la = luminance(a);
    let lb = luminance(b);
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

pub fn theme_contrast_ok(theme: &Theme) -> bool {
    contrast_ratio(theme.fg, theme.bg) + f64::EPSILON >= MIN_CONTRAST
}

fn luminance(color: Rgb) -> f64 {
    let channel = |value: u8| {
        let value = value as f64 / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
}

pub fn blend(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| ((x as f64 * (1.0 - t)) + (y as f64 * t)).round() as u8;
    Rgb::new(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"
[[theme]]
id = "termust-dark"
name = "Termust Dark"
source = "Termust"
variant = "dark"
fg = "#e6e6e6"
bg = "#1b1e24"
palette = [
  "#1b1e24", "#f7768e", "#9ece6a", "#e0af68",
  "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
  "#414868", "#f7768e", "#9ece6a", "#e0af68",
  "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5"
]
"##;

    #[test]
    fn theme_roundtrip() {
        let themes = parse_themes(SAMPLE).unwrap();
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].id, "termust-dark");
        assert_eq!(themes[0].fg, Rgb::new(0xe6, 0xe6, 0xe6));
        assert_eq!(themes[0].palette.len(), 16);
        let spec = ThemeSpec {
            id: "x".into(),
            name: "X".into(),
            source: None,
            variant: ThemeVariant::Light,
            fg: "#111111".into(),
            bg: "#ffffff".into(),
            cursor: None,
            selection_fg: None,
            selection_bg: None,
            palette: vec!["#000000".into(); 16],
        };
        let file = ThemeFile { themes: vec![spec] };
        let text = toml::to_string(&file).unwrap();
        let again = parse_themes(&text).unwrap();
        assert_eq!(again[0].variant, ThemeVariant::Light);
        assert_eq!(again[0].palette[0], Rgb::new(0, 0, 0));
    }

    #[test]
    fn theme_incomplete_is_error() {
        let missing_palette = r##"
[[theme]]
id = "x"
name = "X"
variant = "dark"
fg = "#ffffff"
bg = "#000000"
"##;
        assert!(matches!(
            parse_themes(missing_palette),
            Err(ThemeError::Toml(_))
        ));
        let wrong_size = SAMPLE.replace(
            r##""#1b1e24", "#f7768e", "#9ece6a", "#e0af68",
  "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
  "#414868", "#f7768e", "#9ece6a", "#e0af68",
  "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5""##,
            r##""#1b1e24""##,
        );
        assert!(matches!(
            parse_themes(&wrong_size),
            Err(ThemeError::PaletteSize(1))
        ));
        let bad_color = SAMPLE.replace("#e6e6e6", "nope");
        assert!(matches!(
            parse_themes(&bad_color),
            Err(ThemeError::InvalidColor(_))
        ));
    }

    #[test]
    fn theme_contrast_gate() {
        assert!(contrast_ratio(Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)) > 20.0);
        let dark = parse_themes(SAMPLE).unwrap().remove(0);
        assert!(theme_contrast_ok(&dark));
        let low = ThemeSpec {
            id: "low".into(),
            name: "Low".into(),
            source: None,
            variant: ThemeVariant::Dark,
            fg: "#808080".into(),
            bg: "#707070".into(),
            cursor: None,
            selection_fg: None,
            selection_bg: None,
            palette: vec!["#000000".into(); 16],
        };
        let low = low.resolve().unwrap();
        assert!(!theme_contrast_ok(&low));
    }

    #[test]
    fn shipped_themes_meet_requirements() {
        let themes = builtin_themes().expect("catálogo embebido válido");
        let dark = themes
            .iter()
            .filter(|theme| theme.variant == ThemeVariant::Dark)
            .count();
        let light = themes
            .iter()
            .filter(|theme| theme.variant == ThemeVariant::Light)
            .count();
        assert!(dark >= 20, "solo {dark} temas oscuros");
        assert!(light >= 10, "solo {light} temas claros");
        assert!(themes.len() >= 30);
        for theme in &themes {
            assert_eq!(theme.palette.len(), 16, "{}", theme.id);
            assert!(
                theme_contrast_ok(theme),
                "{} no alcanza 4.5:1 ({:.2})",
                theme.id,
                contrast_ratio(theme.fg, theme.bg)
            );
            assert!(theme.source.is_some(), "{} sin atribución", theme.id);
        }
    }

    #[test]
    fn theme_independent_of_system_scheme() {
        let themes = builtin_themes().unwrap();
        let forced = resolve_theme(
            &themes,
            ThemeSelection {
                choice: "catppuccin-mocha",
                dark_id: "termust-dark",
                light_id: "catppuccin-latte",
                system_dark: false,
            },
        );
        assert_eq!(forced.id, "catppuccin-mocha");
        let auto_dark = resolve_theme(
            &themes,
            ThemeSelection {
                choice: AUTO_THEME,
                dark_id: "gruvbox-dark",
                light_id: "catppuccin-latte",
                system_dark: true,
            },
        );
        let auto_light = resolve_theme(
            &themes,
            ThemeSelection {
                choice: AUTO_THEME,
                dark_id: "gruvbox-dark",
                light_id: "one-light",
                system_dark: false,
            },
        );
        assert_eq!(auto_dark.id, "gruvbox-dark");
        assert_eq!(auto_light.id, "one-light");
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let duplicated = format!("{SAMPLE}\n{SAMPLE}");
        assert!(matches!(
            parse_themes(&duplicated),
            Err(ThemeError::DuplicateId(_))
        ));
    }
}
