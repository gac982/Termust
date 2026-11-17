use std::path::PathBuf;

/// Tamaño de la grilla en celdas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    pub cols: usize,
    pub rows: usize,
}

impl Size {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self { cols, rows }
    }

    pub fn area(&self) -> usize {
        self.cols * self.rows
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Color independiente del motor. Los índices 0–255 son la paleta ANSI/extendida.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    DefaultFg,
    DefaultBg,
    Indexed(u8),
    Rgb(Rgb),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub wide: bool,
    pub wide_spacer: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::DefaultFg,
            bg: Color::DefaultBg,
            attrs: Attrs::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub col: usize,
    pub row: usize,
    pub visible: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    pub alt_screen: bool,
    pub bracketed_paste: bool,
    pub app_cursor: bool,
    pub mouse_reporting: bool,
    pub show_cursor: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub size: Size,
    pub cells: Vec<Cell>,
    pub cursor: Cursor,
    pub modes: Modes,
    pub display_offset: usize,
    pub history_size: usize,
}

impl Snapshot {
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        if row >= self.size.rows || col >= self.size.cols {
            return None;
        }
        self.cells.get(row * self.size.cols + col)
    }

    pub fn row_text(&self, row: usize) -> String {
        if row >= self.size.rows {
            return String::new();
        }
        let start = row * self.size.cols;
        let end = start + self.size.cols;
        let mut out = String::with_capacity(self.size.cols);
        for cell in &self.cells[start..end.min(self.cells.len())] {
            if cell.attrs.wide_spacer {
                continue;
            }
            out.push(cell.ch);
        }
        out.trim_end().to_string()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptState {
    #[default]
    Unknown,
    AtPrompt,
    CommandRunning,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PromptStatus {
    pub state: PromptState,
    pub last_exit: Option<i32>,
}

/// Puerto del motor de emulación VT.
pub trait TerminalEngine {
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, size: Size);
    fn snapshot(&mut self) -> Snapshot;
    fn scroll(&mut self, delta: i32);
    fn title(&self) -> Option<&str>;
    fn cwd(&self) -> Option<&PathBuf>;
    fn prompt(&self) -> PromptStatus;
}
