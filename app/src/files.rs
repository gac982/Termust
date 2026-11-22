//! Panel de archivos: modelo GIO, árbol lazy, filtro, watch y acciones.

use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};
use termust_fs_local::{self as fs, EntryKind, FileEntry};

/// Acciones expuestas por fila. No hay ninguna destructiva (FR-4.6).
#[cfg_attr(not(test), allow(dead_code))]
pub const FILE_ACTIONS: &[&str] = &["navigate", "open-external", "copy-path"];

const ATTR_PATH: &str = "termust::path";
const ATTR_ICON: &str = "termust::icon";
const ATTR_COLOR: &str = "termust::color";
const ATTR_BROKEN: &str = "termust::broken";

/// Convierte una entrada del filesystem en un `GFileInfo` reutilizable por
/// `GtkTreeListModel`. No toca GTK ni requiere display.
pub fn file_info(entry: &FileEntry) -> gio::FileInfo {
    let info = gio::FileInfo::new();
    info.set_name(&entry.name);
    info.set_file_type(match entry.kind {
        EntryKind::Dir | EntryKind::SymlinkToDir => gio::FileType::Directory,
        EntryKind::File | EntryKind::SymlinkToFile => gio::FileType::Regular,
        EntryKind::BrokenSymlink => gio::FileType::SymbolicLink,
        EntryKind::Other => gio::FileType::Special,
    });
    info.set_is_hidden(entry.hidden);
    info.set_attribute_string(ATTR_PATH, &entry.path.to_string_lossy());
    info.set_attribute_string(ATTR_ICON, fs::icon_name(entry));
    info.set_attribute_string(ATTR_COLOR, fs::color_class(entry));
    info.set_attribute_string(
        ATTR_BROKEN,
        if entry.kind == EntryKind::BrokenSymlink {
            "1"
        } else {
            "0"
        },
    );
    info
}

pub fn file_infos(entries: &[FileEntry]) -> Vec<gio::FileInfo> {
    entries.iter().map(file_info).collect()
}

pub fn path_from_info(info: &gio::FileInfo) -> Option<PathBuf> {
    info.attribute_string(ATTR_PATH).map(PathBuf::from)
}

pub fn icon_from_info(info: &gio::FileInfo) -> String {
    info.attribute_string(ATTR_ICON)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "text-x-generic-symbolic".to_string())
}

pub fn color_from_info(info: &gio::FileInfo) -> String {
    info.attribute_string(ATTR_COLOR)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "termust-file".to_string())
}

pub fn is_broken(info: &gio::FileInfo) -> bool {
    info.attribute_string(ATTR_BROKEN).as_deref() == Some("1")
}

pub fn is_navigable(info: &gio::FileInfo) -> bool {
    info.file_type() == gio::FileType::Directory && !is_broken(info)
}

fn store_for(entries: &[FileEntry]) -> gio::ListStore {
    let infos = file_infos(entries);
    gio::ListStore::from_iter(infos)
}

/// Panel lateral con árbol lazy y virtualizado.
pub struct FilePanel {
    root: gtk::Box,
    list: gtk::ListView,
    root_store: gio::ListStore,
    tree: gtk::TreeListModel,
    search: gtk::SearchEntry,
    path_label: gtk::Label,
    up: gtk::Button,
    stack: gtk::Stack,
    status: libadwaita::StatusPage,
    follow: Rc<Cell<bool>>,
    show_hidden: Rc<Cell<bool>>,
    settings: Option<gio::Settings>,
    toast: libadwaita::ToastOverlay,
    current: RefCell<PathBuf>,
    cache: RefCell<Vec<FileEntry>>,
    rows: Cell<usize>,
    load_ms: Cell<f64>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    refresh_source: Cell<Option<glib::SourceId>>,
    search_source: Cell<Option<glib::SourceId>>,
    follow_action: RefCell<Option<gio::SimpleAction>>,
}

impl FilePanel {
    pub fn new(home: &Path, toast: libadwaita::ToastOverlay) -> Rc<Self> {
        let panel = Rc::new_cyclic(|weak: &Weak<Self>| {
            let settings = gio::SettingsSchemaSource::default()
                .filter(|source| source.lookup(termust_core::APP_ID, true).is_some())
                .map(|_| gio::Settings::new(termust_core::APP_ID))
                .filter(|settings| {
                    settings
                        .settings_schema()
                        .map(|schema| schema.has_key("files-follow-cwd"))
                        .unwrap_or(false)
                });
            let follow = settings
                .as_ref()
                .map(|settings| settings.boolean("files-follow-cwd"))
                .unwrap_or(true);
            let show_hidden = settings
                .as_ref()
                .map(|settings| settings.boolean("files-show-hidden"))
                .unwrap_or(false);
            let follow = Rc::new(Cell::new(follow));
            let show_hidden_rc = Rc::new(Cell::new(show_hidden));

            let up = gtk::Button::builder()
                .icon_name("go-up-symbolic")
                .tooltip_text("Directorio superior")
                .build();
            up.add_css_class("flat");
            up.set_visible(false);

            let path_label = gtk::Label::new(None);
            path_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            path_label.set_xalign(0.0);
            path_label.set_hexpand(true);
            path_label.add_css_class("dim-label");
            path_label.add_css_class("caption");

            let menu_button = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .tooltip_text("Opciones del panel")
                .build();
            menu_button.add_css_class("flat");

            let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            header.append(&up);
            header.append(&path_label);
            header.append(&menu_button);

            let search = gtk::SearchEntry::new();
            search.set_placeholder_text(Some("Filtrar archivos…"));
            search.set_size_request(200, -1);

            let root_store = gio::ListStore::new::<gio::FileInfo>();
            let hidden_for_tree = show_hidden_rc.clone();
            let tree = gtk::TreeListModel::new(root_store.clone(), false, false, move |item| {
                let info = item.downcast_ref::<gio::FileInfo>()?;
                if info.file_type() != gio::FileType::Directory || is_broken(info) {
                    return None;
                }
                let path = path_from_info(info)?;
                // Límite defensivo contra cadenas de symlinks.
                if path.components().count() > 64 {
                    return None;
                }
                let entries = fs::list_dir(&path, hidden_for_tree.get()).ok()?;
                Some(store_for(&entries).upcast())
            });
            let selection = gtk::SingleSelection::new(Some(tree.clone()));
            selection.set_autoselect(false);
            selection.set_can_unselect(true);

            let factory = gtk::SignalListItemFactory::new();
            {
                let weak = weak.clone();
                factory.connect_setup(move |_, object| {
                    let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
                    let expander = gtk::TreeExpander::new();
                    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    row.set_margin_top(4);
                    row.set_margin_bottom(4);
                    row.set_margin_start(6);
                    row.set_margin_end(6);
                    let icon = gtk::Image::new();
                    icon.set_icon_size(gtk::IconSize::Normal);
                    let label = gtk::Label::new(None);
                    label.set_xalign(0.0);
                    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    label.set_hexpand(true);
                    row.append(&icon);
                    row.append(&label);
                    expander.set_child(Some(&row));
                    item.set_child(Some(&expander));

                    let weak = weak.clone();
                    let gesture = gtk::GestureClick::new();
                    gesture.set_button(3);
                    let item = item.clone();
                    gesture.connect_pressed(move |_, _, x, y| {
                        if let Some(panel) = weak.upgrade() {
                            panel.show_menu(&item, x, y);
                        }
                    });
                    expander.add_controller(gesture);
                });
            }
            factory.connect_bind(|_, object| {
                let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
                let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
                    return;
                };
                let Some(tree_row) = item.item().and_downcast::<gtk::TreeListRow>() else {
                    return;
                };
                expander.set_list_row(Some(&tree_row));
                let Some(info) = tree_row.item().and_downcast::<gio::FileInfo>() else {
                    return;
                };
                let broken = is_broken(&info);
                let navigable = is_navigable(&info);
                item.set_activatable(!broken);
                if let Some(row) = expander.child().and_downcast::<gtk::Box>() {
                    if let Some(icon) = row.first_child().and_downcast::<gtk::Image>() {
                        icon.set_icon_name(Some(&icon_from_info(&info)));
                        for class in [
                            "termust-folder",
                            "termust-code",
                            "termust-image",
                            "termust-archive",
                            "termust-binary",
                            "termust-file",
                            "termust-broken",
                        ] {
                            icon.remove_css_class(class);
                        }
                        icon.add_css_class(&color_from_info(&info));
                    }
                    if let Some(label) = row.last_child().and_downcast::<gtk::Label>() {
                        label.set_text(&info.name().to_string_lossy());
                        label.remove_css_class("dim-label");
                        label.remove_css_class("termust-dir");
                        if broken {
                            label.add_css_class("dim-label");
                        } else if navigable {
                            label.add_css_class("termust-dir");
                        }
                    }
                }
                expander.set_tooltip_text(Some(
                    &path_from_info(&info)
                        .map(|path| path.to_string_lossy().to_string())
                        .unwrap_or_default(),
                ));
            });

            let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
            list.add_css_class("navigation-sidebar");
            list.set_show_separators(false);
            list.set_single_click_activate(false);

            let status = libadwaita::StatusPage::builder()
                .icon_name("folder-open-symbolic")
                .title("Carpeta vacía")
                .build();
            status.set_vexpand(true);

            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&list)
                .build();

            let stack = gtk::Stack::new();
            stack.set_vexpand(true);
            stack.add_named(&scroller, Some("list"));
            stack.add_named(&status, Some("status"));

            let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
            root.add_css_class("termust-files");
            root.set_margin_top(6);
            root.set_margin_bottom(6);
            root.set_margin_start(6);
            root.set_margin_end(6);
            root.set_size_request(240, -1);
            root.append(&header);
            root.append(&search);
            root.append(&stack);
            stack.set_visible_child_name("status");

            let panel = Self {
                root,
                list: list.clone(),
                root_store: root_store.clone(),
                tree: tree.clone(),
                search: search.clone(),
                path_label,
                up: up.clone(),
                stack,
                status,
                follow,
                show_hidden: show_hidden_rc,
                settings,
                toast: toast.clone(),
                current: RefCell::new(home.to_path_buf()),
                cache: RefCell::new(Vec::new()),
                rows: Cell::new(0),
                load_ms: Cell::new(0.0),
                monitor: RefCell::new(None),
                refresh_source: Cell::new(None),
                search_source: Cell::new(None),
                follow_action: RefCell::new(None),
            };
            panel.update_path_label(home);
            panel.install_actions(weak.clone(), &menu_button);
            panel
        });
        panel.connect_navigation();
        panel
    }

    /// Carga la raíz actual y empieza a observarla. Se llama al mostrar el panel.
    pub fn activate(self: &Rc<Self>) {
        self.reload();
        self.watch_root();
    }

    /// Cancela watch y debounce al ocultar el panel (no consume si no se ve).
    pub fn deactivate(&self) {
        if let Some(monitor) = self.monitor.borrow_mut().take() {
            monitor.cancel();
        }
        if let Some(source) = self.refresh_source.take() {
            source.remove();
        }
        if let Some(source) = self.search_source.take() {
            source.remove();
        }
    }

    pub fn is_showing_status(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("status")
    }

    pub fn status_title(&self) -> String {
        self.status.title().to_string()
    }

    fn install_actions(&self, weak: Weak<Self>, menu_button: &gtk::MenuButton) {
        let group = gio::SimpleActionGroup::new();

        let navigate = gio::SimpleAction::new("navigate", Some(glib::VariantTy::STRING));
        {
            let weak = weak.clone();
            navigate.connect_activate(move |_, param| {
                let Some(path) = param.and_then(|value| value.str()).map(PathBuf::from) else {
                    return;
                };
                if let Some(panel) = weak.upgrade() {
                    panel.note_manual_navigation();
                    panel.set_root(&path);
                }
            });
        }
        group.add_action(&navigate);

        let open = gio::SimpleAction::new("open-external", Some(glib::VariantTy::STRING));
        {
            let weak = weak.clone();
            open.connect_activate(move |_, param| {
                let Some(path) = param.and_then(|value| value.str()).map(PathBuf::from) else {
                    return;
                };
                if let Some(panel) = weak.upgrade() {
                    panel.launch(&path);
                }
            });
        }
        group.add_action(&open);

        let copy = gio::SimpleAction::new("copy-path", Some(glib::VariantTy::STRING));
        {
            let weak = weak.clone();
            copy.connect_activate(move |_, param| {
                let Some(path) = param.and_then(|value| value.str()).map(PathBuf::from) else {
                    return;
                };
                if let Some(panel) = weak.upgrade() {
                    let text = path.to_string_lossy().to_string();
                    if let Some(display) = gtk::gdk::Display::default() {
                        display.clipboard().set_text(&text);
                    }
                    panel
                        .toast
                        .add_toast(libadwaita::Toast::new("Ruta copiada"));
                }
            });
        }
        group.add_action(&copy);

        let reduce = gio::SimpleAction::new("refresh", None);
        {
            let weak = weak.clone();
            reduce.connect_activate(move |_, _| {
                if let Some(panel) = weak.upgrade() {
                    panel.reload();
                }
            });
        }
        group.add_action(&reduce);

        let follow =
            gio::SimpleAction::new_stateful("follow", None, &self.follow.get().to_variant());
        {
            let weak = weak.clone();
            follow.connect_activate(move |action, _| {
                let next = !action
                    .state()
                    .and_then(|state| state.get::<bool>())
                    .unwrap_or(true);
                action.set_state(&next.to_variant());
                if let Some(panel) = weak.upgrade() {
                    panel.set_follow(next);
                }
            });
        }
        group.add_action(&follow);
        *self.follow_action.borrow_mut() = Some(follow);

        let hidden =
            gio::SimpleAction::new_stateful("hidden", None, &self.show_hidden.get().to_variant());
        {
            let weak = weak.clone();
            hidden.connect_activate(move |action, _| {
                let next = !action
                    .state()
                    .and_then(|state| state.get::<bool>())
                    .unwrap_or(false);
                action.set_state(&next.to_variant());
                if let Some(panel) = weak.upgrade() {
                    panel.set_show_hidden(next);
                }
            });
        }
        group.add_action(&hidden);

        self.root.insert_action_group("file", Some(&group));

        let menu = gio::Menu::new();
        menu.append(Some("Seguir el panel activo"), Some("file.follow"));
        menu.append(Some("Mostrar archivos ocultos"), Some("file.hidden"));
        menu.append(Some("Actualizar"), Some("file.refresh"));
        menu_button.set_menu_model(Some(&menu));
    }

    fn connect_navigation(self: &Rc<Self>) {
        {
            let weak = Rc::downgrade(self);
            self.list.connect_activate(move |_, position| {
                if let Some(panel) = weak.upgrade() {
                    panel.activate_at(position);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.up.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    let current = panel.current.borrow().clone();
                    if let Some(parent) = parent_of(&current) {
                        panel.note_manual_navigation();
                        panel.set_root(&parent);
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.search.connect_search_changed(move |_| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                if let Some(source) = panel.search_source.take() {
                    source.remove();
                }
                let weak = weak.clone();
                let source = glib::timeout_add_local_once(Duration::from_millis(50), move || {
                    if let Some(panel) = weak.upgrade() {
                        panel.search_source.set(None);
                        panel.apply_filter();
                    }
                });
                panel.search_source.set(Some(source));
            });
        }
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn current(&self) -> PathBuf {
        self.current.borrow().clone()
    }

    pub fn rows(&self) -> usize {
        self.rows.get()
    }

    pub fn load_ms(&self) -> f64 {
        self.load_ms.get()
    }

    pub fn follows_cwd(&self) -> bool {
        self.follow.get()
    }

    /// Navega la raíz del árbol a `path`. Devuelve `false` si ya era la raíz.
    pub fn set_root(self: &Rc<Self>, path: &Path) -> bool {
        let target = path.to_path_buf();
        if target == *self.current.borrow() {
            return false;
        }
        *self.current.borrow_mut() = target;
        self.search.set_text("");
        self.reload();
        self.watch_root();
        true
    }

    pub fn reload(&self) {
        if let Some(source) = self.refresh_source.take() {
            source.remove();
        }
        let path = self.current.borrow().clone();
        let started = Instant::now();
        let result = fs::list_dir(&path, self.show_hidden.get());
        self.update_path_label(&path);
        self.up.set_sensitive(parent_of(&path).is_some());
        match result {
            Ok(entries) => {
                self.cache.replace(entries);
                self.apply_filter();
            }
            Err(error) => {
                self.cache.replace(Vec::new());
                self.fill(&[]);
                self.show_status(
                    "No se pudo leer la carpeta",
                    &error.to_string(),
                    "dialog-error-symbolic",
                );
            }
        }
        self.load_ms.set(started.elapsed().as_secs_f64() * 1000.0);
    }

    fn apply_filter(&self) {
        let query = self.search.text().to_string();
        let entries = self.cache.borrow();
        if query.trim().is_empty() {
            self.fill(&entries);
            if entries.is_empty() {
                self.show_status(
                    "Carpeta vacía",
                    "No hay archivos para mostrar.",
                    "folder-symbolic",
                );
            }
        } else {
            let filtered: Vec<FileEntry> =
                fs::filter(&entries, &query).into_iter().cloned().collect();
            self.fill(&filtered);
            if filtered.is_empty() {
                self.show_status(
                    "Sin coincidencias",
                    &format!("Ningún archivo coincide con «{query}»."),
                    "edit-find-symbolic",
                );
            }
        }
    }

    fn fill(&self, entries: &[FileEntry]) {
        let infos = file_infos(entries);
        self.root_store.splice(0, self.root_store.n_items(), &infos);
        self.rows.set(infos.len());
        if infos.is_empty() {
            if self.stack.visible_child_name().as_deref() != Some("status") {
                self.show_status(
                    "Sin archivos",
                    "No hay archivos para mostrar.",
                    "folder-symbolic",
                );
            }
        } else {
            self.stack.set_visible_child_name("list");
        }
    }

    fn show_status(&self, title: &str, description: &str, icon: &str) {
        self.status.set_title(title);
        self.status.set_description(Some(description));
        self.status.set_icon_name(Some(icon));
        self.stack.set_visible_child_name("status");
    }

    fn update_path_label(&self, path: &Path) {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let text = home
            .as_deref()
            .and_then(|home| path.strip_prefix(home).ok())
            .map(|rest| {
                if rest.as_os_str().is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", rest.display())
                }
            })
            .unwrap_or_else(|| path.display().to_string());
        self.path_label.set_text(&text);
        self.path_label
            .set_tooltip_text(Some(&path.display().to_string()));
    }

    fn watch_root(self: &Rc<Self>) {
        if let Some(monitor) = self.monitor.borrow_mut().take() {
            monitor.cancel();
        }
        let path = self.current.borrow().clone();
        let file = gio::File::for_path(&path);
        match file.monitor_directory(
            gio::FileMonitorFlags::WATCH_MOVES,
            None::<&gio::Cancellable>,
        ) {
            Ok(monitor) => {
                let weak = Rc::downgrade(self);
                monitor.connect_changed(move |_, _, _, _| {
                    if let Some(panel) = weak.upgrade() {
                        panel.schedule_reload();
                    }
                });
                *self.monitor.borrow_mut() = Some(monitor);
            }
            Err(error) => {
                eprintln!("termust: no se pudo observar {}: {error}", path.display());
            }
        }
    }

    fn schedule_reload(self: &Rc<Self>) {
        if let Some(source) = self.refresh_source.take() {
            source.remove();
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(Duration::from_millis(200), move || {
            if let Some(panel) = weak.upgrade() {
                panel.refresh_source.set(None);
                panel.reload();
            }
        });
        self.refresh_source.set(Some(source));
    }

    pub fn set_follow(&self, follow: bool) {
        self.follow.set(follow);
        if let Some(settings) = &self.settings {
            let _ = settings.set_boolean("files-follow-cwd", follow);
        }
    }

    /// Navegar a mano deja de seguir el cwd; el usuario puede reactivarlo.
    fn note_manual_navigation(&self) {
        if !self.follow.get() {
            return;
        }
        self.set_follow(false);
        if let Some(action) = self.follow_action.borrow().as_ref() {
            action.set_state(&false.to_variant());
        }
    }

    pub fn set_show_hidden(&self, hidden: bool) {
        if self.show_hidden.replace(hidden) == hidden {
            return;
        }
        if let Some(settings) = &self.settings {
            let _ = settings.set_boolean("files-show-hidden", hidden);
        }
        self.reload();
    }

    pub fn set_search(&self, text: &str) {
        self.search.set_text(text);
        self.apply_filter();
    }

    pub fn visible_names(&self) -> Vec<String> {
        (0..self.tree.n_items())
            .filter_map(|index| self.tree.item(index))
            .filter_map(|object| object.downcast::<gtk::TreeListRow>().ok())
            .filter_map(|row| row.item())
            .filter_map(|object| object.downcast::<gio::FileInfo>().ok())
            .map(|info| info.name().to_string_lossy().to_string())
            .collect()
    }

    pub fn name_is_navigable(&self, name: &str) -> Option<bool> {
        self.find_root_row(name)
            .map(|(_, info)| is_navigable(&info) && !is_broken(&info))
    }

    pub fn expand_root(&self, name: &str) -> bool {
        let Some((row, _)) = self.find_root_row(name) else {
            return false;
        };
        row.set_expanded(true);
        true
    }

    pub fn activate_root(self: &Rc<Self>, name: &str) -> bool {
        let Some((_, info)) = self.find_root_row(name) else {
            return false;
        };
        if is_broken(&info) {
            return false;
        }
        if let Some(path) = path_from_info(&info) {
            if is_navigable(&info) {
                self.note_manual_navigation();
                self.set_root(&path);
            }
        }
        true
    }

    fn find_root_row(&self, name: &str) -> Option<(gtk::TreeListRow, gio::FileInfo)> {
        for index in 0..self.tree.n_items() {
            let Some(object) = self.tree.item(index) else {
                continue;
            };
            let Some(row) = object.downcast::<gtk::TreeListRow>().ok() else {
                continue;
            };
            let Some(info) = row.item().and_downcast::<gio::FileInfo>() else {
                continue;
            };
            if info.name().to_string_lossy() == name {
                return Some((row, info));
            }
        }
        None
    }

    fn activate_at(self: &Rc<Self>, position: u32) {
        let Some(object) = self.tree.item(position) else {
            return;
        };
        let Some(row) = object.downcast::<gtk::TreeListRow>().ok() else {
            return;
        };
        let Some(info) = row.item().and_downcast::<gio::FileInfo>() else {
            return;
        };
        if is_broken(&info) {
            return;
        }
        let Some(path) = path_from_info(&info) else {
            return;
        };
        if is_navigable(&info) {
            self.note_manual_navigation();
            self.set_root(&path);
        } else {
            self.launch(&path);
        }
    }

    fn launch(&self, path: &Path) {
        let file = gio::File::for_path(path);
        let launcher = gtk::FileLauncher::new(Some(&file));
        let parent = self.root.root().and_downcast::<gtk::Window>();
        let toast = self.toast.clone();
        launcher.launch(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
            if let Err(error) = result {
                toast.add_toast(libadwaita::Toast::new(&format!(
                    "No se pudo abrir: {error}"
                )));
            }
        });
    }

    fn show_menu(&self, item: &gtk::ListItem, x: f64, y: f64) {
        let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        let Some(info) = row.item().and_downcast::<gio::FileInfo>() else {
            return;
        };
        if is_broken(&info) {
            return;
        }
        let Some(path) = path_from_info(&info) else {
            return;
        };
        let menu = gio::Menu::new();
        if is_navigable(&info) {
            let open = gio::MenuItem::new(Some("Abrir carpeta"), None);
            open.set_action_and_target_value(
                Some("file.navigate"),
                Some(&path.to_string_lossy().to_string().to_variant()),
            );
            menu.append_item(&open);
        } else {
            let open = gio::MenuItem::new(Some("Abrir con aplicación externa"), None);
            open.set_action_and_target_value(
                Some("file.open-external"),
                Some(&path.to_string_lossy().to_string().to_variant()),
            );
            menu.append_item(&open);
        }
        let copy = gio::MenuItem::new(Some("Copiar ruta"), None);
        copy.set_action_and_target_value(
            Some("file.copy-path"),
            Some(&path.to_string_lossy().to_string().to_variant()),
        );
        menu.append_item(&copy);

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_has_arrow(false);
        if let Some(expander) = item.child() {
            popover.set_parent(&expander);
        }
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
    }
}

fn parent_of(path: &Path) -> Option<PathBuf> {
    path.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(name: &str, kind: EntryKind, is_dir: bool) -> FileEntry {
        FileEntry {
            path: PathBuf::from("/tmp").join(name),
            name: name.to_string(),
            kind,
            is_dir,
            hidden: false,
        }
    }

    #[test]
    fn files_no_destructive_actions() {
        assert!(FILE_ACTIONS.contains(&"open-external"));
        assert!(FILE_ACTIONS.contains(&"copy-path"));
        for forbidden in ["delete", "trash", "rename", "move", "remove"] {
            assert!(!FILE_ACTIONS.contains(&forbidden), "{forbidden}");
        }
    }

    #[test]
    fn file_info_carries_path_and_icon() {
        let dir = file_info(&entry("src", EntryKind::Dir, true));
        assert_eq!(dir.name(), PathBuf::from("src"));
        assert_eq!(dir.file_type(), gio::FileType::Directory);
        assert_eq!(path_from_info(&dir), Some(PathBuf::from("/tmp/src")));
        assert_eq!(icon_from_info(&dir), "folder-symbolic");
        assert_eq!(color_from_info(&dir), "termust-folder");
        assert!(is_navigable(&dir));

        let code = file_info(&entry("main.rs", EntryKind::File, false));
        assert_eq!(code.file_type(), gio::FileType::Regular);
        assert_eq!(icon_from_info(&code), "text-x-generic-symbolic");
        assert_eq!(color_from_info(&code), "termust-code");
        assert!(!is_navigable(&code));
    }

    #[test]
    fn file_info_marks_broken_symlinks_inert() {
        let broken = file_info(&entry("roto", EntryKind::BrokenSymlink, false));
        assert_eq!(broken.file_type(), gio::FileType::SymbolicLink);
        assert!(is_broken(&broken));
        assert!(!is_navigable(&broken));
        assert_eq!(icon_from_info(&broken), "dialog-warning-symbolic");

        let link_dir = file_info(&entry("enlace", EntryKind::SymlinkToDir, true));
        assert!(is_navigable(&link_dir));
        assert!(!is_broken(&link_dir));
    }
}
