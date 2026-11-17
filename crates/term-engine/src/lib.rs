//! Adaptador del motor VT sobre `alacritty_terminal`.

mod osc;

pub use osc::{OscEvent, OscScanner, PromptMark};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape, NamedColor, Processor, StdSyncHandler,
};
use std::path::PathBuf;
use termust_core::terminal::{
    Attrs, Cell, Color, Cursor, Modes, PromptStatus, Rgb, Size, Snapshot, TerminalEngine,
};

type Proc = Processor<StdSyncHandler>;

struct Dim {
    cols: usize,
    rows: usize,
}

impl Dimensions for Dim {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct AlacrittyEngine {
    term: Term<VoidListener>,
    parser: Proc,
    osc: OscScanner,
    size: Size,
}

impl AlacrittyEngine {
    pub fn new(size: Size, history: usize) -> Self {
        let cfg = Config {
            scrolling_history: history,
            ..Default::default()
        };
        let term = Term::new(
            cfg,
            &Dim {
                cols: size.cols,
                rows: size.rows,
            },
            VoidListener,
        );
        Self {
            term,
            parser: Proc::new(),
            osc: OscScanner::new(),
            size,
        }
    }

    pub fn osc(&self) -> &OscScanner {
        &self.osc
    }
}

impl TerminalEngine for AlacrittyEngine {
    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        self.osc.feed(bytes);
    }

    fn resize(&mut self, size: Size) {
        self.term.resize(Dim {
            cols: size.cols,
            rows: size.rows,
        });
        self.size = size;
    }

    fn snapshot(&mut self) -> Snapshot {
        let content = self.term.renderable_content();
        let mut cells = vec![Cell::default(); self.size.area()];

        for indexed in content.display_iter {
            let line = indexed.point.line.0;
            let col = indexed.point.column.0;
            if line < 0 || col >= self.size.cols {
                continue;
            }
            let row = line as usize;
            if row >= self.size.rows {
                continue;
            }
            let src = indexed.cell;
            cells[row * self.size.cols + col] = Cell {
                ch: src.c,
                fg: map_color(src.fg),
                bg: map_color(src.bg),
                attrs: Attrs {
                    bold: src.flags.contains(Flags::BOLD),
                    italic: src.flags.contains(Flags::ITALIC),
                    underline: src.flags.contains(Flags::UNDERLINE),
                    inverse: src.flags.contains(Flags::INVERSE),
                    hidden: src.flags.contains(Flags::HIDDEN),
                    wide: src.flags.contains(Flags::WIDE_CHAR),
                    wide_spacer: src.flags.contains(Flags::WIDE_CHAR_SPACER),
                },
            };
        }

        Snapshot {
            size: self.size,
            cells,
            cursor: Cursor {
                row: content.cursor.point.line.0.max(0) as usize,
                col: content.cursor.point.column.0,
                visible: content.cursor.shape != CursorShape::Hidden,
            },
            modes: Modes {
                alt_screen: content.mode.contains(TermMode::ALT_SCREEN),
                bracketed_paste: content.mode.contains(TermMode::BRACKETED_PASTE),
                app_cursor: content.mode.contains(TermMode::APP_CURSOR),
                mouse_reporting: content.mode.intersects(TermMode::MOUSE_MODE),
                show_cursor: content.mode.contains(TermMode::SHOW_CURSOR),
            },
            display_offset: content.display_offset,
            history_size: self.term.grid().history_size(),
        }
    }

    fn scroll(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    fn title(&self) -> Option<&str> {
        self.osc.title()
    }

    fn cwd(&self) -> Option<&PathBuf> {
        self.osc.cwd()
    }

    fn prompt(&self) -> PromptStatus {
        self.osc.prompt()
    }
}

fn map_color(color: AnsiColor) -> Color {
    match color {
        AnsiColor::Spec(rgb) => Color::Rgb(Rgb::new(rgb.r, rgb.g, rgb.b)),
        AnsiColor::Indexed(i) => Color::Indexed(i),
        AnsiColor::Named(named) => match named {
            NamedColor::Foreground
            | NamedColor::BrightForeground
            | NamedColor::DimForeground
            | NamedColor::Cursor => Color::DefaultFg,
            NamedColor::Background => Color::DefaultBg,
            NamedColor::Black
            | NamedColor::Red
            | NamedColor::Green
            | NamedColor::Yellow
            | NamedColor::Blue
            | NamedColor::Magenta
            | NamedColor::Cyan
            | NamedColor::White
            | NamedColor::BrightBlack
            | NamedColor::BrightRed
            | NamedColor::BrightGreen
            | NamedColor::BrightYellow
            | NamedColor::BrightBlue
            | NamedColor::BrightMagenta
            | NamedColor::BrightCyan
            | NamedColor::BrightWhite => Color::Indexed(named as u8),
            NamedColor::DimBlack => Color::Indexed(0),
            NamedColor::DimRed => Color::Indexed(1),
            NamedColor::DimGreen => Color::Indexed(2),
            NamedColor::DimYellow => Color::Indexed(3),
            NamedColor::DimBlue => Color::Indexed(4),
            NamedColor::DimMagenta => Color::Indexed(5),
            NamedColor::DimCyan => Color::Indexed(6),
            NamedColor::DimWhite => Color::Indexed(7),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> AlacrittyEngine {
        AlacrittyEngine::new(Size::new(20, 5), 100)
    }

    #[test]
    fn plain_text_and_cursor() {
        let mut e = engine();
        e.feed(b"hello");
        let s = e.snapshot();
        assert_eq!(s.row_text(0), "hello");
        assert_eq!(s.cursor.col, 5);
        assert_eq!(s.cursor.row, 0);
    }

    #[test]
    fn ansi_and_truecolor() {
        let mut e = engine();
        e.feed(b"\x1b[31mR\x1b[0m\x1b[38;2;1;2;3mT\x1b[38;5;123mI");
        let s = e.snapshot();
        assert_eq!(s.cell(0, 0).unwrap().fg, Color::Indexed(1));
        assert_eq!(s.cell(0, 1).unwrap().fg, Color::Rgb(Rgb::new(1, 2, 3)));
        assert_eq!(s.cell(0, 2).unwrap().fg, Color::Indexed(123));
    }

    #[test]
    fn attributes() {
        let mut e = engine();
        e.feed(b"\x1b[1;3;4mB\x1b[0m");
        let c = e.snapshot().cell(0, 0).cloned().unwrap();
        assert!(c.attrs.bold && c.attrs.italic && c.attrs.underline);
    }

    #[test]
    fn scrolling_keeps_history() {
        let mut e = engine();
        for i in 0..10 {
            e.feed(format!("line{i}\r\n").as_bytes());
        }
        let s = e.snapshot();
        assert!(s.history_size > 0);
        assert_eq!(s.row_text(s.size.rows - 2), "line9");
    }

    #[test]
    fn resize_changes_snapshot() {
        let mut e = engine();
        e.resize(Size::new(30, 8));
        let s = e.snapshot();
        assert_eq!(s.size, Size::new(30, 8));
        assert_eq!(s.cells.len(), 240);
    }

    #[test]
    fn modes_alt_screen_bracketed_paste_and_cursor() {
        let mut e = engine();
        e.feed(b"\x1b[?1049h\x1b[?2004h\x1b[?25l");
        let s = e.snapshot();
        assert!(s.modes.alt_screen);
        assert!(s.modes.bracketed_paste);
        assert!(!s.cursor.visible);
        e.feed(b"\x1b[?1049l\x1b[?2004l\x1b[?25h");
        let s = e.snapshot();
        assert!(!s.modes.alt_screen);
        assert!(!s.modes.bracketed_paste);
        assert!(s.cursor.visible);
    }

    #[test]
    fn osc_title_cwd_and_prompt() {
        let mut e = engine();
        e.feed(b"\x1b]0;cargo test\x07\x1b]7;file://host/home/u/repo\x07\x1b]133;A\x07");
        assert_eq!(e.title(), Some("cargo test"));
        assert_eq!(e.cwd(), Some(&PathBuf::from("/home/u/repo")));
        assert_eq!(
            e.prompt().state,
            termust_core::terminal::PromptState::AtPrompt
        );
    }
}
