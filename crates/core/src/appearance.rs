//! Apariencia: opacidad del fondo, escala de fuente y avisos de contraste.
//!
//! FR-9: la opacidad solo afecta al fondo; texto y cursor quedan opacos.
//! La escala de fuente (zoom) se limita a 0.5–2.0 en pasos de 0.1.

use crate::theme::{blend, contrast_ratio, Theme, MIN_CONTRAST};

pub const MIN_FONT_SCALE: f64 = 0.5;
pub const MAX_FONT_SCALE: f64 = 2.0;
pub const FONT_SCALE_STEP: f64 = 0.1;

/// Tope de transparencia: el fondo del terminal nunca desaparece del todo.
pub const MAX_OPACITY: i32 = 80;

/// Recorta un porcentaje de transparencia al rango 0–80.
pub fn clamp_opacity(percent: i32) -> i32 {
    percent.clamp(0, MAX_OPACITY)
}

/// Alpha del fondo a partir del porcentaje de transparencia (`0` = opaco).
pub fn bg_alpha(percent: i32) -> f64 {
    1.0 - clamp_opacity(percent) as f64 / 100.0
}

/// Alpha efectivo; con alto contraste la transparencia se desactiva (FR-9.2).
pub fn effective_bg_alpha(percent: i32, high_contrast: bool) -> f64 {
    if high_contrast {
        1.0
    } else {
        bg_alpha(percent)
    }
}

/// La transparencia está disponible salvo en alto contraste.
pub fn opacity_allowed(high_contrast: bool) -> bool {
    !high_contrast
}

/// Recorta la escala de fuente y la alinea a pasos de 0.1.
pub fn clamp_font_scale(scale: f64) -> f64 {
    if !scale.is_finite() {
        return 1.0;
    }
    let stepped = (scale * 10.0).round() / 10.0;
    stepped.clamp(MIN_FONT_SCALE, MAX_FONT_SCALE)
}

/// Nueva escala tras `steps` pasos de zoom (positivo acerca).
pub fn zoom_scale(current: f64, steps: i32) -> f64 {
    clamp_font_scale(current + steps as f64 * FONT_SCALE_STEP)
}

/// Tamaño de fuente resultante de aplicar la escala.
pub fn scaled_font_size(base: f64, scale: f64) -> f64 {
    (base * clamp_font_scale(scale)).clamp(1.0, 512.0)
}

/// Peor contraste efectivo del texto sobre el fondo translúcido.
///
/// El fondo se compone sobre un escritorio desconocido; se evalúan negro y
/// blanco como casos extremos y se devuelve el menor contraste. `None` cuando
/// la transparencia está desactivada o no hay riesgo.
pub fn opacity_contrast_warning(theme: &Theme, percent: i32, high_contrast: bool) -> Option<f64> {
    let alpha = effective_bg_alpha(percent, high_contrast);
    if alpha >= 1.0 {
        return None;
    }
    let over_black = blend(crate::terminal::Rgb::new(0, 0, 0), theme.bg, alpha);
    let over_white = blend(crate::terminal::Rgb::new(255, 255, 255), theme.bg, alpha);
    let worst = contrast_ratio(theme.fg, over_black).min(contrast_ratio(theme.fg, over_white));
    if worst + f64::EPSILON < MIN_CONTRAST {
        Some(worst)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::parse_themes;

    const THEME: &str = r##"
[[theme]]
id = "termust-dark"
name = "Termust Dark"
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

    fn theme() -> Theme {
        parse_themes(THEME).unwrap().remove(0)
    }

    #[test]
    fn opacity_clamped_0_80() {
        assert_eq!(clamp_opacity(-5), 0);
        assert_eq!(clamp_opacity(0), 0);
        assert_eq!(clamp_opacity(55), 55);
        assert_eq!(clamp_opacity(150), MAX_OPACITY);
        assert_eq!(bg_alpha(0), 1.0);
        assert!((bg_alpha(50) - 0.5).abs() < f64::EPSILON);
        assert!((bg_alpha(100) - 0.2).abs() < f64::EPSILON);
    }

    #[test]
    fn opacity_disabled_in_high_contrast() {
        assert!(!opacity_allowed(true));
        assert_eq!(effective_bg_alpha(80, true), 1.0);
        assert!((effective_bg_alpha(80, false) - 0.2).abs() < 1e-9);
        assert_eq!(super::opacity_contrast_warning(&theme(), 80, true), None);
    }

    #[test]
    fn opacity_contrast_warning() {
        // Opaco: sin aviso.
        assert_eq!(super::opacity_contrast_warning(&theme(), 0, false), None);
        // Muy transparente sobre blanco: el texto claro pierde contraste.
        let warning = super::opacity_contrast_warning(&theme(), 90, false).expect("aviso");
        assert!(warning < MIN_CONTRAST);
        // Poca transparencia: sigue por encima del umbral.
        assert_eq!(super::opacity_contrast_warning(&theme(), 10, false), None);
    }

    #[test]
    fn font_scale_zoom_is_clamped_and_stepped() {
        assert_eq!(clamp_font_scale(2.5), MAX_FONT_SCALE);
        assert_eq!(clamp_font_scale(0.1), MIN_FONT_SCALE);
        assert_eq!(zoom_scale(1.0, 3), 1.3);
        assert_eq!(zoom_scale(1.0, -20), MIN_FONT_SCALE);
        assert_eq!(zoom_scale(MAX_FONT_SCALE, 1), MAX_FONT_SCALE);
        assert!((scaled_font_size(12.0, 1.5) - 18.0).abs() < 1e-9);
        assert_eq!(clamp_font_scale(f64::NAN), 1.0);
    }
}
