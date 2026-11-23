//! Vista de terminal GTK4: dibuja la grilla y traduce teclado/ratón al PTY.

#![forbid(unsafe_code)]

use cairo::Context;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use termust_core::mouse::{encode_mouse, MouseButton, MouseEventKind, MouseMods};
use termust_core::pty::{PtyEvent, PtySession};
use termust_core::terminal::{CommandRecord, Modes, PromptState, Size, Snapshot, TerminalEngine};
use termust_text_render::{CellMetrics, FontSpec, PaintOpts, Selection, TextRenderer, Theme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GridSize {
    rows: usize,
    cols: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WheelTarget {
    Pty,
    Scrollback,
}

/// En el prompt la rueda siempre recorre el scrollback, aunque el shell tenga
/// reporte de ratón activo: mandársela a zle redibuja el prompt y lo hace
/// parpadear. En pantalla alternativa (btop, less) la rueda va a la app.
fn wheel_target(modes: Modes, prompt: PromptState, shift: bool) -> WheelTarget {
    if modes.reports_mouse() && !shift && (modes.alt_screen || prompt != PromptState::AtPrompt) {
        WheelTarget::Pty
    } else {
        WheelTarget::Scrollback
    }
}

pub struct TerminalView {
    area: gtk::DrawingArea,
    engine: Rc<RefCell<Box<dyn TerminalEngine>>>,
    session: Rc<RefCell<Box<dyn PtySession>>>,
    theme: Rc<RefCell<Theme>>,
    font: Rc<RefCell<FontSpec>>,
    renderer: Rc<Box<dyn TextRenderer>>,
    selection: Rc<Cell<Option<Selection>>>,
    bg_alpha: Rc<Cell<f64>>,
    frames: Rc<Cell<u64>>,
    resize_count: Rc<Cell<usize>>,
    exited: Rc<Cell<bool>>,
    exit_code: Rc<Cell<Option<i32>>>,
    last_output: Rc<Cell<Option<std::time::Instant>>>,
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
        area.set_accessible_role(gtk::AccessibleRole::TextBox);
        area.update_property(&[
            gtk::accessible::Property::Label("Terminal"),
            gtk::accessible::Property::Description("Salida del terminal"),
            gtk::accessible::Property::ReadOnly(true),
        ]);

        let engine = Rc::new(RefCell::new(engine));
        let session = Rc::new(RefCell::new(session));
        let renderer = Rc::new(renderer);
        let font = Rc::new(RefCell::new(system_monospace_font()));
        let theme = Rc::new(RefCell::new(Theme::default()));
        let metrics = Rc::new(Cell::new(compute_metrics(
            &**renderer,
            &font.borrow().clone(),
        )));
        let selection: Rc<Cell<Option<Selection>>> = Rc::new(Cell::new(None));
        let bg_alpha = Rc::new(Cell::new(1.0));
        let frames = Rc::new(Cell::new(0));
        let resize_count = Rc::new(Cell::new(0));
        let exit_code = Rc::new(Cell::new(None));
        let last_output = Rc::new(Cell::new(None));
        let last_modes = Rc::new(Cell::new(Modes::default()));
        let exited = Rc::new(Cell::new(false));
        let last_size = Rc::new(Cell::new(GridSize::default()));
        // El tamaño aplicado al motor/PTY. Durante una animación de layout el
        // área cambia en cada frame; redimensionar en cada frame manda una
        // tormenta de SIGWINCH y el shell redibuja el prompt varias veces,
        // duplicándolo. Se aplica un solo resize cuando el tamaño se estabiliza.
        let applied_size = Rc::new(Cell::new(GridSize::default()));
        let pending_size: Rc<Cell<Option<GridSize>>> = Rc::new(Cell::new(None));
        let resize_timer: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
        let apply_resize: Rc<dyn Fn(GridSize)> = {
            let engine = engine.clone();
            let session = session.clone();
            let area = area.clone();
            let last_size = last_size.clone();
            let applied_size = applied_size.clone();
            let resize_count = resize_count.clone();
            Rc::new(move |grid: GridSize| {
                last_size.set(grid);
                applied_size.set(grid);
                engine.borrow_mut().resize(Size::new(grid.cols, grid.rows));
                let _ = session.borrow_mut().resize(Size::new(grid.cols, grid.rows));
                resize_count.set(resize_count.get() + 1);
                area.queue_draw();
            })
        };

        {
            let renderer = renderer.clone();
            let font = font.clone();
            let theme = theme.clone();
            let engine = engine.clone();
            let selection = selection.clone();
            let bg_alpha = bg_alpha.clone();
            let frames = frames.clone();
            let apply_resize = apply_resize.clone();
            let applied_size = applied_size.clone();
            let pending_size = pending_size.clone();
            let resize_timer = resize_timer.clone();
            let metrics = metrics.clone();
            let last_modes = last_modes.clone();
            let last_size = last_size.clone();
            area.set_draw_func(move |_, cr, w, h| {
                frames.set(frames.get() + 1);
                let font = font.borrow().clone();
                let m = renderer.metrics(cr, &font);
                metrics.set(m);
                let cols = m.cols_for_width(w as f64);
                let rows = m.rows_for_height(h as f64);
                let grid = GridSize { rows, cols };
                if cols > 0 && rows > 0 && grid != last_size.get() {
                    // Campos con nombre: un tuple (cols, rows) invertido
                    // mandaba todos los clics a la columna equivocada.
                    if applied_size.get() == GridSize::default() {
                        // Primer tamaño: aplicarlo ya para que el PTY nazca
                        // con la grilla correcta.
                        pending_size.set(None);
                        if let Some(id) = resize_timer.take() {
                            id.remove();
                        }
                        apply_resize(grid);
                    } else if grid == applied_size.get() {
                        pending_size.set(None);
                        if let Some(id) = resize_timer.take() {
                            id.remove();
                        }
                    } else {
                        pending_size.set(Some(grid));
                        if let Some(id) = resize_timer.take() {
                            id.remove();
                        }
                        let apply = apply_resize.clone();
                        let pending = pending_size.clone();
                        let timer = resize_timer.clone();
                        let id =
                            glib::timeout_add_local_once(Duration::from_millis(150), move || {
                                timer.set(None);
                                if let Some(grid) = pending.take() {
                                    apply(grid);
                                }
                            });
                        resize_timer.set(Some(id));
                    }
                }
                let snapshot = engine.borrow_mut().snapshot();
                last_modes.set(snapshot.modes);
                let opts = PaintOpts {
                    font,
                    selection: selection.get(),
                    bg_alpha: bg_alpha.get(),
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

        let pointer = Rc::new(Cell::new((0.0f64, 0.0f64)));

        {
            let engine = engine.clone();
            let session = session.clone();
            let modes = last_modes.clone();
            let metrics = metrics.clone();
            let last_size = last_size.clone();
            let pointer = pointer.clone();
            let area_scroll = area.clone();
            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            scroll.connect_scroll(move |controller, _dx, dy| {
                let m_modes = modes.get();
                let state = controller.current_event_state();
                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                let prompt = engine.borrow().prompt().state;
                if std::env::var_os("TERMUST_SCROLL_LOG").is_some() {
                    eprintln!(
                        "termust-scroll: dy={dy} shift={shift} mouse={} alt={} prompt={prompt:?}",
                        m_modes.reports_mouse(),
                        m_modes.alt_screen
                    );
                }
                if wheel_target(m_modes, prompt, shift) == WheelTarget::Pty {
                    let m = metrics.get();
                    if m.width <= 0.0 || m.height <= 0.0 {
                        return glib::Propagation::Stop;
                    }
                    let (x, y) = pointer.get();
                    let grid = last_size.get();
                    let (row, col) = clamp_cell(cell_at(x, y, m), grid.rows, grid.cols);
                    let button = if dy > 0.0 {
                        MouseButton::WheelDown
                    } else {
                        MouseButton::WheelUp
                    };
                    if std::env::var_os("TERMUST_SCROLL_LOG").is_some() {
                        eprintln!("termust-scroll: rueda al PTY ({button:?})");
                    }
                    write_mouse(
                        &session,
                        MouseEventKind::Press,
                        button,
                        row,
                        col,
                        state,
                        m_modes,
                    );
                    return glib::Propagation::Stop;
                }
                if std::env::var_os("TERMUST_SCROLL_LOG").is_some() {
                    let before = engine.borrow_mut().snapshot().display_offset;
                    engine.borrow_mut().scroll(if dy > 0.0 { -3 } else { 3 });
                    let after = engine.borrow_mut().snapshot().display_offset;
                    eprintln!("termust-scroll: scrollback {before} -> {after}");
                } else {
                    engine.borrow_mut().scroll(if dy > 0.0 { -3 } else { 3 });
                }
                area_scroll.queue_draw();
                glib::Propagation::Stop
            });
            area.add_controller(scroll);
        }

        {
            let engine = engine.clone();
            let session = session.clone();
            let modes = last_modes.clone();
            let selection = selection.clone();
            let metrics = metrics.clone();
            let last_size = last_size.clone();
            let pointer = pointer.clone();
            let area_input = area.clone();
            let anchor: Rc<Cell<Option<(usize, usize)>>> = Rc::new(Cell::new(None));
            let press_px = Rc::new(Cell::new((0.0f64, 0.0f64)));
            let pressed_button: Rc<Cell<Option<MouseButton>>> = Rc::new(Cell::new(None));
            let moved = Rc::new(Cell::new(false));

            let click = gtk::GestureClick::new();
            {
                let session = session.clone();
                let modes = modes.clone();
                let selection = selection.clone();
                let metrics = metrics.clone();
                let last_size = last_size.clone();
                let pointer = pointer.clone();
                let anchor = anchor.clone();
                let press_px = press_px.clone();
                let pressed_button = pressed_button.clone();
                let moved = moved.clone();
                let area_input = area_input.clone();
                click.connect_pressed(move |gesture, _n_press, x, y| {
                    area_input.grab_focus();
                    pointer.set((x, y));
                    let state = gesture.current_event_state();
                    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                    let m = metrics.get();
                    let grid = last_size.get();
                    if m.width <= 0.0 || m.height <= 0.0 {
                        return;
                    }
                    let (row, col) = clamp_cell(cell_at(x, y, m), grid.rows, grid.cols);
                    let m_modes = modes.get();
                    let button = gesture.current_button();
                    if std::env::var_os("TERMUST_MOUSE_LOG").is_some() {
                        eprintln!(
                            "termust-mouse: press button={button} row={row} col={col} x={x:.1} y={y:.1} cw={:.2} ch={:.2} grid={:?} snap={:?} modes={m_modes:?} shift={shift}",
                            m.width,
                            m.height,
                            grid,
                            engine.borrow_mut().snapshot().size
                        );
                    }
                    if m_modes.reports_mouse() && !shift {
                        selection.set(None);
                        anchor.set(None);
                        area_input.queue_draw();
                        if let Some(b) = gdk_mouse_button(button) {
                            write_mouse(
                                &session,
                                MouseEventKind::Press,
                                b,
                                row,
                                col,
                                state,
                                m_modes,
                            );
                            pressed_button.set(Some(b));
                        }
                        return;
                    }
                    match button {
                        1 => {
                            selection.set(None);
                            anchor.set(Some((row, col)));
                            press_px.set((x, y));
                            moved.set(false);
                            area_input.queue_draw();
                        }
                        2 => {
                            paste_from(
                                area_input.primary_clipboard(),
                                session.clone(),
                                modes.clone(),
                            );
                        }
                        _ => {}
                    }
                });
            }

            {
                let session = session.clone();
                let modes = modes.clone();
                let metrics = metrics.clone();
                let last_size = last_size.clone();
                let pointer = pointer.clone();
                let pressed_button = pressed_button.clone();
                let anchor = anchor.clone();
                click.connect_released(move |gesture, _n_press, x, y| {
                    pointer.set((x, y));
                    let m = metrics.get();
                    let grid = last_size.get();
                    if let Some(b) = pressed_button.get() {
                        let state = gesture.current_event_state();
                        let (row, col) = clamp_cell(cell_at(x, y, m), grid.rows, grid.cols);
                        write_mouse(
                            &session,
                            MouseEventKind::Release,
                            b,
                            row,
                            col,
                            state,
                            modes.get(),
                        );
                        pressed_button.set(None);
                    }
                    anchor.set(None);
                });
            }
            area.add_controller(click);

            let motion = gtk::EventControllerMotion::new();
            {
                let pointer = pointer.clone();
                motion.connect_enter(move |_, x, y| pointer.set((x, y)));
            }
            {
                let session = session.clone();
                let modes = modes.clone();
                let selection = selection.clone();
                let metrics = metrics.clone();
                let last_size = last_size.clone();
                let pointer = pointer.clone();
                let anchor = anchor.clone();
                let press_px = press_px.clone();
                let pressed_button = pressed_button.clone();
                let moved = moved.clone();
                let area_input = area_input.clone();
                motion.connect_motion(move |controller, x, y| {
                    pointer.set((x, y));
                    let m = metrics.get();
                    let grid = last_size.get();
                    if m.width <= 0.0 || m.height <= 0.0 {
                        return;
                    }
                    let state = controller.current_event_state();
                    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                    let m_modes = modes.get();
                    let tracking = m_modes.mouse_motion
                        || (m_modes.mouse_drag && pressed_button.get().is_some());
                    if m_modes.reports_mouse() && !shift && tracking {
                        let (row, col) = clamp_cell(cell_at(x, y, m), grid.rows, grid.cols);
                        let button = pressed_button.get().unwrap_or(MouseButton::None);
                        write_mouse(
                            &session,
                            MouseEventKind::Motion,
                            button,
                            row,
                            col,
                            state,
                            m_modes,
                        );
                        return;
                    }
                    // Sin mouse tracking, un click no selecciona: la
                    // selección empieza recién cuando hay arrastre.
                    let Some(start) = anchor.get() else {
                        return;
                    };
                    let (px, py) = press_px.get();
                    if !moved.get() && (x - px).hypot(y - py) < 3.0 {
                        return;
                    }
                    moved.set(true);
                    let end = clamp_cell(cell_at(x, y, m), grid.rows, grid.cols);
                    selection.set(Some(Selection { start, end }));
                    area_input.queue_draw();
                });
            }
            area.add_controller(motion);
        }

        {
            let engine = engine.clone();
            let session = session.clone();
            let exited = exited.clone();
            let exit_code = exit_code.clone();
            let last_output = last_output.clone();
            let area_weak = area.downgrade();
            glib::timeout_add_local(Duration::from_millis(16), move || {
                let mut got = false;
                while let Ok(ev) = events.try_recv() {
                    match ev {
                        PtyEvent::Output(bytes) => {
                            last_output.set(Some(std::time::Instant::now()));
                            {
                                let mut e = engine.borrow_mut();
                                e.feed(&bytes);
                                for response in e.take_responses() {
                                    let _ = session.borrow_mut().write(&response);
                                }
                            }
                            got = true;
                        }
                        PtyEvent::Exited(code) => {
                            exited.set(true);
                            exit_code.set(code);
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
            engine,
            session,
            theme,
            font,
            renderer,
            selection,
            bg_alpha,
            frames,
            resize_count,
            exited,
            exit_code,
            last_output,
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

    pub fn set_font(&self, font: FontSpec) {
        *self.font.borrow_mut() = font;
        self.area.queue_draw();
    }

    /// Alpha del fondo (1.0 opaco, 0.0 transparente). El texto y el cursor
    /// siguen opacos; FR-9.1.
    pub fn set_bg_alpha(&self, alpha: f64) {
        self.bg_alpha.set(alpha.clamp(0.0, 1.0));
        self.area.queue_draw();
    }

    pub fn bg_alpha(&self) -> f64 {
        self.bg_alpha.get()
    }

    pub fn font_family(&self) -> String {
        self.font.borrow().family.clone()
    }

    pub fn font_size(&self) -> f64 {
        self.font.borrow().size
    }

    /// Fotogramas pintados; se usa para medir el costo de la transparencia.
    pub fn frame_count(&self) -> u64 {
        self.frames.get()
    }

    /// Pinta `iterations` cuadros fuera de pantalla y devuelve el tiempo medio.
    /// Mide el costo del renderer con el alpha actual (FR-9.4).
    pub fn render_benchmark(&self, width: i32, height: i32, iterations: u32) -> Duration {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height)
            .expect("superficie de benchmark");
        let ctx = Context::new(&surface).expect("contexto de benchmark");
        let snapshot = self.engine.borrow_mut().snapshot();
        let opts = PaintOpts {
            font: self.font.borrow().clone(),
            selection: None,
            bg_alpha: self.bg_alpha.get(),
        };
        let start = Instant::now();
        for _ in 0..iterations.max(1) {
            self.renderer
                .paint(&ctx, &snapshot, &self.theme.borrow(), &opts);
        }
        start.elapsed() / iterations.max(1)
    }

    pub fn theme_fg(&self) -> termust_core::terminal::Rgb {
        self.theme.borrow().fg
    }

    pub fn theme_bg(&self) -> termust_core::terminal::Rgb {
        self.theme.borrow().bg
    }

    pub fn exited(&self) -> bool {
        self.exited.get()
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code.get()
    }

    pub fn output_age(&self) -> Option<Duration> {
        self.last_output.get().map(|instant| instant.elapsed())
    }

    pub fn foreground_command(&self) -> Option<String> {
        self.session.borrow().foreground_command()
    }

    /// Últimas `max` líneas visibles, sin relleno a la derecha.
    pub fn resize_count(&self) -> usize {
        self.resize_count.get()
    }

    pub fn recent_lines(&self, max: usize) -> Vec<String> {
        let snapshot = self.engine.borrow_mut().snapshot();
        let mut lines = Vec::with_capacity(snapshot.size.rows);
        for row in 0..snapshot.size.rows {
            let mut text = String::with_capacity(snapshot.size.cols);
            for col in 0..snapshot.size.cols {
                if let Some(cell) = snapshot.cell(row, col) {
                    if !cell.attrs.wide_spacer {
                        text.push(cell.ch);
                    }
                }
            }
            lines.push(text.trim_end().to_string());
        }
        while lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.pop();
        }
        let start = lines.len().saturating_sub(max);
        lines[start..].to_vec()
    }

    pub fn title(&self) -> Option<String> {
        self.engine.borrow().title().map(str::to_string)
    }

    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        self.engine.borrow().cwd().cloned()
    }

    pub fn clear_selection(&self) {
        self.selection.set(None);
        self.area.queue_draw();
    }

    pub fn history_ready(&self) -> bool {
        if self.selection.get().is_some() {
            return false;
        }
        self.engine.borrow_mut().history_ready()
    }

    pub fn modes(&self) -> Modes {
        self.engine.borrow_mut().snapshot().modes
    }

    /// Simula un clic directo al PTY (diagnóstico de mouse, no es un atajo).
    pub fn debug_mouse_click(&self, col: usize, row: usize) {
        let modes = self.modes();
        for kind in [MouseEventKind::Press, MouseEventKind::Release] {
            let bytes = encode_mouse(
                kind,
                MouseButton::Left,
                col + 1,
                row + 1,
                MouseMods::default(),
                modes.sgr_mouse,
                modes.utf8_mouse,
            );
            let _ = self.session.borrow_mut().write(&bytes);
        }
    }

    pub fn history_size(&self) -> usize {
        self.engine.borrow_mut().snapshot().history_size
    }

    /// Texto accesible para lectores de pantalla (última línea visible).
    pub fn set_accessible_description(&self, text: &str) {
        self.area
            .update_property(&[gtk::accessible::Property::Description(text)]);
    }

    pub fn set_history_limit(&self, lines: usize) {
        self.engine.borrow_mut().set_history_limit(lines);
    }

    pub fn take_history(&self) -> Vec<CommandRecord> {
        self.engine.borrow_mut().take_history()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.engine.borrow().bracketed_paste()
    }

    pub fn has_foreground_job(&self) -> bool {
        // OSC 133 sí cruza el PTY del sandbox; tcgetpgrp solo ve el grupo
        // del lanzador cuando el shell vive en el host.
        let running = matches!(
            self.engine.borrow().prompt().state,
            PromptState::CommandRunning
        );
        running || self.session.borrow().has_foreground_job()
    }

    /// Entrada sintética para tests/automatización (no es un atajo de usuario).
    pub fn write_input(&self, bytes: &[u8]) {
        let _ = self.session.borrow_mut().write(bytes);
    }

    /// Termina el PTY. Se llama al desmontar la página; no esperar a que GTK
    /// destruya el widget, porque la página puede mantener una referencia.
    pub fn kill(&self) {
        let _ = self.session.borrow_mut().kill();
    }
}

/// Fuente monoespaciada del sistema, igual que GNOME Console.
pub fn system_monospace_font() -> FontSpec {
    let Some(source) = gio::SettingsSchemaSource::default() else {
        return FontSpec::default();
    };
    if source.lookup("org.gnome.desktop.interface", true).is_none() {
        return FontSpec::default();
    }
    let settings = gio::Settings::new("org.gnome.desktop.interface");
    FontSpec::parse_pango(&settings.string("monospace-font-name")).unwrap_or_default()
}

fn mouse_mods(state: gdk::ModifierType) -> MouseMods {
    MouseMods {
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
    }
}

fn gdk_mouse_button(button: u32) -> Option<MouseButton> {
    match button {
        1 => Some(MouseButton::Left),
        2 => Some(MouseButton::Middle),
        3 => Some(MouseButton::Right),
        _ => None,
    }
}

fn write_mouse(
    session: &Rc<RefCell<Box<dyn PtySession>>>,
    kind: MouseEventKind,
    button: MouseButton,
    row: usize,
    col: usize,
    state: gdk::ModifierType,
    modes: Modes,
) {
    let bytes = encode_mouse(
        kind,
        button,
        col + 1,
        row + 1,
        mouse_mods(state),
        modes.sgr_mouse,
        modes.utf8_mouse,
    );
    let _ = session.borrow_mut().write(&bytes);
}

fn clamp_cell(cell: (usize, usize), rows: usize, cols: usize) -> (usize, usize) {
    (
        cell.0.min(rows.saturating_sub(1)),
        cell.1.min(cols.saturating_sub(1)),
    )
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
    paste_from(area.clipboard(), session, modes);
}

fn paste_from(
    clipboard: gdk::Clipboard,
    session: Rc<RefCell<Box<dyn PtySession>>>,
    modes: Rc<Cell<Modes>>,
) {
    let bracketed = modes.get().bracketed_paste;
    clipboard.read_text_async(None::<&gio::Cancellable>, move |res| {
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
    fn cell_clamping_and_button_mapping() {
        assert_eq!(clamp_cell((5, 9), 3, 4), (2, 3));
        assert_eq!(clamp_cell((0, 0), 0, 0), (0, 0));
        assert_eq!(gdk_mouse_button(1), Some(MouseButton::Left));
        assert_eq!(gdk_mouse_button(2), Some(MouseButton::Middle));
        assert_eq!(gdk_mouse_button(3), Some(MouseButton::Right));
        assert_eq!(gdk_mouse_button(4), None);
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

#[cfg(test)]
mod pointer_tests {
    use super::*;

    #[test]
    fn mouse_cell_maps_to_the_real_grid() {
        let metrics = CellMetrics {
            width: 9.0,
            height: 18.0,
            baseline: 14.0,
        };
        let grid = GridSize {
            rows: 56,
            cols: 213,
        };
        let (row, col) = clamp_cell(cell_at(1919.0, 1021.0, metrics), grid.rows, grid.cols);
        assert_eq!((row, col), (55, 212));
    }
}

#[cfg(test)]
mod wheel_tests {
    use super::*;

    fn mouse_modes() -> Modes {
        Modes {
            mouse_click: true,
            ..Modes::default()
        }
    }

    #[test]
    fn prompt_wheel_scrolls_back_even_with_mouse_reporting() {
        assert_eq!(
            wheel_target(mouse_modes(), PromptState::AtPrompt, false),
            WheelTarget::Scrollback
        );
    }

    #[test]
    fn alt_screen_wheel_goes_to_the_application() {
        let modes = Modes {
            alt_screen: true,
            ..mouse_modes()
        };
        assert_eq!(
            wheel_target(modes, PromptState::CommandRunning, false),
            WheelTarget::Pty
        );
    }

    #[test]
    fn command_running_wheel_goes_to_the_application() {
        assert_eq!(
            wheel_target(mouse_modes(), PromptState::CommandRunning, false),
            WheelTarget::Pty
        );
    }

    #[test]
    fn shift_wheel_scrolls_back() {
        assert_eq!(
            wheel_target(mouse_modes(), PromptState::CommandRunning, true),
            WheelTarget::Scrollback
        );
    }
}
