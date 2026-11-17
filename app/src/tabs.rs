use gtk::glib;
use gtk4 as gtk;
use libadwaita::prelude::*;
use libadwaita::{ApplicationWindow, ResponseAppearance, TabBar, TabPage, TabView};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use termust_core::pty::{PtySpawner, SpawnSpec};
use termust_core::tabs::{Activity, PaneTree, Tab, TabId};
use termust_core::terminal::Size;
use termust_core::title::display_title;
use termust_host_exec::{shell_command, ShellIntegration};
use termust_pty_unix::UnixPtySpawner;
use termust_render_cairo::CairoRenderer;
use termust_term_engine::AlacrittyEngine;
use termust_ui::TerminalView;

struct Entry {
    id: TabId,
    page: TabPage,
    view: TerminalView,
    cwd: PathBuf,
    title_override: Option<String>,
    last_osc_title: Option<String>,
}

pub struct TabManager {
    tab_view: TabView,
    tab_bar: TabBar,
    entries: Vec<Entry>,
    spawner: Rc<UnixPtySpawner>,
    shell: PathBuf,
    integration: ShellIntegration,
    home: PathBuf,
    window: ApplicationWindow,
    next_id: TabId,
}

impl TabManager {
    pub fn new(
        spawner: Rc<UnixPtySpawner>,
        shell: PathBuf,
        integration: ShellIntegration,
        home: PathBuf,
        window: ApplicationWindow,
    ) -> Result<Self, String> {
        let tab_view = TabView::new();
        let tab_bar = TabBar::new();
        tab_bar.set_view(Some(&tab_view));
        tab_bar.set_autohide(false);
        tab_bar.set_expand_tabs(false);
        tab_bar.set_hexpand(true);
        let mut manager = Self {
            tab_view,
            tab_bar,
            entries: Vec::new(),
            spawner,
            shell,
            integration,
            home: home.clone(),
            window,
            next_id: 1,
        };
        manager.open_tab(home)?;
        Ok(manager)
    }

    pub fn tab_view(&self) -> &TabView {
        &self.tab_view
    }

    pub fn tab_bar(&self) -> &TabBar {
        &self.tab_bar
    }

    pub fn open_tab(&mut self, cwd: PathBuf) -> Result<(), String> {
        let view = self.spawn_view(&cwd)?;
        let page = self.tab_view.append(view.widget());
        page.set_title("Terminal");
        page.set_tooltip(&cwd.display().to_string());
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(Entry {
            id,
            page: page.clone(),
            view,
            cwd,
            title_override: None,
            last_osc_title: None,
        });
        self.tab_view.set_selected_page(&page);
        if let Some(e) = self.entries.last() {
            e.view.grab_focus();
        }
        Ok(())
    }

    fn spawn_view(&self, cwd: &Path) -> Result<TerminalView, String> {
        let command = shell_command(
            &self.shell,
            &self.integration,
            termust_host_exec::is_flatpak(),
        );
        let size = Size::new(80, 24);
        let handle = self
            .spawner
            .spawn(SpawnSpec {
                argv: command.argv,
                cwd: Some(cwd.to_path_buf()),
                env: command.env,
                size,
            })
            .map_err(|e| e.to_string())?;
        let engine = AlacrittyEngine::new(size, 2_000);
        let renderer = CairoRenderer::new();
        Ok(TerminalView::new(
            Box::new(engine),
            handle.session,
            Box::new(renderer),
            handle.events,
        ))
    }

    pub fn close_active(&mut self) {
        if let Some(page) = self.tab_view.selected_page() {
            self.tab_view.close_page(&page);
        }
    }

    pub fn select_index(&mut self, index: usize) {
        if let Some(entry) = self.entries.get(index) {
            self.tab_view.set_selected_page(&entry.page);
        }
    }

    pub fn select_last(&mut self) {
        if let Some(entry) = self.entries.last() {
            self.tab_view.set_selected_page(&entry.page);
        }
    }

    pub fn select_next(&mut self) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let next = (self.active_index() + 1) % len;
        self.select_index(next);
    }

    pub fn select_prev(&mut self) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let prev = (self.active_index() + len - 1) % len;
        self.select_index(prev);
    }

    pub fn move_active(&mut self, delta: isize) {
        let len = self.entries.len();
        if len < 2 {
            return;
        }
        let from = self.active_index();
        let to = (from as isize + delta).clamp(0, len as isize - 1) as usize;
        if from == to {
            return;
        }
        let page = self.entries[from].page.clone();
        if self.tab_view.reorder_page(&page, to as i32) {
            let entry = self.entries.remove(from);
            self.entries.insert(to, entry);
        }
    }

    pub fn active_index(&self) -> usize {
        let Some(page) = self.tab_view.selected_page() else {
            return 0;
        };
        self.entries
            .iter()
            .position(|e| e.page == page)
            .unwrap_or(0)
    }

    pub fn focus_active(&self) {
        if let Some(entry) = self.entries.get(self.active_index()) {
            entry.view.grab_focus();
        }
    }

    pub fn rename_active(&mut self, name: String) {
        let index = self.active_index();
        if let Some(entry) = self.entries.get_mut(index) {
            let trimmed = name.trim();
            entry.title_override = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        self.update_titles();
    }

    pub fn update_titles(&mut self) {
        let active = self.tab_view.selected_page().map(|p| p.as_ptr());
        let mut updates = Vec::with_capacity(self.entries.len());
        for entry in &mut self.entries {
            if let Some(title) = entry.view.title() {
                entry.last_osc_title = Some(title);
            }
            if let Some(cwd) = entry.view.cwd() {
                entry.cwd = cwd;
            }
            let tab = Tab {
                id: entry.id,
                layout: PaneTree::Leaf(entry.id),
                active_pane: entry.id,
                title_override: entry.title_override.clone(),
                last_osc_title: entry.last_osc_title.clone(),
                cwd: entry.cwd.clone(),
                theme_id: None,
                activity: if entry.view.exited() {
                    Activity::Exited(None)
                } else {
                    Activity::Idle
                },
            };
            let spec = display_title(&tab, None);
            let exited = entry.view.exited();
            updates.push((entry.page.clone(), spec, exited));
        }
        for (page, spec, exited) in updates {
            page.set_title(&spec.text);
            page.set_tooltip(&spec.tooltip);
            if exited {
                page.set_needs_attention(true);
            }
            if Some(page.as_ptr()) == active {
                self.window.set_title(Some(&spec.text));
            }
        }
    }

    pub fn connect(manager: &Rc<RefCell<Self>>) {
        let tab_view = manager.borrow().tab_view.clone();

        {
            let manager = manager.clone();
            tab_view.connect_page_detached(move |_, page, _| {
                // El cierre puede ocurrir mientras el manager está prestado
                // (p. ej. Ctrl+Shift+W). Diferimos la limpieza al idle para no
                // re-entrar en el RefCell.
                let manager = manager.clone();
                let page = page.clone();
                glib::idle_add_local_once(move || {
                    let mut mgr = manager.borrow_mut();
                    mgr.entries.retain(|e| e.page != page);
                    if mgr.entries.is_empty() {
                        let home = mgr.home.clone();
                        let _ = mgr.open_tab(home);
                    }
                });
            });
        }

        {
            tab_view.connect_selected_page_notify(|view| {
                if let Some(page) = view.selected_page() {
                    page.child().grab_focus();
                }
            });
        }
    }

    pub fn rename_dialog(manager: &Rc<RefCell<Self>>) {
        let (window, current) = {
            let mgr = manager.borrow();
            let current = mgr
                .entries
                .get(mgr.active_index())
                .and_then(|e| e.title_override.clone())
                .unwrap_or_default();
            (mgr.window.clone(), current)
        };
        let entry = gtk::Entry::new();
        entry.set_text(&current);
        let dialog = libadwaita::AlertDialog::builder()
            .title("Rename tab")
            .extra_child(&entry)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("rename", "Rename");
        dialog.set_response_appearance("rename", ResponseAppearance::Suggested);
        let manager = manager.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "rename" {
                manager.borrow_mut().rename_active(entry.text().to_string());
            }
        });
        dialog.present(Some(&window));
    }
}
