//! Visor de archivos de solo lectura: resaltado, números, folds, outline y búsqueda.

use gtk::glib;
use gtk4 as gtk;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::mpsc;
use std::time::Duration;
use termust_syntax::{Document, FoldRegion, HighlightKind, LoadMode, OutlineItem, Parsed};
use termust_text_render::Theme;

/// Colores del resaltado derivados del tema del terminal.
#[derive(Clone, Debug)]
pub struct ViewerColors {
    keyword: String,
    string: String,
    comment: String,
    function: String,
    type_: String,
    number: String,
    constant: String,
    property: String,
    variable: String,
    operator: String,
    punctuation: String,
    escape: String,
    attribute: String,
}

impl ViewerColors {
    pub fn from_theme(theme: &Theme) -> Self {
        let hex =
            |rgb: termust_core::terminal::Rgb| format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b);
        let blend = termust_core::theme::blend;
        Self {
            keyword: hex(theme.palette[4]),
            string: hex(theme.palette[2]),
            comment: hex(blend(theme.bg, theme.fg, 0.55)),
            function: hex(theme.palette[5]),
            type_: hex(theme.palette[6]),
            number: hex(theme.palette[3]),
            constant: hex(theme.palette[5]),
            property: hex(theme.palette[7]),
            variable: hex(theme.fg),
            operator: hex(theme.fg),
            punctuation: hex(blend(theme.bg, theme.fg, 0.65)),
            escape: hex(theme.palette[3]),
            attribute: hex(theme.palette[6]),
        }
    }

    fn color(&self, kind: HighlightKind) -> &str {
        match kind {
            HighlightKind::Keyword => &self.keyword,
            HighlightKind::String => &self.string,
            HighlightKind::Comment => &self.comment,
            HighlightKind::Function => &self.function,
            HighlightKind::Type => &self.type_,
            HighlightKind::Number => &self.number,
            HighlightKind::Constant => &self.constant,
            HighlightKind::Property => &self.property,
            HighlightKind::Variable => &self.variable,
            HighlightKind::Operator => &self.operator,
            HighlightKind::Punctuation => &self.punctuation,
            HighlightKind::Escape => &self.escape,
            HighlightKind::Attribute => &self.attribute,
        }
    }
}

struct ViewerState {
    content: Rc<RefCell<String>>,
    line_starts: Rc<RefCell<Vec<usize>>>,
    folds: Rc<RefCell<Vec<FoldRegion>>>,
    folded: Rc<RefCell<HashSet<usize>>>,
    parsed: Rc<RefCell<Option<Parsed>>>,
    highlighted: Rc<Cell<Option<(usize, usize)>>>,
}

pub struct Viewer {
    root: gtk::Box,
    header_path: gtk::Label,
    header_language: gtk::Label,
    spinner: gtk::Spinner,
    close_button: gtk::Button,
    search_toggle: gtk::ToggleButton,
    editor_button: gtk::Button,
    banner: libadwaita::Banner,
    stack: gtk::Stack,
    status: libadwaita::StatusPage,
    text_view: gtk::TextView,
    buffer: gtk::TextBuffer,
    gutter: gtk::DrawingArea,
    outline_popover: gtk::Popover,
    outline_list: gtk::ListBox,
    outline: RefCell<Vec<OutlineItem>>,
    tags: HashMap<HighlightKind, gtk::TextTag>,
    invisible: gtk::TextTag,
    state: ViewerState,
    document: RefCell<Option<Document>>,
    poll: Cell<Option<glib::SourceId>>,
    scroll_source: Cell<Option<glib::SourceId>>,
    search_entry: gtk::SearchEntry,
    search_box: gtk::Box,
    search_start: RefCell<Option<usize>>,
    visible: Cell<bool>,
    highlight_count: Cell<usize>,
    toast: libadwaita::ToastOverlay,
    on_close: RefCell<Option<Rc<dyn Fn()>>>,
    self_weak: Weak<Self>,
}

impl Viewer {
    pub fn new(colors: ViewerColors, toast: libadwaita::ToastOverlay) -> Rc<Self> {
        let header_path = gtk::Label::new(None);
        header_path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        header_path.set_xalign(0.0);
        header_path.set_hexpand(true);
        header_path.add_css_class("heading");

        let header_language = gtk::Label::new(None);
        header_language.add_css_class("dim-label");
        header_language.add_css_class("caption");

        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        spinner.set_tooltip_text(Some("Calculando esquema…"));

        let close_button = gtk::Button::from_icon_name("go-previous-symbolic");
        close_button.add_css_class("flat");
        close_button.set_tooltip_text(Some("Volver al terminal"));

        let search_toggle = gtk::ToggleButton::builder()
            .icon_name("edit-find-symbolic")
            .tooltip_text("Buscar (Ctrl+F)")
            .build();
        search_toggle.add_css_class("flat");

        let outline_button = gtk::MenuButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text("Esquema del archivo")
            .build();
        outline_button.add_css_class("flat");
        let outline_popover = gtk::Popover::new();
        let outline_list = gtk::ListBox::new();
        outline_list.set_selection_mode(gtk::SelectionMode::Single);
        let outline_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(160)
            .max_content_height(360)
            .child(&outline_list)
            .build();
        outline_popover.set_child(Some(&outline_scroller));
        outline_button.set_popover(Some(&outline_popover));

        let editor_button = gtk::Button::from_icon_name("document-edit-symbolic");
        editor_button.add_css_class("flat");
        editor_button.set_tooltip_text(Some("Abrir en editor externo"));

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_top(6);
        header.set_margin_bottom(6);
        header.set_margin_start(8);
        header.set_margin_end(8);
        header.append(&close_button);
        header.append(&header_path);
        header.append(&header_language);
        header.append(&spinner);
        header.append(&search_toggle);
        header.append(&outline_button);
        header.append(&editor_button);

        let banner = libadwaita::Banner::new("Archivo grande: resaltado limitado.");
        banner.set_revealed(false);

        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some("Buscar en el archivo…"));
        search_entry.set_hexpand(true);
        let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        search_box.set_margin_start(8);
        search_box.set_margin_end(8);
        search_box.set_margin_bottom(6);
        search_box.append(&search_entry);
        search_box.set_visible(false);

        let buffer = gtk::TextBuffer::new(None);
        let text_view = gtk::TextView::with_buffer(&buffer);
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        text_view.set_monospace(true);
        text_view.set_wrap_mode(gtk::WrapMode::None);
        text_view.set_left_margin(8);
        text_view.set_right_margin(8);
        text_view.set_top_margin(6);
        text_view.set_bottom_margin(6);
        text_view.set_hexpand(true);
        text_view.set_vexpand(true);

        let tags = create_tags(&buffer, &colors);
        let invisible = gtk::TextTag::builder().name("termust-folded").build();
        invisible.set_invisible(true);
        buffer.tag_table().add(&invisible);

        let gutter = gtk::DrawingArea::new();
        gutter.set_content_width(58);
        gutter.set_vexpand(true);
        gutter.add_css_class("termust-gutter");

        let text_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .vexpand(true)
            .child(&text_view)
            .build();

        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        body.set_vexpand(true);
        body.append(&gutter);
        body.append(&text_scroller);

        let status = libadwaita::StatusPage::new();
        status.set_vexpand(true);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&body, Some("text"));
        stack.add_named(&status, Some("status"));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("termust-viewer");
        root.append(&header);
        root.append(&banner);
        root.append(&search_box);
        root.append(&stack);
        root.set_visible(false);

        let index = Rc::new(RefCell::new(Vec::new()));
        let line_starts = Rc::new(RefCell::new(Vec::new()));
        let folds = Rc::new(RefCell::new(Vec::new()));
        let folded = Rc::new(RefCell::new(HashSet::new()));
        let parsed = Rc::new(RefCell::new(None));
        let highlighted = Rc::new(Cell::new(None));

        let viewer = Rc::new_cyclic(|weak: &Weak<Self>| Self {
            root,
            header_path,
            header_language,
            spinner,
            close_button,
            search_toggle,
            editor_button,
            banner,
            stack,
            status,
            text_view,
            buffer,
            gutter,
            outline_popover,
            outline_list,
            outline: RefCell::new(Vec::new()),
            tags,
            invisible,
            state: ViewerState {
                content: Rc::new(RefCell::new(String::new())),
                line_starts,
                folds,
                folded,
                parsed,
                highlighted,
            },
            document: RefCell::new(None),
            poll: Cell::new(None),
            scroll_source: Cell::new(None),
            search_entry,
            search_box,
            search_start: RefCell::new(None),
            visible: Cell::new(false),
            highlight_count: Cell::new(0),
            toast,
            on_close: RefCell::new(None),
            self_weak: weak.clone(),
        });
        viewer.connect(index);
        viewer
    }

    fn connect(self: &Rc<Self>, index: Rc<RefCell<Vec<usize>>>) {
        let _ = index;
        // Cerrar.
        {
            let weak = self.self_weak.clone();
            self.close_button.connect_clicked(move |_| {
                if let Some(viewer) = weak.upgrade() {
                    viewer.close();
                }
            });
        }

        // Buscar.
        {
            let weak = self.self_weak.clone();
            let box_ = self.search_box.clone();
            let entry = self.search_entry.clone();
            self.search_toggle.connect_toggled(move |toggle| {
                let open = toggle.is_active();
                box_.set_visible(open);
                if open {
                    entry.grab_focus();
                } else if let Some(viewer) = weak.upgrade() {
                    viewer.text_view.grab_focus();
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            self.search_entry.connect_search_changed(move |entry| {
                if let Some(viewer) = weak.upgrade() {
                    viewer.find_from(entry.text().as_str(), 0);
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            self.search_entry.connect_activate(move |entry| {
                if let Some(viewer) = weak.upgrade() {
                    let start = viewer.search_start.borrow().unwrap_or(0);
                    viewer.find_from(entry.text().as_str(), start);
                }
            });
        }

        // Outline.
        {
            let weak = self.self_weak.clone();
            self.outline_list.connect_row_activated(move |_, row| {
                if let Some(viewer) = weak.upgrade() {
                    let index = row.index().max(0) as usize;
                    let line = viewer.outline.borrow().get(index).map(|item| item.line);
                    if let Some(line) = line {
                        viewer.goto_line(line);
                    }
                    viewer.outline_popover.popdown();
                }
            });
        }

        // Abrir en editor.
        {
            let weak = self.self_weak.clone();
            self.editor_button.connect_clicked(move |_| {
                if let Some(viewer) = weak.upgrade() {
                    viewer.open_in_editor();
                }
            });
        }

        // Scroll: recalcular viewport.
        if let Some(scroller) = self
            .text_view
            .parent()
            .and_downcast::<gtk::ScrolledWindow>()
        {
            let adjustment = scroller.vadjustment();
            let weak = self.self_weak.clone();
            adjustment.connect_value_changed(move |_| {
                if let Some(viewer) = weak.upgrade() {
                    viewer.schedule_viewport(60);
                }
            });
        }

        // Gutter: números y marcadores de fold.
        {
            let weak = self.self_weak.clone();
            let text_view = self.text_view.clone();
            let buffer = self.buffer.clone();
            let folds = self.state.folds.clone();
            let folded = self.state.folded.clone();
            self.gutter.set_draw_func(move |_, cr, width, _height| {
                if weak.upgrade().is_none() {
                    return;
                }
                let (first_iter, _) = text_view.line_at_y(0);
                let first = first_iter.line().max(0) as usize;
                let line_count = buffer.line_count().max(1) as usize;
                let last = (first + 200).min(line_count.saturating_sub(1));
                let folds = folds.borrow();
                let folded = folded.borrow();
                for line in first..=last {
                    let Some(iter) = buffer.iter_at_line(line as i32) else {
                        continue;
                    };
                    let rect = text_view.iter_location(&iter);
                    let layout = text_view.create_pango_layout(Some(&(line + 1).to_string()));
                    let text_width = layout.pixel_size().0;
                    let x = (width - text_width - 8) as f64;
                    let y = rect.y() as f64;
                    cr.set_source_rgba(0.55, 0.55, 0.55, 0.85);
                    cr.move_to(x, y);
                    pangocairo::functions::show_layout(cr, &layout);
                    if folds.iter().any(|fold| fold.start_line == line) {
                        let expanded = !folded.contains(&line);
                        cr.set_source_rgba(0.65, 0.65, 0.65, 0.9);
                        cr.move_to(8.0, y + 1.0);
                        let _ = cr.show_text(if expanded { "▾" } else { "▸" });
                    }
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            let text_view = self.text_view.clone();
            let gesture = gtk::GestureClick::new();
            gesture.connect_pressed(move |_, _, _x, y| {
                let Some(viewer) = weak.upgrade() else {
                    return;
                };
                let (iter, _) = text_view.line_at_y(y as i32);
                let line = iter.line().max(0) as usize;
                viewer.toggle_fold_at_line(line);
            });
            self.gutter.add_controller(gesture);
        }
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn is_open(&self) -> bool {
        self.root.is_visible()
    }

    pub fn is_showing_text(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("text")
    }

    pub fn is_showing_status(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("status")
    }

    pub fn language_label(&self) -> String {
        self.header_language.text().to_string()
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.document
            .borrow()
            .as_ref()
            .map(|document| document.path.clone())
    }

    pub fn line_count(&self) -> usize {
        self.buffer.line_count().max(0) as usize
    }

    pub fn highlight_count(&self) -> usize {
        self.highlight_count.get()
    }

    pub fn fold_count(&self) -> usize {
        self.state.folds.borrow().len()
    }

    pub fn folded_count(&self) -> usize {
        self.state.folded.borrow().len()
    }

    pub fn first_fold_line(&self) -> Option<usize> {
        self.state
            .folds
            .borrow()
            .first()
            .map(|fold| fold.start_line)
    }

    pub fn outline_count(&self) -> usize {
        self.outline.borrow().len()
    }

    pub fn banner_visible(&self) -> bool {
        self.banner.is_revealed()
    }

    pub fn is_editable(&self) -> bool {
        self.text_view.is_editable()
    }

    pub fn buffer_text(&self) -> String {
        let buffer = &self.buffer;
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string()
    }

    pub fn set_on_close(&self, callback: impl Fn() + 'static) {
        *self.on_close.borrow_mut() = Some(Rc::new(callback));
    }

    pub fn open_file(self: &Rc<Self>, path: &Path) {
        self.cancel_poll();
        self.state.folded.borrow_mut().clear();
        self.state.folds.borrow_mut().clear();
        self.state.parsed.replace(None);
        self.state.highlighted.set(None);
        self.highlight_count.set(0);
        self.outline.borrow_mut().clear();
        while let Some(row) = self.outline_list.first_child() {
            self.outline_list.remove(&row);
        }
        self.banner.set_revealed(false);
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.search_box.set_visible(false);
        self.search_entry.set_text("");
        self.search_start.replace(None);

        let document = match Document::load(path) {
            Ok(document) => document,
            Err(error) => {
                self.show_status(
                    "No se pudo abrir el archivo",
                    &error.to_string(),
                    "dialog-error-symbolic",
                );
                self.header_path.set_text(&path.display().to_string());
                self.header_language.set_text("");
                self.root.set_visible(true);
                self.visible.set(true);
                return;
            }
        };
        self.header_path
            .set_text(&document.path.display().to_string());
        self.header_language.set_text(document.language.label());
        self.state.content.replace(document.content.clone());
        self.state
            .line_starts
            .replace(line_starts(&document.content));
        self.document.replace(Some(document.clone()));

        match document.mode {
            LoadMode::Binary => {
                self.show_status(
                    "Archivo binario",
                    "No se muestra contenido binario en el visor.",
                    "application-x-executable-symbolic",
                );
                self.root.set_visible(true);
                self.visible.set(true);
                return;
            }
            LoadMode::LargeFile => {
                self.banner
                    .set_title("Archivo grande: se muestra el primer bloque en texto plano.");
                self.banner.set_revealed(true);
            }
            LoadMode::NonUtf8 => {
                self.banner
                    .set_title("El archivo no es UTF-8: se muestra como texto plano.");
                self.banner.set_revealed(true);
            }
            LoadMode::Normal => {}
        }
        if document.line_count() > 10_000 {
            self.banner.set_title(
                "Archivo grande: el resaltado y el esquema se calculan en segundo plano.",
            );
            self.banner.set_revealed(true);
        }

        self.buffer.set_text(&document.content);
        self.stack.set_visible_child_name("text");
        self.root.set_visible(true);
        self.visible.set(true);
        self.text_view.grab_focus();
        self.text_view
            .scroll_to_iter(&mut self.buffer.start_iter(), 0.0, false, 0.0, 0.0);
        self.gutter.queue_draw();

        if document.language == termust_syntax::Language::Plain {
            return;
        }
        self.spinner.set_visible(true);
        self.spinner.start();
        let content = document.content.clone();
        let language = document.language;
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("termust-syntax".into())
            .spawn(move || {
                let _ = tx.send(Parsed::new(language, &content));
            });
        let weak = self.self_weak.clone();
        let source =
            glib::timeout_add_local(Duration::from_millis(40), move || match rx.try_recv() {
                Ok(parsed) => {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.poll.set(None);
                        viewer.apply_parsed(parsed);
                    }
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
            });
        self.poll.set(Some(source));
    }

    fn apply_parsed(self: &Rc<Self>, parsed: Option<Parsed>) {
        let Some(parsed) = parsed else {
            return;
        };
        self.spinner.stop();
        self.spinner.set_visible(false);
        let folds = parsed.folds();
        let outline = parsed.outline();
        self.state.folds.replace(folds);
        self.outline.replace(outline.clone());
        self.state.parsed.replace(Some(parsed));
        self.populate_outline(&outline);
        self.apply_viewport();
        self.gutter.queue_draw();
    }

    fn populate_outline(&self, items: &[OutlineItem]) {
        while let Some(row) = self.outline_list.first_child() {
            self.outline_list.remove(&row);
        }
        for item in items {
            let row = gtk::ListBoxRow::new();
            let label = gtk::Label::new(Some(&format!(
                "{}  ·  {}:{}",
                item.name,
                item.kind,
                item.line + 1
            )));
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.set_child(Some(&label));
            self.outline_list.append(&row);
        }
    }

    fn show_status(&self, title: &str, description: &str, icon: &str) {
        self.status.set_title(title);
        self.status.set_description(Some(description));
        self.status.set_icon_name(Some(icon));
        self.stack.set_visible_child_name("status");
    }

    pub fn close(&self) {
        if !self.visible.replace(false) {
            return;
        }
        self.cancel_poll();
        if let Some(source) = self.scroll_source.take() {
            source.remove();
        }
        self.root.set_visible(false);
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.state.parsed.replace(None);
        self.state.folded.borrow_mut().clear();
        self.outline.borrow_mut().clear();
        if let Some(callback) = self.on_close.borrow().as_ref() {
            callback();
        }
    }

    fn cancel_poll(&self) {
        if let Some(source) = self.poll.take() {
            source.remove();
        }
    }

    fn schedule_viewport(self: &Rc<Self>, millis: u64) {
        if let Some(source) = self.scroll_source.take() {
            source.remove();
        }
        let weak = self.self_weak.clone();
        let source = glib::timeout_add_local_once(Duration::from_millis(millis), move || {
            if let Some(viewer) = weak.upgrade() {
                viewer.scroll_source.set(None);
                viewer.apply_viewport();
            }
        });
        self.scroll_source.set(Some(source));
    }

    fn visible_lines(&self) -> (usize, usize) {
        let (iter, _) = self.text_view.line_at_y(0);
        let first = iter.line().max(0) as usize;
        let height = self.text_view.allocated_height().max(1) as usize;
        let line_height = self
            .text_view
            .iter_location(&self.buffer.start_iter())
            .height()
            .max(1) as usize;
        let visible = (height / line_height).max(1);
        let last = (first + visible).min(self.line_count().saturating_sub(1));
        (first, last)
    }

    fn apply_viewport(&self) {
        let (first, last) = self.visible_lines();
        let from = first.saturating_sub(100);
        let to = (last + 100).min(self.line_count().saturating_sub(1));
        self.apply_highlight(from, to);
        self.gutter.queue_draw();
    }

    fn apply_highlight(&self, first: usize, last: usize) {
        let spans = {
            let parsed = self.state.parsed.borrow();
            let Some(parsed) = parsed.as_ref() else {
                return;
            };
            parsed.highlight_lines(first, last)
        };
        let start = self.buffer.start_iter();
        let end = self.buffer.end_iter();
        for tag in self.tags.values() {
            self.buffer.remove_tag(tag, &start, &end);
        }
        for span in &spans {
            let Some((start_iter, end_iter)) = self.byte_range_to_iters(span.start, span.end)
            else {
                continue;
            };
            if let Some(tag) = self.tags.get(&span.kind) {
                self.buffer.apply_tag(tag, &start_iter, &end_iter);
            }
        }
        self.state.highlighted.set(Some((first, last)));
        self.highlight_count.set(spans.len());
    }

    fn byte_range_to_iters(
        &self,
        start: usize,
        end: usize,
    ) -> Option<(gtk::TextIter, gtk::TextIter)> {
        let content = self.state.content.borrow();
        let starts = self.state.line_starts.borrow();
        let start_line = line_of_byte(&starts, start);
        let end_line = line_of_byte(&starts, end);
        let start_col = content.get(starts[start_line]..start)?.chars().count() as i32;
        let end_col = content.get(starts[end_line]..end)?.chars().count() as i32;
        let start_iter = self
            .buffer
            .iter_at_line_index(start_line as i32, start_col)?;
        let end_iter = self.buffer.iter_at_line_index(end_line as i32, end_col)?;
        Some((start_iter, end_iter))
    }

    pub fn goto_line(&self, line: usize) {
        let Some(mut iter) = self.buffer.iter_at_line(line as i32) else {
            return;
        };
        self.buffer.place_cursor(&iter);
        self.text_view
            .scroll_to_iter(&mut iter, 0.2, false, 0.0, 0.0);
        self.text_view.grab_focus();
    }

    pub fn toggle_fold_at_line(&self, line: usize) {
        let fold = self
            .state
            .folds
            .borrow()
            .iter()
            .find(|fold| fold.start_line == line)
            .copied();
        let Some(fold) = fold else {
            return;
        };
        let mut folded = self.state.folded.borrow_mut();
        if folded.remove(&line) {
            if let Some((start, end)) = self.fold_iters(fold) {
                self.buffer.remove_tag(&self.invisible, &start, &end);
            }
        } else {
            folded.insert(line);
            if let Some((start, end)) = self.fold_iters(fold) {
                self.buffer.apply_tag(&self.invisible, &start, &end);
            }
        }
        drop(folded);
        self.gutter.queue_draw();
    }

    fn fold_iters(&self, fold: FoldRegion) -> Option<(gtk::TextIter, gtk::TextIter)> {
        let start = self.buffer.iter_at_line(fold.start_line as i32 + 1)?;
        let end = self
            .buffer
            .iter_at_line(fold.end_line as i32 + 1)
            .unwrap_or_else(|| self.buffer.end_iter());
        Some((start, end))
    }

    pub fn toggle_search(&self) {
        self.search_toggle
            .set_active(!self.search_toggle.is_active());
    }

    pub fn selection_text(&self) -> Option<String> {
        let (start, end) = self.buffer.selection_bounds()?;
        Some(self.buffer.text(&start, &end, true).to_string())
    }

    /// Intento directo de mutar el buffer (no del archivo): el test E2E
    /// comprueba que el hash en disco no cambia.
    pub fn try_insert_for_test(&self, text: &str) -> bool {
        let before = self.buffer.char_count();
        self.buffer.insert_at_cursor(text);
        self.buffer.char_count() != before
    }

    pub fn find(&self, query: &str) -> bool {
        self.find_from(query, 0)
    }

    fn find_from(&self, query: &str, from: usize) -> bool {
        if query.is_empty() {
            return false;
        }
        let limit = self.buffer.char_count().max(0) as usize;
        let start = self.buffer.iter_at_offset(from.min(limit) as i32);
        let Some((match_start, match_end)) =
            start.forward_search(query, gtk::TextSearchFlags::CASE_INSENSITIVE, None)
        else {
            self.search_start.replace(None);
            return false;
        };
        self.buffer.select_range(&match_start, &match_end);
        let mut end = match_end;
        self.text_view
            .scroll_to_iter(&mut end, 0.2, false, 0.0, 0.0);
        self.search_start
            .replace(Some(match_end.offset().max(0) as usize));
        true
    }

    fn open_in_editor(&self) {
        let Some(path) = self
            .document
            .borrow()
            .as_ref()
            .map(|document| document.path.clone())
        else {
            return;
        };
        let Some(editor) = termust_host_exec::find_editor() else {
            self.toast.add_toast(libadwaita::Toast::new(
                "No se encontró un editor permitido (code, codium, zed, gnome-text-editor).",
            ));
            return;
        };
        match termust_host_exec::editor_argv(&editor, &path, termust_host_exec::is_flatpak()) {
            Ok(argv) => {
                let mut command = std::process::Command::new(&argv[0]);
                command.args(&argv[1..]);
                if let Err(error) = command.spawn() {
                    self.toast.add_toast(libadwaita::Toast::new(&format!(
                        "No se pudo abrir el editor: {error}"
                    )));
                }
            }
            Err(error) => {
                self.toast
                    .add_toast(libadwaita::Toast::new(&format!("Editor inválido: {error}")));
            }
        }
    }
}

fn create_tags(
    buffer: &gtk::TextBuffer,
    colors: &ViewerColors,
) -> HashMap<HighlightKind, gtk::TextTag> {
    use HighlightKind as K;
    let mut tags = HashMap::new();
    for kind in [
        K::Keyword,
        K::String,
        K::Comment,
        K::Function,
        K::Type,
        K::Number,
        K::Constant,
        K::Property,
        K::Variable,
        K::Operator,
        K::Punctuation,
        K::Escape,
        K::Attribute,
    ] {
        let tag = gtk::TextTag::builder()
            .name(format!("termust-syntax-{kind:?}"))
            .build();
        tag.set_foreground(Some(colors.color(kind)));
        if kind == K::Comment {
            tag.set_style(gtk::pango::Style::Italic);
        }
        if kind == K::Keyword {
            tag.set_weight(600);
        }
        buffer.tag_table().add(&tag);
        tags.insert(kind, tag);
    }
    tags
}

fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_of_byte(starts: &[usize], byte: usize) -> usize {
    match starts.binary_search(&byte) {
        Ok(index) => index,
        Err(index) => index.saturating_sub(1),
    }
}
