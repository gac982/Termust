use gtk::glib;
use gtk4 as gtk;
use libadwaita::prelude::*;
use libadwaita::{ApplicationWindow, ResponseAppearance, TabBar, TabPage, TabView};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use termust_agents::{AgentCatalog, AgentInput, AgentStatus};
use termust_core::pty::{PtySpawner, SpawnSpec};
use termust_core::tabs::{
    fits_min_width, Direction, PaneId, PaneTree, Preset, SplitError, SplitOrientation, TabId,
    TabModel,
};
use termust_core::terminal::Size;
use termust_core::title::display_title;
use termust_host_exec::{shell_command, ShellIntegration};
use termust_pty_unix::UnixPtySpawner;
use termust_render_cairo::CairoRenderer;
use termust_term_engine::AlacrittyEngine;
use termust_ui::TerminalView;

use crate::appearance::Appearance;
use crate::i18n;

struct Entry {
    id: TabId,
    page: TabPage,
    container: gtk::Box,
    panes: HashMap<PaneId, TerminalView>,
    pane_cwds: HashMap<PaneId, PathBuf>,
    attached: bool,
}

/// Vista de una pestaña para la barra vertical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabSummary {
    pub id: TabId,
    pub title: String,
    pub cwd: PathBuf,
    pub active: bool,
    pub attention: bool,
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
    appearance: Rc<Appearance>,
    agents: HashMap<(TabId, PaneId), AgentStatus>,
    suspend_ratios: Rc<Cell<bool>>,
    suspend_focus: Rc<Cell<bool>>,
    close_dialog_open: Rc<Cell<bool>>,
    restore_queue: Vec<TabId>,
}

impl TabManager {
    pub fn new(
        spawner: Rc<UnixPtySpawner>,
        shell: PathBuf,
        integration: ShellIntegration,
        home: PathBuf,
        window: ApplicationWindow,
        appearance: Rc<Appearance>,
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
            appearance,
            agents: HashMap::new(),
            suspend_ratios: Rc::new(Cell::new(false)),
            suspend_focus: Rc::new(Cell::new(false)),
            close_dialog_open: Rc::new(Cell::new(false)),
            restore_queue: Vec::new(),
        };
        let first = model.borrow().active_id();
        let first_cwds = {
            let model = model.borrow();
            let tab = model
                .get(first)
                .ok_or_else(|| i18n::tr("tab does not exist"))?;
            tab.layout
                .leaves()
                .into_iter()
                .map(|pane| (pane, tab.cwd.clone()))
                .collect()
        };
        manager.create_entry(first, first_cwds)?;
        manager.attach_tab(first)?;
        Ok(manager)
    }

    /// Construye el manager a partir de una sesión restaurada. Solo la pestaña
    /// activa se adjunta de inmediato; el resto va en `restore_queue`.
    pub fn restore(
        spawner: Rc<UnixPtySpawner>,
        shell: PathBuf,
        integration: ShellIntegration,
        window: ApplicationWindow,
        appearance: Rc<Appearance>,
        model: TabModel,
        pane_cwds: Vec<HashMap<PaneId, PathBuf>>,
    ) -> Result<Self, String> {
        let tab_view = TabView::new();
        let tab_bar = TabBar::new();
        tab_bar.set_view(Some(&tab_view));
        tab_bar.set_autohide(false);
        tab_bar.set_expand_tabs(false);
        tab_bar.set_hexpand(true);

        let model = Rc::new(RefCell::new(model));
        let active = model.borrow().active_id();
        let tab_ids: Vec<TabId> = model.borrow().tabs().iter().map(|tab| tab.id).collect();
        let mut manager = Self {
            tab_view,
            tab_bar,
            entries: Vec::new(),
            model: model.clone(),
            spawner,
            shell,
            integration,
            window,
            appearance,
            agents: HashMap::new(),
            suspend_ratios: Rc::new(Cell::new(false)),
            suspend_focus: Rc::new(Cell::new(false)),
            close_dialog_open: Rc::new(Cell::new(false)),
            restore_queue: Vec::new(),
        };
        for (index, tab_id) in tab_ids.iter().enumerate() {
            let cwds = pane_cwds.get(index).cloned().unwrap_or_default();
            manager.create_entry(*tab_id, cwds)?;
        }
        if let Some(entry) = manager.entries.iter().find(|entry| entry.id == active) {
            manager.tab_view.set_selected_page(&entry.page);
        }
        manager.attach_tab(active)?;
        manager.restore_queue = tab_ids
            .into_iter()
            .filter(|tab_id| *tab_id != active)
            .collect();
        manager.focus_active();
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
        let cwds = {
            let model = self.model.borrow();
            let tab = model
                .get(tab_id)
                .ok_or_else(|| i18n::tr("tab does not exist"))?;
            tab.layout
                .leaves()
                .into_iter()
                .map(|pane| (pane, tab.cwd.clone()))
                .collect()
        };
        self.create_entry(tab_id, cwds)?;
        self.attach_tab(tab_id)?;
        if let Some(entry) = self.entries.iter().find(|e| e.id == tab_id) {
            self.select_page(entry.page.clone());
        }
        self.focus_active();
        Ok(())
    }

    fn create_entry(
        &mut self,
        tab_id: TabId,
        pane_cwds: HashMap<PaneId, PathBuf>,
    ) -> Result<(), String> {
        let (cwd, title) = {
            let model = self.model.borrow();
            let tab = model
                .get(tab_id)
                .ok_or_else(|| i18n::tr("tab does not exist"))?;
            (tab.cwd.clone(), tab.title_override.clone())
        };
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        container.set_hexpand(true);
        container.set_vexpand(true);
        let placeholder = gtk::Label::new(Some(&i18n::tr("Restoring…")));
        placeholder.add_css_class("dim-label");
        placeholder.set_vexpand(true);
        container.append(&placeholder);
        let page = self.tab_view.append(&container);
        page.set_title(title.as_deref().unwrap_or("Terminal"));
        page.set_tooltip(&cwd.display().to_string());
        self.entries.push(Entry {
            id: tab_id,
            page,
            container,
            panes: HashMap::new(),
            pane_cwds,
            attached: false,
        });
        Ok(())
    }

    fn attach_tab(&mut self, tab_id: TabId) -> Result<(), String> {
        let Some(index) = self.entries.iter().position(|entry| entry.id == tab_id) else {
            return Ok(());
        };
        if self.entries[index].attached {
            return Ok(());
        }
        let (layout, tab_cwd, title_override) = {
            let model = self.model.borrow();
            let tab = model
                .get(tab_id)
                .ok_or_else(|| i18n::tr("tab does not exist"))?;
            (
                tab.layout.clone(),
                tab.cwd.clone(),
                tab.title_override.clone(),
            )
        };

        let resolved: Vec<(PaneId, PathBuf)> = layout
            .leaves()
            .into_iter()
            .map(|pane| {
                let cwd = self.entries[index]
                    .pane_cwds
                    .get(&pane)
                    .cloned()
                    .unwrap_or_else(|| tab_cwd.clone());
                (pane, cwd)
            })
            .collect();

        let mut panes = HashMap::new();
        for (pane, cwd) in &resolved {
            let view = self.spawn_view(tab_id, cwd)?;
            self.install_focus(&view, tab_id, *pane);
            panes.insert(*pane, view);
        }

        self.suspend_ratios.set(true);
        self.suspend_focus.set(true);
        let suspend_focus = self.suspend_focus.clone();
        glib::timeout_add_local_once(Duration::from_millis(80), move || suspend_focus.set(false));
        let mut positions = Vec::new();
        let mut path = Vec::new();
        let root = self.build_node(tab_id, &layout, &panes, &mut path, &mut positions);

        let entry = &mut self.entries[index];
        while let Some(child) = entry.container.first_child() {
            entry.container.remove(&child);
        }
        entry.container.append(&root);
        if layout.len() > 1 {
            entry.container.add_css_class("termust-multi-pane");
        } else {
            entry.container.remove_css_class("termust-multi-pane");
        }
        for (pane, cwd) in resolved {
            entry.pane_cwds.insert(pane, cwd);
        }
        entry.panes = panes;
        entry.attached = true;
        let page = entry.page.clone();
        page.set_title(title_override.as_deref().unwrap_or("Terminal"));
        page.set_tooltip(&tab_cwd.display().to_string());

        schedule_positions(positions);
        let suspend = self.suspend_ratios.clone();
        glib::timeout_add_local_once(Duration::from_millis(80), move || suspend.set(false));
        Ok(())
    }

    fn spawn_view(&self, tab_id: TabId, cwd: &Path) -> Result<TerminalView, String> {
        let host_pty = termust_host_exec::host_pty();
        let command = shell_command(
            &self.shell,
            &self.integration,
            termust_host_exec::is_flatpak(),
            Some(cwd),
            host_pty.as_ref(),
        );
        let size = Size::new(80, 24);
        let panes = self
            .entries
            .iter()
            .map(|entry| entry.panes.len())
            .sum::<usize>()
            + 1;
        let history = scrollback_lines(panes);
        let handle = self
            .spawner
            .spawn(SpawnSpec {
                argv: command.argv,
                cwd: Some(cwd.to_path_buf()),
                env: command.env,
                size,
            })
            .map_err(|e| e.to_string())?;
        let engine = AlacrittyEngine::new(size, history);
        let renderer = CairoRenderer::new();
        let view = TerminalView::new(
            Box::new(engine),
            handle.session,
            Box::new(renderer),
            handle.events,
        );
        let tab_theme = self
            .model
            .borrow()
            .get(tab_id)
            .and_then(|tab| tab.theme_id.clone());
        self.appearance.apply_to_view(&view, tab_theme.as_deref());
        Ok(view)
    }

    /// Reaplica tema, zoom y opacidad a todas las vistas.
    pub fn refresh_appearance(&self) {
        for entry in &self.entries {
            let tab_theme = self
                .model
                .borrow()
                .get(entry.id)
                .and_then(|tab| tab.theme_id.clone());
            for view in entry.panes.values() {
                self.appearance.apply_to_view(view, tab_theme.as_deref());
            }
        }
        self.update_transparency();
    }

    fn update_transparency(&self) {
        let transparent = self.appearance.bg_alpha() < 1.0;
        if transparent {
            self.window.add_css_class("termust-transparent");
        } else {
            self.window.remove_css_class("termust-transparent");
        }
    }

    pub fn active_tab_theme_id(&self) -> Option<String> {
        let tab_id = self.active_tab_id()?;
        self.model
            .borrow()
            .get(tab_id)
            .and_then(|tab| tab.theme_id.clone())
    }

    pub fn active_tab_id(&self) -> Option<TabId> {
        let index = self.active_entry_index()?;
        Some(self.entries[index].id)
    }

    /// Tema por pestaña; `None` vuelve al global.
    pub fn set_tab_theme(&mut self, tab_id: TabId, theme_id: Option<String>) -> bool {
        if !self
            .model
            .borrow_mut()
            .set_theme_id(tab_id, theme_id.clone())
        {
            return false;
        }
        if let Some(entry) = self.entries.iter().find(|entry| entry.id == tab_id) {
            for view in entry.panes.values() {
                self.appearance.apply_to_view(view, theme_id.as_deref());
            }
        }
        true
    }

    pub fn set_active_tab_theme(&mut self, theme_id: Option<String>) -> bool {
        match self.active_tab_id() {
            Some(tab_id) => self.set_tab_theme(tab_id, theme_id),
            None => false,
        }
    }

    fn install_focus(&self, view: &TerminalView, tab_id: TabId, pane: PaneId) {
        view.widget().add_css_class("termust-pane");
        let model = self.model.clone();
        let suspend = self.suspend_focus.clone();
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(move |_| {
            if suspend.get() {
                return;
            }
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
                .unwrap_or_else(|| gtk::Label::new(Some(&i18n::tr("pane not available"))).upcast()),
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
                        // Paneles viejos (aún referenciados por posiciones
                        // pendientes) no deben tocar el modelo.
                        if suspend.get() || paned.parent().is_none() {
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
        self.suspend_focus.set(true);
        let suspend_focus = self.suspend_focus.clone();
        glib::timeout_add_local_once(Duration::from_millis(80), move || suspend_focus.set(false));
        let entry = &self.entries[index];
        if layout.len() > 1 {
            entry.container.add_css_class("termust-multi-pane");
        } else {
            entry.container.remove_css_class("termust-multi-pane");
        }
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
            return Err(i18n::tr("no active tab"));
        };
        let tab_id = self.entries[index].id;
        let (active, len, tab_cwd) = {
            let model = self.model.borrow();
            let tab = model
                .get(tab_id)
                .ok_or_else(|| i18n::tr("tab does not exist"))?;
            (tab.active_pane, tab.layout.len(), tab.cwd.clone())
        };
        if len >= termust_core::tabs::MAX_PANES {
            return Err(i18n::tr("Maximum 4 panes per tab"));
        }
        let cwd = self.entries[index]
            .panes
            .get(&active)
            .and_then(|view| view.cwd())
            .unwrap_or(tab_cwd);
        let width = self.entries[index].container.width() as f64;
        if orientation == SplitOrientation::Horizontal {
            let half = {
                let model = self.model.borrow();
                model
                    .get(tab_id)
                    .and_then(|tab| tab.layout.rect_of(active))
                    .map(|(_, _, w, _)| w * width / 2.0)
            };
            if let Some(half) = half {
                if !fits_min_width(half, 1) {
                    return Err(i18n::tr("Not enough width (minimum 320 px per pane)"));
                }
            }
        }

        let new_pane = self
            .model
            .borrow_mut()
            .split_pane(tab_id, active, orientation)
            .map_err(|e| match e {
                SplitError::MaxPanes => i18n::tr("Maximum 4 panes per tab"),
                _ => i18n::tr("Could not split the pane"),
            })?;

        let view = match self.spawn_view(tab_id, &cwd) {
            Ok(view) => view,
            Err(e) => {
                let _ = self.model.borrow_mut().close_pane(tab_id, new_pane);
                return Err(e);
            }
        };
        self.install_focus(&view, tab_id, new_pane);
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == tab_id) {
            entry.pane_cwds.insert(new_pane, cwd.clone());
            entry.panes.insert(new_pane, view);
        }
        self.rebuild(tab_id);
        self.focus_active();
        Ok(new_pane)
    }

    pub fn apply_preset(&mut self, preset: Preset) -> Result<(), String> {
        let Some(index) = self.active_entry_index() else {
            return Err(i18n::tr("no active tab"));
        };
        let tab_id = self.entries[index].id;
        let width = self.entries[index].container.width() as f64;
        if !fits_min_width(width, preset.columns()) {
            return Err(i18n::tr("Not enough width (minimum 320 px per pane)"));
        }
        let tab_cwd = {
            let model = self.model.borrow();
            model.get(tab_id).map(|tab| tab.cwd.clone())
        };
        let cwd = self
            .active_cwd()
            .or(tab_cwd)
            .unwrap_or_else(|| PathBuf::from("/"));
        let created = self
            .model
            .borrow_mut()
            .apply_preset(tab_id, preset)
            .map_err(|e| match e {
                SplitError::TooManyPanes => {
                    i18n::tr("That layout has fewer panes; close the others first")
                }
                _ => i18n::tr("Could not apply the layout"),
            })?;
        for pane in created {
            let view = self.spawn_view(tab_id, &cwd)?;
            self.install_focus(&view, tab_id, pane);
            if let Some(entry) = self.entries.iter_mut().find(|e| e.id == tab_id) {
                entry.pane_cwds.insert(pane, cwd.clone());
                entry.panes.insert(pane, view);
            }
        }
        self.rebuild(tab_id);
        self.focus_active();
        Ok(())
    }

    pub fn resize_direction(&mut self, direction: Direction) -> bool {
        let Some(index) = self.active_entry_index() else {
            return false;
        };
        let tab_id = self.entries[index].id;
        let changed = self.model.borrow_mut().nudge_split(tab_id, direction, 0.05);
        if changed {
            self.rebuild(tab_id);
            self.focus_active();
        }
        changed
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
            entry.pane_cwds.remove(&pane);
            if let Some(view) = entry.panes.remove(&pane) {
                view.kill();
            }
        }
        self.rebuild(tab_id);
        self.focus_active();
        true
    }

    /// Cierra los paneles cuyo shell terminó. Devuelve `true` si hay que
    /// cerrar la ventana (era el último panel de la última pestaña).
    pub fn close_exited(&mut self) -> bool {
        let mut exited: Vec<(TabId, PaneId)> = Vec::new();
        for entry in &self.entries {
            for (pane, view) in &entry.panes {
                if view.exited() {
                    exited.push((entry.id, *pane));
                }
            }
        }
        if exited.is_empty() {
            return false;
        }
        if self.entries.len() == 1
            && self.entries[0].panes.len() == 1
            && exited.iter().any(|(tab, _)| *tab == self.entries[0].id)
        {
            return true;
        }
        for (tab_id, pane) in exited {
            let panes = {
                let model = self.model.borrow();
                model.get(tab_id).map(|tab| tab.layout.len()).unwrap_or(0)
            };
            if panes > 1 {
                self.close_pane(tab_id, pane);
            } else if let Some(page) = self
                .entries
                .iter()
                .find(|entry| entry.id == tab_id)
                .map(|entry| entry.page.clone())
            {
                self.tab_view.close_page(&page);
            }
        }
        false
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

    /// Selecciona una página y sincroniza el modelo en el acto. El notify de
    /// `AdwTabView` corre con el manager prestado y no puede activar el modelo,
    /// así que se hace explícitamente después de mover la selección.
    fn select_page(&mut self, page: TabPage) {
        self.tab_view.set_selected_page(&page);
        self.on_page_selected(&page);
    }

    pub fn select_index(&mut self, index: usize) {
        if let Some(entry) = self.entries.get(index) {
            self.select_page(entry.page.clone());
        }
    }

    pub fn select_tab(&mut self, id: TabId) -> bool {
        match self.entries.iter().find(|entry| entry.id == id) {
            Some(entry) => {
                self.select_page(entry.page.clone());
                true
            }
            None => false,
        }
    }

    pub fn select_last(&mut self) {
        if let Some(entry) = self.entries.last() {
            self.select_page(entry.page.clone());
        }
    }

    /// Cierra una pestaña completa por id (no un panel). La confirmación por
    /// proceso en foreground la resuelve `connect_close_page`.
    pub fn close_tab(manager: &Rc<RefCell<Self>>, id: TabId) {
        let (view, page) = {
            let Ok(manager) = manager.try_borrow() else {
                return;
            };
            let Some(entry) = manager.entries.iter().find(|entry| entry.id == id) else {
                return;
            };
            (manager.tab_view.clone(), entry.page.clone())
        };
        view.close_page(&page);
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

    pub fn tab_count(&self) -> usize {
        self.entries.len()
    }

    /// Pestañas en orden, con título visible, cwd y cuál está activa.
    /// Detecta agentes en todos los paneles y actualiza su estado.
    pub fn update_agents(&mut self, catalog: &AgentCatalog) {
        let mut detected: HashMap<(TabId, PaneId), AgentStatus> = HashMap::new();
        for entry in &self.entries {
            for (pane, view) in &entry.panes {
                if view.exited() {
                    // Se conserva la detección para mostrar Exited.
                }
                let argv = view.foreground_command();
                let title = view.title();
                let Some(rule) = catalog.detect(argv.as_deref(), title.as_deref()) else {
                    continue;
                };
                let lines = view.recent_lines(50);
                let cwd = view
                    .cwd()
                    .or_else(|| entry.pane_cwds.get(pane).cloned())
                    .unwrap_or_default();
                let alive = !view.exited() && view.has_foreground_job();
                let status = catalog.evaluate(
                    rule,
                    &AgentInput {
                        title: title.as_deref(),
                        recent_lines: &lines,
                        alive,
                        exit_code: view.exit_code(),
                        activity_age: view.output_age(),
                        cwd: &cwd,
                    },
                );
                detected.insert((entry.id, *pane), status);
            }
        }
        self.agents = detected;
    }

    pub fn active_agent(&self) -> Option<AgentStatus> {
        let index = self.active_entry_index()?;
        let entry = &self.entries[index];
        let pane = self
            .model
            .borrow()
            .get(entry.id)
            .map(|tab| tab.active_pane)?;
        self.agents.get(&(entry.id, pane)).cloned()
    }

    pub fn tab_summaries(&self) -> Vec<TabSummary> {
        let active = self.active_entry_index();
        let model = self.model.borrow();
        self.entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let cwd = model
                    .get(entry.id)
                    .map(|tab| tab.cwd.clone())
                    .unwrap_or_default();
                TabSummary {
                    id: entry.id,
                    title: entry.page.title().to_string(),
                    cwd,
                    active: Some(index) == active,
                    attention: entry.page.needs_attention(),
                }
            })
            .collect()
    }

    pub fn active_pane_count(&self) -> usize {
        self.model.borrow().active().layout.len()
    }

    pub fn active_title_override(&self) -> Option<String> {
        let model = self.model.borrow();
        model
            .get(model.active_id())
            .and_then(|tab| tab.title_override.clone())
    }

    pub fn rename_tab(&mut self, name: &str) {
        let tab_id = self.model.borrow().active_id();
        self.model
            .borrow_mut()
            .rename(tab_id, Some(name.to_string()));
        self.update_titles();
    }

    pub fn snapshot(&self) -> termust_store::SessionSnapshot {
        let mut tabs = Vec::new();
        for entry in &self.entries {
            let Some(tab) = self.model.borrow().get(entry.id).cloned() else {
                continue;
            };
            let mut panes = Vec::new();
            for pane in tab.layout.leaves() {
                let view = entry.panes.get(&pane);
                let cwd = view
                    .and_then(|view| view.cwd())
                    .or_else(|| entry.pane_cwds.get(&pane).cloned())
                    .unwrap_or_else(|| tab.cwd.clone());
                let title = view.and_then(|view| view.title());
                panes.push(termust_store::PaneSnapshot {
                    id: pane,
                    cwd,
                    last_osc_title: title,
                    agent_hint: None,
                });
            }
            let active_pane = panes
                .iter()
                .find(|pane| pane.id == tab.active_pane)
                .cloned();
            let cwd = active_pane
                .as_ref()
                .map(|pane| pane.cwd.clone())
                .unwrap_or_else(|| tab.cwd.clone());
            let last_osc_title = active_pane
                .as_ref()
                .and_then(|pane| pane.last_osc_title.clone())
                .or_else(|| tab.last_osc_title.clone());
            tabs.push(termust_store::TabSnapshot {
                layout: tab.layout,
                active_pane: tab.active_pane,
                cwd,
                title_override: tab.title_override,
                last_osc_title,
                theme_id: tab.theme_id,
                panes,
            });
        }
        let active_tab = self.active_entry_index().unwrap_or(0);
        let width = self.window.width().max(320);
        let height = self.window.height().max(240);
        termust_store::SessionSnapshot {
            window: termust_store::WindowState {
                width,
                height,
                maximized: self.window.is_maximized(),
            },
            active_tab,
            tabs,
        }
    }

    pub fn active_pane(&self) -> Option<PaneId> {
        self.model
            .borrow()
            .get(self.model.borrow().active_id())
            .map(|tab| tab.active_pane)
    }

    pub fn active_view(&self) -> Option<&TerminalView> {
        let index = self.active_entry_index()?;
        let entry = &self.entries[index];
        let active = self
            .model
            .borrow()
            .get(entry.id)
            .map(|tab| tab.active_pane)?;
        entry.panes.get(&active)
    }

    pub fn take_history_records(&self) -> Vec<termust_core::terminal::CommandRecord> {
        let mut out = Vec::new();
        for entry in &self.entries {
            for view in entry.panes.values() {
                out.extend(view.take_history());
            }
        }
        out
    }

    pub fn active_cwd(&self) -> Option<PathBuf> {
        let index = self.active_entry_index()?;
        let entry = &self.entries[index];
        let active = self
            .model
            .borrow()
            .get(entry.id)
            .map(|tab| tab.active_pane)?;
        entry.panes.get(&active).and_then(|view| view.cwd())
    }

    pub fn send_active(&self, bytes: &[u8]) {
        let Some(index) = self.active_entry_index() else {
            return;
        };
        let entry = &self.entries[index];
        let Some(active) = self.model.borrow().get(entry.id).map(|tab| tab.active_pane) else {
            return;
        };
        if let Some(view) = entry.panes.get(&active) {
            view.write_input(bytes);
        }
    }

    pub fn ratio(&self, path: &[bool]) -> Option<f64> {
        let model = self.model.borrow();
        model.ratio(model.active_id(), path)
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

    /// Cap global de scrollback: 64 MB, repartidos entre todos los paneles.
    pub fn ready_views(&self) -> usize {
        self.entries
            .iter()
            .flat_map(|entry| entry.panes.values())
            .filter(|view| view.history_ready())
            .count()
    }

    pub fn history_total(&self) -> usize {
        self.entries
            .iter()
            .flat_map(|entry| entry.panes.values())
            .map(|view| view.history_size())
            .sum()
    }

    pub fn apply_scrollback_budget(&self) {
        let panes: usize = self.entries.iter().map(|entry| entry.panes.len()).sum();
        let lines = scrollback_lines(panes.max(1));
        for entry in &self.entries {
            for view in entry.panes.values() {
                view.set_history_limit(lines);
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
            if let Some(view) = view {
                let last = view.recent_lines(1).join(" ");
                if !last.is_empty() {
                    view.set_accessible_description(&last);
                }
            }
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

    fn page_has_foreground(&self, page: &TabPage) -> bool {
        self.entries
            .iter()
            .find(|entry| entry.page == *page)
            .map(|entry| entry.panes.values().any(|view| view.has_foreground_job()))
            .unwrap_or(false)
    }

    /// Punto de entrada para cerrar panel/pestaña con confirmación.
    pub fn close_request(manager: &Rc<RefCell<Self>>) {
        let (tab_id, active, multi, needs, page) = {
            let Ok(manager) = manager.try_borrow() else {
                return;
            };
            let Some(index) = manager.active_entry_index() else {
                return;
            };
            let entry = &manager.entries[index];
            let (multi, active) = {
                let model = manager.model.borrow();
                match model.get(entry.id) {
                    Some(tab) => (tab.layout.len() > 1, tab.active_pane),
                    None => return,
                }
            };
            let needs = entry
                .panes
                .get(&active)
                .map(|view| view.has_foreground_job())
                .unwrap_or(false);
            (entry.id, active, multi, needs, entry.page.clone())
        };

        if !multi {
            // La confirmación de la pestaña la resuelve connect_close_page.
            manager.borrow().tab_view.close_page(&page);
            return;
        }

        if !needs {
            if let Ok(mut manager) = manager.try_borrow_mut() {
                manager.close_pane(tab_id, active);
            }
            return;
        }

        let window = manager.borrow().window.clone();
        let manager = manager.clone();
        let dialog = libadwaita::AlertDialog::builder()
            .title(i18n::tr("Close pane with a running process"))
            .body(i18n::tr(
                "A process is running in this pane. Closing it will terminate it.",
            ))
            .build();
        dialog.add_response("cancel", &i18n::tr("Cancel"));
        dialog.add_response("close", &i18n::tr("Close"));
        dialog.set_response_appearance("close", ResponseAppearance::Destructive);
        dialog.connect_response(None, move |_, response| {
            if response == "close" {
                if let Ok(mut manager) = manager.try_borrow_mut() {
                    manager.close_pane(tab_id, active);
                }
            }
        });
        dialog.present(Some(&window));
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
                    if let Some(page) = self
                        .entries
                        .iter()
                        .find(|entry| entry.id == id)
                        .map(|entry| entry.page.clone())
                    {
                        self.select_page(page);
                    }
                    self.focus_active();
                }
            }
        }
    }

    fn on_page_selected(&mut self, page: &TabPage) {
        let Some((tab_id, attached)) = self
            .entries
            .iter()
            .find(|entry| entry.page == *page)
            .map(|entry| (entry.id, entry.attached))
        else {
            return;
        };
        if !attached {
            let _ = self.attach_tab(tab_id);
        }
        self.model.borrow_mut().activate(tab_id);
        self.focus_active();
    }

    pub fn connect(manager: &Rc<RefCell<Self>>) {
        let tab_view = manager.borrow().tab_view.clone();

        {
            let manager = manager.clone();
            tab_view.connect_close_page(move |view, page| {
                let (needs, window, already) = {
                    let Ok(manager) = manager.try_borrow() else {
                        return glib::Propagation::Proceed;
                    };
                    let needs = manager.page_has_foreground(page);
                    (
                        needs,
                        manager.window.clone(),
                        manager.close_dialog_open.get(),
                    )
                };
                if !needs {
                    return glib::Propagation::Proceed;
                }
                if already {
                    return glib::Propagation::Stop;
                }
                {
                    let Ok(manager) = manager.try_borrow() else {
                        return glib::Propagation::Proceed;
                    };
                    manager.close_dialog_open.set(true);
                }
                let view = view.clone();
                let page = page.clone();
                let manager = manager.clone();
                let dialog = libadwaita::AlertDialog::builder()
                    .title(i18n::tr("Close with a running process"))
                    .body(i18n::tr(
                        "A process is running. Closing it will terminate it.",
                    ))
                    .build();
                dialog.add_response("cancel", &i18n::tr("Cancel"));
                dialog.add_response("close", &i18n::tr("Close"));
                dialog.set_response_appearance("close", ResponseAppearance::Destructive);
                dialog.connect_response(None, move |_, response| {
                    if let Ok(manager) = manager.try_borrow() {
                        manager.close_dialog_open.set(false);
                    }
                    view.close_page_finish(&page, response == "close");
                });
                dialog.present(Some(&window));
                glib::Propagation::Stop
            });
        }

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

        if !manager.borrow().restore_queue.is_empty() {
            let manager = manager.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || {
                let Ok(mut manager) = manager.try_borrow_mut() else {
                    return glib::ControlFlow::Continue;
                };
                for _ in 0..2 {
                    let Some(tab_id) = manager.restore_queue.first().copied() else {
                        break;
                    };
                    manager.restore_queue.remove(0);
                    let _ = manager.attach_tab(tab_id);
                }
                if manager.restore_queue.is_empty() {
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
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
            .title(i18n::tr("Rename tab"))
            .extra_child(&entry)
            .build();
        dialog.add_response("cancel", &i18n::tr("Cancel"));
        dialog.add_response("rename", &i18n::tr("Rename"));
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

pub struct RestoredSession {
    pub model: TabModel,
    pub pane_cwds: Vec<HashMap<PaneId, PathBuf>>,
}

/// Construye el modelo y los cwd por panel desde una sesión guardada.
pub fn restored_session(
    snapshot: &termust_store::SessionSnapshot,
    home: &Path,
) -> Result<RestoredSession, String> {
    let mut tabs = Vec::with_capacity(snapshot.tabs.len());
    let mut pane_cwds = Vec::with_capacity(snapshot.tabs.len());
    for tab in &snapshot.tabs {
        let mut cwds = HashMap::new();
        for pane in &tab.panes {
            let cwd = if pane.cwd.is_dir() {
                pane.cwd.clone()
            } else {
                home.to_path_buf()
            };
            cwds.insert(pane.id, cwd);
        }
        let active_cwd = cwds
            .get(&tab.active_pane)
            .cloned()
            .unwrap_or_else(|| home.to_path_buf());
        tabs.push(termust_core::tabs::RestoreTab {
            layout: tab.layout.clone(),
            active_pane: tab.active_pane,
            cwd: active_cwd,
            title_override: tab.title_override.clone(),
            last_osc_title: tab.last_osc_title.clone(),
            theme_id: tab.theme_id.clone(),
        });
        pane_cwds.push(cwds);
    }
    let model = TabModel::restore(tabs, snapshot.active_tab).map_err(|error| {
        i18n::tr_args(
            "invalid session: {error:?}",
            &[("error", &format!("{error:?}"))],
        )
    })?;
    Ok(RestoredSession { model, pane_cwds })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use termust_core::tabs::SplitOrientation;
    use termust_store::{PaneSnapshot, SessionSnapshot, TabSnapshot, WindowState};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "termust-app-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn restored_session_falls_back_and_preserves_layout() {
        let dir = temp_dir("restore");
        let home = dir.join("home");
        let repo = dir.join("repo");
        let gone = dir.join("gone");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let layout = PaneTree::Split {
            orientation: SplitOrientation::Horizontal,
            ratio: 0.3,
            first: Box::new(PaneTree::Leaf(1)),
            second: Box::new(PaneTree::Leaf(2)),
        };
        let snapshot = SessionSnapshot {
            window: WindowState::default(),
            active_tab: 0,
            tabs: vec![TabSnapshot {
                layout,
                active_pane: 2,
                cwd: gone.clone(),
                title_override: Some("build".into()),
                last_osc_title: None,
                theme_id: None,
                panes: vec![
                    PaneSnapshot {
                        id: 1,
                        cwd: repo.clone(),
                        last_osc_title: None,
                        agent_hint: None,
                    },
                    PaneSnapshot {
                        id: 2,
                        cwd: gone.clone(),
                        last_osc_title: None,
                        agent_hint: None,
                    },
                ],
            }],
        };
        let restored = restored_session(&snapshot, &home).unwrap();
        assert_eq!(restored.pane_cwds[0][&1], repo);
        assert_eq!(restored.pane_cwds[0][&2], home);
        let model = restored.model;
        assert_eq!(model.active_id(), 1);
        assert_eq!(model.active().active_pane, 2);
        assert_eq!(model.active().cwd, home);
        assert_eq!(model.active().layout.ratio_at_path(&[]), Some(0.3));
        assert_eq!(model.active().title_override.as_deref(), Some("build"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restored_session_rejects_invalid_duplicate_panes() {
        let dir = temp_dir("invalid");
        let layout = PaneTree::Split {
            orientation: SplitOrientation::Horizontal,
            ratio: 0.5,
            first: Box::new(PaneTree::Leaf(1)),
            second: Box::new(PaneTree::Leaf(1)),
        };
        let snapshot = SessionSnapshot {
            window: WindowState::default(),
            active_tab: 0,
            tabs: vec![TabSnapshot {
                layout,
                active_pane: 1,
                cwd: dir.clone(),
                title_override: None,
                last_osc_title: None,
                theme_id: None,
                panes: vec![PaneSnapshot {
                    id: 1,
                    cwd: dir.clone(),
                    last_osc_title: None,
                    agent_hint: None,
                }],
            }],
        };
        assert!(restored_session(&snapshot, &dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}

/// Líneas de scrollback por panel para respetar 64 MB globales. La medición
/// real con la grilla de alacritty da ≈4 KB/línea (capacidad redondeada a
/// potencias de dos), así que el presupuesto usa 4 KB.
pub fn scrollback_lines(panes: usize) -> usize {
    const BUDGET: usize = 64 * 1024 * 1024;
    const BYTES_PER_LINE: usize = 4 * 1024;
    (BUDGET / (BYTES_PER_LINE * panes.max(1))).clamp(500, 2_000)
}

#[cfg(test)]
mod budget_tests {
    use super::scrollback_lines;

    #[test]
    fn scrollback_budget_caps_global_history() {
        for panes in [1, 4, 9, 16, 30] {
            let lines = scrollback_lines(panes);
            assert!((500..=2_000).contains(&lines));
            assert!(
                lines * 4 * 1024 * panes <= 64 * 1024 * 1024,
                "{panes} paneles: {lines} líneas"
            );
        }
        assert_eq!(scrollback_lines(9), 1_820);
        assert!(scrollback_lines(30) < 2_000);
    }
}
