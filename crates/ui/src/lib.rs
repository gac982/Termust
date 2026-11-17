//! Vista de terminal GTK4: dibuja la grilla y traduce teclado/ratón al PTY.

#![forbid(unsafe_code)]

use cairo::Context;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use termust_core::pty::{PtyEvent, PtySession};
use termust_core::terminal::{Modes, Size, Snapshot, TerminalEngine};
use termust_text_render::{CellMetrics, FontSpec, PaintOpts, Selection, TextRenderer, Theme};

pub struct TerminalView {
    area: gtk::DrawingArea,
    theme: Rc<RefCell<Theme>>,
    font: Rc<FontSpec>,
    selection: Rc<Cell<Option<Selection>>>,
    exited: Rc<Cell<bool>>,
}

impl TerminalView {
    pub fn new(
        engine: Box<dyn TerminalEngine>,
        session: Box<dyn PtySession>,
        renderer: Box<dyn TextRenderer>,
        events: Receiver<PtyEvent>,
    ) -> Self {
        let area = gtk::DrawingArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_focusable(true);
        area.set_can_focus(true);

        let engine = Rc::new(RefCell::new(engine));
        let session = Rc::new(RefCell::new(session));
        let renderer = Rc::new(renderer);
        let font = Rc::new(FontSpec::default());
        let theme = Rc::new(RefCell::new(Theme::default()));
        let metrics = Rc::new(Cell::new(compute_metrics(&**renderer, &font)));
        let selection: Rc<Cell<Option<Selection>>> = Rc::new(Cell::new(None));
        let last_modes = Rc::new(Cell::new(Modes::default()));
        let exited = Rc::new(Cell::new(false));
        let last_size = Rc::new(Cell::new((0usize, 0usize)));

        {
            let renderer = renderer.clone();
            let font = font.clone();
            let theme = theme.clone();
            let engine = engine.clone();
            let session = session.clone();
            let selection = selection.clone();
            let metrics = metrics.clone();
            let last_modes = last_modes.clone();
            let last_size = last_size.clone();
            area.set_draw_func(move |_, cr, w, h| {
                let m = renderer.metrics(cr, &font);
                metrics.set(m);
                let cols = m.cols_for_width(w as f64);
                let rows = m.rows_for_height(h as f64);
                if cols > 0 && rows > 0 && (cols, rows) != last_size.get() {
                    last_size.set((cols, rows));
                    engine.borrow_mut().resize(Size::new(cols, rows));
                    let _ = session.borrow_mut().resize(Size::new(cols, rows));
                }
                let snapshot = engine.borrow_mut().snapshot();
                last_modes.set(snapshot.modes);
                let opts = PaintOpts {
                    font: (*font).clone(),
                    selection: selection.get(),
                    bg_alpha: 1.0,
                };
                renderer.paint(cr, &snapshot, &theme.borrow(), &opts);
            });
        }

        {
            let engine = engine.clone();
            let session = session.clone();
            let selection = selection.clone();
            let modes = last_modes.clone();
            let area_key = area.clone();
            let key = gtk::EventControllerKey::new();
            key.connect_key_pressed(move |_, keyval, _code, state| {
                let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                let ch = keyval.to_unicode();
                let copy =
                    ctrl && shift && ch.map(|c| c.eq_ignore_ascii_case(&'c')).unwrap_or(false);
                let paste =
                    ctrl && shift && ch.map(|c| c.eq_ignore_ascii_case(&'v')).unwrap_or(false);
                if copy {
                    let snapshot = engine.borrow_mut().snapshot();
                    if let Some(text) = copy_selection(&snapshot, selection.get()) {
                        area_key.clipboard().set_text(&text);
                    }
                    return glib::Propagation::Stop;
                }
                if paste {
                    paste_clipboard(&area_key, session.clone(), modes.clone());
                    return glib::Propagation::Stop;
                }
                if let Some(bytes) = encode_key(keyval, state, modes.get()) {
                    let _ = session.borrow_mut().write(&bytes);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            area.add_controller(key);
        }

        {
            let engine = engine.clone();
            let area_scroll = area.clone();
            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            scroll.connect_scroll(move |_, _dx, dy| {
                engine.borrow_mut().scroll(if dy > 0.0 { -3 } else { 3 });
                area_scroll.queue_draw();
                glib::Propagation::Stop
            });
            area.add_controller(scroll);
        }

        {
            let selection = selection.clone();
            let metrics = metrics.clone();
            let area_drag = area.clone();
            let start_px = Rc::new(Cell::new((0.0f64, 0.0f64)));
            let drag = gtk::GestureDrag::new();
            let drag_begin = drag.clone();
            let area_draw = area_drag.clone();
            let selection_begin = selection.clone();
            let metrics_begin = metrics.clone();
            let start_begin = start_px.clone();
            drag_begin.connect_drag_begin(move |_, x, y| {
                start_begin.set((x, y));
                let cell = cell_at(x, y, metrics_begin.get());
                selection_begin.set(Some(Selection {
                    start: cell,
                    end: cell,
                }));
                area_draw.queue_draw();
            });
            let area_draw = area_drag.clone();
            let selection_update = selection.clone();
            let metrics_update = metrics.clone();
            let start_update = start_px.clone();
            drag.connect_drag_update(move |_, ox, oy| {
                let (sx, sy) = start_update.get();
                let end = cell_at(sx + ox, sy + oy, metrics_update.get());
                if let Some(mut s) = selection_update.get() {
                    s.end = end;
                    selection_update.set(Some(s));
                }
                area_draw.queue_draw();
            });
            area.add_controller(drag);
        }

        {
            let session = session.clone();
            area.connect_destroy(move |_| {
                let _ = session.borrow_mut().kill();
            });
        }

        {
            let area_click = area.clone();
            let click = gtk::GestureClick::new();
            click.connect_pressed(move |_, _, _, _| {
                area_click.grab_focus();
            });
            area.add_controller(click);
        }

        {
            let engine = engine.clone();
            let exited = exited.clone();
            let area_weak = area.downgrade();
            glib::timeout_add_local(Duration::from_millis(16), move || {
                let mut got = false;
                while let Ok(ev) = events.try_recv() {
                    match ev {
                        PtyEvent::Output(bytes) => {
                            engine.borrow_mut().feed(&bytes);
                            got = true;
                        }
                        PtyEvent::Exited(_) => {
                            exited.set(true);
                            got = true;
                        }
                        PtyEvent::Error(_) => {
                            got = true;
                        }
                    }
                }
                if got {
                    if let Some(a) = area_weak.upgrade() {
                        a.queue_draw();
                    }
                }
                if area_weak.upgrade().is_some() {
                    glib::ControlFlow::Continue
                } else {
                    glib::ControlFlow::Break
                }
            });
        }

        Self {
            area,
            theme,
            font,
            selection,
            exited,
        }
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    pub fn grab_focus(&self) {
        self.area.grab_focus();
    }

    pub fn set_theme(&self, theme: Theme) {
        *self.theme.borrow_mut() = theme;
        self.area.queue_draw();
    }

    pub fn set_font(&mut self, font: FontSpec) {
        self.font = Rc::new(font);
        self.area.queue_draw();
    }

    pub fn exited(&self) -> bool {
        self.exited.get()
    }

    pub fn clear_selection(&self) {
        self.selection.set(None);
        self.area.queue_draw();
    }
}

fn compute_metrics(renderer: &dyn TextRenderer, font: &FontSpec) -> CellMetrics {
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 8, 16).expect("cairo surface");
    let ctx = Context::new(&surface).expect("cairo context");
    renderer.metrics(&ctx, font)
}

fn cell_at(x: f64, y: f64, m: CellMetrics) -> (usize, usize) {
    let col = (x.max(0.0) / m.width).floor() as usize;
    let row = (y.max(0.0) / m.height).floor() as usize;
    (row, col)
}

fn copy_selection(snapshot: &Snapshot, selection: Option<Selection>) -> Option<String> {
    let sel = selection?;
    let s = sel.normalized();
    if snapshot.size.rows == 0 || snapshot.size.cols == 0 {
        return None;
    }
    let mut out = String::new();
    let last_row = s.end.0.min(snapshot.size.rows - 1);
    for row in s.start.0.min(last_row)..=last_row {
        let start_col = if row == s.start.0 { s.start.1 } else { 0 };
        let end_col = if row == s.end.0 {
            s.end.1.min(snapshot.size.cols - 1)
        } else {
            snapshot.size.cols - 1
        };
        let mut line = String::new();
        for col in start_col..=end_col {
            if let Some(cell) = snapshot.cell(row, col) {
                if !cell.attrs.wide_spacer {
                    line.push(cell.ch);
                }
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    let trimmed = out.trim_end().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn paste_clipboard(
    area: &gtk::DrawingArea,
    session: Rc<RefCell<Box<dyn PtySession>>>,
    modes: Rc<Cell<Modes>>,
) {
    let bracketed = modes.get().bracketed_paste;
    area.clipboard()
        .read_text_async(None::<&gio::Cancellable>, move |res| {
            if let Ok(Some(text)) = res {
                let mut bytes = Vec::with_capacity(text.len() + 12);
                if bracketed {
                    bytes.extend_from_slice(b"\x1b[200~");
                }
                bytes.extend_from_slice(text.as_bytes());
                if bracketed {
                    bytes.extend_from_slice(b"\x1b[201~");
                }
                let _ = session.borrow_mut().write(&bytes);
            }
        });
}

/// Traduce una tecla de GDK a bytes del terminal.
pub fn encode_key(key: gdk::Key, mods: gdk::ModifierType, modes: Modes) -> Option<Vec<u8>> {
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    let alt = mods.contains(gdk::ModifierType::ALT_MASK);
    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }

    if ctrl {
        if let Some(ch) = key.to_unicode() {
            let l = ch.to_ascii_lowercase();
            if l.is_ascii_lowercase() {
                out.push(l as u8 - b'a' + 1);
                return Some(out);
            }
            match l {
                '[' => out.push(0x1b),
                '\\' => out.push(0x1c),
                ']' => out.push(0x1d),
                '^' => out.push(0x1e),
                '_' => out.push(0x1f),
                ' ' | '@' => out.push(0),
                _ => return None,
            }
            return Some(out);
        }
    }

    let seq = match key {
        gdk::Key::Return => Some(b"\r".to_vec()),
        gdk::Key::BackSpace => Some(vec![0x7f]),
        gdk::Key::Tab => Some(b"\t".to_vec()),
        gdk::Key::Escape => Some(vec![0x1b]),
        gdk::Key::Up => Some(arrow(b'A', modes.app_cursor)),
        gdk::Key::Down => Some(arrow(b'B', modes.app_cursor)),
        gdk::Key::Right => Some(arrow(b'C', modes.app_cursor)),
        gdk::Key::Left => Some(arrow(b'D', modes.app_cursor)),
        gdk::Key::Home => Some(arrow(b'H', modes.app_cursor)),
        gdk::Key::End => Some(arrow(b'F', modes.app_cursor)),
        gdk::Key::Delete => Some(b"\x1b[3~".to_vec()),
        gdk::Key::Page_Up => Some(b"\x1b[5~".to_vec()),
        gdk::Key::Page_Down => Some(b"\x1b[6~".to_vec()),
        gdk::Key::F1 => Some(b"\x1bOP".to_vec()),
        gdk::Key::F2 => Some(b"\x1bOQ".to_vec()),
        gdk::Key::F3 => Some(b"\x1bOR".to_vec()),
        gdk::Key::F4 => Some(b"\x1bOS".to_vec()),
        gdk::Key::F5 => Some(b"\x1b[15~".to_vec()),
        gdk::Key::F6 => Some(b"\x1b[17~".to_vec()),
        gdk::Key::F7 => Some(b"\x1b[18~".to_vec()),
        gdk::Key::F8 => Some(b"\x1b[19~".to_vec()),
        gdk::Key::F9 => Some(b"\x1b[20~".to_vec()),
        gdk::Key::F10 => Some(b"\x1b[21~".to_vec()),
        gdk::Key::F11 => Some(b"\x1b[23~".to_vec()),
        gdk::Key::F12 => Some(b"\x1b[24~".to_vec()),
        _ => None,
    };
    if let Some(seq) = seq {
        out.extend_from_slice(&seq);
        return Some(out);
    }

    if let Some(ch) = key.to_unicode() {
        if !ctrl {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            return Some(out);
        }
    }
    None
}

fn arrow(final_byte: u8, app_cursor: bool) -> Vec<u8> {
    if app_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termust_core::terminal::{Cell, Cursor, Size};

    #[test]
    fn plain_and_control_keys() {
        let none = gdk::ModifierType::empty();
        assert_eq!(
            encode_key(gdk::Key::Return, none, Modes::default()),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            encode_key(gdk::Key::a, none, Modes::default()),
            Some(b"a".to_vec())
        );
        assert_eq!(
            encode_key(
                gdk::Key::c,
                gdk::ModifierType::CONTROL_MASK,
                Modes::default()
            ),
            Some(vec![3])
        );
        assert_eq!(
            encode_key(gdk::Key::x, gdk::ModifierType::ALT_MASK, Modes::default()),
            Some(vec![0x1b, b'x'])
        );
    }

    #[test]
    fn arrows_normal_and_application() {
        let none = gdk::ModifierType::empty();
        assert_eq!(
            encode_key(gdk::Key::Up, none, Modes::default()),
            Some(b"\x1b[A".to_vec())
        );
        let app = Modes {
            app_cursor: true,
            ..Modes::default()
        };
        assert_eq!(
            encode_key(gdk::Key::Up, none, app),
            Some(b"\x1bOA".to_vec())
        );
    }

    #[test]
    fn selection_copy() {
        let mut snap = Snapshot {
            size: Size::new(5, 2),
            cells: vec![Cell::default(); 10],
            cursor: Cursor::default(),
            modes: Modes::default(),
            display_offset: 0,
            history_size: 0,
        };
        for (i, ch) in "hello".chars().enumerate() {
            snap.cells[i].ch = ch;
        }
        let sel = Selection {
            start: (0, 0),
            end: (0, 4),
        };
        assert_eq!(copy_selection(&snap, Some(sel)).as_deref(), Some("hello"));
        assert_eq!(copy_selection(&snap, None), None);
    }
}
