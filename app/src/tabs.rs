use gtk::glib;
use gtk4 as gtk;
use libadwaita::prelude::*;
use libadwaita::{ApplicationWindow, ResponseAppearance, TabBar, TabPage, TabView};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use termust_core::pty::{PtySpawner, SpawnSpec};
use termust_core::tabs::{
    Direction, PaneId, PaneTree, SplitError, SplitOrientation, TabId, TabModel,
};
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
    container: gtk::Box,
    panes: HashMap<PaneId, TerminalView>,
}

pub struct TabManager {
    tab_view: TabView,
    tab_bar: TabBar,
    entries: Vec<Entry>,
    model: Rc<RefCell<TabModel>>,
    spawner: Rc<UnixPtySpawner>,
    shell: PathBuf,
    integration: ShellIntegration,
    window: ApplicationWindow,
    suspend_ratios: Rc<Cell<bool>>,
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

        let model = Rc::new(RefCell::new(TabModel::new(home)));
        let mut manager = Self {
            tab_view,
            tab_bar,
            entries: Vec::new(),
            model: model.clone(),
            spawner,
            shell,
            integration,
            window,
            suspend_ratios: Rc::new(Cell::new(false)),
        };
        let first = model.borrow().active_id();
        manager.attach_tab(first)?;
        Ok(manager)
    }

    pub fn tab_view(&self) -> &TabView {
        &self.tab_view
    }

    pub fn tab_bar(&self) -> &TabBar {
        &self.tab_bar
    }

    pub fn open_tab(&mut self, cwd: PathBuf) -> Result<(), String> {
        let tab_id = self.model.borrow_mut().new_tab(cwd);
        self.attach_tab(tab_id)?;
        if let Some(entry) = self.entries.iter().find(|e| e.id == tab_id) {
            self.tab_view.set_selected_page(&entry.page);
        }
        self.focus_active();
        Ok(())
    }

    fn attach_tab(&mut self, tab_id: TabId) -> Result<(), String> {
        let (layout, cwd) = {
            let model = self.model.borrow();
            let tab = model
                .get(tab_id)
                .ok_or_else(|| "pestaña inexistente".to_string())?;
            (tab.layout.clone(), tab.cwd.clone())
        };

        let mut panes = HashMap::new();
        for pane in layout.leaves() {
            let view = self.spawn_view(&cwd)?;
            self.install_focus(&view, tab_id, pane);
            panes.insert(pane, view);
        }

        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        container.set_hexpand(true);
        container.set_vexpand(true);

        self.suspend_ratios.set(true);
        let mut positions = Vec::new();
        let mut path = Vec::new();
        let root = self.build_node(tab_id, &layout, &panes, &mut path, &mut positions);
        container.append(&root);
        schedule_positions(positions);
        let suspend = self.suspend_ratios.clone();
        glib::timeout_add_local_once(Duration::from_millis(80), move || suspend.set(false));

        let page = self.tab_view.append(&container);
        page.set_title("Terminal");
        page.set_tooltip(&cwd.display().to_string());
        self.entries.push(Entry {
            id: tab_id,
            page,
            container,
            panes,
        });
        Ok(())
    }

    fn spawn_view(&self, cwd: &Path) -> Result<TerminalView, String> {
        let host_pty = termust_host_exec::host_pty();
        let command = shell_command(
            &self.shell,
            &self.integration,
            termust_host_exec::is_flatpak(),
            Some(cwd),
            host_pty.as_ref(),
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

    fn install_focus(&self, view: &TerminalView, tab_id: TabId, pane: PaneId) {
        let model = self.model.clone();
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(move |_| {
            if let Ok(mut model) = model.try_borrow_mut() {
                let _ = model.focus_pane(tab_id, pane);
            }
        });
        view.widget().add_controller(focus);
    }

    fn build_node(
        &self,
        tab_id: TabId,
        node: &PaneTree,
        panes: &HashMap<PaneId, TerminalView>,
        path: &mut Vec<bool>,
        positions: &mut Vec<(gtk::Paned, f64)>,
    ) -> gtk::Widget {
        match node {
            PaneTree::Leaf(pane) => panes
                .get(pane)
                .map(|view| view.widget().clone().upcast())
                .unwrap_or_else(|| gtk::Label::new(Some("panel no disponible")).upcast()),
            PaneTree::Split {
                orientation,
                ratio,
                first,
                second,
            } => {
                let gtk_orientation = match orientation {
                    SplitOrientation::Horizontal => gtk::Orientation::Horizontal,
                    SplitOrientation::Vertical => gtk::Orientation::Vertical,
                };
                let paned = gtk::Paned::new(gtk_orientation);
                paned.set_hexpand(true);
                paned.set_vexpand(true);
                paned.set_resize_start_child(true);
                paned.set_resize_end_child(true);
                paned.set_shrink_start_child(false);
                paned.set_shrink_end_child(false);

                let split_path = path.clone();
                {
                    let model = self.model.clone();
                    let suspend = self.suspend_ratios.clone();
                    paned.connect_position_notify(move |paned| {
                        if suspend.get() {
                            return;
                        }
                        let extent = match paned.orientation() {
                            gtk::Orientation::Horizontal => paned.width(),
                            _ => paned.height(),
                        };
                        let position = paned.position();
                        if extent < 20 || position <= 0 {
                            return;
                        }
                        let ratio = (position as f64 / extent as f64).clamp(0.1, 0.9);
                        if let Ok(mut model) = model.try_borrow_mut() {
                            let _ = model.set_ratio(tab_id, &split_path, ratio);
                        }
                    });
                }

                path.push(false);
                let start = self.build_node(tab_id, first, panes, path, positions);
                path.pop();
                path.push(true);
                let end = self.build_node(tab_id, second, panes, path, positions);
                path.pop();

                paned.set_start_child(Some(&start));
                paned.set_end_child(Some(&end));
                positions.push((paned.clone(), *ratio));
                paned.upcast()
            }
        }
    }

    fn rebuild(&mut self, tab_id: TabId) {
        let Some(index) = self.entries.iter().position(|e| e.id == tab_id) else {
            return;
        };
        let layout = {
            let model = self.model.borrow();
            model.get(tab_id).map(|tab| tab.layout.clone())
        };
        let Some(layout) = layout else {
            return;
        };

        self.suspend_ratios.set(true);
        let entry = &self.entries[index];
        // Desmontar primero: las vistas deben quedar sin padre antes de
        // volver a insertarlas en el árbol nuevo (GtkPaned lo exige).
        while let Some(child) = entry.container.first_child() {
            entry.container.remove(&child);
        }
        // El árbol viejo puede seguir vivo por posiciones pendientes; sacar
        // las vistas de su padre antes de reinsertarlas.
        for view in entry.panes.values() {
            view.widget().unparent();
        }
        let mut positions = Vec::new();
        let mut path = Vec::new();
        let root = self.build_node(tab_id, &layout, &entry.panes, &mut path, &mut positions);
        entry.container.append(&root);
        schedule_positions(positions);
        let suspend = self.suspend_ratios.clone();
        glib::timeout_add_local_once(Duration::from_millis(80), move || suspend.set(false));
    }

    pub fn split_active(&mut self, orientation: SplitOrientation) -> Result<PaneId, String> {
        let Some(index) = self.active_entry_index() else {
            return Err("sin pestaña activa".into());
        };
        let tab_id = self.entries[index].id;
        let (active, len, tab_cwd) = {
            let model = self.model.borrow();
            let tab = model
                .get(tab_id)
                .ok_or_else(|| "pestaña inexistente".to_string())?;
            (tab.active_pane, tab.layout.len(), tab.cwd.clone())
        };
        if len >= termust_core::tabs::MAX_PANES {
            return Err("Máximo 4 paneles por pestaña".into());
        }
        let cwd = self.entries[index]
            .panes
            .get(&active)
            .and_then(|view| view.cwd())
            .unwrap_or(tab_cwd);

        let new_pane = self
            .model
            .borrow_mut()
            .split_pane(tab_id, active, orientation)
            .map_err(|e| match e {
                SplitError::MaxPanes => "Máximo 4 paneles por pestaña".to_string(),
                _ => "No se pudo dividir el panel".to_string(),
            })?;

        let view = match self.spawn_view(&cwd) {
            Ok(view) => view,
            Err(e) => {
                let _ = self.model.borrow_mut().close_pane(tab_id, new_pane);
                return Err(e);
            }
        };
        self.install_focus(&view, tab_id, new_pane);
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == tab_id) {
            entry.panes.insert(new_pane, view);
        }
        self.rebuild(tab_id);
        self.focus_active();
        Ok(new_pane)
    }

    pub fn focus_direction(&mut self, direction: Direction) -> bool {
        let Some(index) = self.active_entry_index() else {
            return false;
        };
        let tab_id = self.entries[index].id;
        let moved = self.model.borrow_mut().focus_direction(tab_id, direction);
        if moved {
            self.focus_active();
        }
        moved
    }

    fn close_pane(&mut self, tab_id: TabId, pane: PaneId) -> bool {
        let closed = self.model.borrow_mut().close_pane(tab_id, pane);
        if !closed {
            return false;
        }
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == tab_id) {
            if let Some(view) = entry.panes.remove(&pane) {
                view.kill();
            }
        }
        self.rebuild(tab_id);
        self.focus_active();
        true
    }

    pub fn close_active(&mut self) {
        let Some(index) = self.active_entry_index() else {
            return;
        };
        let tab_id = self.entries[index].id;
        let (len, active) = {
            let model = self.model.borrow();
            match model.get(tab_id) {
                Some(tab) => (tab.layout.len(), tab.active_pane),
                None => return,
            }
        };
        if len > 1 {
            self.close_pane(tab_id, active);
        } else if let Some(page) = self.tab_view.selected_page() {
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
            self.model.borrow_mut().move_tab(from, to);
        }
    }

    pub fn active_pane_count(&self) -> usize {
        self.model.borrow().active().layout.len()
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

    fn active_entry_index(&self) -> Option<usize> {
        let page = self.tab_view.selected_page()?;
        self.entries.iter().position(|e| e.page == page)
    }

    pub fn focus_active(&self) {
        let Some(index) = self.active_entry_index() else {
            return;
        };
        let entry = &self.entries[index];
        let active = self.model.borrow().get(entry.id).map(|tab| tab.active_pane);
        if let Some(pane) = active {
            if let Some(view) = entry.panes.get(&pane) {
                view.grab_focus();
            }
        }
    }

    pub fn update_titles(&mut self) {
        let selected = self.tab_view.selected_page().map(|p| p.as_ptr());
        struct Update {
            page: TabPage,
            tab_id: TabId,
            title: Option<String>,
            cwd: Option<PathBuf>,
            exited: bool,
            panes: Vec<String>,
        }
        let mut updates = Vec::new();

        for entry in &self.entries {
            let (active, leaves) = {
                let model = self.model.borrow();
                match model.get(entry.id) {
                    Some(tab) => (tab.active_pane, tab.layout.leaves()),
                    None => continue,
                }
            };
            let view = entry.panes.get(&active);
            let title = view.and_then(|view| view.title());
            let cwd = view.and_then(|view| view.cwd());
            let exited = entry.panes.values().any(|view| view.exited());
            let mut pane_lines = Vec::new();
            for pane in leaves {
                if let Some(view) = entry.panes.get(&pane) {
                    let cwd = view
                        .cwd()
                        .map(|c| c.display().to_string())
                        .unwrap_or_else(|| "?".into());
                    pane_lines.push(format!("panel {pane}: {cwd}"));
                }
            }
            updates.push(Update {
                page: entry.page.clone(),
                tab_id: entry.id,
                title,
                cwd,
                exited,
                panes: pane_lines,
            });
        }

        for update in updates {
            {
                let mut model = self.model.borrow_mut();
                if let Some(tab) = model.get_mut(update.tab_id) {
                    tab.last_osc_title = update.title;
                    if let Some(cwd) = update.cwd {
                        tab.cwd = cwd;
                    }
                }
            }
            let spec = {
                let model = self.model.borrow();
                let Some(tab) = model.get(update.tab_id) else {
                    continue;
                };
                display_title(tab, None)
            };
            update.page.set_title(&spec.text);
            let mut tooltip = spec.tooltip;
            for line in update.panes {
                tooltip.push('\n');
                tooltip.push_str(&line);
            }
            update.page.set_tooltip(&tooltip);
            update.page.set_needs_attention(update.exited);
            if Some(update.page.as_ptr()) == selected {
                self.window.set_title(Some(&spec.text));
            }
        }
    }

    fn detach_page(&mut self, page: &TabPage) {
        let Some(index) = self.entries.iter().position(|e| e.page == *page) else {
            return;
        };
        let tab_id = self.entries[index].id;
        for (_, view) in self.entries[index].panes.drain() {
            view.kill();
        }
        self.entries.remove(index);
        let new_active = self.model.borrow_mut().close(tab_id);
        if self.entries.is_empty() {
            if let Some(id) = new_active {
                if self.attach_tab(id).is_ok() {
                    if let Some(entry) = self.entries.iter().find(|e| e.id == id) {
                        self.tab_view.set_selected_page(&entry.page);
                    }
                    self.focus_active();
                }
            }
        }
    }

    fn on_page_selected(&mut self, page: &TabPage) {
        if let Some(entry) = self.entries.iter().find(|e| e.page == *page) {
            let tab_id = entry.id;
            self.model.borrow_mut().activate(tab_id);
            self.focus_active();
        }
    }

    pub fn connect(manager: &Rc<RefCell<Self>>) {
        let tab_view = manager.borrow().tab_view.clone();

        {
            let manager = manager.clone();
            tab_view.connect_page_detached(move |_, page, _| {
                let manager = manager.clone();
                let page = page.clone();
                glib::idle_add_local_once(move || {
                    if let Ok(mut manager) = manager.try_borrow_mut() {
                        manager.detach_page(&page);
                    }
                });
            });
        }

        {
            let manager = manager.clone();
            tab_view.connect_selected_page_notify(move |view| {
                let Some(page) = view.selected_page() else {
                    return;
                };
                if let Ok(mut manager) = manager.try_borrow_mut() {
                    manager.on_page_selected(&page);
                }
            });
        }
    }

    pub fn rename_dialog(manager: &Rc<RefCell<Self>>) {
        let (window, tab_id, current) = {
            let manager = manager.borrow();
            let tab_id = manager.model.borrow().active_id();
            let current = manager
                .model
                .borrow()
                .get(tab_id)
                .and_then(|tab| tab.title_override.clone())
                .unwrap_or_default();
            (manager.window.clone(), tab_id, current)
        };
        let entry = gtk::Entry::new();
        entry.set_text(&current);
        entry.set_activates_default(true);
        let dialog = libadwaita::AlertDialog::builder()
            .title("Rename tab")
            .extra_child(&entry)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("rename", "Rename");
        dialog.set_response_appearance("rename", ResponseAppearance::Suggested);
        dialog.set_default_response(Some("rename"));
        {
            let entry = entry.clone();
            dialog.connect_map(move |_| {
                let entry = entry.clone();
                glib::timeout_add_local_once(Duration::from_millis(50), move || {
                    entry.grab_focus();
                    entry.select_region(0, -1);
                });
            });
        }
        let manager = manager.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "rename" {
                if let Ok(mut manager) = manager.try_borrow_mut() {
                    let name = entry.text().to_string();
                    manager.model.borrow_mut().rename(tab_id, Some(name));
                    manager.update_titles();
                }
            }
        });
        dialog.present(Some(&window));
    }
}

fn schedule_positions(positions: Vec<(gtk::Paned, f64)>) {
    if positions.is_empty() {
        return;
    }
    glib::idle_add_local_once(move || apply_positions(positions, 8));
}

fn apply_positions(positions: Vec<(gtk::Paned, f64)>, attempts: u8) {
    let mut pending = Vec::new();
    for (paned, ratio) in positions {
        let extent = match paned.orientation() {
            gtk::Orientation::Horizontal => paned.width(),
            _ => paned.height(),
        };
        if extent > 10 {
            paned.set_position((extent as f64 * ratio).round() as i32);
        } else if attempts > 0 {
            pending.push((paned, ratio));
        }
    }
    if !pending.is_empty() {
        glib::timeout_add_local_once(Duration::from_millis(30), move || {
            apply_positions(pending, attempts - 1)
        });
    }
}
