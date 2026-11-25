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
    /// 1000/1002/1003: cualquier modo de reporte de ratón.
    pub mouse_click: bool,
    /// 1002/1003: reportar movimiento mientras hay un botón presionado.
    pub mouse_drag: bool,
    /// 1003: reportar todo movimiento.
    pub mouse_motion: bool,
    /// 1006: coordenadas SGR.
    pub sgr_mouse: bool,
    /// 1005: coordenadas UTF-8 en el protocolo legacy.
    pub utf8_mouse: bool,
    pub show_cursor: bool,
}

impl Modes {
    pub fn reports_mouse(&self) -> bool {
        self.mouse_click || self.mouse_drag || self.mouse_motion
    }
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

/// Comando completado, capturado por OSC 133/633.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandRecord {
    pub text: String,
    pub cwd: std::path::PathBuf,
    pub timestamp: u64,
    pub exit_code: Option<i32>,
}

/// Puerto del motor de emulación VT.
pub trait TerminalEngine {
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, size: Size);
    fn snapshot(&mut self) -> Snapshot;

    /// Ajusta el techo de scrollback (cap global entre paneles).
    fn set_history_limit(&mut self, _lines: usize) {}
    /// Mueve el scrollback; devuelve si el desplazamiento cambió.
    fn scroll(&mut self, delta: i32) -> bool;
    fn title(&self) -> Option<&str>;
    fn cwd(&self) -> Option<&PathBuf>;
    fn prompt(&self) -> PromptStatus;

    /// Comando que el shell está ejecutando ahora (OSC 133;C), si lo reporta.
    /// Funciona aunque el PTY viva en el host (Flatpak), donde `/proc` no es
    /// visible desde el sandbox.
    fn running_command(&self) -> Option<&str> {
        None
    }

    /// Respuestas que el motor debe escribir de vuelta al PTY (DSR, DA, etc.).
    fn take_responses(&mut self) -> Vec<Vec<u8>> {
        Vec::new()
    }

    /// ¿El prompt está vacío y listo para abrir el historial? (FR-13.2)
    fn history_ready(&mut self) -> bool {
        false
    }

    /// Comandos completados desde la última llamada.
    fn take_history(&mut self) -> Vec<CommandRecord> {
        Vec::new()
    }

    fn bracketed_paste(&self) -> bool {
        false
    }
}
