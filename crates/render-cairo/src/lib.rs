//! Renderer Cairo/Pango para la grilla del terminal.

use cairo::Context;
use pango::{FontDescription, Layout};
use pangocairo::functions::{create_layout, show_layout};
use termust_core::terminal::{Cell, Rgb, Snapshot};
use termust_text_render::{resolve_color, CellMetrics, FontSpec, PaintOpts, TextRenderer, Theme};

pub struct CairoRenderer;

impl CairoRenderer {
    pub fn new() -> Self {
        Self
    }

    fn font_desc(font: &FontSpec, bold: bool, italic: bool) -> FontDescription {
        let mut fd = FontDescription::new();
        fd.set_family(&font.family);
        fd.set_absolute_size(font.size * pango::SCALE as f64);
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
                let x = col as f64 * cw;
                let y = row as f64 * ch;

                ctx.set_source_rgb(
                    bg.r as f64 / 255.0,
                    bg.g as f64 / 255.0,
                    bg.b as f64 / 255.0,
                );
                ctx.rectangle(x, y, cw, ch);
                ctx.fill().ok();

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
                    ctx.move_to(x, y);
                    show_layout(ctx, &layout);

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
            ctx.set_source_rgba(
                theme.cursor.r as f64 / 255.0,
                theme.cursor.g as f64 / 255.0,
                theme.cursor.b as f64 / 255.0,
                0.75,
            );
            ctx.rectangle(x, y, cw, ch);
            ctx.fill().ok();
        }
    }
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
