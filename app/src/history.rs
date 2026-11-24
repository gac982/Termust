use crate::i18n;
use gtk::glib;
use gtk4 as gtk;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::mpsc;
use std::time::Duration;
use termust_core::history::{
    parse_bash_history, parse_zsh_history, HistoryEntry, HistoryIndex, HistorySource,
};

/// Lee los historiales de los shells en modo solo-lectura.
pub fn import_histories(home: &Path) -> Vec<HistoryEntry> {
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string(home.join(".zsh_history")) {
        out.extend(parse_zsh_history(&text));
    }
    if let Ok(text) = std::fs::read_to_string(home.join(".bash_history")) {
        out.extend(parse_bash_history(&text));
    }
    out
}

pub type InsertFn = Rc<dyn Fn(String)>;

pub struct HistoryController {
    index: Rc<RefCell<HistoryIndex>>,
    home: PathBuf,
    dialog: libadwaita::Dialog,
    entry: gtk::SearchEntry,
    list: gtk::ListBox,
    commands: RefCell<Vec<String>>,
    insert: RefCell<Option<InsertFn>>,
    anchor: RefCell<Option<gtk::Widget>>,
    open: Cell<bool>,
    debounce: Cell<Option<glib::SourceId>>,
    imported: Cell<bool>,
    importing: Cell<bool>,
    self_weak: Weak<HistoryController>,
}

impl HistoryController {
    pub fn new(
        index: Rc<RefCell<HistoryIndex>>,
        home: PathBuf,
        _toast: libadwaita::ToastOverlay,
    ) -> Rc<Self> {
        Rc::new_cyclic(|weak: &Weak<HistoryController>| {
            let entry = gtk::SearchEntry::new();
            entry.set_placeholder_text(Some(&i18n::tr("Search history…")));
            entry.set_hexpand(true);

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.set_activate_on_single_click(true);

            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&list)
                .build();

            let title = gtk::Label::new(Some(&i18n::tr("Command history")));
            title.add_css_class("heading");
            title.set_xalign(0.0);
            title.set_hexpand(true);
            let close = gtk::Button::from_icon_name("window-close-symbolic");
            close.add_css_class("flat");
            close.set_tooltip_text(Some(&i18n::tr("Close (Esc)")));
            let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            header.append(&title);
            header.append(&close);

            let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
            container.set_margin_top(8);
            container.set_margin_bottom(8);
            container.set_margin_start(8);
            container.set_margin_end(8);
            container.set_size_request(560, 320);
            container.append(&header);
            container.append(&entry);
            container.append(&scroll);

            let dialog = libadwaita::Dialog::new();
            dialog.set_title(&i18n::tr("Command history"));
            dialog.set_content_width(560);
            dialog.set_content_height(420);
            dialog.set_child(Some(&container));

            {
                let weak = weak.clone();
                let key = gtk::EventControllerKey::new();
                key.set_propagation_phase(gtk::PropagationPhase::Capture);
                key.connect_key_pressed(move |_, keyval, _, _| {
                    let Some(controller) = weak.upgrade() else {
                        return glib::Propagation::Proceed;
                    };
                    match keyval {
                        gtk::gdk::Key::Up => {
                            controller.move_selection(-1);
                            glib::Propagation::Stop
                        }
                        gtk::gdk::Key::Down => {
                            controller.move_selection(1);
                            glib::Propagation::Stop
                        }
                        gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                            controller.insert_selected();
                            glib::Propagation::Stop
                        }
                        gtk::gdk::Key::Escape => {
                            controller.close();
                            glib::Propagation::Stop
                        }
                        _ => glib::Propagation::Proceed,
                    }
                });
                entry.add_controller(key);
            }

            {
                let weak = weak.clone();
                entry.connect_search_changed(move |_| {
                    let Some(controller) = weak.upgrade() else {
                        return;
                    };
                    if let Some(source) = controller.debounce.take() {
                        source.remove();
                    }
                    let weak = weak.clone();
                    let source =
                        glib::timeout_add_local_once(Duration::from_millis(50), move || {
                            if let Some(controller) = weak.upgrade() {
                                controller.debounce.set(None);
                                controller.refresh();
                            }
                        });
                    controller.debounce.set(Some(source));
                });
            }

            {
                let weak = weak.clone();
                list.connect_row_activated(move |_, _| {
                    if let Some(controller) = weak.upgrade() {
                        controller.insert_selected();
                    }
                });
            }

            {
                let weak = weak.clone();
                dialog.connect_closed(move |_| {
                    if let Some(controller) = weak.upgrade() {
                        controller.on_closed();
                    }
                });
            }
            {
                let weak = weak.clone();
                close.connect_clicked(move |_| {
                    if let Some(controller) = weak.upgrade() {
                        controller.close();
                    }
                });
            }

            Self {
                index,
                home,
                dialog,
                entry,
                list,
                commands: RefCell::new(Vec::new()),
                insert: RefCell::new(None),
                anchor: RefCell::new(None),
                open: Cell::new(false),
                debounce: Cell::new(None),
                imported: Cell::new(false),
                importing: Cell::new(false),
                self_weak: weak.clone(),
            }
        })
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    pub fn dialog(&self) -> &libadwaita::Dialog {
        &self.dialog
    }

    /// Reemplaza el índice con comandos de ejemplo para capturas públicas.
    pub fn seed_demo(&self, commands: &[&str]) {
        {
            let mut index = self.index.borrow_mut();
            index.clear();
            for (position, command) in commands.iter().enumerate() {
                index.insert(HistoryEntry::new(
                    *command,
                    PathBuf::from("/home/usuario/proyecto"),
                    position as u64,
                    Some(0),
                    HistorySource::Session,
                ));
            }
        }
        self.imported.set(true);
        if self.open.get() {
            self.refresh();
        }
    }

    pub fn open(&self, anchor: &impl IsA<gtk::Widget>, insert: InsertFn) {
        if self.open.get() {
            return;
        }
        let Some(window) = anchor.root().and_downcast::<gtk::Window>() else {
            return;
        };
        self.ensure_import();
        *self.insert.borrow_mut() = Some(insert);
        *self.anchor.borrow_mut() = Some(anchor.clone().upcast());
        self.entry.set_text("");
        self.refresh();
        self.open.set(true);
        self.dialog.present(Some(&window));
        self.entry.grab_focus();
    }

    pub fn close(&self) {
        if !self.open.get() {
            return;
        }
        self.dialog.close();
        self.on_closed();
    }

    /// Inserta el comando seleccionado en el prompt (sin Enter).
    pub fn insert_selected(&self) {
        let Some(text) = self.selected_command() else {
            return;
        };
        self.index.borrow_mut().touch(&text);
        if let Some(insert) = self.insert.borrow().as_ref() {
            insert(text);
        }
        self.close();
    }

    pub fn row_count(&self) -> usize {
        self.commands.borrow().len()
    }

    pub fn row_texts(&self) -> Vec<String> {
        self.commands.borrow().clone()
    }

    /// Refresca la lista si el modal está abierto.
    pub fn refresh_if_open(&self) {
        if self.open.get() {
            self.refresh();
        }
    }

    fn on_closed(&self) {
        if !self.open.replace(false) {
            return;
        }
        if let Some(source) = self.debounce.take() {
            source.remove();
        }
        let anchor = self.anchor.borrow_mut().take();
        self.insert.borrow_mut().take();
        if let Some(anchor) = anchor {
            anchor.grab_focus();
        }
    }

    fn selected_command(&self) -> Option<String> {
        let row = self.list.selected_row()?;
        let index = row.index();
        if index < 0 {
            return None;
        }
        self.commands.borrow().get(index as usize).cloned()
    }

    fn move_selection(&self, delta: i32) {
        let count = self.commands.borrow().len() as i32;
        if count == 0 {
            return;
        }
        let current = self
            .list
            .selected_row()
            .map(|row| row.index())
            .unwrap_or(0)
            .clamp(0, count - 1);
        let next = (current + delta).clamp(0, count - 1);
        if let Some(row) = self.list.row_at_index(next) {
            self.list.select_row(Some(&row));
            row.grab_focus();
            self.entry.grab_focus();
        }
    }

    fn refresh(&self) {
        let results = self.index.borrow().search(&self.entry.text(), 50);
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        let mut commands = self.commands.borrow_mut();
        commands.clear();
        for entry in results {
            let row = gtk::ListBoxRow::new();
            let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
            content.set_margin_top(6);
            content.set_margin_bottom(6);
            content.set_margin_start(6);
            content.set_margin_end(6);

            let title = gtk::Label::new(Some(&entry.text));
            title.set_xalign(0.0);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.add_css_class("monospace");
            content.append(&title);

            let meta = gtk::Label::new(Some(&meta_text(&entry)));
            meta.set_xalign(0.0);
            meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
            meta.add_css_class("dim-label");
            meta.add_css_class("caption");
            content.append(&meta);

            row.set_child(Some(&content));
            row.set_tooltip_text(Some(&entry.text));
            self.list.append(&row);
            commands.push(entry.text.clone());
        }
        drop(commands);
        if let Some(row) = self.list.row_at_index(0) {
            self.list.select_row(Some(&row));
        }
    }

    fn ensure_import(&self) {
        if self.imported.get()
            || self.importing.get()
            || std::env::var_os("TERMUST_HISTORY_NO_IMPORT").is_some()
        {
            return;
        }
        self.importing.set(true);
        let (tx, rx) = mpsc::channel();
        let home = self.home.clone();
        let _ = std::thread::Builder::new()
            .name("termust-history".into())
            .spawn(move || {
                let _ = tx.send(import_histories(&home));
            });
        let weak = self.self_weak.clone();
        glib::timeout_add_local(Duration::from_millis(100), move || match rx.try_recv() {
            Ok(entries) => {
                if let Some(controller) = weak.upgrade() {
                    {
                        let mut index = controller.index.borrow_mut();
                        for entry in entries {
                            index.insert(entry);
                        }
                    }
                    controller.imported.set(true);
                    if controller.open.get() {
                        controller.refresh();
                    }
                }
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        });
    }
}

fn meta_text(entry: &HistoryEntry) -> String {
    let mut parts = Vec::new();
    match entry.source {
        HistorySource::Session => {
            if !entry.cwd.as_os_str().is_empty() {
                parts.push(entry.cwd.display().to_string());
            }
            if let Some(code) = entry.exit_code {
                parts.push(format!("exit {code}"));
            }
        }
        HistorySource::Imported => parts.push(i18n::tr("history")),
    }
    parts.join("  ·  ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn imports_read_only_from_both_shells() {
        let dir = std::env::temp_dir().join(format!(
            "termust-hist-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let zsh = dir.join(".zsh_history");
        let bash = dir.join(".bash_history");
        fs::write(&zsh, ": 1700000000:0;cargo test\n").unwrap();
        fs::write(&bash, "git status\n").unwrap();
        let before_zsh = fs::read(&zsh).unwrap();
        let before_bash = fs::read(&bash).unwrap();
        let entries = import_histories(&dir);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "cargo test");
        assert_eq!(entries[1].text, "git status");
        assert_eq!(fs::read(&zsh).unwrap(), before_zsh);
        assert_eq!(fs::read(&bash).unwrap(), before_bash);
        let _ = fs::remove_dir_all(&dir);
    }
}
