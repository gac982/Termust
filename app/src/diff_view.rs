//! Diff de un archivo concreto, para la columna de archivos.
//!
//! Calcula en un hilo aparte y muestra el working tree o el index según el
//! toggle `Index`. Comparte el pintado con el panel Git (`diff_render`).

use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::mpsc;
use std::sync::Arc;
use termust_git_local::{Diff, Git};

use crate::git_async::listen;
use crate::git_host::HostGitRunner;

const CAP: usize = 5_000;

enum Outcome {
    Diff(Diff),
    NoRepo,
    Error(String),
}

pub struct DiffView {
    root: gtk::Box,
    buffer: gtk::TextBuffer,
    staged_toggle: gtk::ToggleButton,
    status: gtk::Label,
    git: Git,
    path: RefCell<Option<PathBuf>>,
    staged: Cell<bool>,
    generation: Cell<u64>,
    loading: Cell<bool>,
    lines: Cell<usize>,
    binary: Cell<bool>,
    truncated: Cell<bool>,
    self_weak: Weak<Self>,
}

impl DiffView {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("termust-diff");

        let staged_toggle = gtk::ToggleButton::with_label("Index");
        staged_toggle.add_css_class("flat");
        staged_toggle.set_tooltip_text(Some("Ver el diff del index"));
        staged_toggle.update_property(&[gtk::accessible::Property::Label("Index")]);

        let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh.add_css_class("flat");
        refresh.set_tooltip_text(Some("Actualizar diff"));
        refresh.update_property(&[gtk::accessible::Property::Label("Actualizar diff")]);

        let status = gtk::Label::new(None);
        status.add_css_class("dim-label");
        status.add_css_class("caption");
        status.set_xalign(0.0);
        status.set_hexpand(true);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        toolbar.set_margin_top(4);
        toolbar.set_margin_bottom(4);
        toolbar.set_margin_start(8);
        toolbar.set_margin_end(8);
        toolbar.append(&staged_toggle);
        toolbar.append(&refresh);
        toolbar.append(&status);

        let buffer = gtk::TextBuffer::new(None);
        crate::diff_render::create_diff_tags(&buffer);
        let text_view = gtk::TextView::with_buffer(&buffer);
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        text_view.set_monospace(true);
        text_view.set_wrap_mode(gtk::WrapMode::None);
        text_view.set_left_margin(8);
        text_view.set_top_margin(6);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&text_view)
            .build();

        root.append(&toolbar);
        root.append(&scroller);

        let view = Rc::new_cyclic(|weak: &Weak<Self>| Self {
            root,
            buffer,
            staged_toggle,
            status,
            git: Git::new(Arc::new(HostGitRunner)),
            path: RefCell::new(None),
            staged: Cell::new(false),
            generation: Cell::new(0),
            loading: Cell::new(false),
            lines: Cell::new(0),
            binary: Cell::new(false),
            truncated: Cell::new(false),
            self_weak: weak.clone(),
        });

        {
            let weak = view.self_weak.clone();
            view.staged_toggle.connect_toggled(move |toggle| {
                if let Some(view) = weak.upgrade() {
                    view.staged.set(toggle.is_active());
                    view.reload();
                }
            });
        }
        {
            let weak = view.self_weak.clone();
            refresh.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.reload();
                }
            });
        }

        view
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn load(self: &Rc<Self>, path: &Path) {
        self.path.replace(Some(path.to_path_buf()));
        self.reload();
    }

    pub fn reload(self: &Rc<Self>) {
        let Some(path) = self.path.borrow().clone() else {
            return;
        };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.loading.set(true);
        self.lines.set(0);
        self.binary.set(false);
        self.truncated.set(false);
        self.buffer.set_text("");
        self.status.set_text("Calculando diff…");

        let git = self.git.clone();
        let staged = self.staged.get();
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("termust-file-diff".into())
            .spawn(move || {
                let parent = path.parent().unwrap_or(Path::new("."));
                let outcome = match git.discover(parent) {
                    Ok(repo) => match git.diff(&repo, Some(&path), staged, CAP) {
                        Ok(diff) => Outcome::Diff(diff),
                        Err(error) => Outcome::Error(error.to_string()),
                    },
                    Err(_) => Outcome::NoRepo,
                };
                let _ = tx.send(outcome);
            });

        listen(self.self_weak.clone(), rx, move |view, outcome| {
            if view.generation.get() != generation {
                return;
            }
            view.apply(outcome);
        });
    }

    pub fn set_staged(self: &Rc<Self>, staged: bool) {
        if self.staged_toggle.is_active() == staged {
            self.staged.set(staged);
            self.reload();
        } else {
            self.staged_toggle.set_active(staged);
        }
    }

    pub fn line_count(&self) -> usize {
        self.lines.get()
    }

    pub fn diff_text(&self) -> String {
        self.buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), false)
            .to_string()
    }

    pub fn status_text(&self) -> String {
        self.status.text().to_string()
    }

    pub fn is_binary(&self) -> bool {
        self.binary.get()
    }

    pub fn is_loading(&self) -> bool {
        self.loading.get()
    }

    fn apply(&self, outcome: Outcome) {
        self.loading.set(false);
        self.buffer.set_text("");
        self.lines.set(0);
        self.binary.set(false);
        self.truncated.set(false);
        match outcome {
            Outcome::NoRepo => self
                .status
                .set_text("Este archivo no está en un repositorio git"),
            Outcome::Error(message) => {
                self.status
                    .set_text(&format!("No se pudo calcular el diff: {message}"));
            }
            Outcome::Diff(diff) => {
                self.lines.set(diff.lines.len());
                self.binary.set(diff.binary);
                self.truncated.set(diff.truncated);
                if diff.binary {
                    self.buffer
                        .set_text("Archivo binario: no se muestra el diff.");
                    self.status.set_text("");
                } else if diff.lines.is_empty() {
                    self.status.set_text("Sin diferencias");
                } else {
                    crate::diff_render::fill_diff_buffer(&self.buffer, &diff);
                    if diff.truncated {
                        self.status
                            .set_text(&format!("Diff truncado ({} líneas)", diff.lines.len()));
                    } else {
                        self.status.set_text("");
                    }
                }
            }
        }
    }
}
