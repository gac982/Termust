//! Adaptador del motor VT sobre `alacritty_terminal`.

mod osc;

pub use osc::{OscEvent, OscScanner, PromptMark};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape, NamedColor, Processor, StdSyncHandler,
};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use termust_core::terminal::{
    Attrs, Cell, Color, CommandRecord, Cursor, Modes, PromptState, PromptStatus, Rgb, Size,
    Snapshot, TerminalEngine,
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

struct Listener {
    tx: Sender<Event>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.tx.send(event);
    }
}

struct PendingCommand {
    text: String,
    cwd: PathBuf,
    timestamp: u64,
    started: bool,
}

pub struct AlacrittyEngine {
    term: Term<Listener>,
    parser: Proc,
    osc: OscScanner,
    size: Size,
    responses: Receiver<Event>,
    prompt_end: Option<(usize, usize)>,
    vi_mode: bool,
    pending: Option<PendingCommand>,
    history: Vec<CommandRecord>,
}

impl AlacrittyEngine {
    pub fn set_history_limit(&mut self, lines: usize) {
        self.term.grid_mut().update_history(lines);
    }

    pub fn new(size: Size, history: usize) -> Self {
        let cfg = Config {
            scrolling_history: history,
            ..Default::default()
        };
        let (tx, responses) = channel();
        let term = Term::new(
            cfg,
            &Dim {
                cols: size.cols,
                rows: size.rows,
            },
            Listener { tx },
        );
        Self {
            term,
            parser: Proc::new(),
            osc: OscScanner::new(),
            size,
            responses,
            prompt_end: None,
            vi_mode: false,
            pending: None,
            history: Vec::new(),
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
        for event in self.osc.take_events() {
            match event {
                crate::osc::OscEvent::Prompt(crate::osc::PromptMark::CommandStart) => {
                    let content = self.term.renderable_content();
                    self.prompt_end = Some((
                        content.cursor.point.line.0.max(0) as usize,
                        content.cursor.point.column.0,
                    ));
                }
                crate::osc::OscEvent::Prompt(crate::osc::PromptMark::CommandExecuted) => {
                    if let Some(pending) = self.pending.as_mut() {
                        pending.started = true;
                    }
                }
                crate::osc::OscEvent::Prompt(crate::osc::PromptMark::Finished(code)) => {
                    if let Some(pending) = self.pending.take() {
                        let text = pending.text.trim().to_string();
                        if pending.started && !text.is_empty() {
                            self.history.push(CommandRecord {
                                text,
                                cwd: pending.cwd,
                                timestamp: pending.timestamp,
                                exit_code: code,
                            });
                        }
                    }
                }
                crate::osc::OscEvent::Command(text) => {
                    let text = text.trim().to_string();
                    let cwd = self.osc.cwd().cloned().unwrap_or_default();
                    let timestamp = now_secs();
                    if !text.is_empty() {
                        self.history.push(CommandRecord {
                            text: text.clone(),
                            cwd: cwd.clone(),
                            timestamp,
                            exit_code: None,
                        });
                    }
                    self.pending = Some(PendingCommand {
                        text,
                        cwd,
                        timestamp,
                        started: false,
                    });
                }
                crate::osc::OscEvent::ViMode(vi) => self.vi_mode = vi,
                _ => {}
            }
        }
    }

    fn resize(&mut self, size: Size) {
        self.term.resize(Dim {
            cols: size.cols,
            rows: size.rows,
        });
        self.size = size;
    }

    fn set_history_limit(&mut self, lines: usize) {
        self.set_history_limit(lines);
    }

    fn snapshot(&mut self) -> Snapshot {
        let content = self.term.renderable_content();
        let mut cells = vec![Cell::default(); self.size.area()];

        // display_iter usa 0 para la primera fila de la pantalla actual y
        // valores negativos para el scrollback. Hay que sumar display_offset
        // para mapearlo a las filas visibles; si no, el scrollback se descarta.
        let offset = content.display_offset as i32;
        for indexed in content.display_iter {
            let line = indexed.point.line.0 + offset;
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
                row: (content.cursor.point.line.0 + offset).max(0) as usize,
                col: content.cursor.point.column.0,
                visible: content.cursor.shape != CursorShape::Hidden
                    && content.cursor.point.line.0 + offset >= 0
                    && content.cursor.point.line.0 + offset < self.size.rows as i32,
            },
            modes: Modes {
                alt_screen: content.mode.contains(TermMode::ALT_SCREEN),
                bracketed_paste: content.mode.contains(TermMode::BRACKETED_PASTE),
                app_cursor: content.mode.contains(TermMode::APP_CURSOR),
                mouse_click: content.mode.intersects(
                    TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
                ),
                mouse_drag: content
                    .mode
                    .intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
                mouse_motion: content.mode.contains(TermMode::MOUSE_MOTION),
                sgr_mouse: content.mode.contains(TermMode::SGR_MOUSE),
                utf8_mouse: content.mode.contains(TermMode::UTF8_MOUSE),
                show_cursor: content.mode.contains(TermMode::SHOW_CURSOR),
            },
            display_offset: content.display_offset,
            history_size: self.term.grid().history_size(),
        }
    }

    fn scroll(&mut self, delta: i32) -> bool {
        let before = self.term.grid().display_offset();
        self.term.scroll_display(Scroll::Delta(delta));
        self.term.grid().display_offset() != before
    }

    fn title(&self) -> Option<&str> {
        self.osc.title()
    }

    fn running_command(&self) -> Option<&str> {
        self.pending
            .as_ref()
            .filter(|pending| pending.started)
            .map(|pending| pending.text.trim())
            .filter(|text| !text.is_empty())
    }

    fn cwd(&self) -> Option<&PathBuf> {
        self.osc.cwd()
    }

    fn prompt(&self) -> PromptStatus {
        self.osc.prompt()
    }

    fn take_responses(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Ok(event) = self.responses.try_recv() {
            if let Event::PtyWrite(text) = event {
                out.push(text.into_bytes());
            }
        }
        out
    }

    fn history_ready(&mut self) -> bool {
        let content = self.term.renderable_content();
        if content.mode.contains(TermMode::ALT_SCREEN) || self.vi_mode {
            return false;
        }
        if content.display_offset > 0 {
            return false;
        }
        if self.osc.prompt().state != PromptState::AtPrompt {
            return false;
        }
        let cursor = (
            content.cursor.point.line.0.max(0) as usize,
            content.cursor.point.column.0,
        );
        self.prompt_end == Some(cursor)
    }

    fn take_history(&mut self) -> Vec<CommandRecord> {
        std::mem::take(&mut self.history)
    }

    fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
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
    fn scrolling_renders_scrollback() {
        let mut e = engine();
        for i in 0..10 {
            e.feed(format!("line{i}\r\n").as_bytes());
        }
        assert!(e.scroll(3));
        let s = e.snapshot();
        let rows: Vec<String> = (0..s.size.rows).map(|row| s.row_text(row)).collect();
        assert_eq!(rows, vec!["line3", "line4", "line5", "line6", "line7"]);
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
    fn scroll_reports_only_real_movement() {
        let mut e = engine();
        assert!(!e.scroll(3));
        for i in 0..10 {
            e.feed(format!("line{i}\r\n").as_bytes());
        }
        assert!(e.scroll(3));
        assert!(e.scroll(-3));
        assert!(!e.scroll(-3));
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
    fn history_capture_and_readiness() {
        let mut e = engine();
        e.feed(b"\x1b]133;A\x07\x1b]133;B\x07");
        assert!(e.history_ready());
        e.feed(b"ls");
        assert!(!e.history_ready());
        e.feed(b"\x08\x08");
        assert!(e.history_ready());
        e.feed(b"\x1b]633;E;cargo test\x07\x1b]133;C\x07");
        e.feed(b"\x1b]133;D;3\x07");
        let history = e.take_history();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].text, "cargo test");
        assert_eq!(history[0].exit_code, None);
        assert_eq!(history[1].text, "cargo test");
        assert_eq!(history[1].exit_code, Some(3));
        assert!(e.take_history().is_empty());
    }

    #[test]
    fn running_command_follows_osc133_marks() {
        let mut e = engine();
        assert_eq!(e.running_command(), None);
        e.feed(b"\x1b]133;A\x07\x1b]133;B\x07");
        assert_eq!(e.running_command(), None);
        e.feed(b"\x1b]633;E;btop\x07\x1b]133;C\x07");
        assert_eq!(e.running_command(), Some("btop"));
        e.feed(b"\x1b]133;D;0\x07");
        assert_eq!(e.running_command(), None);
    }

    #[test]
    fn history_index_from_osc133() {
        let mut e = engine();
        e.feed(b"\x1b]633;E;ls -la\x07\x1b]133;C\x07");
        e.feed(b"\x1b]133;D;1\x07");
        let history = e.take_history();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].text, "ls -la");
        assert_eq!(history[0].exit_code, None);
        assert_eq!(history[1].text, "ls -la");
        assert_eq!(history[1].exit_code, Some(1));
    }

    #[test]
    fn history_ready_rejects_alt_screen_and_vi_mode() {
        let mut e = engine();
        e.feed(b"\x1b]133;A\x07\x1b]133;B\x07");
        assert!(e.history_ready());
        e.feed(b"\x1b[?1049h");
        assert!(!e.history_ready());
        e.feed(b"\x1b[?1049l\x1b]633;V;vicmd\x07");
        assert!(!e.history_ready());
        e.feed(b"\x1b]633;V;main\x07");
        assert!(e.history_ready());
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
    fn mouse_modes_are_detailed() {
        let mut e = engine();
        e.feed(b"\x1b[?1002h\x1b[?1006h");
        let s = e.snapshot();
        assert!(s.modes.reports_mouse());
        assert!(s.modes.mouse_click);
        assert!(s.modes.mouse_drag);
        assert!(!s.modes.mouse_motion);
        assert!(s.modes.sgr_mouse);
        assert!(!s.modes.utf8_mouse);

        e.feed(b"\x1b[?1002l\x1b[?1003h\x1b[?1006l\x1b[?1005h");
        let s = e.snapshot();
        assert!(s.modes.mouse_click);
        assert!(s.modes.mouse_drag);
        assert!(s.modes.mouse_motion);
        assert!(!s.modes.sgr_mouse);
        assert!(s.modes.utf8_mouse);

        e.feed(b"\x1b[?1000l\x1b[?1003l\x1b[?1005l");
        let s = e.snapshot();
        assert!(!s.modes.reports_mouse());
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

#[cfg(test)]
mod throughput_tests {
    use super::*;

    #[test]
    #[ignore]
    fn raw_alacritty_throughput() {
        let size = Size::new(120, 40);
        let dim = Dim {
            cols: size.cols,
            rows: size.rows,
        };
        let (tx, _rx) = channel();
        let mut term = Term::new(
            Config {
                scrolling_history: 2_000,
                ..Default::default()
            },
            &dim,
            Listener { tx },
        );
        let mut parser = Proc::new();
        let mut payload = String::with_capacity(10 * 1024 * 1024);
        while payload.len() < 10 * 1024 * 1024 {
            payload.push_str(&"x".repeat(120));
            payload.push('\n');
        }
        let bytes = payload.as_bytes();
        let start = std::time::Instant::now();
        for part in bytes.chunks(64 * 1024) {
            parser.advance(&mut term, part);
        }
        let elapsed = start.elapsed();
        eprintln!(
            "raw alacritty: {:.1} MB/s",
            bytes.len() as f64 / 1024.0 / 1024.0 / elapsed.as_secs_f64()
        );
    }
}
