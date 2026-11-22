//! Tipos y contrato del motor de texto compartido.

use cairo::Context;
use termust_core::terminal::{Color, Rgb, Snapshot};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: f64,
    pub height: f64,
    pub baseline: f64,
}

impl CellMetrics {
    pub fn cols_for_width(&self, width: f64) -> usize {
        if self.width <= 0.0 {
            0
        } else {
            (width / self.width).floor() as usize
        }
    }

    pub fn rows_for_height(&self, height: f64) -> usize {
        if self.height <= 0.0 {
            0
        } else {
            (height / self.height).floor() as usize
        }
    }
}

#[derive(Clone, Debug)]
pub struct FontSpec {
    pub family: String,
    pub size: f64,
}

impl FontSpec {
    /// Parsea el formato de `org.gnome.desktop.interface monospace-font-name`
    /// (`"Hack Nerd Font 11"`). Devuelve `None` si no se puede interpretar.
    pub fn parse_pango(value: &str) -> Option<Self> {
        let value = value.trim();
        let (family, size) = value.rsplit_once(' ')?;
        let family = family.trim();
        let size: f64 = size.trim().parse().ok()?;
        if family.is_empty() || !size.is_finite() || size <= 0.0 {
            return None;
        }
        Some(Self {
            family: family.to_string(),
            size,
        })
    }
}

impl Default for FontSpec {
    fn default() -> Self {
        Self {
            family: "monospace".to_string(),
            size: 12.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub selection_bg: Rgb,
    pub selection_fg: Rgb,
    pub palette: [Rgb; 16],
}

impl From<&termust_core::theme::Theme> for Theme {
    fn from(theme: &termust_core::theme::Theme) -> Self {
        Self {
            fg: theme.fg,
            bg: theme.bg,
            cursor: theme.cursor,
            selection_bg: theme.selection_bg,
            selection_fg: theme.selection_fg,
            palette: theme.palette,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            fg: Rgb::new(0xe6, 0xe6, 0xe6),
            bg: Rgb::new(0x1b, 0x1e, 0x24),
            cursor: Rgb::new(0x7a, 0xa2, 0xf7),
            selection_bg: Rgb::new(0x2d, 0x33, 0x40),
            selection_fg: Rgb::new(0xe6, 0xe6, 0xe6),
            palette: [
                Rgb::new(0x1b, 0x1e, 0x24),
                Rgb::new(0xf7, 0x76, 0x8e),
                Rgb::new(0x9e, 0xce, 0x6a),
                Rgb::new(0xe0, 0xaf, 0x68),
                Rgb::new(0x7a, 0xa2, 0xf7),
                Rgb::new(0xbb, 0x9a, 0xf7),
                Rgb::new(0x7d, 0xcf, 0xff),
                Rgb::new(0xa9, 0xb1, 0xd6),
                Rgb::new(0x41, 0x48, 0x68),
                Rgb::new(0xf7, 0x76, 0x8e),
                Rgb::new(0x9e, 0xce, 0x6a),
                Rgb::new(0xe0, 0xaf, 0x68),
                Rgb::new(0x7a, 0xa2, 0xf7),
                Rgb::new(0xbb, 0x9a, 0xf7),
                Rgb::new(0x7d, 0xcf, 0xff),
                Rgb::new(0xc0, 0xca, 0xf5),
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

impl Selection {
    pub fn normalized(self) -> Self {
        if (self.start.0, self.start.1) <= (self.end.0, self.end.1) {
            self
        } else {
            Self {
                start: self.end,
                end: self.start,
            }
        }
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        let s = self.normalized();
        (row, col) >= s.start && (row, col) <= s.end
    }
}

#[derive(Clone, Debug)]
pub struct PaintOpts {
    pub font: FontSpec,
    pub selection: Option<Selection>,
    pub bg_alpha: f64,
}

impl Default for PaintOpts {
    fn default() -> Self {
        Self {
            font: FontSpec::default(),
            selection: None,
            bg_alpha: 1.0,
        }
    }
}

pub trait TextRenderer {
    fn metrics(&self, ctx: &Context, font: &FontSpec) -> CellMetrics;
    fn paint(&self, ctx: &Context, snapshot: &Snapshot, theme: &Theme, opts: &PaintOpts);
}

pub fn resolve_color(color: Color, theme: &Theme) -> Rgb {
    match color {
        Color::DefaultFg => theme.fg,
        Color::DefaultBg => theme.bg,
        Color::Rgb(rgb) => rgb,
        Color::Indexed(i) => xterm_256(i, theme),
    }
}

fn xterm_256(i: u8, theme: &Theme) -> Rgb {
    if (i as usize) < 16 {
        return theme.palette[i as usize];
    }
    if i >= 232 {
        let v = 8 + (i - 232) * 10;
        return Rgb::new(v, v, v);
    }
    let n = i - 16;
    let r = n / 36;
    let g = (n % 36) / 6;
    let b = n % 6;
    let c = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
    Rgb::new(c(r), c(g), c(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_contains_and_normalizes() {
        let s = Selection {
            start: (2, 3),
            end: (1, 5),
        };
        assert!(s.contains(1, 5));
        assert!(s.contains(2, 3));
        assert!(!s.contains(0, 0));
        assert!(!s.contains(2, 4));
    }

    #[test]
    fn indexed_palette_and_cube() {
        let t = Theme::default();
        assert_eq!(resolve_color(Color::Indexed(1), &t), t.palette[1]);
        assert_eq!(resolve_color(Color::Indexed(16), &t), Rgb::new(0, 0, 0));
        assert_eq!(
            resolve_color(Color::Indexed(231), &t),
            Rgb::new(255, 255, 255)
        );
        assert_eq!(resolve_color(Color::Indexed(232), &t), Rgb::new(8, 8, 8));
    }

    #[test]
    fn parse_pango_font_names() {
        let f = FontSpec::parse_pango("Hack Nerd Font 11").unwrap();
        assert_eq!(f.family, "Hack Nerd Font");
        assert_eq!(f.size, 11.0);
        let f = FontSpec::parse_pango("Adwaita Mono 10.5").unwrap();
        assert_eq!(f.family, "Adwaita Mono");
        assert_eq!(f.size, 10.5);
        assert!(FontSpec::parse_pango("monospace").is_none());
        assert!(FontSpec::parse_pango("Hack 0").is_none());
        assert!(FontSpec::parse_pango(" 11").is_none());
        assert!(FontSpec::parse_pango("").is_none());
    }

    #[test]
    fn metrics_division() {
        let m = CellMetrics {
            width: 8.0,
            height: 16.0,
            baseline: 12.0,
        };
        assert_eq!(m.cols_for_width(80.0), 10);
        assert_eq!(m.rows_for_height(32.0), 2);
        assert_eq!(m.cols_for_width(0.0), 0);
    }
}
