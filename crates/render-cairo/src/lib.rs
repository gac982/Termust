//! Renderer Cairo/Pango para la grilla del terminal.

use std::collections::HashMap;

use cairo::Context;
use pango::{FontDescription, Layout};
use pangocairo::functions::{create_layout, show_layout};
use termust_core::terminal::{Cell, Color, Rgb, Snapshot};
use termust_text_render::{resolve_color, CellMetrics, FontSpec, PaintOpts, TextRenderer, Theme};

pub struct CairoRenderer;

impl CairoRenderer {
    pub fn new() -> Self {
        Self
    }

    fn font_desc(font: &FontSpec, bold: bool, italic: bool) -> FontDescription {
        let mut fd = FontDescription::new();
        fd.set_family(&family_list(&font.family));
        // `set_size` interpreta el valor en puntos (como GNOME Console); usar
        // `set_absolute_size` lo tomaba como píxeles y dejaba la letra chica.
        fd.set_size((font.size * pango::SCALE as f64).round() as i32);
        if bold {
            fd.set_weight(pango::Weight::Bold);
        }
        if italic {
            fd.set_style(pango::Style::Italic);
        }
        fd
    }

    fn make_layout(ctx: &Context, font: &FontSpec, bold: bool, italic: bool) -> Layout {
        let layout = create_layout(ctx);
        layout.set_font_description(Some(&Self::font_desc(font, bold, italic)));
        layout
    }
}

impl Default for CairoRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextRenderer for CairoRenderer {
    fn metrics(&self, ctx: &Context, font: &FontSpec) -> CellMetrics {
        let layout = Self::make_layout(ctx, font, false, false);
        layout.set_text("M");
        let (w, h) = layout.pixel_size();
        CellMetrics {
            width: w.max(1) as f64,
            height: h.max(1) as f64,
            baseline: h as f64 * 0.8,
        }
    }

    fn paint(&self, ctx: &Context, snapshot: &Snapshot, theme: &Theme, opts: &PaintOpts) {
        let metrics = self.metrics(ctx, &opts.font);
        let cw = metrics.width;
        let ch = metrics.height;

        ctx.set_source_rgba(
            theme.bg.r as f64 / 255.0,
            theme.bg.g as f64 / 255.0,
            theme.bg.b as f64 / 255.0,
            opts.bg_alpha,
        );
        ctx.paint().ok();

        let layout = Self::make_layout(ctx, &opts.font, false, false);
        let mut glyph_layouts: HashMap<(char, bool, bool), Layout> = HashMap::new();

        for row in 0..snapshot.size.rows {
            let mut col = 0;
            while col < snapshot.size.cols {
                let Some(cell) = snapshot.cell(row, col) else {
                    break;
                };
                if cell.attrs.wide_spacer {
                    col += 1;
                    continue;
                }
                let selected = opts
                    .selection
                    .map(|s| s.contains(row, col))
                    .unwrap_or(false);
                let (fg, bg) = colors(cell, theme, selected);
                // Con fondo translúcido, las celdas sin color propio dejan ver
                // el fondo base; los colores explícitos y la selección siguen
                // opacos (FR-9.1).
                let paint_bg = paints_background(cell, selected);
                let x = col as f64 * cw;
                let y = row as f64 * ch;

                let mut end = col + 1;
                let mut text = String::new();
                if !cell.attrs.hidden {
                    text.push(cell.ch);
                }
                while end < snapshot.size.cols {
                    let Some(next) = snapshot.cell(row, end) else {
                        break;
                    };
                    if next.attrs.wide_spacer {
                        end += 1;
                        continue;
                    }
                    let next_selected = opts
                        .selection
                        .map(|s| s.contains(row, end))
                        .unwrap_or(false);
                    let (nfg, nbg) = colors(next, theme, next_selected);
                    if nfg != fg
                        || nbg != bg
                        || paints_background(next, next_selected) != paint_bg
                        || next.attrs.bold != cell.attrs.bold
                        || next.attrs.italic != cell.attrs.italic
                        || next.attrs.underline != cell.attrs.underline
                        || next.attrs.inverse != cell.attrs.inverse
                    {
                        break;
                    }
                    if !next.attrs.hidden {
                        text.push(next.ch);
                    }
                    end += 1;
                }

                // El fondo cubre todo el tramo, no solo la primera celda.
                if paint_bg {
                    ctx.set_source_rgb(
                        bg.r as f64 / 255.0,
                        bg.g as f64 / 255.0,
                        bg.b as f64 / 255.0,
                    );
                    ctx.rectangle(x, y, (end - col) as f64 * cw, ch);
                    ctx.fill().ok();
                }

                if !text.is_empty() {
                    layout.set_text(&text);
                    layout.set_font_description(Some(&Self::font_desc(
                        &opts.font,
                        cell.attrs.bold,
                        cell.attrs.italic,
                    )));
                    ctx.set_source_rgb(
                        fg.r as f64 / 255.0,
                        fg.g as f64 / 255.0,
                        fg.b as f64 / 255.0,
                    );
                    let advance = layout_advance(&layout);
                    if run_fits_grid(advance, end - col, cw) {
                        ctx.move_to(x, y);
                        show_layout(ctx, &layout);
                    } else {
                        // Un glifo de fallback con otro ancho (braille, emoji,
                        // simbolos) corre todo el tramo y rompe los graficos de
                        // btop/htop: en ese caso cada celda va a su columna.
                        for c in col..end {
                            let Some(cell) = snapshot.cell(row, c) else {
                                break;
                            };
                            if cell.attrs.wide_spacer || cell.attrs.hidden {
                                continue;
                            }
                            let key = (cell.ch, cell.attrs.bold, cell.attrs.italic);
                            let l = glyph_layouts.entry(key).or_insert_with(|| {
                                let l = Self::make_layout(
                                    ctx,
                                    &opts.font,
                                    cell.attrs.bold,
                                    cell.attrs.italic,
                                );
                                l.set_text(&cell.ch.to_string());
                                l
                            });
                            ctx.move_to(c as f64 * cw, y);
                            show_layout(ctx, l);
                        }
                    }

                    if cell.attrs.underline {
                        ctx.set_line_width(1.0);
                        ctx.move_to(x, y + ch - 1.0);
                        ctx.line_to(x + text.chars().count() as f64 * cw, y + ch - 1.0);
                        ctx.stroke().ok();
                    }
                }

                col = end.max(col + 1);
            }
        }

        if snapshot.cursor.visible {
            let x = snapshot.cursor.col as f64 * cw;
            let y = snapshot.cursor.row as f64 * ch;
            // Con el fondo translúcido el cursor debe seguir opaco (FR-9.1).
            let alpha = if opts.bg_alpha < 1.0 { 1.0 } else { 0.75 };
            ctx.set_source_rgba(
                theme.cursor.r as f64 / 255.0,
                theme.cursor.g as f64 / 255.0,
                theme.cursor.b as f64 / 255.0,
                alpha,
            );
            ctx.rectangle(x, y, cw, ch);
            ctx.fill().ok();
        }
    }
}

/// Los glifos que la fuente elegida no tiene (braille, bloques, cajas) caen a
/// otra familia. Si esa familia es proporcional su avance no coincide con la
/// celda y el texto se corre celda a celda; por eso el fallback es monospace.
fn family_list(family: &str) -> String {
    let family = family.trim();
    if family.is_empty() {
        return "monospace".to_string();
    }
    if family
        .split(',')
        .any(|f| f.trim().eq_ignore_ascii_case("monospace"))
    {
        return family.to_string();
    }
    format!("{family}, monospace")
}

fn layout_advance(layout: &Layout) -> f64 {
    layout.extents().1.width() as f64 / pango::SCALE as f64
}

/// Pango redondea avances segun el hinting; un cuarto de pixel alcanza para
/// tolerar esa variacion sin aceptar un glifo de otro ancho.
const ADVANCE_TOLERANCE: f64 = 0.25;

/// `true` si el tramo ocupa exactamente las celdas que le corresponden.
fn run_fits_grid(advance: f64, cells: usize, cell_width: f64) -> bool {
    cell_width > 0.0 && (advance - cells as f64 * cell_width).abs() <= ADVANCE_TOLERANCE
}

fn paints_background(cell: &Cell, selected: bool) -> bool {
    selected || cell.attrs.inverse || !matches!(cell.bg, Color::DefaultBg)
}

fn colors(cell: &Cell, theme: &Theme, selected: bool) -> (Rgb, Rgb) {
    let mut fg = resolve_color(cell.fg, theme);
    let mut bg = resolve_color(cell.bg, theme);
    if selected {
        fg = theme.selection_fg;
        bg = theme.selection_bg;
    }
    if cell.attrs.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    (fg, bg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairo::{Format, ImageSurface};
    use termust_core::terminal::{Attrs, Cell, Color, Cursor, Modes, Rgb, Size};
    use termust_text_render::{FontSpec, Selection};

    fn surface(w: i32, h: i32) -> (ImageSurface, Context) {
        let s = ImageSurface::create(Format::ARgb32, w, h).unwrap();
        let c = Context::new(&s).unwrap();
        (s, c)
    }

    fn pixel(s: &mut ImageSurface, x: i32, y: i32) -> (u8, u8, u8, u8) {
        s.flush();
        let stride = s.stride() as usize;
        let data = s.data().unwrap();
        let idx = y as usize * stride + x as usize * 4;
        (data[idx + 2], data[idx + 1], data[idx], data[idx + 3])
    }

    fn blank_snapshot(cols: usize, rows: usize) -> Snapshot {
        Snapshot {
            size: Size::new(cols, rows),
            cells: vec![Cell::default(); cols * rows],
            cursor: Cursor::default(),
            modes: Modes::default(),
            display_offset: 0,
            history_size: 0,
        }
    }

    #[test]
    fn metrics_are_positive() {
        let (_s, ctx) = surface(200, 100);
        let r = CairoRenderer::new();
        let m = r.metrics(&ctx, &FontSpec::default());
        assert!(m.width > 0.0 && m.height > 0.0);
    }

    #[test]
    fn background_and_selection() {
        let (mut s, ctx) = surface(200, 100);
        let r = CairoRenderer::new();
        let theme = Theme::default();
        let m = r.metrics(&ctx, &FontSpec::default());
        let mut snap = blank_snapshot(5, 2);
        snap.cursor.visible = false;
        let opts = PaintOpts {
            font: FontSpec::default(),
            selection: Some(Selection {
                start: (0, 0),
                end: (0, 0),
            }),
            bg_alpha: 1.0,
        };
        r.paint(&ctx, &snap, &theme, &opts);
        drop(ctx);
        let sel = pixel(&mut s, (m.width * 0.5) as i32, (m.height * 0.5) as i32);
        let bg = pixel(&mut s, (m.width * 2.5) as i32, (m.height * 0.5) as i32);
        assert_eq!(
            (sel.0, sel.1, sel.2),
            (
                theme.selection_bg.r,
                theme.selection_bg.g,
                theme.selection_bg.b
            )
        );
        assert_eq!((bg.0, bg.1, bg.2), (theme.bg.r, theme.bg.g, theme.bg.b));
    }

    #[test]
    fn background_covers_whole_run() {
        let (mut s, ctx) = surface(120, 60);
        let r = CairoRenderer::new();
        let theme = Theme::default();
        let m = r.metrics(&ctx, &FontSpec::default());
        let mut snap = blank_snapshot(6, 1);
        snap.cursor.visible = false;
        let run_bg = Color::Rgb(Rgb::new(10, 20, 30));
        for cell in snap.cells.iter_mut() {
            cell.bg = run_bg;
        }
        r.paint(&ctx, &snap, &theme, &PaintOpts::default());
        drop(ctx);
        for col in 0..6 {
            let x = (m.width * (col as f64 + 0.5)) as i32;
            let p = pixel(&mut s, x, (m.height * 0.5) as i32);
            assert_eq!((p.0, p.1, p.2), (10, 20, 30), "cell {col}");
        }
    }

    #[test]
    fn font_size_is_in_points() {
        let (_s, ctx) = surface(200, 100);
        let r = CairoRenderer::new();
        let small = FontSpec {
            family: "monospace".into(),
            size: 8.0,
        };
        let large = FontSpec {
            family: "monospace".into(),
            size: 11.0,
        };
        let ms = r.metrics(&ctx, &small);
        let ml = r.metrics(&ctx, &large);
        assert!(ml.width > ms.width);
        assert!(ml.height > ms.height);
    }

    #[test]
    fn opacity_only_background() {
        let (mut s, c) = surface(40, 40);
        let mut snap = blank_snapshot(5, 2);
        snap.cursor = Cursor {
            col: 0,
            row: 0,
            visible: true,
        };
        let theme = Theme::default();
        let opts = PaintOpts {
            bg_alpha: 0.5,
            ..PaintOpts::default()
        };
        CairoRenderer::new().paint(&c, &snap, &theme, &opts);
        drop(c);
        // Lejos del cursor y de las celdas: solo el fondo base, translúcido.
        let (_, _, _, bg_alpha) = pixel(&mut s, 35, 35);
        assert!(bg_alpha > 0 && bg_alpha < 255, "alpha {bg_alpha}");
        // El cursor se mantiene opaco.
        let (_, _, _, cursor_alpha) = pixel(&mut s, 2, 2);
        assert_eq!(cursor_alpha, 255);
    }

    #[test]
    fn cell_backgrounds_stay_opaque_with_transparency() {
        let (mut s, c) = surface(40, 40);
        let mut snap = blank_snapshot(1, 1);
        snap.cells[0].bg = Color::Indexed(1);
        let opts = PaintOpts {
            bg_alpha: 0.5,
            ..PaintOpts::default()
        };
        CairoRenderer::new().paint(&c, &snap, &Theme::default(), &opts);
        drop(c);
        let (_, _, _, alpha) = pixel(&mut s, 3, 3);
        assert_eq!(alpha, 255);
    }

    #[test]
    fn cursor_paints() {
        let (mut s, ctx) = surface(200, 100);
        let r = CairoRenderer::new();
        let theme = Theme::default();
        let m = r.metrics(&ctx, &FontSpec::default());
        let mut snap = blank_snapshot(5, 2);
        snap.cursor = Cursor {
            col: 0,
            row: 0,
            visible: true,
        };
        r.paint(&ctx, &snap, &theme, &PaintOpts::default());
        drop(ctx);
        let p = pixel(&mut s, (m.width * 0.5) as i32, (m.height * 0.5) as i32);
        assert_ne!((p.0, p.1, p.2), (theme.bg.r, theme.bg.g, theme.bg.b));
    }

    #[test]
    fn run_fits_grid_checks_cell_pitch() {
        assert!(run_fits_grid(30.0, 3, 10.0));
        assert!(run_fits_grid(30.2, 3, 10.0));
        assert!(!run_fits_grid(36.0, 3, 10.0));
        assert!(!run_fits_grid(12.0, 1, 10.0));
        assert!(!run_fits_grid(10.0, 1, 0.0));
    }

    #[test]
    fn family_list_appends_monospace_fallback() {
        assert_eq!(family_list("Hack"), "Hack, monospace");
        assert_eq!(family_list(" Hack "), "Hack, monospace");
        assert_eq!(family_list("Hack, monospace"), "Hack, monospace");
        assert_eq!(family_list("monospace"), "monospace");
        assert_eq!(family_list(""), "monospace");
    }

    #[test]
    fn glyph_with_other_advance_does_not_shift_the_run() {
        // Los espacios ideograficos avanzan mas que una celda; si el tramo se
        // pinta de una sola vez el `X` se corre varias columnas (bug de los
        // graficos braille de btop/htop con fuentes sin esos glifos).
        let (mut s, ctx) = surface(60, 30);
        let r = CairoRenderer::new();
        let font = FontSpec {
            family: "monospace".into(),
            size: 12.0,
        };
        let m = r.metrics(&ctx, &font);
        let theme = Theme::default();
        let mut snap = blank_snapshot(5, 1);
        snap.cursor.visible = false;
        for cell in &mut snap.cells[..4] {
            cell.ch = '\u{3000}';
        }
        snap.cells[4].ch = 'X';
        for cell in snap.cells.iter_mut() {
            cell.fg = Color::Rgb(Rgb::new(255, 255, 255));
        }
        r.paint(
            &ctx,
            &snap,
            &theme,
            &PaintOpts {
                font,
                ..PaintOpts::default()
            },
        );
        drop(ctx);
        let start = (m.width * 4.0) as i32;
        let end = ((m.width * 5.0) as i32).min(59);
        let ink = (start..end).any(|x| {
            (0..(m.height as i32).min(30)).any(|y| {
                let p = pixel(&mut s, x, y);
                (p.0, p.1, p.2) != (theme.bg.r, theme.bg.g, theme.bg.b)
            })
        });
        assert!(
            ink,
            "el glifo de la ultima celda se corrio fuera de su columna"
        );
    }

    #[test]
    fn text_paints_pixels() {
        let (mut s, ctx) = surface(200, 100);
        let r = CairoRenderer::new();
        let theme = Theme::default();
        let m = r.metrics(&ctx, &FontSpec::default());
        let mut snap = blank_snapshot(5, 2);
        snap.cursor.visible = false;
        snap.cells[0] = Cell {
            ch: 'X',
            fg: Color::Rgb(Rgb::new(255, 255, 255)),
            bg: Color::DefaultBg,
            attrs: Attrs::default(),
        };
        r.paint(&ctx, &snap, &theme, &PaintOpts::default());
        drop(ctx);
        let mut non_bg = 0;
        for y in 0..m.height as i32 {
            for x in 0..m.width as i32 {
                let p = pixel(&mut s, x, y);
                if (p.0, p.1, p.2) != (theme.bg.r, theme.bg.g, theme.bg.b) {
                    non_bg += 1;
                }
            }
        }
        assert!(non_bg > 0, "expected glyph pixels");
    }
}
