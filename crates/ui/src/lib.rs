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
    context_menu_labels: Rc<RefCell<Option<(String, String)>>>,
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
        let context_menu_labels = Rc::new(RefCell::new(None));
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
            let session = session.clone();
            let modes = last_modes.clone();
            let key = gtk::EventControllerKey::new();
            key.connect_key_pressed(move |_, keyval, _code, state| {
                // Copiar/pegar/salto de línea los resuelve el mapa de atajos
                // editable en la ventana; acá solo se codifican teclas.
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
                let moved = engine.borrow_mut().scroll(if dy > 0.0 { -3 } else { 3 });
                if std::env::var_os("TERMUST_SCROLL_LOG").is_some() {
                    let after = engine.borrow_mut().snapshot();
                    eprintln!(
                        "termust-scroll: scrollback movido={moved} offset={} (historial {})",
                        after.display_offset, after.history_size
                    );
                }
                if moved {
                    area_scroll.queue_draw();
                }
                glib::Propagation::Stop
            });
            area.add_controller(scroll);
        }

        {
            let engine = engine.clone();
            let engine_released = engine.clone();
            let session = session.clone();
            let modes = last_modes.clone();
            let selection = selection.clone();
            let metrics = metrics.clone();
            let last_size = last_size.clone();
            let pointer = pointer.clone();
            let area_input = area.clone();
            let menu_labels = context_menu_labels.clone();
            let anchor: Rc<Cell<Option<(usize, usize)>>> = Rc::new(Cell::new(None));
            let press_px = Rc::new(Cell::new((0.0f64, 0.0f64)));
            let pressed_button: Rc<Cell<Option<MouseButton>>> = Rc::new(Cell::new(None));
            let moved = Rc::new(Cell::new(false));
            let engine_log = engine.clone();

            let click = gtk::GestureClick::new();
            // GtkGestureClick escucha solo el botón primario por defecto;
            // con 0 llegan también el derecho (menú) y el medio (pegar).
            click.set_button(0);
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
                    if button == 3 {
                        // El click derecho queda para Termust aunque la TUI
                        // pida reporte de mouse. El menú se abre al soltar:
                        // abrirlo al presionar hace que el release lo cierre.
                        return;
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
                                modes.get().bracketed_paste,
                            );
                        }
                        _ => {}
                    }
                });
            }

            {
                let engine = engine_released;
                let session = session.clone();
                let selection = selection.clone();
                let modes = modes.clone();
                let metrics = metrics.clone();
                let last_size = last_size.clone();
                let pointer = pointer.clone();
                let pressed_button = pressed_button.clone();
                let anchor = anchor.clone();
                let area_menu = area_input.clone();
                let menu_labels = menu_labels.clone();
                click.connect_released(move |gesture, _n_press, x, y| {
                    pointer.set((x, y));
                    if gesture.current_button() == 3 {
                        if std::env::var_os("TERMUST_MOUSE_LOG").is_some() {
                            eprintln!("termust-mouse: menu contextual x={x:.1} y={y:.1}");
                        }
                        terminal_context_menu(
                            &area_menu,
                            &engine,
                            &selection,
                            &session,
                            &modes,
                            &menu_labels,
                            x,
                            y,
                        );
                        return;
                    }
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
                motion.connect_enter(move |_, x, y| {
                    pointer.set((x, y));
                    if std::env::var_os("TERMUST_MOUSE_LOG").is_some() {
                        eprintln!("termust-mouse-enter: x={x:.1} y={y:.1}");
                    }
                });
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
                    if std::env::var_os("TERMUST_MOUSE_LOG").is_some() {
                        let mut engine = engine_log.borrow_mut();
                        let snap = engine.snapshot();
                        let prompt = engine.prompt();
                        eprintln!(
                            "termust-mouse-motion: x={x:.1} y={y:.1} modes={m_modes:?} tracking={tracking} pressed={:?} prompt={:?} offset={} history={} selection={} moved={}",
                            pressed_button.get(),
                            prompt.state,
                            snap.display_offset,
                            snap.history_size,
                            selection.get().is_some(),
                            moved.get()
                        );
                    }
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
                            if std::env::var_os("TERMUST_SCROLL_LOG").is_some() {
                                let preview = String::from_utf8_lossy(&bytes);
                                let preview: String = preview.chars().take(80).collect();
                                eprintln!("termust-scroll: pty {} bytes: {preview:?}", bytes.len());
                            }
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
            context_menu_labels,
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

    /// Comando que el shell reportó como en ejecución (OSC 133;C).
    pub fn running_command(&self) -> Option<String> {
        self.engine.borrow().running_command().map(str::to_string)
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

    /// Abre el menú contextual en coordenadas del widget (equivale al click
    /// derecho). Expuesto para automatización y pruebas.
    pub fn open_context_menu_at(&self, x: f64, y: f64) -> gtk::Popover {
        let modes = Rc::new(Cell::new(self.modes()));
        terminal_context_menu(
            &self.area,
            &self.engine,
            &self.selection,
            &self.session,
            &modes,
            &self.context_menu_labels,
            x,
            y,
        )
    }

    /// Textos del menú contextual (los traduce la app).
    pub fn set_context_menu_labels(&self, copy: String, paste: String) {
        *self.context_menu_labels.borrow_mut() = Some((copy, paste));
    }

    /// Texto de la selección actual, si hay una.
    pub fn copy_selection_text(&self) -> Option<String> {
        let snapshot = self.engine.borrow_mut().snapshot();
        copy_selection(&snapshot, self.selection.get())
    }

    /// Copia la selección al portapapeles.
    pub fn copy_selection(&self) -> bool {
        let Some(text) = self.copy_selection_text() else {
            return false;
        };
        self.area.clipboard().set_text(&text);
        true
    }

    /// Pega el portapapeles como entrada del terminal.
    pub fn paste(&self) {
        let bracketed = self.bracketed_paste();
        paste_from(self.area.clipboard(), self.session.clone(), bracketed);
    }

    /// Inserta un salto de línea sin ejecutar (Shift+Enter). Devuelve `false`
    /// si el shell no soporta bracketed paste: en ese caso hay que dejar pasar
    /// la tecla como un Enter normal.
    pub fn insert_newline(&self) -> bool {
        let Some(bytes) = newline_bytes(self.bracketed_paste()) else {
            return false;
        };
        let _ = self.session.borrow_mut().write(bytes);
        true
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

fn paste_from(
    clipboard: gdk::Clipboard,
    session: Rc<RefCell<Box<dyn PtySession>>>,
    bracketed: bool,
) {
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

/// Abre el menú contextual en coordenadas del widget (tests y click derecho).
#[allow(clippy::too_many_arguments)]
fn terminal_context_menu(
    area: &gtk::DrawingArea,
    engine: &Rc<RefCell<Box<dyn TerminalEngine>>>,
    selection: &Rc<Cell<Option<Selection>>>,
    session: &Rc<RefCell<Box<dyn PtySession>>>,
    modes: &Rc<Cell<Modes>>,
    labels: &Rc<RefCell<Option<(String, String)>>>,
    x: f64,
    y: f64,
) -> gtk::Popover {
    let can_copy = {
        let snapshot = engine.borrow_mut().snapshot();
        copy_selection(&snapshot, selection.get()).is_some()
    };
    let (copy_label, paste_label) = labels
        .borrow()
        .clone()
        .unwrap_or_else(|| ("Copy".to_string(), "Paste".to_string()));
    let engine_copy = engine.clone();
    let selection_copy = selection.clone();
    let area_copy = area.clone();
    let session_paste = session.clone();
    let modes_paste = modes.clone();
    let area_paste = area.clone();
    show_terminal_menu(
        area,
        x,
        y,
        can_copy,
        &copy_label,
        &paste_label,
        move || {
            let snapshot = engine_copy.borrow_mut().snapshot();
            if let Some(text) = copy_selection(&snapshot, selection_copy.get()) {
                area_copy.clipboard().set_text(&text);
            }
        },
        move || {
            paste_from(
                area_paste.clipboard(),
                session_paste.clone(),
                modes_paste.get().bracketed_paste,
            );
        },
    )
}

/// Menú contextual del terminal: solo Copiar y Pegar. El click derecho está
/// reservado para Termust, así que no se reenvía a la TUI.
#[allow(clippy::too_many_arguments)]
fn show_terminal_menu(
    area: &gtk::DrawingArea,
    x: f64,
    y: f64,
    can_copy: bool,
    copy_label: &str,
    paste_label: &str,
    on_copy: impl Fn() + 'static,
    on_paste: impl Fn() + 'static,
) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    popover.set_autohide(true);

    let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
    menu.add_css_class("menu");
    menu.set_margin_top(6);
    menu.set_margin_bottom(6);
    menu.set_margin_start(6);
    menu.set_margin_end(6);

    let copy = menu_item("edit-copy-symbolic", copy_label, can_copy);
    let paste = menu_item("edit-paste-symbolic", paste_label, true);
    menu.append(&copy);
    menu.append(&paste);
    popover.set_child(Some(&menu));
    popover.set_parent(area);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));

    {
        let popover = popover.clone();
        copy.connect_clicked(move |_| {
            on_copy();
            popover.popdown();
        });
    }
    {
        let popover = popover.clone();
        paste.connect_clicked(move |_| {
            on_paste();
            popover.popdown();
        });
    }
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
    popover
}

fn menu_item(icon: &str, label: &str, sensitive: bool) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let image = gtk::Image::from_icon_name(icon);
    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_hexpand(true);
    row.append(&image);
    row.append(&text);

    let button = gtk::Button::new();
    button.set_child(Some(&row));
    button.add_css_class("flat");
    button.set_sensitive(sensitive);
    button
}

/// Salto de línea sin ejecutar: solo es seguro con bracketed paste, donde el
/// shell lo inserta en el buffer en vez de aceptar la línea.
pub fn newline_bytes(bracketed: bool) -> Option<&'static [u8]> {
    if bracketed {
        Some(b"\x1b[200~\n\x1b[201~")
    } else {
        None
    }
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
    fn newline_uses_bracketed_paste() {
        assert_eq!(
            newline_bytes(true),
            Some(b"\x1b[200~\n\x1b[201~".as_slice())
        );
        assert_eq!(newline_bytes(false), None);
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
