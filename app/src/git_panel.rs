//! Panel de git local: status, staging, diff y commit.

use gtk::glib;
use gtk4 as gtk;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use termust_git_local::{
    commit_hint, split_staged, ChangeKind, Diff, Git, GitError, GitRunner, Repo, RepoStatus,
};
use termust_git_remote::{parse_remote_url, Remote, Transport};

const DIFF_CAP: usize = 5_000;
const DIFF_CAP_LARGE: usize = 20_000;

struct HostGitRunner;

impl GitRunner for HostGitRunner {
    fn run(
        &self,
        cwd: &Path,
        args: &[std::ffi::OsString],
    ) -> Result<std::process::Output, GitError> {
        termust_host_exec::host_command("git", args, Some(cwd))
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|error| GitError::Io(format!("no se pudo ejecutar git: {error}")))
    }
}

pub struct GitPanel {
    root: gtk::Box,
    repo_label: gtk::Label,
    branch_label: gtk::Label,
    push_button: gtk::Button,
    list: gtk::ListBox,
    row_files: RefCell<Vec<Option<usize>>>,
    diff_buffer: gtk::TextBuffer,
    diff_path: gtk::Label,
    staged_toggle: gtk::ToggleButton,
    load_more: gtk::Button,
    subject: gtk::Entry,
    body: gtk::Entry,
    hint: gtk::Label,
    commit_button: gtk::Button,
    toast: libadwaita::ToastOverlay,
    git: Git,
    cwd: RefCell<Option<PathBuf>>,
    repo: RefCell<Option<Repo>>,
    status: RefCell<Option<RepoStatus>>,
    remotes: RefCell<Vec<Remote>>,
    last_push: RefCell<Option<Result<String, String>>>,
    selected: RefCell<Option<PathBuf>>,
    diff_cap: Cell<usize>,
    diff_lines: Cell<usize>,
    diff_binary: Cell<bool>,
    diff_truncated: Cell<bool>,
    diff_generation: Cell<u64>,
    repo_generation: Cell<u64>,
    busy: Cell<bool>,
    visible: Cell<bool>,
    on_close: RefCell<Option<Rc<dyn Fn()>>>,
    self_weak: Weak<Self>,
}

impl GitPanel {
    pub fn new(toast: libadwaita::ToastOverlay) -> Rc<Self> {
        let back = gtk::Button::from_icon_name("go-previous-symbolic");
        back.add_css_class("flat");
        back.update_property(&[gtk::accessible::Property::Label("Volver al terminal")]);
        back.set_tooltip_text(Some("Volver al terminal"));
        let repo_label = gtk::Label::new(None);
        repo_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        repo_label.set_xalign(0.0);
        repo_label.set_hexpand(true);
        repo_label.add_css_class("heading");
        let branch_label = gtk::Label::new(None);
        branch_label.add_css_class("dim-label");
        branch_label.add_css_class("caption");
        let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh.add_css_class("flat");
        refresh.update_property(&[gtk::accessible::Property::Label("Actualizar")]);
        refresh.set_tooltip_text(Some("Actualizar"));
        let remotes_button = gtk::Button::with_label("Remotes");
        remotes_button.add_css_class("flat");
        let push_button = gtk::Button::with_label("Push");
        push_button.add_css_class("suggested-action");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_top(6);
        header.set_margin_bottom(6);
        header.set_margin_start(8);
        header.set_margin_end(8);
        header.append(&back);
        header.append(&repo_label);
        header.append(&branch_label);
        header.append(&remotes_button);
        header.append(&push_button);
        header.append(&refresh);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        let list_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();

        let stage = gtk::Button::with_label("Stage");
        stage.add_css_class("flat");
        let unstage = gtk::Button::with_label("Unstage");
        unstage.add_css_class("flat");
        let stage_all = gtk::Button::with_label("Stage all");
        stage_all.add_css_class("flat");
        let unstage_all = gtk::Button::with_label("Unstage all");
        unstage_all.add_css_class("flat");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        actions.set_margin_top(4);
        actions.set_margin_bottom(4);
        actions.set_margin_start(6);
        actions.set_margin_end(6);
        actions.append(&stage);
        actions.append(&unstage);
        actions.append(&stage_all);
        actions.append(&unstage_all);

        let left = gtk::Box::new(gtk::Orientation::Vertical, 0);
        left.set_size_request(300, -1);
        left.append(&list_scroller);
        left.append(&actions);

        let diff_path = gtk::Label::new(None);
        diff_path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        diff_path.set_xalign(0.0);
        diff_path.set_hexpand(true);
        diff_path.add_css_class("caption");
        let staged_toggle = gtk::ToggleButton::with_label("Staged");
        staged_toggle.add_css_class("flat");
        staged_toggle.set_tooltip_text(Some("Ver el diff del index"));
        let load_more = gtk::Button::with_label("Cargar más");
        load_more.add_css_class("flat");
        load_more.set_visible(false);
        let diff_header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        diff_header.set_margin_top(6);
        diff_header.set_margin_bottom(6);
        diff_header.set_margin_start(8);
        diff_header.set_margin_end(8);
        diff_header.append(&diff_path);
        diff_header.append(&staged_toggle);
        diff_header.append(&load_more);

        let diff_buffer = gtk::TextBuffer::new(None);
        let diff_view = gtk::TextView::with_buffer(&diff_buffer);
        diff_view.set_editable(false);
        diff_view.set_cursor_visible(false);
        diff_view.set_monospace(true);
        diff_view.set_wrap_mode(gtk::WrapMode::None);
        diff_view.set_left_margin(8);
        diff_view.set_top_margin(6);
        let diff_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&diff_view)
            .build();
        crate::diff_render::create_diff_tags(&diff_buffer);

        let right = gtk::Box::new(gtk::Orientation::Vertical, 0);
        right.append(&diff_header);
        right.append(&diff_scroller);

        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&left));
        paned.set_end_child(Some(&right));
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(false);
        paned.set_position(300);
        paned.set_vexpand(true);

        let subject = gtk::Entry::new();
        subject.set_placeholder_text(Some("Asunto del commit (≤50)"));
        subject.set_hexpand(true);
        let body = gtk::Entry::new();
        body.set_placeholder_text(Some("Cuerpo opcional (líneas ≤72)"));
        body.set_hexpand(true);
        let hint = gtk::Label::new(Some("Asunto 0/50"));
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");
        let commit_button = gtk::Button::with_label("Commit");
        commit_button.add_css_class("suggested-action");
        commit_button.set_sensitive(false);
        let commit_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        commit_box.set_margin_top(6);
        commit_box.set_margin_bottom(8);
        commit_box.set_margin_start(8);
        commit_box.set_margin_end(8);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&subject);
        row.append(&commit_button);
        commit_box.append(&row);
        commit_box.append(&body);
        commit_box.append(&hint);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("termust-git");
        root.append(&header);
        root.append(&paned);
        root.append(&commit_box);
        root.set_visible(false);

        let panel = Rc::new_cyclic(|weak: &Weak<Self>| Self {
            root,
            repo_label,
            branch_label,
            push_button: push_button.clone(),
            list,
            row_files: RefCell::new(Vec::new()),
            diff_buffer,
            diff_path,
            staged_toggle,
            load_more: load_more.clone(),
            subject,
            body,
            hint,
            commit_button,
            toast,
            git: Git::new(Arc::new(HostGitRunner)),
            cwd: RefCell::new(None),
            repo: RefCell::new(None),
            status: RefCell::new(None),
            remotes: RefCell::new(Vec::new()),
            last_push: RefCell::new(None),
            selected: RefCell::new(None),
            diff_cap: Cell::new(DIFF_CAP),
            diff_lines: Cell::new(0),
            diff_binary: Cell::new(false),
            diff_truncated: Cell::new(false),
            diff_generation: Cell::new(0),
            repo_generation: Cell::new(0),
            busy: Cell::new(false),
            visible: Cell::new(false),
            on_close: RefCell::new(None),
            self_weak: weak.clone(),
        });
        panel.connect(
            back,
            refresh,
            remotes_button,
            push_button,
            stage,
            unstage,
            stage_all,
            unstage_all,
            load_more,
        );
        panel
    }

    #[allow(clippy::too_many_arguments)]
    fn connect(
        self: &Rc<Self>,
        back: gtk::Button,
        refresh: gtk::Button,
        remotes_button: gtk::Button,
        push_button: gtk::Button,
        stage: gtk::Button,
        unstage: gtk::Button,
        stage_all: gtk::Button,
        unstage_all: gtk::Button,
        load_more: gtk::Button,
    ) {
        {
            let weak = self.self_weak.clone();
            back.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.close();
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            refresh.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.refresh();
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            remotes_button.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.show_remotes();
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            push_button.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.push_now();
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            stage.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.stage_selected();
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            unstage.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.unstage_selected();
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            stage_all.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.stage_paths(&[PathBuf::from(".")]);
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            unstage_all.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.unstage_paths(&[PathBuf::from(".")]);
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            load_more.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.diff_cap.set(DIFF_CAP_LARGE);
                    if let Some(path) = panel.selected.borrow().clone() {
                        panel.load_diff(path, panel.staged_toggle.is_active());
                    }
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            self.list.connect_row_selected(move |_, row| {
                let Some(row) = row else {
                    return;
                };
                if let Some(panel) = weak.upgrade() {
                    let row_index = row.index().max(0) as usize;
                    let file_index = panel.row_files.borrow().get(row_index).copied().flatten();
                    let path = file_index.and_then(|index| {
                        panel
                            .status
                            .borrow()
                            .as_ref()
                            .and_then(|status| status.files.get(index))
                            .map(|file| file.path.clone())
                    });
                    if let Some(path) = path {
                        panel.selected.replace(Some(path.clone()));
                        panel.load_diff(path, panel.staged_toggle.is_active());
                    }
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            self.staged_toggle.connect_toggled(move |_| {
                if let Some(panel) = weak.upgrade() {
                    if let Some(path) = panel.selected.borrow().clone() {
                        panel.load_diff(path, panel.staged_toggle.is_active());
                    }
                }
            });
        }
        for entry in [&self.subject, &self.body] {
            let weak = self.self_weak.clone();
            entry.connect_changed(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.update_commit_hint();
                }
            });
        }
        {
            let weak = self.self_weak.clone();
            self.commit_button.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.commit();
                }
            });
        }
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn is_open(&self) -> bool {
        self.root.is_visible()
    }

    pub fn set_on_close(&self, callback: impl Fn() + 'static) {
        *self.on_close.borrow_mut() = Some(Rc::new(callback));
    }

    pub fn open(self: &Rc<Self>, cwd: &Path) {
        self.root.set_visible(true);
        self.visible.set(true);
        self.repo_label.set_text("…");
        self.branch_label.set_text("");
        self.load_repo(cwd);
    }

    pub fn close(&self) {
        if !self.visible.replace(false) {
            return;
        }
        self.root.set_visible(false);
        if let Some(callback) = self.on_close.borrow().as_ref() {
            callback();
        }
    }

    pub fn refresh(self: &Rc<Self>) {
        let cwd = self.cwd.borrow().clone();
        if let Some(cwd) = cwd {
            self.load_repo(&cwd);
        }
    }

    fn load_repo(self: &Rc<Self>, cwd: &Path) {
        self.cwd.replace(Some(cwd.to_path_buf()));
        let generation = self.repo_generation.get() + 1;
        self.repo_generation.set(generation);
        let git = self.git.clone();
        let cwd = cwd.to_path_buf();
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("termust-git".into())
            .spawn(move || {
                let result = git
                    .discover(&cwd)
                    .and_then(|repo| git.status(&repo).map(|status| (repo, status)));
                let _ = tx.send(result);
            });
        listen(self.self_weak.clone(), rx, move |panel, result| {
            if panel.repo_generation.get() != generation {
                return;
            }
            match result {
                Ok((repo, status)) => panel.apply_status(repo, status),
                Err(error) => panel.show_error(&error.to_string()),
            }
        });
    }

    fn apply_status(self: &Rc<Self>, repo: Repo, status: RepoStatus) {
        self.repo_label.set_text(&repo.root.display().to_string());
        self.repo_label
            .set_tooltip_text(Some(&repo.root.display().to_string()));
        let branch = if status.branch.detached {
            format!(
                "detached @ {}",
                &status.branch.head[..7.min(status.branch.head.len())]
            )
        } else {
            status.branch.head.clone()
        };
        let mut label = branch;
        if let Some(upstream) = &status.branch.upstream {
            label.push_str(&format!("  ↑{upstream}"));
        }
        if status.branch.ahead > 0 || status.branch.behind > 0 {
            label.push_str(&format!(
                "  +{} −{}",
                status.branch.ahead, status.branch.behind
            ));
        }
        self.branch_label.set_text(&label);
        self.render_files(&status);
        let root = repo.root.clone();
        self.repo.replace(Some(repo));
        self.status.replace(Some(status));
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("termust-git-remotes".into())
            .spawn(move || {
                let _ = tx.send(crate::remotes::list_remotes(&root));
            });
        listen(self.self_weak.clone(), rx, |panel, result| match result {
            Ok(remotes) => {
                panel.remotes.replace(remotes);
            }
            Err(error) => eprintln!("termust: remotes: {error}"),
        });
    }

    fn render_files(&self, status: &RepoStatus) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        self.row_files.borrow_mut().clear();
        let (staged, unstaged) = split_staged(&status.files);
        if !status.files.is_empty() {
            for (label, count) in [
                ("Preparados", staged.len()),
                ("Sin preparar", unstaged.len()),
            ] {
                if count == 0 {
                    continue;
                }
                let header = gtk::Label::new(Some(&format!("{label} ({count})")));
                header.set_xalign(0.0);
                header.add_css_class("dim-label");
                header.add_css_class("caption");
                header.set_margin_top(6);
                header.set_margin_start(6);
                let row = gtk::ListBoxRow::new();
                row.set_selectable(false);
                row.set_activatable(false);
                row.set_child(Some(&header));
                self.list.append(&row);
                self.row_files.borrow_mut().push(None);
                if label == "Preparados" {
                    for file in &staged {
                        let index = status
                            .files
                            .iter()
                            .position(|candidate| candidate == *file)
                            .unwrap_or(0);
                        self.append_file_row(file, index);
                    }
                } else {
                    for file in &unstaged {
                        let index = status
                            .files
                            .iter()
                            .position(|candidate| candidate == *file)
                            .unwrap_or(0);
                        self.append_file_row(file, index);
                    }
                }
            }
        }
        if status.files.is_empty() {
            let row = gtk::ListBoxRow::new();
            row.set_selectable(false);
            let label = gtk::Label::new(Some("Sin cambios"));
            label.add_css_class("dim-label");
            row.set_child(Some(&label));
            self.list.append(&row);
            self.row_files.borrow_mut().push(None);
        }
    }

    fn append_file_row(&self, file: &termust_git_local::ChangedFile, index: usize) {
        let row = gtk::ListBoxRow::new();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.set_margin_top(4);
        content.set_margin_bottom(4);
        content.set_margin_start(6);
        content.set_margin_end(6);
        let badge = gtk::Label::new(Some(&format!("{}{}", file.index, file.worktree)));
        badge.set_width_chars(2);
        badge.add_css_class("monospace");
        badge.add_css_class(status_class(file.kind));
        content.append(&badge);
        let name = if let Some(orig) = &file.orig_path {
            format!("{} → {}", orig.display(), file.path.display())
        } else {
            file.path.display().to_string()
        };
        let label = gtk::Label::new(Some(&name));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        label.set_hexpand(true);
        content.append(&label);
        row.set_child(Some(&content));
        row.set_tooltip_text(Some(&file.path.display().to_string()));
        self.list.append(&row);
        self.row_files.borrow_mut().push(Some(index));
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.subject.set_sensitive(!busy);
        self.body.set_sensitive(!busy);
        self.commit_button
            .set_sensitive(!busy && !self.message().trim().is_empty());
    }

    fn load_diff(self: &Rc<Self>, path: PathBuf, staged: bool) {
        let Some(repo) = self.repo.borrow().clone() else {
            return;
        };
        let generation = self.diff_generation.get() + 1;
        self.diff_generation.set(generation);
        self.diff_path.set_text(&path.display().to_string());
        let git = self.git.clone();
        let cap = self.diff_cap.get();
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("termust-git-diff".into())
            .spawn(move || {
                let _ = tx.send(git.diff(&repo, Some(&path), staged, cap));
            });
        listen(self.self_weak.clone(), rx, move |panel, result| {
            if panel.diff_generation.get() != generation {
                return;
            }
            match result {
                Ok(diff) => panel.apply_diff(diff),
                Err(error) => panel.show_error(&error.to_string()),
            }
        });
    }

    fn apply_diff(&self, diff: Diff) {
        self.diff_buffer.set_text("");
        self.diff_lines.set(diff.lines.len());
        self.diff_binary.set(diff.binary);
        self.diff_truncated.set(diff.truncated);
        self.load_more.set_visible(diff.truncated);
        if diff.binary {
            self.diff_buffer
                .set_text("Archivo binario: no se muestra el diff.");
            return;
        }
        crate::diff_render::fill_diff_buffer(&self.diff_buffer, &diff);
    }

    fn stage_selected(self: &Rc<Self>) {
        if let Some(path) = self.selected.borrow().clone() {
            self.stage_paths(&[path]);
        }
    }

    fn unstage_selected(self: &Rc<Self>) {
        if let Some(path) = self.selected.borrow().clone() {
            self.unstage_paths(&[path]);
        }
    }

    fn stage_paths(self: &Rc<Self>, paths: &[PathBuf]) {
        let Some(repo) = self.repo.borrow().clone() else {
            return;
        };
        self.repo_generation.set(self.repo_generation.get() + 1);
        self.set_busy(true);
        let git = self.git.clone();
        let paths = paths.to_vec();
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("termust-git-stage".into())
            .spawn(move || {
                let result = git
                    .stage(&repo, &paths)
                    .and_then(|_| git.status(&repo).map(|status| (repo.clone(), status)));
                let _ = tx.send(result);
            });
        let weak = self.self_weak.clone();
        listen(weak, rx, |panel, result| {
            panel.set_busy(false);
            match result {
                Ok((repo, status)) => panel.apply_status(repo, status),
                Err(error) => panel.show_error(&error.to_string()),
            }
        });
    }

    fn unstage_paths(self: &Rc<Self>, paths: &[PathBuf]) {
        let Some(repo) = self.repo.borrow().clone() else {
            return;
        };
        self.repo_generation.set(self.repo_generation.get() + 1);
        self.set_busy(true);
        let git = self.git.clone();
        let paths = paths.to_vec();
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("termust-git-unstage".into())
            .spawn(move || {
                let result = git
                    .unstage(&repo, &paths)
                    .and_then(|_| git.status(&repo).map(|status| (repo.clone(), status)));
                let _ = tx.send(result);
            });
        listen(self.self_weak.clone(), rx, |panel, result| {
            panel.set_busy(false);
            match result {
                Ok((repo, status)) => panel.apply_status(repo, status),
                Err(error) => panel.show_error(&error.to_string()),
            }
        });
    }

    fn commit(self: &Rc<Self>) {
        let message = self.message();
        if message.trim().is_empty() {
            self.toast
                .add_toast(libadwaita::Toast::new("El mensaje de commit está vacío"));
            return;
        }
        if self.busy.get() {
            return;
        }
        let Some(repo) = self.repo.borrow().clone() else {
            return;
        };
        self.repo_generation.set(self.repo_generation.get() + 1);
        self.set_busy(true);
        let git = self.git.clone();
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("termust-git-commit".into())
            .spawn(move || {
                let result = git.commit(&repo, &message).and_then(|commit| {
                    git.status(&repo)
                        .map(|status| (commit, repo.clone(), status))
                });
                let _ = tx.send(result);
            });
        listen(self.self_weak.clone(), rx, |panel, result| {
            panel.set_busy(false);
            match result {
                Ok((commit, repo, status)) => {
                    panel.toast.add_toast(libadwaita::Toast::new(&format!(
                        "Commit {}",
                        &commit.oid[..7.min(commit.oid.len())]
                    )));
                    panel.subject.set_text("");
                    panel.body.set_text("");
                    panel.apply_status(repo, status);
                }
                Err(error) => panel.show_error(&error.to_string()),
            }
        });
    }

    fn message(&self) -> String {
        let subject = self.subject.text().trim().to_string();
        let body = self.body.text().trim().to_string();
        if body.is_empty() {
            subject
        } else {
            format!("{subject}\n\n{body}")
        }
    }

    fn update_commit_hint(&self) {
        let message = self.message();
        let hint = commit_hint(&message);
        let text = format!(
            "Asunto {}/50 · cuerpo ≤72{}",
            hint.subject_len,
            if hint.body_line_too_long {
                " · línea larga"
            } else {
                ""
            }
        );
        self.hint.set_text(&text);
        if hint.subject_too_long || hint.body_line_too_long {
            self.hint.add_css_class("warning");
        } else {
            self.hint.remove_css_class("warning");
        }
        self.commit_button
            .set_sensitive(!self.busy.get() && !message.trim().is_empty());
    }

    fn show_error(&self, message: &str) {
        self.toast.add_toast(libadwaita::Toast::new(message));
    }

    fn show_remotes(self: &Rc<Self>) {
        let Some(repo) = self.repo.borrow().clone() else {
            self.show_error("No hay repositorio");
            return;
        };
        let dialog = libadwaita::Dialog::new();
        dialog.set_title("Remotes");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_size_request(460, -1);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        for remote in self.remotes.borrow().iter() {
            let row = gtk::ListBoxRow::new();
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            text.set_hexpand(true);
            let name = gtk::Label::new(Some(&remote.name));
            name.set_xalign(0.0);
            name.add_css_class("heading");
            let url = gtk::Label::new(Some(if remote.push_url.is_empty() {
                &remote.fetch_url
            } else {
                &remote.push_url
            }));
            url.set_xalign(0.0);
            url.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            url.add_css_class("dim-label");
            url.add_css_class("caption");
            text.append(&name);
            text.append(&url);
            line.append(&text);
            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some("Eliminar remoto"));
            let repo_root = repo.root.clone();
            let name_value = remote.name.clone();
            let weak = self.self_weak.clone();
            let dialog_clone = dialog.clone();
            remove.connect_clicked(move |_| {
                match crate::remotes::remove_remote(&repo_root, &name_value) {
                    Ok(()) => {
                        dialog_clone.close();
                        if let Some(panel) = weak.upgrade() {
                            panel.refresh();
                        }
                    }
                    Err(error) => eprintln!("termust: remoto: {error}"),
                }
            });
            line.append(&remove);
            row.set_child(Some(&line));
            list.append(&row);
        }
        if self.remotes.borrow().is_empty() {
            let label = gtk::Label::new(Some("Sin remotos"));
            label.add_css_class("dim-label");
            list.append(&label);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(100)
            .max_content_height(220)
            .child(&list)
            .build();
        content.append(&scroller);

        let name = gtk::Entry::new();
        name.set_placeholder_text(Some("Nombre (origin)"));
        let url = gtk::Entry::new();
        url.set_placeholder_text(Some("https://host/repo.git o git@host:repo.git"));
        url.set_hexpand(true);
        let add = gtk::Button::with_label("Agregar");
        add.add_css_class("suggested-action");
        let form = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        form.append(&name);
        form.append(&url);
        form.append(&add);
        content.append(&form);
        {
            let repo_root = repo.root.clone();
            let weak = self.self_weak.clone();
            let dialog_clone = dialog.clone();
            let name = name.clone();
            let url = url.clone();
            let toast = self.toast.clone();
            add.connect_clicked(move |_| {
                let name_value = name.text().trim().to_string();
                let url_value = url.text().trim().to_string();
                match crate::remotes::add_remote(&repo_root, &name_value, &url_value) {
                    Ok(()) => {
                        dialog_clone.close();
                        if let Some(panel) = weak.upgrade() {
                            panel.refresh();
                        }
                    }
                    Err(error) => {
                        toast.add_toast(libadwaita::Toast::new(&error));
                    }
                }
            });
        }
        dialog.set_child(Some(&content));
        dialog.present(Some(&self.root));
    }

    fn push_now(self: &Rc<Self>) {
        let Some(repo) = self.repo.borrow().clone() else {
            self.show_error("No hay repositorio");
            return;
        };
        let (branch, detached) = self
            .status
            .borrow()
            .as_ref()
            .map(|status| (status.branch.head.clone(), status.branch.detached))
            .unwrap_or_default();
        if branch.is_empty() || detached {
            self.show_error("No hay una rama actual para pushear");
            return;
        }
        let remotes = self.remotes.borrow().clone();
        let Some(remote) = remotes
            .iter()
            .find(|remote| remote.name == "origin")
            .or_else(|| remotes.first())
            .cloned()
        else {
            self.show_error("No hay remotos configurados");
            return;
        };
        let url = if remote.push_url.is_empty() {
            remote.fetch_url.clone()
        } else {
            remote.push_url.clone()
        };
        let spec = match parse_remote_url(&url) {
            Ok(spec) => spec,
            Err(error) => {
                self.show_error(&error.to_string());
                return;
            }
        };
        if spec.transport == Transport::Https {
            let origin = spec.origin.clone();
            match crate::remotes::load_credentials(&origin) {
                Ok(Some((username, secret))) => {
                    self.start_push(
                        repo.root,
                        remote.name,
                        branch,
                        spec.transport,
                        Some((username, secret)),
                    );
                }
                Ok(None) => {
                    self.ask_https(origin, repo.root, remote.name, branch);
                }
                Err(error) => self.show_error(&error),
            }
        } else {
            self.start_push(repo.root, remote.name, branch, spec.transport, None);
        }
    }

    fn ask_https(
        self: &Rc<Self>,
        origin: String,
        repo_root: PathBuf,
        remote: String,
        branch: String,
    ) {
        let username = gtk::Entry::new();
        username.set_placeholder_text(Some("Usuario"));
        let password = gtk::PasswordEntry::new();
        password.set_placeholder_text(Some("Token o contraseña"));
        password.set_show_peek_icon(true);
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 6);
        box_.append(&username);
        box_.append(&password);
        let dialog = libadwaita::AlertDialog::builder()
            .title("Credenciales HTTPS")
            .body(format!("Se guardarán cifradas en el vault para {origin}."))
            .extra_child(&box_)
            .build();
        dialog.add_response("cancel", "Cancelar");
        dialog.add_response("save", "Guardar y pushear");
        dialog.set_response_appearance("save", libadwaita::ResponseAppearance::Suggested);
        let weak = self.self_weak.clone();
        let toast = self.toast.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "save" {
                return;
            }
            let Some(panel) = weak.upgrade() else {
                return;
            };
            let user = username.text().to_string();
            let pass = password.text().to_string();
            if user.trim().is_empty() || pass.is_empty() {
                toast.add_toast(libadwaita::Toast::new("Usuario y token son obligatorios"));
                return;
            }
            match crate::remotes::save_credentials(&origin, &user, pass.as_bytes()) {
                Ok(()) => match crate::remotes::load_credentials(&origin) {
                    Ok(Some(credentials)) => panel.start_push(
                        repo_root.clone(),
                        remote.clone(),
                        branch.clone(),
                        Transport::Https,
                        Some(credentials),
                    ),
                    Ok(None) => {}
                    Err(error) => panel.show_error(&error),
                },
                Err(error) => panel.show_error(&error),
            }
        });
        dialog.present(Some(&self.root));
    }

    fn start_push(
        self: &Rc<Self>,
        repo_root: PathBuf,
        remote: String,
        branch: String,
        transport: Transport,
        credentials: Option<(String, termust_vault::Secret)>,
    ) {
        if self.busy.get() {
            return;
        }
        self.set_busy(true);
        self.push_button.set_sensitive(false);
        self.last_push.replace(None);
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("termust-git-push".into())
            .spawn(move || {
                let _ = tx.send(crate::remotes::push(
                    &repo_root,
                    &remote,
                    &branch,
                    transport,
                    credentials,
                ));
            });
        listen(self.self_weak.clone(), rx, |panel, result| {
            panel.set_busy(false);
            panel.push_button.set_sensitive(true);
            match &result {
                Ok(message) => {
                    panel.toast.add_toast(libadwaita::Toast::new(message));
                    panel.refresh();
                }
                Err(error) => panel.show_error(error),
            }
            panel.last_push.replace(Some(result));
        });
    }

    pub fn push_now_for_test(self: &Rc<Self>) {
        self.push_now();
    }

    pub fn push_result(&self) -> Option<Result<String, String>> {
        self.last_push.borrow().clone()
    }

    pub fn remotes_count(&self) -> usize {
        self.remotes.borrow().len()
    }

    pub fn remote_names(&self) -> Vec<String> {
        self.remotes
            .borrow()
            .iter()
            .map(|remote| remote.name.clone())
            .collect()
    }

    pub fn add_remote_for_test(self: &Rc<Self>, name: &str, url: &str) -> bool {
        let Some(repo) = self.repo.borrow().clone() else {
            return false;
        };
        if crate::remotes::add_remote(&repo.root, name, url).is_ok() {
            if let Ok(remotes) = crate::remotes::list_remotes(&repo.root) {
                self.remotes.replace(remotes);
            }
            true
        } else {
            false
        }
    }

    pub fn remove_remote_for_test(self: &Rc<Self>, name: &str) -> bool {
        let Some(repo) = self.repo.borrow().clone() else {
            return false;
        };
        if crate::remotes::remove_remote(&repo.root, name).is_ok() {
            if let Ok(remotes) = crate::remotes::list_remotes(&repo.root) {
                self.remotes.replace(remotes);
            }
            true
        } else {
            false
        }
    }

    // Getters para hooks E2E.
    pub fn file_count(&self) -> usize {
        self.status
            .borrow()
            .as_ref()
            .map(|status| status.files.len())
            .unwrap_or(0)
    }

    pub fn branch(&self) -> String {
        self.status
            .borrow()
            .as_ref()
            .map(|status| status.branch.head.clone())
            .unwrap_or_default()
    }

    pub fn repo_path(&self) -> Option<PathBuf> {
        self.repo.borrow().as_ref().map(|repo| repo.root.clone())
    }

    pub fn diff_line_count(&self) -> usize {
        self.diff_lines.get()
    }

    pub fn diff_is_binary(&self) -> bool {
        self.diff_binary.get()
    }

    pub fn diff_truncated(&self) -> bool {
        self.diff_truncated.get()
    }

    pub fn index_status_of(&self, name: &str) -> Option<char> {
        self.status.borrow().as_ref().and_then(|status| {
            status
                .files
                .iter()
                .find(|file| file.path == Path::new(name))
                .map(|file| file.index)
        })
    }

    pub fn load_full_diff_now(self: &Rc<Self>) {
        self.diff_cap.set(DIFF_CAP_LARGE);
        if let Some(path) = self.selected.borrow().clone() {
            self.load_diff(path, self.staged_toggle.is_active());
        }
    }

    pub fn has_file(&self, name: &str) -> bool {
        self.status
            .borrow()
            .as_ref()
            .map(|status| status.files.iter().any(|file| file.path == Path::new(name)))
            .unwrap_or(false)
    }

    pub fn kind_of(&self, name: &str) -> Option<ChangeKind> {
        self.status.borrow().as_ref().and_then(|status| {
            status
                .files
                .iter()
                .find(|file| file.path == Path::new(name))
                .map(|file| file.kind)
        })
    }

    pub fn select_path(self: &Rc<Self>, name: &str) -> bool {
        let index = self.status.borrow().as_ref().and_then(|status| {
            status
                .files
                .iter()
                .position(|file| file.path == Path::new(name))
        });
        let Some(index) = index else {
            return false;
        };
        let row_index = self
            .row_files
            .borrow()
            .iter()
            .position(|mapped| *mapped == Some(index));
        if let Some(row_index) = row_index {
            if let Some(row) = self.list.row_at_index(row_index as i32) {
                self.list.select_row(Some(&row));
            }
        }
        let path = PathBuf::from(name);
        self.selected.replace(Some(path.clone()));
        self.load_diff(path, self.staged_toggle.is_active());
        true
    }

    pub fn stage_selected_now(self: &Rc<Self>) {
        self.stage_selected();
    }

    pub fn unstage_selected_now(self: &Rc<Self>) {
        self.unstage_selected();
    }

    pub fn set_message(&self, subject: &str, body: &str) {
        self.subject.set_text(subject);
        self.body.set_text(body);
        self.update_commit_hint();
    }

    pub fn commit_now(self: &Rc<Self>) {
        self.commit();
    }

    pub fn set_staged_view(&self, staged: bool) {
        self.staged_toggle.set_active(staged);
    }
}

fn listen<T: Send + 'static>(
    weak: Weak<GitPanel>,
    rx: mpsc::Receiver<T>,
    apply: impl Fn(&Rc<GitPanel>, T) + 'static,
) {
    glib::timeout_add_local(Duration::from_millis(40), move || match rx.try_recv() {
        Ok(value) => {
            if let Some(panel) = weak.upgrade() {
                apply(&panel, value);
            }
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
    });
}

fn status_class(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added | ChangeKind::Untracked => "termust-git-added",
        ChangeKind::Deleted => "termust-git-deleted",
        ChangeKind::Conflict => "termust-git-conflict",
        ChangeKind::Ignored => "dim-label",
        _ => "termust-git-modified",
    }
}
