mod appearance;
mod backup;
mod diff_render;
mod diff_view;
mod files;
mod git_async;
mod git_host;
mod git_panel;
mod history;
mod i18n;
mod markdown;
mod preferences;
mod remotes;
mod screenshot;
mod tab_sidebar;
mod tabs;
mod viewer;
mod work_area;

use appearance::{Appearance, AUTO_THEME};
use git_panel::GitPanel;
use gtk::gdk;
use gtk::glib;
use gtk4 as gtk;
use libadwaita::prelude::*;
use libadwaita::{Application, ApplicationWindow, HeaderBar, ToolbarView};
use preferences::PreferencesController;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;
use tab_sidebar::TabSidebar;
use tabs::TabManager;
use termust_agents::{AgentCatalog, AgentState, AgentStatus};
use termust_core::history::{HistoryEntry, HistoryIndex, HistorySource};
use termust_core::keymap::{
    resolve_shortcut, Action, Focus, Key as CoreKey, KeyConfig, KeyEvent, Mods,
};
use termust_core::tabs::{Direction, Preset, SplitOrientation};
use termust_pty_unix::UnixPtySpawner;
use viewer::{Viewer, ViewerColors};

fn main() -> glib::ExitCode {
    // ADR-003: Cairo es el renderer de v1. GSK con GL agrega ~22 MB PSS a una
    // ventana vacía y rompe el presupuesto de RAM. Se respeta un override explícito.
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "cairo");
    }

    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("termust {}", env!("CARGO_PKG_VERSION"));
        return glib::ExitCode::SUCCESS;
    }

    let app = Application::builder()
        .application_id(termust_core::APP_ID)
        .build();
    app.set_flags(gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE);
    let no_restore = Rc::new(Cell::new(false));
    {
        let no_restore = no_restore.clone();
        app.connect_command_line(move |app, command_line| {
            let args: Vec<String> = command_line
                .arguments()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            no_restore.set(args.iter().any(|arg| arg == "--no-restore"));
            app.activate();
            glib::ExitCode::SUCCESS
        });
    }
    let slot: Rc<RefCell<Option<ApplicationWindow>>> = Rc::new(RefCell::new(None));
    let activate_slot = slot.clone();
    let activate_no_restore = no_restore.clone();
    app.connect_activate(move |app| {
        if let Some(window) = activate_slot.borrow().as_ref() {
            window.present();
            return;
        }
        let window = build_ui(app, activate_no_restore.get());
        *activate_slot.borrow_mut() = Some(window);
    });
    app.run()
}

fn build_ui(app: &Application, no_restore: bool) -> ApplicationWindow {
    i18n::init();
    let started = std::time::Instant::now();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let testing = std::env::vars_os().any(|(key, _)| {
        let key = key.to_string_lossy();
        key.starts_with("TERMUST_") && key.ends_with("_TEST")
    });
    if !testing && std::env::var_os("TERMUST_NO_BACKUP").is_none() {
        backup::backup_state(&home);
    }
    let dev_tabs = initial_tabs() > 1;
    let seed = std::env::var_os("TERMUST_SESSION_SEED").is_some();
    let verify = std::env::var_os("TERMUST_SESSION_VERIFY").is_some();
    let corrupt_test = std::env::var_os("TERMUST_SESSION_CORRUPT_TEST").is_some();
    let fresh_hooks = [
        "TERMUST_PANES_TEST",
        "TERMUST_CWD_TEST",
        "TERMUST_CLOSE_CONFIRM_TEST",
        "TERMUST_CLOSE_TEST",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some());
    if fresh_hooks && std::env::var_os("TERMUST_SESSION_PATH").is_none() {
        let path =
            std::env::temp_dir().join(format!("termust-hook-session-{}.json", std::process::id()));
        std::env::set_var("TERMUST_SESSION_PATH", path);
    }
    let restore_enabled = !no_restore
        && !dev_tabs
        && !seed
        && !fresh_hooks
        && (verify || corrupt_test || restore_preference());
    let load = if restore_enabled {
        termust_store::load(&termust_store::session_path())
    } else {
        termust_store::LoadResult::Missing
    };
    let (win_width, win_height, maximized) = match &load {
        termust_store::LoadResult::Loaded(snapshot) => (
            snapshot.window.width,
            snapshot.window.height,
            snapshot.window.maximized,
        ),
        _ => (1024, 700, false),
    };
    // El hook angosto necesita una ventana real <720 px. GTK4/Wayland no tiene
    // resize programático, así que el tamaño se fija antes de mapear.
    let (win_width, win_height, maximized) =
        if std::env::var("TERMUST_FILECOLUMN_TEST").as_deref() == Ok("narrow") {
            (700, 600, false)
        } else if std::env::var_os("TERMUST_SCREENSHOT_TEST").is_some() {
            (1600, 950, false)
        } else {
            (win_width, win_height, maximized)
        };

    let header = HeaderBar::new();
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);

    let agent_icon = gtk::Image::from_icon_name("avatar-default-symbolic");
    let agent_name = gtk::Label::new(None);
    agent_name.add_css_class("heading");
    let agent_task = gtk::Label::new(None);
    agent_task.set_ellipsize(gtk::pango::EllipsizeMode::End);
    agent_task.set_xalign(0.0);
    agent_task.set_hexpand(true);
    agent_task.add_css_class("dim-label");
    agent_task.add_css_class("caption");
    let agent_dot = gtk::Label::new(Some("●"));
    agent_dot.add_css_class("termust-agent-dot");
    let agent_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    agent_bar.add_css_class("termust-agent-bar");
    agent_bar.set_margin_top(4);
    agent_bar.set_margin_bottom(4);
    agent_bar.set_margin_start(10);
    agent_bar.set_margin_end(10);
    agent_bar.append(&agent_icon);
    agent_bar.append(&agent_name);
    agent_bar.append(&agent_task);
    agent_bar.append(&agent_dot);
    agent_bar.set_visible(false);
    toolbar.add_bottom_bar(&agent_bar);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Termust")
        .default_width(win_width)
        .default_height(win_height)
        .content(&toolbar)
        .build();

    let appearance = Appearance::load(&home, termust_ui::system_monospace_font());
    let spawner = match UnixPtySpawner::new() {
        Ok(s) => Rc::new(s),
        Err(e) => {
            show_error(&toolbar, &e.to_string());
            window.present();
            return window;
        }
    };
    let shell = termust_host_exec::default_shell();
    let integration = match termust_host_exec::materialize() {
        Ok(si) => si,
        Err(e) => {
            show_error(&toolbar, &e.to_string());
            window.present();
            return window;
        }
    };

    let mut notice: Option<String> = None;
    let make_fresh = || {
        TabManager::new(
            spawner.clone(),
            shell.clone(),
            integration.clone(),
            home.clone(),
            window.clone(),
            appearance.clone(),
        )
    };
    let manager = match load {
        termust_store::LoadResult::Loaded(mut snapshot) => {
            let missing = termust_store::fallback_cwds(&mut snapshot, &home);
            match tabs::restored_session(&snapshot, &home) {
                Ok(restored) => match TabManager::restore(
                    spawner.clone(),
                    shell.clone(),
                    integration.clone(),
                    window.clone(),
                    appearance.clone(),
                    restored.model,
                    restored.pane_cwds,
                ) {
                    Ok(manager) => {
                        notice = Some(if missing {
                            i18n::tr("Session restored; some directories did not exist and $HOME was used.")
                        } else {
                            i18n::tr("Session restored. Processes are not restored.")
                        });
                        Ok(manager)
                    }
                    Err(e) => {
                        eprintln!("termust: no se pudo restaurar la sesión: {e}");
                        notice = Some(i18n::tr(
                            "Could not restore the session; a new one was started.",
                        ));
                        make_fresh()
                    }
                },
                Err(e) => {
                    eprintln!("termust: sesión inválida: {e}");
                    notice = Some(i18n::tr(
                        "The saved session is invalid; a new one was started.",
                    ));
                    make_fresh()
                }
            }
        }
        termust_store::LoadResult::Corrupt(e) => {
            eprintln!("termust: sesión corrupta: {e}");
            notice = Some(i18n::tr(
                "Could not read the saved session; a new one was started.",
            ));
            make_fresh()
        }
        termust_store::LoadResult::FutureVersion(version) => {
            eprintln!("termust: sesión con schema v{version} no soportado");
            notice = Some(i18n::tr(
                "The saved session is from a newer version; it was ignored.",
            ));
            make_fresh()
        }
        termust_store::LoadResult::Missing => make_fresh(),
    };
    let manager = match manager {
        Ok(m) => Rc::new(RefCell::new(m)),
        Err(e) => {
            show_error(&toolbar, &e);
            window.present();
            return window;
        }
    };
    TabManager::connect(&manager);
    {
        let manager = Rc::downgrade(&manager);
        appearance.on_change(move || {
            if let Some(manager) = manager.upgrade() {
                if let Ok(manager) = manager.try_borrow() {
                    manager.refresh_appearance();
                }
            }
        });
    }
    manager.borrow().refresh_appearance();
    if std::env::var_os("TERMUST_STARTUP_TEST").is_some() {
        eprintln!(
            "termust-startup: pestaña activa adjunta en {} ms",
            started.elapsed().as_millis()
        );
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local(Duration::from_millis(5), move || {
            let ready = manager
                .try_borrow()
                .ok()
                .and_then(|manager| manager.active_view().map(|view| view.history_ready()))
                .unwrap_or(false);
            if ready || started.elapsed() > Duration::from_secs(2) {
                eprintln!(
                    "termust-startup: primer prompt en {} ms",
                    started.elapsed().as_millis()
                );
                app.quit();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }
    let agent_catalog = Rc::new(AgentCatalog::load());
    for error in &agent_catalog.errors {
        eprintln!("termust: regla de agente ignorada: {error}");
    }
    for _ in 1..initial_tabs() {
        if let Err(e) = manager.borrow_mut().open_tab(home.clone()) {
            eprintln!("termust: no se pudo abrir pestaña: {e}");
            break;
        }
    }
    if std::env::var_os("TERMUST_STARTUP9_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local(Duration::from_millis(20), move || {
            let (tabs, ready) = manager
                .try_borrow()
                .map(|manager| (manager.tab_count(), manager.ready_views()))
                .unwrap_or((0, 0));
            if (tabs >= 9 && ready >= 9) || started.elapsed() > Duration::from_secs(3) {
                eprintln!(
                    "termust-startup9: pestañas={tabs} listas={ready} en {} ms",
                    started.elapsed().as_millis()
                );
                app.quit();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    install_css();
    let new_tab = gtk::Button::builder()
        .icon_name("tab-new-symbolic")
        .tooltip_text(i18n::tr("New tab"))
        .build();
    new_tab.add_css_class("flat");
    new_tab.update_property(&[gtk::accessible::Property::Label(&i18n::tr("New tab"))]);
    {
        let manager = manager.clone();
        let home = home.clone();
        new_tab.connect_clicked(move |_| {
            if let Err(e) = manager.borrow_mut().open_tab(home.clone()) {
                eprintln!("termust: no se pudo abrir pestaña: {e}");
            }
        });
    }
    let files_visible = setting_bool("files-panel", false);
    let vertical_visible = setting_bool("vertical-tabs", false);
    let files_button = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text(i18n::tr("File panel (F9)"))
        .active(files_visible)
        .build();
    files_button.add_css_class("flat");
    files_button.update_property(&[gtk::accessible::Property::Label(&i18n::tr("File panel"))]);
    let tabs_button = gtk::ToggleButton::builder()
        .icon_name("view-list-symbolic")
        .tooltip_text(i18n::tr("Vertical tabs (Ctrl+Shift+L)"))
        .active(vertical_visible)
        .build();
    tabs_button.add_css_class("flat");
    tabs_button.update_property(&[gtk::accessible::Property::Label(&i18n::tr("Vertical tabs"))]);
    let git_button = gtk::ToggleButton::builder()
        .label("Git")
        .tooltip_text(i18n::tr("Git (Ctrl+Shift+G)"))
        .build();
    git_button.add_css_class("flat");
    git_button.update_property(&[gtk::accessible::Property::Label(&i18n::tr("Git"))]);
    header.pack_start(&files_button);
    header.pack_start(&new_tab);
    header.pack_start(&tabs_button);
    header.pack_start(&git_button);
    let overview_button = gtk::ToggleButton::builder()
        .icon_name("view-grid-symbolic")
        .tooltip_text(i18n::tr("Tab overview"))
        .build();
    overview_button.add_css_class("flat");
    overview_button.update_property(&[gtk::accessible::Property::Label(&i18n::tr("Tab overview"))]);
    header.pack_start(&overview_button);
    let tab_bar = manager.borrow().tab_bar().clone();
    tab_bar.set_hexpand(true);
    tab_bar.set_halign(gtk::Align::Fill);
    tab_bar.set_visible(!vertical_visible);
    header.set_title_widget(Some(&tab_bar));
    let toast_overlay = libadwaita::ToastOverlay::new();
    toast_overlay.set_child(Some(manager.borrow().tab_view()));

    let viewer = Viewer::new(
        ViewerColors::from_theme(&appearance.render_theme(None)),
        toast_overlay.clone(),
    );
    let diff_view = diff_view::DiffView::new();
    viewer.set_diff_page(diff_view.widget());
    {
        let weak = Rc::downgrade(&viewer);
        let diff_view = diff_view.clone();
        viewer.set_on_page_changed(move |diff| {
            if !diff {
                return;
            }
            if let Some(viewer) = weak.upgrade() {
                if let Some(path) = viewer.path() {
                    diff_view.load(&path);
                }
            }
        });
    }
    let file_split = libadwaita::OverlaySplitView::new();
    file_split.set_sidebar(Some(viewer.widget()));
    file_split.set_content(Some(&toast_overlay));
    file_split.set_sidebar_position(gtk::PackType::End);
    file_split.set_min_sidebar_width(300.0);
    file_split.set_max_sidebar_width(760.0);
    file_split.set_sidebar_width_fraction(0.35);
    file_split.set_show_sidebar(false);
    viewer.widget().set_hexpand(true);
    viewer.widget().set_vexpand(true);
    {
        let manager = Rc::downgrade(&manager);
        let file_split = file_split.clone();
        viewer.set_on_close(move || {
            file_split.set_show_sidebar(false);
            if let Some(manager) = manager.upgrade() {
                if let Ok(manager) = manager.try_borrow() {
                    manager.focus_active();
                }
            }
        });
    }
    let git_panel = GitPanel::new(toast_overlay.clone());
    {
        let git_panel = git_panel.clone();
        let git_button = git_button.clone();
        let manager = Rc::downgrade(&manager);
        git_panel.set_on_close(move || {
            if git_button.is_active() {
                git_button.set_active(false);
            }
            if let Some(manager) = manager.upgrade() {
                if let Ok(manager) = manager.try_borrow() {
                    manager.focus_active();
                }
            }
        });
    }
    let suppress_files_persist = Rc::new(Cell::new(false));
    let hide_files_panel_session: Rc<dyn Fn()> = {
        let suppress = suppress_files_persist.clone();
        let files_button = files_button.clone();
        Rc::new(move || {
            suppress.set(true);
            files_button.set_active(false);
            suppress.set(false);
        })
    };
    let file_panel = files::FilePanel::new(&home, toast_overlay.clone());
    {
        let viewer = viewer.clone();
        let git_panel = git_panel.clone();
        let git_button = git_button.clone();
        let file_split = file_split.clone();
        let hide_files_panel_session = hide_files_panel_session.clone();
        file_panel.set_open_handler(move |path| {
            git_panel.close();
            if git_button.is_active() {
                git_button.set_active(false);
            }
            viewer.open_file(&path);
            file_split.set_show_sidebar(true);
            if file_split.is_collapsed() {
                hide_files_panel_session();
            }
        });
    }
    if files_visible {
        file_panel.activate();
    }
    let files_split = libadwaita::OverlaySplitView::new();
    files_split.set_sidebar(Some(file_panel.widget()));
    files_split.set_content(Some(&file_split));
    files_split.set_sidebar_position(gtk::PackType::Start);
    files_split.set_min_sidebar_width(220.0);
    files_split.set_max_sidebar_width(420.0);
    files_split.set_sidebar_width_fraction(0.22);
    files_split.set_show_sidebar(files_visible);

    let tab_sidebar = TabSidebar::new(&manager);
    let outer_split = libadwaita::OverlaySplitView::new();
    outer_split.set_sidebar(Some(tab_sidebar.widget()));
    outer_split.set_content(Some(&files_split));
    outer_split.set_sidebar_position(gtk::PackType::Start);
    outer_split.set_min_sidebar_width(190.0);
    outer_split.set_max_sidebar_width(320.0);
    outer_split.set_sidebar_width_fraction(0.18);
    outer_split.set_show_sidebar(vertical_visible);

    let tab_overview = libadwaita::TabOverview::new();
    tab_overview.set_view(Some(manager.borrow().tab_view()));
    tab_overview.set_enable_new_tab(false);
    tab_overview.set_enable_search(false);
    // Cerrado sigue siendo un overlay visible que, si es targetable, se come
    // todos los clics del terminal y de los sidebars.
    tab_overview.set_can_target(false);
    tab_overview.set_focusable(false);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&outer_split));
    overlay.add_overlay(&tab_overview);
    git_panel.widget().set_halign(gtk::Align::Fill);
    git_panel.widget().set_valign(gtk::Align::Fill);
    overlay.add_overlay(git_panel.widget());
    toolbar.set_content(Some(&overlay));
    {
        let overview = tab_overview.clone();
        overview_button.connect_toggled(move |button| {
            overview.set_open(button.is_active());
        });
        let button = overview_button.clone();
        tab_overview.connect_notify_local(Some("open"), move |overview, _| {
            overview.set_can_target(overview.is_open());
            overview.set_focusable(overview.is_open());
            if button.is_active() != overview.is_open() {
                button.set_active(overview.is_open());
            }
        });
    }

    {
        let condition = libadwaita::BreakpointCondition::parse("max-width: 720px")
            .expect("condición de breakpoint");
        let breakpoint = libadwaita::Breakpoint::new(condition);
        let files = files_split.clone();
        let outer = outer_split.clone();
        let file = file_split.clone();
        breakpoint.connect_apply(move |_| {
            files.set_collapsed(true);
            outer.set_collapsed(true);
            file.set_collapsed(true);
        });
        let files = files_split.clone();
        let outer = outer_split.clone();
        let file = file_split.clone();
        breakpoint.connect_unapply(move |_| {
            files.set_collapsed(false);
            outer.set_collapsed(false);
            file.set_collapsed(false);
        });
        window.add_breakpoint(breakpoint);
    }

    {
        let split = files_split.clone();
        let settings = gsettings_with_key("files-panel");
        let panel = file_panel.clone();
        let viewer = viewer.clone();
        let suppress = suppress_files_persist.clone();
        files_button.connect_toggled(move |button| {
            let active = button.is_active();
            split.set_show_sidebar(active);
            if active {
                panel.activate();
                if split.is_collapsed() && viewer.is_open() {
                    viewer.close();
                }
            } else {
                panel.deactivate();
            }
            if !suppress.get() {
                if let Some(settings) = &settings {
                    let _ = settings.set_boolean("files-panel", active);
                }
            }
        });
    }
    {
        let split = outer_split.clone();
        let bar = tab_bar.clone();
        let settings = gsettings_with_key("vertical-tabs");
        tabs_button.connect_toggled(move |button| {
            let active = button.is_active();
            split.set_show_sidebar(active);
            bar.set_visible(!active);
            if let Some(settings) = &settings {
                let _ = settings.set_boolean("vertical-tabs", active);
            }
        });
    }
    {
        let viewer = viewer.clone();
        let git_panel = git_panel.clone();
        let manager = manager.clone();
        let home = home.clone();
        git_button.connect_toggled(move |button| {
            if !button.is_active() {
                git_panel.close();
                return;
            }
            viewer.close();
            let cwd = manager
                .borrow()
                .active_cwd()
                .filter(|cwd| cwd.is_dir())
                .unwrap_or_else(|| home.clone());
            git_panel.open(&cwd);
        });
    }

    let history_index = Rc::new(RefCell::new(HistoryIndex::new()));
    // El historial de la sesión se persiste aparte del `.zsh_history`: si el
    // shell muere sin volcar (cierre de ventana, apagado), no se pierde.
    let history_store: Option<PathBuf> = if testing || no_restore {
        // Los tests escriben a un archivo temporal via TERMUST_HISTORY_PATH.
        std::env::var_os("TERMUST_HISTORY_PATH").map(PathBuf::from)
    } else {
        Some(termust_store::history_path())
    };
    if let Some(path) = history_store.as_deref() {
        let mut loaded = termust_store::load_history(path);
        if loaded.len() > 5_000
            || std::fs::metadata(path)
                .map(|m| m.len() > termust_store::MAX_HISTORY_FILE_BYTES)
                .unwrap_or(false)
        {
            let keep = loaded.len().saturating_sub(5_000);
            let _ = termust_store::rewrite_history(path, &loaded[keep..]);
            loaded.drain(..keep);
        }
        let mut index = history_index.borrow_mut();
        for record in loaded {
            index.insert(HistoryEntry::new(
                record.text,
                record.cwd,
                record.timestamp,
                record.exit_code,
                HistorySource::Session,
            ));
        }
    }
    let history =
        history::HistoryController::new(history_index.clone(), home.clone(), toast_overlay.clone());
    let preferences = PreferencesController::new(appearance.clone());
    preferences::add_session_page(preferences.dialog());

    {
        let group = gtk::gio::SimpleActionGroup::new();
        let history_action = gtk::gio::SimpleAction::new("history", None);
        {
            let manager = manager.clone();
            let history = history.clone();
            let toast = toast_overlay.clone();
            history_action.connect_activate(move |_, _| {
                open_history(&manager, &history, &toast);
            });
        }
        group.add_action(&history_action);
        let prefs_action = gtk::gio::SimpleAction::new("preferences", None);
        {
            let window = window.clone();
            let preferences = preferences.clone();
            prefs_action.connect_activate(move |_, _| {
                preferences.present(&window);
            });
        }
        group.add_action(&prefs_action);
        let shortcuts_action = gtk::gio::SimpleAction::new("shortcuts", None);
        {
            let window = window.clone();
            shortcuts_action.connect_activate(move |_, _| show_shortcuts(&window));
        }
        group.add_action(&shortcuts_action);
        let about_action = gtk::gio::SimpleAction::new("about", None);
        {
            let window = window.clone();
            about_action.connect_activate(move |_, _| show_about(&window));
        }
        group.add_action(&about_action);
        let tab_theme = gtk::gio::SimpleAction::new("tab-theme", Some(glib::VariantTy::STRING));
        {
            let manager = manager.clone();
            let toast = toast_overlay.clone();
            tab_theme.connect_activate(move |_, param| {
                let Some(id) = param.and_then(|value| value.str()).map(str::to_string) else {
                    return;
                };
                let theme_id = if id == AUTO_THEME { None } else { Some(id) };
                if !manager.borrow_mut().set_active_tab_theme(theme_id) {
                    toast.add_toast(libadwaita::Toast::new(&i18n::tr("No active tab")));
                }
            });
        }
        group.add_action(&tab_theme);
        let clear_history = gtk::gio::SimpleAction::new("clear-history", None);
        {
            let index = history_index.clone();
            let toast = toast_overlay.clone();
            clear_history.connect_activate(move |_, _| {
                index.borrow_mut().clear();
                toast.add_toast(libadwaita::Toast::new(&i18n::tr("Command history cleared")));
            });
        }
        group.add_action(&clear_history);
        let split_right = gtk::gio::SimpleAction::new("split-right", None);
        {
            let manager = manager.clone();
            let toast = toast_overlay.clone();
            split_right.connect_activate(move |_, _| {
                if let Err(e) = manager
                    .borrow_mut()
                    .split_active(SplitOrientation::Horizontal)
                {
                    toast.add_toast(libadwaita::Toast::new(&e));
                }
            });
        }
        group.add_action(&split_right);
        let split_down = gtk::gio::SimpleAction::new("split-down", None);
        {
            let manager = manager.clone();
            let toast = toast_overlay.clone();
            split_down.connect_activate(move |_, _| {
                if let Err(e) = manager
                    .borrow_mut()
                    .split_active(SplitOrientation::Vertical)
                {
                    toast.add_toast(libadwaita::Toast::new(&e));
                }
            });
        }
        group.add_action(&split_down);

        for (name, preset) in [
            ("preset-two-columns", Preset::TwoColumns),
            ("preset-two-rows", Preset::TwoRows),
            ("preset-three-columns", Preset::ThreeColumns),
            ("preset-grid", Preset::Grid2x2),
        ] {
            let action = gtk::gio::SimpleAction::new(name, None);
            let manager = manager.clone();
            let toast = toast_overlay.clone();
            action.connect_activate(move |_, _| {
                if let Err(e) = manager.borrow_mut().apply_preset(preset) {
                    toast.add_toast(libadwaita::Toast::new(&e));
                }
            });
            group.add_action(&action);
        }

        let close_pane = gtk::gio::SimpleAction::new("close-pane", None);
        {
            let manager = manager.clone();
            close_pane.connect_activate(move |_, _| TabManager::close_request(&manager));
        }
        group.add_action(&close_pane);
        let rename_tab = gtk::gio::SimpleAction::new("rename-tab", None);
        {
            let manager = manager.clone();
            rename_tab.connect_activate(move |_, _| TabManager::rename_dialog(&manager));
        }
        group.add_action(&rename_tab);
        window.insert_action_group("pane", Some(&group));

        let menu = gtk::gio::Menu::new();
        menu.append(
            Some(i18n::tr("Split right").as_str()),
            Some("pane.split-right"),
        );
        menu.append(
            Some(i18n::tr("Split down").as_str()),
            Some("pane.split-down"),
        );
        let designs = gtk::gio::Menu::new();
        designs.append(
            Some(i18n::tr("Two columns").as_str()),
            Some("pane.preset-two-columns"),
        );
        designs.append(
            Some(i18n::tr("Two rows").as_str()),
            Some("pane.preset-two-rows"),
        );
        designs.append(
            Some(i18n::tr("Three columns").as_str()),
            Some("pane.preset-three-columns"),
        );
        designs.append(
            Some(i18n::tr("2×2 grid").as_str()),
            Some("pane.preset-grid"),
        );
        menu.append_submenu(Some(i18n::tr("Layouts").as_str()), &designs);
        let themes_menu = gtk::gio::Menu::new();
        let global_theme = gtk::gio::MenuItem::new(Some(i18n::tr("Global theme").as_str()), None);
        global_theme
            .set_action_and_target_value(Some("pane.tab-theme"), Some(&AUTO_THEME.to_variant()));
        themes_menu.append_item(&global_theme);
        for theme in appearance.themes() {
            let item = gtk::gio::MenuItem::new(Some(&theme.name), None);
            item.set_action_and_target_value(Some("pane.tab-theme"), Some(&theme.id.to_variant()));
            themes_menu.append_item(&item);
        }
        menu.append_submenu(Some(i18n::tr("Tab theme").as_str()), &themes_menu);
        menu.append(
            Some(i18n::tr("Preferences").as_str()),
            Some("pane.preferences"),
        );
        menu.append(
            Some(i18n::tr("Keyboard shortcuts").as_str()),
            Some("pane.shortcuts"),
        );
        menu.append(Some(i18n::tr("About Termust").as_str()), Some("pane.about"));
        menu.append(
            Some(i18n::tr("Command history").as_str()),
            Some("pane.history"),
        );
        menu.append(
            Some(i18n::tr("Clear history").as_str()),
            Some("pane.clear-history"),
        );
        menu.append(
            Some(i18n::tr("Rename tab").as_str()),
            Some("pane.rename-tab"),
        );
        menu.append(
            Some(i18n::tr("Close pane").as_str()),
            Some("pane.close-pane"),
        );
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text(i18n::tr("Menu"))
            .build();
        menu_button.update_property(&[gtk::accessible::Property::Label(&i18n::tr("Menu"))]);
        menu_button.set_menu_model(Some(&menu));
        header.pack_end(&menu_button);
    }

    manager.borrow().focus_active();

    {
        let manager = manager.clone();
        let home = home.clone();
        let toast_overlay = toast_overlay.clone();
        let history = history.clone();
        let appearance = appearance.clone();
        let preferences = preferences.clone();
        let key_window = window.clone();
        let files_button = files_button.clone();
        let tabs_button = tabs_button.clone();
        let viewer = viewer.clone();
        let tab_overview = tab_overview.clone();
        let git_button = git_button.clone();
        let git_panel = git_panel.clone();
        let files_split = files_split.clone();
        let hide_files_panel_session = hide_files_panel_session.clone();
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _, state| {
            if tab_overview.is_open() {
                if keyval == gdk::Key::Escape {
                    tab_overview.set_open(false);
                    return glib::Propagation::Stop;
                }
                return glib::Propagation::Proceed;
            }
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            match viewer_key(
                viewer.is_open(),
                viewer.is_showing_file_page(),
                keyval,
                ctrl,
            ) {
                ViewerKey::Close => {
                    viewer.close();
                    return glib::Propagation::Stop;
                }
                ViewerKey::Search => {
                    viewer.toggle_search();
                    return glib::Propagation::Stop;
                }
                ViewerKey::Pass => {}
            }
            if git_panel.is_open() {
                if keyval == gdk::Key::Escape {
                    git_button.set_active(false);
                    return glib::Propagation::Stop;
                }
                return glib::Propagation::Proceed;
            }
            if files_split.is_collapsed()
                && files_split.shows_sidebar()
                && keyval == gdk::Key::Escape
            {
                hide_files_panel_session();
                return glib::Propagation::Stop;
            }
            if history.is_open() {
                return glib::Propagation::Proceed;
            }
            let Some(key) = map_key(keyval) else {
                return glib::Propagation::Proceed;
            };
            let mods = Mods {
                ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
                shift: state.contains(gdk::ModifierType::SHIFT_MASK),
                alt: state.contains(gdk::ModifierType::ALT_MASK),
                logo: state.contains(gdk::ModifierType::SUPER_MASK),
            };
            if key == CoreKey::Up && mods == Mods::none() {
                let ready = manager
                    .try_borrow()
                    .ok()
                    .and_then(|manager| manager.active_view().map(|view| view.history_ready()))
                    .unwrap_or(false);
                if ready {
                    open_history(&manager, &history, &toast_overlay);
                    return glib::Propagation::Stop;
                }
            }
            let Some(action) = resolve_shortcut(
                KeyEvent::new(key, mods),
                Focus::Terminal,
                &KeyConfig::default(),
            ) else {
                return glib::Propagation::Proceed;
            };

            if action == Action::Preferences {
                preferences.present(&key_window);
                return glib::Propagation::Stop;
            }
            if action == Action::ToggleFiles {
                files_button.set_active(!files_button.is_active());
                return glib::Propagation::Stop;
            }
            if action == Action::ToggleVerticalTabs {
                tabs_button.set_active(!tabs_button.is_active());
                return glib::Propagation::Stop;
            }
            if action == Action::ToggleGit {
                git_button.set_active(!git_button.is_active());
                return glib::Propagation::Stop;
            }
            if action == Action::RenameTab {
                TabManager::rename_dialog(&manager);
                return glib::Propagation::Stop;
            }
            if action == Action::CloseTab {
                TabManager::close_request(&manager);
                return glib::Propagation::Stop;
            }

            let mut mgr = manager.borrow_mut();
            match action {
                Action::NewTab => {
                    if let Err(e) = mgr.open_tab(home.clone()) {
                        eprintln!("termust: no se pudo abrir pestaña: {e}");
                    }
                }
                Action::NextTab => mgr.select_next(),
                Action::PrevTab => mgr.select_prev(),
                Action::Activate(n) => mgr.select_index(n.saturating_sub(1)),
                Action::ActivateLast => mgr.select_last(),
                Action::MoveTabLeft => mgr.move_active(-1),
                Action::MoveTabRight => mgr.move_active(1),
                Action::SplitPane(orientation) => {
                    if let Err(e) = mgr.split_active(orientation) {
                        toast_overlay.add_toast(libadwaita::Toast::new(&e));
                    }
                }
                Action::FocusPane(direction) => {
                    mgr.focus_direction(direction);
                }
                Action::ResizePane(direction) => {
                    mgr.resize_direction(direction);
                }
                Action::ZoomIn => {
                    appearance.zoom(1);
                    mgr.refresh_appearance();
                }
                Action::ZoomOut => {
                    appearance.zoom(-1);
                    mgr.refresh_appearance();
                }
                Action::ZoomReset => {
                    appearance.set_font_scale(1.0);
                    mgr.refresh_appearance();
                }
                _ => {}
            }
            glib::Propagation::Stop
        });
        window.add_controller(key);
    }

    if std::env::var_os("TERMUST_CLOSE_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(800), move || {
            for _ in 0..3 {
                manager.borrow_mut().close_active();
            }
            let app = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(500), move || app.quit());
        });
    }

    {
        let manager = manager.clone();
        let history_index = history_index.clone();
        let history = history.clone();
        let file_panel = file_panel.clone();
        let files_split = files_split.clone();
        let tab_sidebar = tab_sidebar.clone();
        let git_panel = git_panel.clone();
        let window = window.clone();
        let agent_catalog = agent_catalog.clone();
        let agent_bar = agent_bar.clone();
        let agent_icon = agent_icon.clone();
        let agent_name = agent_name.clone();
        let agent_task = agent_task.clone();
        let agent_dot = agent_dot.clone();
        let last_followed: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
        let last_git_cwd: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
        glib::timeout_add_local(Duration::from_millis(500), move || {
            if manager.borrow_mut().close_exited() {
                let window = window.clone();
                glib::idle_add_local_once(move || window.close());
                return glib::ControlFlow::Continue;
            }
            manager.borrow_mut().update_titles();
            manager.borrow().apply_scrollback_budget();
            manager.borrow_mut().update_agents(&agent_catalog);
            let agent = manager.borrow().active_agent();
            update_agent_bar(
                &agent_bar,
                &agent_icon,
                &agent_name,
                &agent_task,
                &agent_dot,
                agent.as_ref(),
            );
            tab_sidebar.refresh();
            if files_split.shows_sidebar() {
                if let Some(cwd) = manager.borrow().active_cwd() {
                    if cwd.is_dir() {
                        let changed = last_followed.borrow().as_ref() != Some(&cwd);
                        if changed {
                            *last_followed.borrow_mut() = Some(cwd.clone());
                            if file_panel.follows_cwd() {
                                file_panel.force_follow(&cwd);
                            }
                        } else if file_panel.follows_cwd() {
                            file_panel.follow_if_idle(&cwd);
                        }
                    }
                }
            } else {
                *last_followed.borrow_mut() = None;
            }
            if git_panel.is_open() {
                if let Some(cwd) = manager.borrow().active_cwd() {
                    if cwd.is_dir() && last_git_cwd.borrow().as_ref() != Some(&cwd) {
                        *last_git_cwd.borrow_mut() = Some(cwd.clone());
                        git_panel.open(&cwd);
                    }
                }
            } else {
                *last_git_cwd.borrow_mut() = None;
            }
            let records = manager.borrow().take_history_records();
            if !records.is_empty() {
                let mut index = history_index.borrow_mut();
                let mut persisted = Vec::with_capacity(records.len());
                for record in records {
                    persisted.push(termust_store::HistoryRecord {
                        text: record.text.clone(),
                        cwd: record.cwd.clone(),
                        timestamp: record.timestamp,
                        exit_code: record.exit_code,
                    });
                    index.insert(HistoryEntry::new(
                        record.text,
                        record.cwd,
                        record.timestamp,
                        record.exit_code,
                        HistorySource::Session,
                    ));
                }
                drop(index);
                if let Some(path) = history_store.as_deref() {
                    let _ = termust_store::append_history(path, &persisted);
                }
                history.refresh_if_open();
            }
            glib::ControlFlow::Continue
        });
    }

    if std::env::var_os("TERMUST_PANES_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(700), move || {
            let mut failed = false;
            let mut check = |label: &str, ok: bool| {
                eprintln!(
                    "termust-panes-test: {label} {}",
                    if ok { "ok" } else { "FAIL" }
                );
                failed |= !ok;
            };
            {
                let mut mgr = manager.borrow_mut();
                check("inicio 1 panel", mgr.active_pane_count() == 1);
                check(
                    "split horizontal",
                    mgr.split_active(SplitOrientation::Horizontal).is_ok(),
                );
                check("2 paneles", mgr.active_pane_count() == 2);
                check("foco izquierda", mgr.focus_direction(Direction::Left));
                check("foco derecha", mgr.focus_direction(Direction::Right));
                check(
                    "split vertical",
                    mgr.split_active(SplitOrientation::Vertical).is_ok(),
                );
                check("3 paneles", mgr.active_pane_count() == 3);
                check(
                    "split vertical 2",
                    mgr.split_active(SplitOrientation::Vertical).is_ok(),
                );
                check("4 paneles", mgr.active_pane_count() == 4);
                check(
                    "quinto panel rechazado",
                    mgr.split_active(SplitOrientation::Vertical).is_err(),
                );
                mgr.close_active();
                check("cierre 4->3", mgr.active_pane_count() == 3);
                mgr.close_active();
                check("cierre 3->2", mgr.active_pane_count() == 2);
                mgr.close_active();
                check("cierre 2->1", mgr.active_pane_count() == 1);
            }
            if failed {
                std::process::exit(1);
            }
            app.quit();
        });
    }

    if std::env::var_os("TERMUST_CWD_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            manager.borrow().send_active(b"cd /tmp\n");
            let manager2 = manager.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(900), move || {
                let mut failed = false;
                let mut check = |label: &str, ok: bool| {
                    eprintln!(
                        "termust-cwd-test: {label} {}",
                        if ok { "ok" } else { "FAIL" }
                    );
                    failed |= !ok;
                };
                let cwd = manager2.borrow().active_cwd();
                check(
                    "cwd /tmp antes del split",
                    cwd.as_ref().map(|p| p.ends_with("tmp")).unwrap_or(false),
                );
                {
                    let mut mgr = manager2.borrow_mut();
                    check(
                        "split hereda cwd",
                        mgr.split_active(SplitOrientation::Horizontal).is_ok(),
                    );
                    check("2 paneles", mgr.active_pane_count() == 2);
                    check(
                        "ratio inicial 0.5",
                        mgr.ratio(&[])
                            .map(|r| (r - 0.5).abs() < 0.01)
                            .unwrap_or(false),
                    );
                    mgr.resize_direction(Direction::Right);
                    check(
                        "resize a la derecha",
                        mgr.ratio(&[]).map(|r| r > 0.5).unwrap_or(false),
                    );
                    let active_before = mgr.active_pane();
                    check(
                        "preset cuadrícula 2x2",
                        mgr.apply_preset(Preset::Grid2x2).is_ok(),
                    );
                    check(
                        "preset conserva panel activo",
                        active_before.is_some() && active_before == mgr.active_pane(),
                    );
                    check("4 paneles tras preset", mgr.active_pane_count() == 4);
                    check(
                        "preset menor rechazado",
                        mgr.apply_preset(Preset::TwoColumns).is_err(),
                    );
                }
                let manager3 = manager2.clone();
                let app3 = app2.clone();
                glib::timeout_add_local_once(Duration::from_millis(2000), move || {
                    let mut failed = false;
                    let mut check = |label: &str, ok: bool| {
                        eprintln!(
                            "termust-cwd-test: {label} {}",
                            if ok { "ok" } else { "FAIL" }
                        );
                        failed |= !ok;
                    };
                    let cwd = manager3.borrow().active_cwd();
                    eprintln!("termust-cwd-test: cwd final = {cwd:?}");
                    check(
                        "panel nuevo en /tmp",
                        cwd.as_ref().map(|p| p.ends_with("tmp")).unwrap_or(false),
                    );
                    if failed {
                        std::process::exit(1);
                    }
                    app3.quit();
                });
                if failed {
                    std::process::exit(1);
                }
            });
        });
    }

    if std::env::var_os("TERMUST_CLOSE_CONFIRM_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(800), move || {
            {
                let mut mgr = manager.borrow_mut();
                let _ = mgr.split_active(SplitOrientation::Horizontal);
                mgr.send_active(b"sleep 30\n");
            }
            let manager2 = manager.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(800), move || {
                let before = manager2.borrow().active_pane_count();
                TabManager::close_request(&manager2);
                let manager3 = manager2.clone();
                let app3 = app2.clone();
                glib::timeout_add_local_once(Duration::from_millis(500), move || {
                    let mut failed = false;
                    let mut check = |label: &str, ok: bool| {
                        eprintln!(
                            "termust-close-confirm-test: {label} {}",
                            if ok { "ok" } else { "FAIL" }
                        );
                        failed |= !ok;
                    };
                    check(
                        "no cerró con foreground",
                        manager3.borrow().active_pane_count() == before,
                    );
                    {
                        let mut mgr = manager3.borrow_mut();
                        mgr.focus_direction(Direction::Left);
                    }
                    TabManager::close_request(&manager3);
                    let manager4 = manager3.clone();
                    let app4 = app3.clone();
                    glib::timeout_add_local_once(Duration::from_millis(500), move || {
                        let mut failed = false;
                        let mut check = |label: &str, ok: bool| {
                            eprintln!(
                                "termust-close-confirm-test: {label} {}",
                                if ok { "ok" } else { "FAIL" }
                            );
                            failed |= !ok;
                        };
                        check(
                            "cerró el panel inactivo",
                            manager4.borrow().active_pane_count() == 1,
                        );
                        if failed {
                            std::process::exit(1);
                        }
                        // Cerrar la pestaña (X) con foreground debe quedar en Stop
                        // y no cerrar hasta confirmar.
                        TabManager::close_request(&manager4);
                        let manager5 = manager4.clone();
                        let app5 = app4.clone();
                        glib::timeout_add_local_once(Duration::from_millis(400), move || {
                            let pages = manager5.borrow().tab_view().n_pages();
                            let ok = pages == 1;
                            eprintln!(
                                "termust-close-confirm-test: pestaña no cerró sin confirmar {}",
                                if ok { "ok" } else { "FAIL" }
                            );
                            if !ok {
                                std::process::exit(1);
                            }
                            app5.quit();
                        });
                    });
                    if failed {
                        std::process::exit(1);
                    }
                });
            });
        });
    }

    if std::env::var_os("TERMUST_HISTORY_TEST").is_some() {
        history_index.borrow_mut().insert(HistoryEntry::new(
            "echo HISTORY_OK",
            home.clone(),
            0,
            Some(0),
            HistorySource::Session,
        ));
        let manager = manager.clone();
        let history = history.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            let anchor = manager
                .borrow()
                .active_view()
                .map(|view| view.widget().clone());
            let Some(anchor) = anchor else {
                eprintln!("termust-history-test: FAIL sin vista");
                std::process::exit(1);
            };
            let inserted: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
            let captured = inserted.clone();
            let insert: Rc<dyn Fn(String)> =
                Rc::new(move |text| *captured.borrow_mut() = Some(text));
            history.open(&anchor, insert);
            let history2 = history.clone();
            let app2 = app.clone();
            let inserted2 = inserted.clone();
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                let open = history2.is_open();
                let rows = history2.row_count();
                history2.insert_selected();
                let text = inserted2.borrow().clone();
                // El historial real importado puede tener entradas antes de la
                // del test; lo relevante es que haya filas, que se inserte una
                // y que el diálogo se cierre.
                let ok = open
                    && rows >= 1
                    && text.as_deref().is_some_and(|text| !text.is_empty())
                    && !history2.is_open();
                eprintln!(
                    "termust-history-test: open={open} rows={rows} text={text:?} closed={} {}",
                    !history2.is_open(),
                    if ok { "ok" } else { "FAIL" }
                );
                if !ok {
                    std::process::exit(1);
                }
                app2.quit();
            });
        });
    }

    if std::env::var_os("TERMUST_HISTORY_READY_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(2500), move || {
            let initial = manager
                .borrow()
                .active_view()
                .map(|view| view.history_ready())
                .unwrap_or(false);
            manager.borrow().send_active(b"x");
            let manager2 = manager.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(400), move || {
                let typed = manager2
                    .borrow()
                    .active_view()
                    .map(|view| view.history_ready())
                    .unwrap_or(true);
                manager2.borrow().send_active(&[3]);
                let manager3 = manager2.clone();
                let app3 = app2.clone();
                glib::timeout_add_local_once(Duration::from_millis(800), move || {
                    let again = manager3
                        .borrow()
                        .active_view()
                        .map(|view| view.history_ready())
                        .unwrap_or(false);
                    let ok = initial && !typed && again;
                    eprintln!(
                        "termust-history-ready: inicial={initial} con_texto={} tras_ctrl_c={again} {}",
                        !typed,
                        if ok { "ok" } else { "FAIL" }
                    );
                    if !ok {
                        std::process::exit(1);
                    }
                    app3.quit();
                });
            });
        });
    }

    if std::env::var_os("TERMUST_HISTORY_PENDING_TEST").is_some() {
        let manager = manager.clone();
        let history = history.clone();
        let history_index = history_index.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(2000), move || {
            let anchor = manager
                .borrow()
                .active_view()
                .map(|view| view.widget().clone());
            let Some(anchor) = anchor else {
                eprintln!("termust-history-pending: FAIL sin vista");
                std::process::exit(1);
            };
            let insert: history::InsertFn = Rc::new(|_| {});
            history.open(&anchor, insert);
            manager.borrow().send_active(b"sleep 5\n");
            let history2 = history.clone();
            let history_index2 = history_index.clone();
            let manager2 = manager.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(1200), move || {
                let rows = history2.row_texts();
                let started = rows.first().is_some_and(|row| row == "sleep 5")
                    && history_index2
                        .borrow()
                        .search("sleep 5", 5)
                        .iter()
                        .any(|entry| entry.text == "sleep 5" && entry.exit_code.is_none());
                manager2.borrow().send_active(&[3]);
                let history3 = history2.clone();
                let history_index3 = history_index2.clone();
                let app3 = app2.clone();
                glib::timeout_add_local_once(Duration::from_millis(1200), move || {
                    let rows = history3.row_texts();
                    let finished = rows.first().is_some_and(|row| row == "sleep 5")
                        && history_index3
                            .borrow()
                            .search("sleep 5", 5)
                            .iter()
                            .any(|entry| entry.text == "sleep 5" && entry.exit_code.is_some());
                    let ok = started && finished;
                    eprintln!(
                        "termust-history-pending: started={started} finished={finished} {}",
                        if ok { "ok" } else { "FAIL" }
                    );
                    if !ok {
                        std::process::exit(1);
                    }
                    app3.quit();
                });
            });
        });
    }

    if std::env::var_os("TERMUST_TAB_TITLE_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1200), move || {
            manager.borrow().send_active(b"sleep 6\n");
            let manager = manager.clone();
            let app = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                let title = manager
                    .borrow()
                    .tab_summaries()
                    .first()
                    .map(|tab| tab.title.clone())
                    .unwrap_or_default();
                let ok = title.contains("· sleep");
                eprintln!(
                    "termust-tab-title: {title:?} {}",
                    if ok { "ok" } else { "FAIL" }
                );
                if !ok {
                    std::process::exit(1);
                }
                app.quit();
            });
        });
    }

    if std::env::var_os("TERMUST_HISTORY_CAPTURE_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        let history_index = history_index.clone();
        glib::timeout_add_local_once(Duration::from_millis(2500), move || {
            manager.borrow().send_active(b"echo HISTORY_REC\n");
            let history_index = history_index.clone();
            let app = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                let found = history_index
                    .borrow()
                    .search("HISTORY_REC", 5)
                    .iter()
                    .any(|entry| entry.text == "echo HISTORY_REC");
                eprintln!(
                    "termust-history-capture: encontrado={found} {}",
                    if found { "ok" } else { "FAIL" }
                );
                if !found {
                    std::process::exit(1);
                }
                app.quit();
            });
        });
    }

    if std::env::var_os("TERMUST_THEME_TEST").is_some() {
        let appearance = appearance.clone();
        let manager = manager.clone();
        let preferences = preferences.clone();
        let window = window.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            let before_theme = appearance.theme_id();
            let before_opacity = appearance.opacity();
            let before_scale = appearance.font_scale();
            appearance.set_theme_id(Some("catppuccin-mocha"));
            appearance.set_opacity(50);
            appearance.set_font_scale(1.5);
            let expected = appearance.theme("catppuccin-mocha").map(|theme| theme.fg);
            let gruvbox = appearance.theme("gruvbox-dark").map(|theme| theme.fg);
            let base = {
                let chosen = appearance.font_size();
                if chosen > 0.0 {
                    chosen
                } else {
                    appearance.base_font().size
                }
            };
            let high = appearance.high_contrast();
            let manager2 = manager.clone();
            let appearance2 = appearance.clone();
            let preferences2 = preferences.clone();
            let window2 = window.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(400), move || {
                let mut failed = false;
                let mut check = |label: &str, ok: bool| {
                    eprintln!(
                        "termust-theme-test: {label} {}",
                        if ok { "ok" } else { "FAIL" }
                    );
                    failed |= !ok;
                };
                manager2.borrow().refresh_appearance();
                let state = manager2
                    .borrow()
                    .active_view()
                    .map(|view| (view.theme_fg(), view.bg_alpha(), view.font_size()));
                let Some((fg, alpha, size)) = state else {
                    eprintln!("termust-theme-test: sin vista FAIL");
                    std::process::exit(1);
                };
                check("tema global aplicado", Some(fg) == expected);
                check(
                    "opacidad solo de fondo",
                    if high {
                        alpha == 1.0
                    } else {
                        (alpha - 0.5).abs() < 0.01
                    },
                );
                check("zoom aplicado", (size - base * 1.5).abs() < 0.01);
                check(
                    "ventana transparente",
                    window2.has_css_class("termust-transparent") == !high,
                );

                manager2
                    .borrow_mut()
                    .set_active_tab_theme(Some("gruvbox-dark".to_string()));
                let tab_fg = manager2.borrow().active_view().map(|view| view.theme_fg());
                check("tema por pestaña", tab_fg == gruvbox);
                manager2.borrow_mut().set_active_tab_theme(None);
                let back_fg = manager2.borrow().active_view().map(|view| view.theme_fg());
                check("vuelve al tema global", back_fg == expected);

                preferences2.sync_now();
                check(
                    "preferencias listan el catálogo",
                    preferences2.theme_count() >= 30,
                );
                check(
                    "preferencias reflejan opacidad",
                    (preferences2.opacity_value() - 50.0).abs() < 0.01,
                );
                check(
                    "preferencias reflejan zoom",
                    (preferences2.zoom_value() - 1.5).abs() < 0.01,
                );
                check(
                    "transparencia según alto contraste",
                    preferences2.transparency_sensitive() == !high,
                );
                appearance2.set_opacity(90);
                preferences2.sync_now();
                check(
                    "aviso de contraste con transparencia",
                    if high {
                        !preferences2.warning_visible()
                    } else {
                        preferences2.warning_visible()
                    },
                );

                let persisted = gtk::gio::SettingsSchemaSource::default()
                    .filter(|source| source.lookup(termust_core::APP_ID, true).is_some())
                    .map(|_| gtk::gio::Settings::new(termust_core::APP_ID))
                    .filter(|settings| {
                        settings
                            .settings_schema()
                            .map(|schema| schema.has_key("theme"))
                            .unwrap_or(false)
                    });
                if let Some(settings) = persisted {
                    check(
                        "tema persistido en GSettings",
                        settings.string("theme") == "catppuccin-mocha",
                    );
                    check(
                        "zoom persistido en GSettings",
                        (settings.double("font-scale") - 1.5).abs() < 0.01,
                    );
                }

                appearance2.set_theme_id(if before_theme == AUTO_THEME {
                    None
                } else {
                    Some(before_theme.as_str())
                });
                appearance2.set_opacity(before_opacity);
                appearance2.set_font_scale(before_scale);
                manager2.borrow().refresh_appearance();
                eprintln!("termust-theme-test: {}", if failed { "FAIL" } else { "ok" });
                if failed {
                    std::process::exit(1);
                }
                app2.quit();
            });
        });
    }

    if std::env::var_os("TERMUST_OPACITY_COST_TEST").is_some() {
        let appearance = appearance.clone();
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1000), move || {
            let before = appearance.opacity();
            let high = appearance.high_contrast();
            let mut failed = false;
            let mut results = Vec::new();
            for percent in [0, 50, 100] {
                appearance.set_opacity(percent);
                manager.borrow().refresh_appearance();
                let sample = {
                    let manager = manager.borrow();
                    manager.active_view().map(|view| {
                        let per_frame = view.render_benchmark(800, 600, 40);
                        (per_frame, view.bg_alpha())
                    })
                };
                let Some((per_frame, alpha)) = sample else {
                    eprintln!("termust-opacity-cost: sin vista FAIL");
                    std::process::exit(1);
                };
                let expected = if high {
                    1.0
                } else {
                    termust_core::appearance::bg_alpha(percent)
                };
                let alpha_ok = (alpha - expected).abs() < 0.01;
                failed |= !alpha_ok;
                results.push((percent, per_frame, alpha));
            }
            for (percent, per_frame, alpha) in &results {
                eprintln!(
                    "termust-opacity-cost: opacidad={percent}% cuadro={:.2} ms alpha={alpha:.2} rss={} kB",
                    per_frame.as_secs_f64() * 1000.0,
                    rss_kb()
                );
            }
            for (_, per_frame, _) in &results {
                if per_frame.as_secs_f64() * 1000.0 > 100.0 {
                    failed = true;
                }
            }
            appearance.set_opacity(before);
            manager.borrow().refresh_appearance();
            eprintln!(
                "termust-opacity-cost: {}",
                if failed { "FAIL" } else { "ok" }
            );
            if failed {
                std::process::exit(1);
            }
            app.quit();
        });
    }

    if std::env::var_os("TERMUST_PREFS_TEST").is_some() {
        let preferences = preferences.clone();
        let window = window.clone();
        let app = app.clone();
        let manager = manager.clone();
        let appearance = appearance.clone();
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            preferences.present(&window);
            let preferences2 = preferences.clone();
            let app2 = app.clone();
            let manager2 = manager.clone();
            let appearance2 = appearance.clone();
            glib::timeout_add_local_once(Duration::from_millis(700), move || {
                let visible = preferences2.is_visible();
                eprintln!(
                    "termust-prefs-test: visible={visible} temas={}",
                    preferences2.theme_count()
                );
                let mut failed = !visible;
                let mut check = |label: &str, ok: bool| {
                    eprintln!(
                        "termust-prefs-test: {label} {}",
                        if ok { "ok" } else { "FAIL" }
                    );
                    failed |= !ok;
                };
                let before_family = preferences2.font_family();
                let before_size = preferences2.font_size();
                let scale = appearance2.font_scale();
                preferences2.set_font_size_value(17.0);
                let size_ok = manager2
                    .borrow()
                    .active_view()
                    .map(|view| (view.font_size() - 17.0 * scale).abs() < 0.01)
                    .unwrap_or(false);
                check(
                    "tamaño del terminal",
                    size_ok && (preferences2.font_size() - 17.0).abs() < 0.001,
                );

                let family = preferences2
                    .font_family_names()
                    .into_iter()
                    .find(|name| !name.is_empty());
                let family_ok = match &family {
                    Some(family) => {
                        preferences2.select_font_family(family);
                        manager2
                            .borrow()
                            .active_view()
                            .map(|view| view.font_family() == *family)
                            .unwrap_or(false)
                    }
                    None => true,
                };
                check("familia del terminal", family_ok);

                if let Some(settings) = gsettings_with_key("font-size") {
                    check(
                        "font-size persiste",
                        (settings.double("font-size") - 17.0).abs() < 0.001,
                    );
                }
                if let Some(settings) = gsettings_with_key("font-family") {
                    if let Some(family) = &family {
                        check(
                            "font-family persiste",
                            settings.string("font-family").as_str() == family.as_str(),
                        );
                    }
                }

                appearance2.set_font_family(&before_family);
                appearance2.set_font_size(before_size);
                preferences2.sync_now();
                preferences2.close();
                eprintln!("termust-prefs-test: {}", if failed { "FAIL" } else { "ok" });
                if failed {
                    std::process::exit(1);
                }
                app2.quit();
            });
        });
    }

    if std::env::var_os("TERMUST_FILES_TEST").is_some() {
        let manager = manager.clone();
        let file_panel = file_panel.clone();
        let files_button = files_button.clone();
        let files_split = files_split.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            let mut failed = false;
            let mut check = |label: &str, ok: bool| {
                eprintln!(
                    "termust-files-test: {label} {}",
                    if ok { "ok" } else { "FAIL" }
                );
                failed |= !ok;
            };
            let root = std::env::temp_dir().join(format!("termust-m8-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("a-dir")).unwrap();
            std::fs::create_dir_all(root.join("big")).unwrap();
            std::fs::create_dir_all(root.join("locked")).unwrap();
            std::fs::set_permissions(
                root.join("locked"),
                std::os::unix::fs::PermissionsExt::from_mode(0o000),
            )
            .unwrap();
            std::fs::write(root.join("a-dir/nested.txt"), b"nested").unwrap();
            std::fs::write(root.join("b.txt"), b"b").unwrap();
            std::fs::write(root.join("c.rs"), b"fn main() {}").unwrap();
            std::fs::write(root.join(".hidden"), b"h").unwrap();
            let _ = std::os::unix::fs::symlink("/ruta/que/no/existe", root.join("roto"));
            for index in 0..10_000 {
                std::fs::write(root.join(format!("big/f-{index:05}.txt")), b"").unwrap();
            }

            files_button.set_active(true);
            check("panel visible", files_split.shows_sidebar());
            file_panel.set_root(&root);
            let names = file_panel.visible_names();
            check(
                "tree_sorted_folders_first",
                names == vec!["a-dir", "big", "locked", "b.txt", "c.rs", "roto"],
            );
            let panel_settings = gsettings_with_key("files-panel");
            check(
                "files_panel_toggle_persists",
                panel_settings
                    .as_ref()
                    .map(|settings| settings.boolean("files-panel"))
                    .unwrap_or(true),
            );

            file_panel.set_search("C.R");
            check(
                "files_search_typeahead",
                file_panel.visible_names() == vec!["c.rs"],
            );
            file_panel.set_search("");
            check("filtro limpiado", file_panel.rows() == 6);

            check(
                "roto no navegable",
                file_panel.name_is_navigable("roto") == Some(false),
            );
            let before = file_panel.current();
            check("activar roto no navega", !file_panel.activate_root("roto"));
            check(
                "files_broken_symlink_is_inert",
                file_panel.current() == before,
            );
            check(
                "carpeta sin permisos es fila inerte",
                file_panel.name_is_navigable("locked") == Some(false)
                    && !file_panel.activate_root("locked")
                    && file_panel.current() == before,
            );
            check("subir visible", file_panel.up_visible());

            let rows_before = file_panel.visible_names().len();
            check("expandir carpeta", file_panel.expand_root("a-dir"));
            check(
                "árbol lazy carga hijos al expandir",
                file_panel.visible_names().len() > rows_before
                    && file_panel.visible_names().iter().any(|n| n == "nested.txt"),
            );

            file_panel.set_show_hidden(true);
            check(
                "archivos ocultos visibles",
                file_panel.visible_names().iter().any(|n| n == ".hidden"),
            );
            file_panel.set_show_hidden(false);

            std::fs::create_dir(root.join("vacia")).unwrap();
            file_panel.set_root(&root.join("vacia"));
            check(
                "carpeta vacía muestra estado",
                file_panel.is_showing_status() && file_panel.status_title() == "Carpeta vacía",
            );

            file_panel.set_root(&root.join("big"));
            check("subir visible en subcarpeta", file_panel.up_visible());
            let load_ms = file_panel.load_ms();
            eprintln!(
                "termust-files-test: 10k filas={} en {load_ms:.1} ms (objetivo 150 / duro 400)",
                file_panel.rows()
            );
            check(
                "tree_10k_entries_within_budget",
                file_panel.rows() == 10_000 && load_ms < 400.0,
            );

            file_panel.set_root(&root);
            manager.borrow().send_active(b"cd /tmp\n");
            let manager2 = manager.clone();
            let file_panel2 = file_panel.clone();
            let files_button2 = files_button.clone();
            let root2 = root.clone();
            let app2 = app.clone();
            let mut failed2 = failed;
            glib::timeout_add_local_once(Duration::from_millis(1600), move || {
                let mut check = |label: &str, ok: bool| {
                    eprintln!(
                        "termust-files-test: {label} {}",
                        if ok { "ok" } else { "FAIL" }
                    );
                    failed2 |= !ok;
                };
                check(
                    "files_follow_active_pane",
                    file_panel2.current().ends_with("tmp"),
                );
                file_panel2.set_follow(false);
                manager2.borrow().send_active(b"cd /\n");
                let file_panel3 = file_panel2.clone();
                let files_button3 = files_button2.clone();
                let root3 = root2.clone();
                let app3 = app2.clone();
                let mut failed3 = failed2;
                glib::timeout_add_local_once(Duration::from_millis(1600), move || {
                    let mut check = |label: &str, ok: bool| {
                        eprintln!(
                            "termust-files-test: {label} {}",
                            if ok { "ok" } else { "FAIL" }
                        );
                        failed3 |= !ok;
                    };
                    check(
                        "follow desactivado no navega",
                        file_panel3.current().ends_with("tmp"),
                    );
                    files_button3.set_active(false);
                    let panel_settings = gsettings_with_key("files-panel");
                    check(
                        "panel oculto",
                        panel_settings
                            .as_ref()
                            .map(|settings| !settings.boolean("files-panel"))
                            .unwrap_or(true),
                    );
                    files_button3.set_active(true);
                    check(
                        "panel reabierto",
                        panel_settings
                            .as_ref()
                            .map(|settings| settings.boolean("files-panel"))
                            .unwrap_or(true),
                    );

                    file_panel3.set_root(&root3);
                    {
                        // GIO establece el watch de forma asíncrona; se espera
                        // antes de crear el archivo para probar la notificación.
                        let root_watch = root3.clone();
                        glib::timeout_add_local_once(Duration::from_millis(350), move || {
                            let _ = std::fs::write(root_watch.join("watch-now.txt"), b"new");
                        });
                    }
                    let app4 = app3.clone();
                    let file_panel4 = file_panel3.clone();
                    let files_button4 = files_button3.clone();
                    let root4 = root3.clone();
                    let mut failed4 = failed3;
                    glib::timeout_add_local_once(Duration::from_millis(900), move || {
                        let mut check = |label: &str, ok: bool| {
                            eprintln!(
                                "termust-files-test: {label} {}",
                                if ok { "ok" } else { "FAIL" }
                            );
                            failed4 |= !ok;
                        };
                        check(
                            "watch refresca la carpeta",
                            file_panel4
                                .visible_names()
                                .iter()
                                .any(|name| name == "watch-now.txt"),
                        );
                        file_panel4.set_follow(true);
                        files_button4.set_active(false);
                        let _ = std::fs::set_permissions(
                            root4.join("locked"),
                            std::os::unix::fs::PermissionsExt::from_mode(0o700),
                        );
                        let _ = std::fs::remove_dir_all(&root4);
                        eprintln!(
                            "termust-files-test: {}",
                            if failed4 { "FAIL" } else { "ok" }
                        );
                        if failed4 {
                            std::process::exit(1);
                        }
                        app4.quit();
                    });
                });
            });
        });
    }

    if std::env::var_os("TERMUST_VERTICAL_TABS_TEST").is_some() {
        let manager = manager.clone();
        let window = window.clone();
        let tabs_button = tabs_button.clone();
        let tab_sidebar = tab_sidebar.clone();
        let tab_bar = tab_bar.clone();
        let outer_split = outer_split.clone();
        let home = home.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            let mut failed = false;
            let mut check = |label: &str, ok: bool| {
                eprintln!(
                    "termust-vertical-tabs: {label} {}",
                    if ok { "ok" } else { "FAIL" }
                );
                failed |= !ok;
            };
            {
                let mut manager = manager.borrow_mut();
                manager.rename_tab("vertical");
                let _ = manager.split_active(SplitOrientation::Horizontal);
                let _ = manager.open_tab(home.clone());
                manager.rename_tab("otra");
                let _ = manager.split_active(SplitOrientation::Horizontal);
            }
            let first_id = manager.borrow().tab_summaries()[0].id;
            let second_id = manager.borrow().tab_summaries()[1].id;
            manager.borrow_mut().select_tab(first_id);
            check(
                "selección por id actualiza el modelo",
                manager.borrow().active_tab_id() == Some(first_id),
            );
            let focused = gtk::prelude::GtkWindowExt::focus(&window).and_then(|widget| {
                manager
                    .borrow()
                    .active_view()
                    .map(|view| view.widget() == &widget)
            });
            check("selección enfoca el terminal", focused == Some(true));

            let tabs = manager.borrow().tab_count();
            let panes = manager.borrow().active_pane_count();
            let cwd = manager.borrow().active_cwd();
            check("2 paneles antes", panes == 2);
            check("dos pestañas", tabs == 2);

            tabs_button.set_active(true);
            tab_sidebar.refresh();
            check(
                "sidebar visible",
                outer_split.shows_sidebar() && !tab_bar.is_visible(),
            );
            check(
                "una fila por pestaña",
                tab_sidebar.row_count() == tabs && tabs >= 1,
            );
            check(
                "vertical↔horizontal no pierde paneles",
                manager.borrow().tab_count() == tabs
                    && manager.borrow().active_pane_count() == panes
                    && manager.borrow().active_cwd() == cwd,
            );
            check(
                "vertical-tabs persistido",
                gsettings_with_key("vertical-tabs")
                    .as_ref()
                    .map(|settings| settings.boolean("vertical-tabs"))
                    .unwrap_or(true),
            );

            tabs_button.set_active(false);
            check(
                "vuelve la barra horizontal",
                !outer_split.shows_sidebar() && tab_bar.is_visible(),
            );
            check(
                "horizontal conserva paneles",
                manager.borrow().tab_count() == tabs
                    && manager.borrow().active_pane_count() == panes
                    && manager.borrow().active_cwd() == cwd,
            );
            TabManager::close_tab(&manager, second_id);
            let manager2 = manager.clone();
            let app2 = app.clone();
            let mut failed2 = failed;
            glib::timeout_add_local_once(Duration::from_millis(400), move || {
                let mut check = |label: &str, ok: bool| {
                    eprintln!(
                        "termust-vertical-tabs: {label} {}",
                        if ok { "ok" } else { "FAIL" }
                    );
                    failed2 |= !ok;
                };
                check(
                    "cerrar por id cierra la pestaña completa",
                    manager2.borrow().tab_count() == 1
                        && manager2.borrow().active_tab_id() == Some(first_id),
                );
                eprintln!(
                    "termust-vertical-tabs: {}",
                    if failed2 { "FAIL" } else { "ok" }
                );
                if failed2 {
                    std::process::exit(1);
                }
                app2.quit();
            });
        });
    }

    if std::env::var_os("TERMUST_VIEWER_TEST").is_some() {
        let viewer = viewer.clone();
        let file_panel = file_panel.clone();
        let files_button = files_button.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            let root = std::env::temp_dir().join(format!("termust-viewer-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let rust = "struct App {\n    name: String,\n}\n\nimpl App {\n    fn new() -> Self {\n        Self { name: String::new() }\n    }\n}\n\nfn main() {\n    let app = App::new();\n    println!(\"{}\", app.name);\n}\n";
            std::fs::write(root.join("main.rs"), rust).unwrap();
            std::fs::write(
                root.join("widget.vue"),
                "<template><div>{{ msg }}</div></template>\n<script>\nconst msg = 1;\nfunction hi() { return msg; }\n</script>\n",
            )
            .unwrap();
            std::fs::write(root.join("data.bin"), [0u8, 159, 146, 150, 0, 1]).unwrap();
            std::fs::write(root.join("large.txt"), vec![b'a'; 2 * 1024 * 1024 + 10]).unwrap();
            std::fs::write(root.join("latin1.txt"), b"hola \xff mundo\n").unwrap();
            let hash_before = file_hash(&root.join("main.rs"));

            let mut failed = false;
            let mut check = |label: &str, ok: bool| {
                eprintln!(
                    "termust-viewer-test: {label} {}",
                    if ok { "ok" } else { "FAIL" }
                );
                failed |= !ok;
            };
            files_button.set_active(true);
            file_panel.set_root(&root);
            check(
                "el panel abre el archivo en el visor",
                file_panel.activate_root("main.rs") && viewer.is_open(),
            );
            check("muestra texto", viewer.is_showing_text());
            check(
                "lee el contenido",
                viewer.buffer_text().contains("struct App") && viewer.line_count() > 5,
            );
            check("lenguaje detectado", viewer.language_label() == "Rust");
            check(
                "ruta abierta",
                viewer.path().as_deref() == Some(root.join("main.rs").as_path()),
            );
            check("solo lectura", !viewer.is_editable());
            check("números de línea", viewer.line_count() > 0);

            let viewer2 = viewer.clone();
            let root2 = root.clone();
            let app2 = app.clone();
            let mut failed2 = failed;
            glib::timeout_add_local_once(Duration::from_millis(800), move || {
                let mut check = |label: &str, ok: bool| {
                    eprintln!(
                        "termust-viewer-test: {label} {}",
                        if ok { "ok" } else { "FAIL" }
                    );
                    failed2 |= !ok;
                };
                check("resaltado del viewport", viewer2.highlight_count() > 0);
                check("folds calculados", viewer2.fold_count() > 0);
                check("outline calculado", viewer2.outline_count() > 0);
                let fold_line = viewer2.first_fold_line().unwrap_or(0);
                viewer2.toggle_fold_at_line(fold_line);
                check("plegar bloque", viewer2.folded_count() == 1);
                viewer2.toggle_fold_at_line(fold_line);
                check("desplegar bloque", viewer2.folded_count() == 0);
                check("búsqueda", viewer2.find("println"));
                check(
                    "búsqueda selecciona",
                    viewer2.selection_text().as_deref() == Some("println"),
                );
                let _buffer_changed = viewer2.try_insert_for_test("HACK");
                check(
                    "viewer_never_writes_file",
                    file_hash(&root2.join("main.rs")) == hash_before,
                );

                viewer2.open_file(&root2.join("data.bin"));
                check("viewer_binary_status_page", viewer2.is_showing_status());
                viewer2.open_file(&root2.join("large.txt"));
                check("archivo grande avisa", viewer2.banner_visible());
                viewer2.open_file(&root2.join("latin1.txt"));
                check("no UTF-8 avisa", viewer2.banner_visible());
                viewer2.open_file(&root2.join("widget.vue"));
                check("Vue detectado", viewer2.language_label().contains("Vue"));

                let viewer3 = viewer2.clone();
                let root3 = root2.clone();
                let app3 = app2.clone();
                let mut failed3 = failed2;
                glib::timeout_add_local_once(Duration::from_millis(700), move || {
                    let mut check = |label: &str, ok: bool| {
                        eprintln!(
                            "termust-viewer-test: {label} {}",
                            if ok { "ok" } else { "FAIL" }
                        );
                        failed3 |= !ok;
                    };
                    check("Vue resaltado por inyección", viewer3.highlight_count() > 0);
                    check(
                        "pagina archivo",
                        viewer3.is_showing_file_page() && !viewer3.is_showing_diff(),
                    );
                    let missing = root3.join("no-existe.rs");
                    viewer3.open_file(&missing);
                    check(
                        "ruta en error",
                        viewer3.path().as_deref() == Some(missing.as_path()),
                    );
                    check("estado en error", viewer3.is_showing_status());
                    viewer3.show_diff_page();
                    check("pagina diff", viewer3.is_showing_diff());
                    viewer3.show_file_page();
                    check(
                        "sin texto viejo",
                        viewer3.is_showing_status() && !viewer3.is_showing_text(),
                    );
                    check("sin buffer viejo", viewer3.buffer_text().is_empty());
                    viewer3.close();
                    check("cierra el visor", !viewer3.is_open());
                    let _ = std::fs::remove_dir_all(&root3);
                    eprintln!(
                        "termust-viewer-test: {}",
                        if failed3 { "FAIL" } else { "ok" }
                    );
                    if failed3 {
                        std::process::exit(1);
                    }
                    app3.quit();
                });
            });
        });
    }

    if std::env::var_os("TERMUST_FILECOLUMN_TEST").is_some() {
        let natural_narrow = std::env::var("TERMUST_FILECOLUMN_TEST")
            .map(|mode| mode == "narrow")
            .unwrap_or(false);
        let app = app.clone();
        let viewer = viewer.clone();
        let diff_view = diff_view.clone();
        let file_split = file_split.clone();
        let files_split = files_split.clone();
        let files_button = files_button.clone();
        let file_panel = file_panel.clone();
        let manager = manager.clone();
        let window = window.clone();
        glib::timeout_add_local_once(Duration::from_millis(700), move || {
            let root =
                std::env::temp_dir().join(format!("termust-filecolumn-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let git = |args: &[&str]| {
                let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
                termust_host_exec::host_command("git", &args, Some(&root))
                    .output()
                    .unwrap()
            };
            git(&["init", "-q"]);
            git(&["config", "user.email", "t@t"]);
            git(&["config", "user.name", "t"]);
            std::fs::write(root.join("demo.rs"), "fn main() {}\n").unwrap();
            std::fs::write(root.join("demo2.rs"), "fn other() {}\n").unwrap();
            git(&["add", "demo.rs"]);
            git(&["commit", "-qm", "init"]);
            std::fs::write(
                root.join("demo.rs"),
                "fn main() {\n    println!(\"hola\");\n}\n",
            )
            .unwrap();

            let mut failed = false;
            let mut check = |label: &str, ok: bool| {
                eprintln!(
                    "termust-filecolumn-test: {label} {}",
                    if ok { "ok" } else { "FAIL" }
                );
                failed |= !ok;
            };
            // El arranque puede traer F9 activo desde GSettings; en angosto el
            // breakpoint colapsa y oculta el sidebar. Forzar el ciclo visible.
            files_button.set_active(false);
            files_button.set_active(true);
            let _ = file_panel.set_root(&root);
            check("arbol visible", files_split.shows_sidebar());
            let opened = file_panel.activate_root("demo.rs");
            check("archivo abierto", opened && viewer.is_open());
            check("columna visible", file_split.shows_sidebar());
            check("terminal visible", manager.borrow().tab_view().width() > 0);
            check(
                "arranca en archivo",
                viewer.is_showing_file_page() && !viewer.is_showing_diff(),
            );
            viewer.show_diff_page();

            let app2 = app.clone();
            let viewer2 = viewer.clone();
            let diff_view2 = diff_view.clone();
            let file_split2 = file_split.clone();
            let files_split2 = files_split.clone();
            let files_button2 = files_button.clone();
            let file_panel2 = file_panel.clone();
            let root2 = root.clone();
            glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                let mut failed2 = failed;
                let mut check = |label: &str, ok: bool| {
                    eprintln!(
                        "termust-filecolumn-test: {label} {}",
                        if ok { "ok" } else { "FAIL" }
                    );
                    failed2 |= !ok;
                };
                check(
                    "diff cargado",
                    diff_view2.line_count() > 0 && diff_view2.diff_text().contains("+"),
                );
                viewer2.close();
                check("columna oculta", !file_split2.shows_sidebar());
                let focused = gtk::prelude::GtkWindowExt::focus(&window)
                    .map(|widget| widget.type_().name().to_string())
                    .unwrap_or_default();
                check("foco terminal", focused.contains("GtkDrawingArea"));

                if natural_narrow {
                    check("breakpoint angosto", file_split2.is_collapsed());
                }
                file_split2.set_collapsed(true);
                check("angosto simulado", file_split2.is_collapsed());
                files_button2.set_active(true);
                check("arbol visible angosto", files_split2.shows_sidebar());
                let before = gsettings_with_key("files-panel")
                    .map(|settings| settings.boolean("files-panel"))
                    .unwrap_or(false);
                let _ = file_panel2.set_root(&root2);
                check(
                    "demo2 listado",
                    file_panel2
                        .visible_names()
                        .iter()
                        .any(|name| name == "demo2.rs"),
                );
                let opened2 = file_panel2.activate_root("demo2.rs");
                check("segundo archivo", opened2 && viewer2.is_open());
                check("angosto oculta arbol", !files_split2.shows_sidebar());
                check("columna visible angosto", file_split2.shows_sidebar());
                check(
                    "preferencia intacta",
                    gsettings_with_key("files-panel")
                        .map(|settings| settings.boolean("files-panel"))
                        .unwrap_or(false)
                        == before,
                );

                let _ = std::fs::remove_dir_all(&root2);
                eprintln!(
                    "termust-filecolumn-test: {}",
                    if failed2 { "FAIL" } else { "ok" }
                );
                if failed2 {
                    std::process::exit(1);
                }
                app2.quit();
            });
        });
    }

    if std::env::var_os("TERMUST_SCREENSHOT_TEST").is_some() {
        let window = window.clone();
        let manager = manager.clone();
        let viewer = viewer.clone();
        let file_split = file_split.clone();
        let files_button = files_button.clone();
        let file_panel = file_panel.clone();
        let preferences = preferences.clone();
        let history = history.clone();
        let agent_bar = agent_bar.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1200), move || {
            let _ = std::fs::create_dir_all("data/screenshots");
            let repo = std::env::current_dir().unwrap_or_default();
            let cwd = std::env::temp_dir().join("opencode").join("proyecto");
            let _ = std::fs::remove_dir_all(&cwd);
            std::fs::create_dir_all(&cwd).unwrap();
            for dir in ["app", "crates", "data", "flatpak", "scripts"] {
                let _ = std::fs::create_dir_all(cwd.join(dir));
            }
            for file in [
                "CHANGELOG.md",
                "README.md",
                "Cargo.toml",
                "LICENSE",
                "NOTICE",
                "rust-toolchain.toml",
            ] {
                let _ = std::fs::copy(repo.join(file), cwd.join(file));
            }
            agent_bar.set_opacity(0.0);
            files_button.set_active(true);
            file_panel.set_follow(false);
            let _ = file_panel.set_root(&cwd);
            viewer.open_file(&cwd.join("CHANGELOG.md"));
            file_split.set_show_sidebar(true);
            let command = format!(
                "PS1='\\[\\e[1;32m\\]usuario@equipo\\[\\e[0m\\]:\\[\\e[1;34m\\]~/proyecto\\[\\e[0m\\]$ '; cd '{}'; clear\n",
                cwd.display()
            );
            manager.borrow().send_active(command.as_bytes());

            let window2 = window.clone();
            let preferences2 = preferences.clone();
            let manager2 = manager.clone();
            let history2 = history.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(1600), move || {
                let main_ok =
                    screenshot::save_widget_png(&window2, "data/screenshots/terminal.png");
                eprintln!("termust-screenshot: terminal={main_ok}");
                preferences2.present(&window2);

                let preferences3 = preferences2.clone();
                let manager3 = manager2.clone();
                let history3 = history2.clone();
                let app3 = app2.clone();
                glib::timeout_add_local_once(Duration::from_millis(900), move || {
                    let prefs_ok = screenshot::save_widget_png(
                        preferences3.dialog(),
                        "data/screenshots/preferences.png",
                    );
                    eprintln!("termust-screenshot: preferences={prefs_ok}");
                    preferences3.close();

                    let anchor = manager3
                        .borrow()
                        .active_view()
                        .map(|view| view.widget().clone());
                    let Some(anchor) = anchor else {
                        eprintln!("termust-screenshot: sin terminal FAIL");
                        app3.quit();
                        return;
                    };
                    history3.seed_demo(&[
                        "cargo build --release",
                        "git status --short",
                        "cargo test --workspace",
                        "git log --oneline -5",
                        "btop",
                    ]);
                    history3.open(&anchor, Rc::new(|_| {}));
                    let history4 = history3.clone();
                    let app4 = app3.clone();
                    glib::timeout_add_local_once(Duration::from_millis(700), move || {
                        let history_ok = screenshot::save_widget_png(
                            history4.dialog(),
                            "data/screenshots/history.png",
                        );
                        eprintln!("termust-screenshot: history={history_ok}");
                        history4.close();
                        app4.quit();
                    });
                });
            });
        });
    }

    if std::env::var_os("TERMUST_MARKDOWN_TEST").is_some() {
        let viewer = viewer.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            let root = std::env::temp_dir().join(format!("termust-md-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let icon = PathBuf::from("data/icons/hicolor/64x64/apps/io.github.gac982.Termust.png");
            let _ = std::fs::copy(&icon, root.join("icono.png"));
            let md = "\
# Título principal

Un párrafo con **negrita**, *itálica*, `código` y un [link](https://example.com).

## Lista

- uno
- dos
  - anidado

> una cita

```rust
fn main() {}
```

| Col | Valor |
| --- | --- |
| a | 1 |
| b | 2 |

![logo](icono.png)
";
            std::fs::write(root.join("guia.md"), md).unwrap();
            viewer.open_file(&root.join("guia.md"));
            let viewer2 = viewer.clone();
            let root2 = root.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                let mut failed = false;
                let mut check = |label: &str, ok: bool| {
                    eprintln!(
                        "termust-markdown-test: {label} {}",
                        if ok { "ok" } else { "FAIL" }
                    );
                    failed |= !ok;
                };
                let counts = viewer2.markdown_counts();
                let text = viewer2.markdown_text();
                check("abre renderizado", viewer2.is_showing_rendered());
                check(
                    "documento sin marcas",
                    text.contains("Título principal") && !text.contains("# Título principal"),
                );
                check("títulos", counts.headings >= 2);
                check("tabla", counts.tables == 1);
                check("link", counts.links >= 1);
                check("imagen", counts.images == 1);
                check("código", counts.code_blocks >= 1);
                check("editor externo visible", viewer2.external_editor_visible());
                viewer2.show_source_page();
                check(
                    "fuente disponible",
                    viewer2.is_showing_text()
                        && viewer2.buffer_text().contains("# Título principal"),
                );
                check(
                    "editor externo en fuente",
                    viewer2.external_editor_visible(),
                );
                viewer2.show_preview_page();
                check("vuelve a vista", viewer2.is_showing_rendered());
                let _ = std::fs::remove_dir_all(&root2);
                eprintln!(
                    "termust-markdown-test: {}",
                    if failed { "FAIL" } else { "ok" }
                );
                if failed {
                    std::process::exit(1);
                }
                app2.quit();
            });
        });
    }

    if std::env::var_os("TERMUST_RESIZE_TEST").is_some() {
        let manager = manager.clone();
        let viewer = viewer.clone();
        let file_split = file_split.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(3500), move || {
            manager.borrow().send_active(
                b"PS1='AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA> '; clear\n",
            );
            let manager2 = manager.clone();
            let viewer2 = viewer.clone();
            let file_split2 = file_split.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(700), move || {
                let before_count = manager2
                    .borrow()
                    .active_view()
                    .map(|view| view.resize_count())
                    .unwrap_or(0);
                let before = manager2
                    .borrow()
                    .active_view()
                    .map(|view| view.recent_lines(6))
                    .unwrap_or_default();
                viewer2.open_file(std::path::Path::new("Cargo.toml"));
                file_split2.set_show_sidebar(true);
                let manager3 = manager2.clone();
                let app3 = app2.clone();
                glib::timeout_add_local_once(Duration::from_millis(900), move || {
                    let after_count = manager3
                        .borrow()
                        .active_view()
                        .map(|view| view.resize_count())
                        .unwrap_or(0);
                    let after = manager3
                        .borrow()
                        .active_view()
                        .map(|view| view.recent_lines(6))
                        .unwrap_or_default();
                    let expected = format!("{}>", "A".repeat(70));
                    let joined = after.join("");
                    let delta = after_count.saturating_sub(before_count);
                    let ok = delta <= 3 && joined == expected && before.join("").ends_with('>');
                    eprintln!(
                        "termust-resize-test: resizes={delta} lineas={:?} ok={}",
                        after, ok
                    );
                    if !ok {
                        eprintln!("termust-resize-test: esperado={expected:?} obtenido={joined:?}");
                        std::process::exit(1);
                    }
                    app3.quit();
                });
            });
        });
    }

    if std::env::var_os("TERMUST_DIFFVIEW_TEST").is_some() {
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(600), move || {
            let root =
                std::env::temp_dir().join(format!("termust-diffview-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let git = |args: &[&str]| {
                let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
                termust_host_exec::host_command("git", &args, Some(&root))
                    .output()
                    .unwrap()
            };
            git(&["init", "-q"]);
            git(&["config", "user.email", "t@t"]);
            git(&["config", "user.name", "t"]);
            std::fs::write(root.join("demo.txt"), "uno\n").unwrap();
            git(&["add", "demo.txt"]);
            git(&["commit", "-qm", "init"]);
            std::fs::write(root.join("demo.txt"), "uno\ndos\n").unwrap();

            let view = diff_view::DiffView::new();
            view.load(&root.join("demo.txt"));
            let view2 = view.clone();
            let root2 = root.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(1200), move || {
                let mut ok = !view2.is_loading()
                    && view2.line_count() >= 2
                    && view2.diff_text().contains("+dos")
                    && !view2.is_binary()
                    && view2.status_text().is_empty();
                eprintln!(
                    "termust-diffview-test: unstaged={} lineas={} ok={}",
                    view2.diff_text().contains("+dos"),
                    view2.line_count(),
                    ok
                );
                view2.set_staged(true);
                let view3 = view2.clone();
                let app3 = app2.clone();
                glib::timeout_add_local_once(Duration::from_millis(900), move || {
                    ok &= !view3.is_loading() && view3.line_count() == 0;
                    eprintln!(
                        "termust-diffview-test: index_vacio={} {}",
                        view3.line_count() == 0,
                        if ok { "ok" } else { "FAIL" }
                    );
                    view3.load(&root2.join("missing.txt"));
                    let view4 = view3.clone();
                    let app4 = app3.clone();
                    let root3 = root2.clone();
                    glib::timeout_add_local_once(Duration::from_millis(900), move || {
                        let missing_text = i18n::tr("The file does not exist");
                        let missing = view4.status_text().contains(&missing_text);
                        ok &= !view4.is_loading() && view4.line_count() == 0 && missing;
                        eprintln!(
                            "termust-diffview-test: faltante={} {}",
                            missing,
                            if ok { "ok" } else { "FAIL" }
                        );
                        let _ = std::fs::remove_dir_all(&root3);
                        if !ok {
                            std::process::exit(1);
                        }
                        app4.quit();
                    });
                });
            });
        });
    }

    if std::env::var_os("TERMUST_GIT_TEST").is_some() {
        let git_panel = git_panel.clone();
        let app = app.clone();
        let step = Rc::new(Cell::new(0usize));
        let fixture = std::env::var_os("TERMUST_GIT_DIR").map(PathBuf::from);
        let mut failed = false;
        glib::timeout_add_local(Duration::from_millis(700), move || {
            let mut check = |label: &str, ok: bool| {
                eprintln!(
                    "termust-git-test: {label} {}",
                    if ok { "ok" } else { "FAIL" }
                );
                failed |= !ok;
            };
            let current = step.get();
            step.set(current + 1);
            match current {
                0 => {
                    if let Some(dir) = &fixture {
                        git_panel.open(dir);
                    } else {
                        check("TERMUST_GIT_DIR definido", false);
                    }
                }
                1 => {
                    check("panel abierto", git_panel.is_open());
                    check(
                        "repo descubierto",
                        fixture
                            .as_ref()
                            .and_then(|dir| std::fs::canonicalize(dir).ok())
                            .zip(
                                git_panel
                                    .repo_path()
                                    .and_then(|p| std::fs::canonicalize(p).ok()),
                            )
                            .map(|(a, b)| a == b)
                            .unwrap_or(false),
                    );
                    check("rama main", git_panel.branch() == "main");
                    check("seis cambios", git_panel.file_count() == 6);
                    check("modificado", git_panel.has_file("mod.txt"));
                    check("sin preparar", git_panel.has_file("nuevo.txt"));
                    check(
                        "rename",
                        git_panel.kind_of("renombrado.txt")
                            == Some(termust_git_local::ChangeKind::Renamed),
                    );
                    check("unicode", git_panel.has_file("ñandú file.txt"));
                    check("select binario", git_panel.select_path("bin.dat"));
                }
                2 => {
                    check("diff binario", git_panel.diff_is_binary());
                    check("select grande", git_panel.select_path("big.txt"));
                    git_panel.set_staged_view(true);
                }
                3 => {
                    check("diff cap 5k", git_panel.diff_truncated());
                    git_panel.load_full_diff_now();
                }
                4 => {
                    check(
                        "cargar más",
                        !git_panel.diff_truncated() && git_panel.diff_line_count() > 5_000,
                    );
                    git_panel.set_staged_view(false);
                    check("select mod", git_panel.select_path("mod.txt"));
                    git_panel.stage_selected_now();
                }
                5 => {
                    check(
                        "stage aplicado",
                        git_panel.index_status_of("mod.txt") == Some('M'),
                    );
                    git_panel.unstage_selected_now();
                }
                6 => {
                    check(
                        "unstage aplicado",
                        git_panel.index_status_of("mod.txt") == Some('.'),
                    );
                    check("select mod", git_panel.select_path("mod.txt"));
                    git_panel.stage_selected_now();
                }
                7 => {
                    check(
                        "stage previo al commit",
                        git_panel.index_status_of("mod.txt") == Some('M'),
                    );
                    git_panel.set_message("test: commit desde Termust", "");
                    git_panel.commit_now();
                }
                8 => {
                    check(
                        "commit aplicado",
                        !git_panel.has_file("mod.txt") && !git_panel.has_file("big.txt"),
                    );
                    check("sin preparar sigue", git_panel.has_file("nuevo.txt"));
                    check("rama tras commit", git_panel.branch() == "main");
                    git_panel.close();
                    check("panel cerrado", !git_panel.is_open());
                    eprintln!("termust-git-test: {}", if failed { "FAIL" } else { "ok" });
                    if failed {
                        std::process::exit(1);
                    }
                    app.quit();
                    return glib::ControlFlow::Break;
                }
                _ => {}
            }
            glib::ControlFlow::Continue
        });
    }

    if std::env::var_os("TERMUST_GIT_REMOTE_TEST").is_some() {
        let git_panel = git_panel.clone();
        let app = app.clone();
        let step = Rc::new(Cell::new(0usize));
        let fixture = std::env::var_os("TERMUST_GIT_DIR").map(PathBuf::from);
        let bare = std::env::var_os("TERMUST_GIT_BARE").map(PathBuf::from);
        let mut failed = false;
        glib::timeout_add_local(Duration::from_millis(700), move || {
            let mut check = |label: &str, ok: bool| {
                eprintln!(
                    "termust-git-remote: {label} {}",
                    if ok { "ok" } else { "FAIL" }
                );
                failed |= !ok;
            };
            let current = step.get();
            step.set(current + 1);
            match current {
                0 => {
                    if let Some(dir) = &fixture {
                        git_panel.open(dir);
                    } else {
                        check("TERMUST_GIT_DIR definido", false);
                    }
                }
                1 => {
                    check("panel abierto", git_panel.is_open());
                    check("un remoto", git_panel.remotes_count() == 1);
                    check(
                        "origin presente",
                        git_panel.remote_names().iter().any(|name| name == "origin"),
                    );
                    check(
                        "agregar remoto válido",
                        git_panel.add_remote_for_test("test", "https://example.com/r.git"),
                    );
                    check(
                        "rechaza ext::",
                        !git_panel.add_remote_for_test("bad", "ext::sh -c whoami"),
                    );
                }
                2 => {
                    check("dos remotos", git_panel.remotes_count() == 2);
                    check("quitar remoto", git_panel.remove_remote_for_test("test"));
                }
                3 => {
                    check("un remoto tras quitar", git_panel.remotes_count() == 1);
                    git_panel.push_now_for_test();
                }
                4 => {
                    match git_panel.push_result() {
                        Some(Ok(message)) => check(&format!("push ok: {message}"), true),
                        Some(Err(error)) => check(&format!("push falló: {error}"), false),
                        None => check("push sin resultado", false),
                    }
                    if let Some(bare) = &bare {
                        let output = termust_host_exec::host_command(
                            "git",
                            &[
                                std::ffi::OsString::from(format!("--git-dir={}", bare.display())),
                                std::ffi::OsString::from("rev-parse"),
                                std::ffi::OsString::from("main"),
                            ],
                            None,
                        )
                        .output();
                        check(
                            "commit en el repo bare",
                            output
                                .map(|output| output.status.success())
                                .unwrap_or(false),
                        );
                    }
                    git_panel.close();
                    eprintln!("termust-git-remote: {}", if failed { "FAIL" } else { "ok" });
                    if failed {
                        std::process::exit(1);
                    }
                    app.quit();
                    return glib::ControlFlow::Break;
                }
                _ => {}
            }
            glib::ControlFlow::Continue
        });
    }

    if std::env::var_os("TERMUST_VAULT_TEST").is_some() {
        let mut failed = false;
        let mut check = |label: &str, ok: bool| {
            eprintln!(
                "termust-vault-test: {label} {}",
                if ok { "ok" } else { "FAIL" }
            );
            failed |= !ok;
        };
        match remotes::save_credentials("https://example.com", "alice", b"TOKEN-E2E-42") {
            Ok(()) => check("guardar credencial", true),
            Err(error) => {
                eprintln!("termust-vault-test: {error}");
                check("guardar credencial", false);
            }
        }
        match remotes::load_credentials("https://example.com") {
            Ok(Some((user, secret))) => check(
                "leer credencial",
                user == "alice" && secret.as_str().ok() == Some("TOKEN-E2E-42"),
            ),
            _ => check("leer credencial", false),
        }
        check(
            "origen ajeno no recibe la credencial",
            matches!(
                remotes::load_credentials("https://example.com.evil.tld"),
                Ok(None)
            ),
        );
        if let Ok(path) = remotes::vault_path() {
            let bytes = std::fs::read(&path).unwrap_or_default();
            check(
                "sin texto plano en el archivo",
                !bytes
                    .windows(b"TOKEN-E2E-42".len())
                    .any(|window| window == b"TOKEN-E2E-42"),
            );
        } else {
            check("ruta del vault", false);
        }
        eprintln!("termust-vault-test: {}", if failed { "FAIL" } else { "ok" });
        if failed {
            std::process::exit(1);
        }
        app.quit();
        return window;
    }

    if std::env::var_os("TERMUST_AGENT_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        let agent_bar = agent_bar.clone();
        let agent_icon = agent_icon.clone();
        let agent_name = agent_name.clone();
        let agent_task = agent_task.clone();
        let agent_dot = agent_dot.clone();
        let step = Rc::new(Cell::new(0usize));
        let mut failed = false;
        glib::timeout_add_local(Duration::from_millis(700), move || {
            let mut check = |label: &str, ok: bool| {
                eprintln!(
                    "termust-agent-test: {label} {}",
                    if ok { "ok" } else { "FAIL" }
                );
                failed |= !ok;
            };
            let current = step.get();
            step.set(current + 1);
            match current {
                0 => {
                    manager
                        .borrow()
                        .send_active(b"printf '\\033]0;opencode\\007'; exec sleep 30\n");
                }
                1 => {
                    let agent = manager.borrow().active_agent();
                    check(
                        "detecta opencode",
                        agent.as_ref().map(|agent| agent.id.as_str()) == Some("opencode"),
                    );
                    check(
                        "activo al recibir output",
                        agent
                            .as_ref()
                            .map(|agent| agent.state == AgentState::Active)
                            .unwrap_or(false),
                    );
                    check("barra visible", agent_bar.is_visible());
                }
                2..=5 => {}
                6 => {
                    let agent = manager.borrow().active_agent();
                    check(
                        "silencio pasa a Idle",
                        agent
                            .as_ref()
                            .map(|agent| agent.state == AgentState::Idle)
                            .unwrap_or(false),
                    );
                    check(
                        "Idle no es verde",
                        agent
                            .as_ref()
                            .map(|agent| !agent.state.is_green())
                            .unwrap_or(false),
                    );
                    manager.borrow().send_active(&[3]);
                }
                7 => {
                    let agent = manager.borrow().active_agent();
                    check(
                        "terminado tras Ctrl+C",
                        agent
                            .as_ref()
                            .map(|agent| matches!(agent.state, AgentState::Exited { .. }))
                            .unwrap_or(true),
                    );
                    let injected = AgentStatus {
                        id: "antigravity".into(),
                        display_name: "Antigravity".into(),
                        icon: "avatar-default-symbolic".into(),
                        task: "Antigravity · /tmp/proyecto".into(),
                        state: AgentState::WaitingInput,
                    };
                    update_agent_bar(
                        &agent_bar,
                        &agent_icon,
                        &agent_name,
                        &agent_task,
                        &agent_dot,
                        Some(&injected),
                    );
                    check(
                        "barra muestra Antigravity",
                        agent_name.text() == "Antigravity"
                            && agent_task.text().contains("Antigravity")
                            && agent_dot.has_css_class("termust-agent-waiting"),
                    );
                    eprintln!("termust-agent-test: {}", if failed { "FAIL" } else { "ok" });
                    if failed {
                        std::process::exit(1);
                    }
                    app.quit();
                    return glib::ControlFlow::Break;
                }
                _ => {}
            }
            glib::ControlFlow::Continue
        });
    }

    if std::env::var_os("TERMUST_PERF_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        let home = home.clone();
        let case = std::env::var("TERMUST_PERF_CASE").unwrap_or_else(|_| "1t".into());
        let tabs: usize = match case.as_str() {
            "1t" => 1,
            "9t" | "9h" | "idle9" => 9,
            "30t" | "30h" => 30,
            other => other.parse().unwrap_or(1),
        };
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            for _ in 1..tabs {
                if manager.borrow_mut().open_tab(home.clone()).is_err() {
                    break;
                }
            }
            let manager2 = manager.clone();
            let app2 = app.clone();
            let case2 = case.clone();
            glib::timeout_add_local_once(Duration::from_millis(1200), move || {
                if case2.ends_with('h') {
                    for index in 0..tabs {
                        manager2.borrow_mut().select_index(index);
                        manager2.borrow().send_active(b"seq 1 2000\n");
                    }
                    manager2.borrow_mut().select_index(0);
                }
                let manager3 = manager2.clone();
                let app3 = app2.clone();
                let case3 = case2.clone();
                let cpu_before = process_cpu_ticks();
                let cpu_clock = std::time::Instant::now();
                let wait_ms = if case3 == "idle9" { 10_000 } else { 2_200 };
                glib::timeout_add_local_once(Duration::from_millis(wait_ms), move || {
                    let pss = pss_kb();
                    let rss = rss_kb();
                    let mut cpu = 0.0;
                    if case3 == "idle9" {
                        cpu = cpu_percent_between(cpu_before, cpu_clock.elapsed());
                    }
                    eprintln!(
                        "termust-perf: caso={case3} pestañas={} pss={pss} kB rss={rss} kB cpu={cpu:.2}% history={}",
                        manager3.borrow().tab_count(),
                        manager3.borrow().history_total()
                    );
                    app3.quit();
                });
            });
        });
    }

    if std::env::var_os("TERMUST_ENGINE_BENCH").is_some() {
        use termust_core::terminal::{Size, TerminalEngine};
        use termust_text_render::{PaintOpts, TextRenderer};
        let app = app.clone();
        let mut engine = termust_term_engine::AlacrittyEngine::new(Size::new(120, 40), 2_000);
        let chunk = "x".repeat(120) + "\n";
        let mut payload = String::with_capacity(10 * 1024 * 1024);
        while payload.len() < 10 * 1024 * 1024 {
            payload.push_str(&chunk);
        }
        let bytes = payload.as_bytes();
        let started = std::time::Instant::now();
        for part in bytes.chunks(64 * 1024) {
            engine.feed(part);
        }
        let elapsed = started.elapsed();
        let mbps = bytes.len() as f64 / 1024.0 / 1024.0 / elapsed.as_secs_f64();
        let snapshot = engine.snapshot();
        let renderer = termust_render_cairo::CairoRenderer::new();
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 1280, 800).unwrap();
        let ctx = cairo::Context::new(&surface).unwrap();
        let opts = PaintOpts {
            font: termust_text_render::FontSpec::default(),
            selection: None,
            bg_alpha: 1.0,
        };
        let mut frames = Vec::new();
        for _ in 0..120 {
            let frame_start = std::time::Instant::now();
            renderer.paint(
                &ctx,
                &snapshot,
                &termust_text_render::Theme::default(),
                &opts,
            );
            frames.push(frame_start.elapsed().as_secs_f64() * 1000.0);
        }
        frames.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p50 = frames[frames.len() / 2];
        let p95 = frames[(frames.len() * 95) / 100];
        let mut key_frames = Vec::new();
        for _ in 0..200 {
            let start = std::time::Instant::now();
            engine.feed(b"a");
            let snapshot = engine.snapshot();
            renderer.paint(
                &ctx,
                &snapshot,
                &termust_text_render::Theme::default(),
                &opts,
            );
            key_frames.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        key_frames.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let key_p50 = key_frames[key_frames.len() / 2];
        let key_p95 = key_frames[(key_frames.len() * 95) / 100];
        eprintln!(
            "termust-engine-bench: parse={mbps:.1} MB/s frame_p50={p50:.2} ms frame_p95={p95:.2} ms key_p50={key_p50:.2} ms key_p95={key_p95:.2} ms"
        );
        app.quit();
    }

    if std::env::var_os("TERMUST_FLOOD_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        let home = home.clone();
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            for _ in 1..3 {
                let _ = manager.borrow_mut().open_tab(home.clone());
            }
            for index in 0..3 {
                manager.borrow_mut().select_index(index);
                manager.borrow().send_active(b"yes\n");
            }
            manager.borrow_mut().select_index(0);
            let manager2 = manager.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(6000), move || {
                let pss1 = pss_kb();
                let manager3 = manager2.clone();
                let app3 = app2.clone();
                glib::timeout_add_local_once(Duration::from_millis(4000), move || {
                    let pss2 = pss_kb();
                    eprintln!(
                        "termust-flood: pss_6s={pss1} kB pss_10s={pss2} kB delta={} kB",
                        pss2 as i64 - pss1 as i64
                    );
                    app3.quit();
                    let _ = manager3;
                });
            });
        });
    }

    if std::env::var_os("TERMUST_HISTORY_BENCH").is_some() {
        use termust_core::history::{HistoryEntry, HistoryIndex, HistorySource};
        let app = app.clone();
        let home = home.clone();
        let mut index = HistoryIndex::new();
        for item in 0..10_000 {
            index.insert(HistoryEntry::new(
                format!("comando-{item} --flag"),
                home.clone(),
                item,
                Some(0),
                HistorySource::Session,
            ));
        }
        let mut samples = Vec::new();
        for query in 0..1_000 {
            let text = format!("comando-{}", query * 7 % 10_000);
            let start = std::time::Instant::now();
            let results = index.search(&text, 50);
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            std::hint::black_box(results);
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let p95 = samples[(samples.len() * 95) / 100];
        eprintln!(
            "termust-history-bench: entries=10000 mean={mean:.3} ms p95={p95:.3} ms max={:.3} ms",
            samples.last().copied().unwrap_or(0.0)
        );
        app.quit();
    }

    if std::env::var_os("TERMUST_I18N_TEST").is_some() {
        eprintln!(
            "termust-i18n-test: preferencias={:?} panel={:?}",
            i18n::tr("Preferences"),
            i18n::tr("File panel")
        );
        app.quit();
        return window;
    }

    if std::env::var_os("TERMUST_HIG_TEST").is_some() {
        let window = window.clone();
        let overview_button = overview_button.clone();
        let tab_overview = tab_overview.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(700), move || {
            show_shortcuts(&window);
            let window2 = window.clone();
            let overview_button2 = overview_button.clone();
            let tab_overview2 = tab_overview.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                show_about(&window2);
                overview_button2.set_active(true);
                let tab_overview3 = tab_overview2.clone();
                let app3 = app2.clone();
                glib::timeout_add_local_once(Duration::from_millis(400), move || {
                    let overview = tab_overview3.is_open();
                    tab_overview3.set_open(false);
                    eprintln!(
                        "termust-hig-test: atajos+acerca+overview={} {}",
                        if overview { "vista" } else { "cerrada" },
                        if overview { "ok" } else { "FAIL" }
                    );
                    if !overview {
                        std::process::exit(1);
                    }
                    app3.quit();
                });
            });
        });
    }

    if std::env::var_os("TERMUST_POINTER_TEST").is_some() {
        let window = window.clone();
        let files_button = files_button.clone();
        let overview_button = overview_button.clone();
        let tab_overview = tab_overview.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1200), move || {
            files_button.set_active(true);
            let mut failed = false;
            let mut check = |label: &str, ok: bool| {
                eprintln!(
                    "termust-pointer-test: {label} {}",
                    if ok { "ok" } else { "FAIL" }
                );
                failed |= !ok;
            };
            let chain = |x: f64, y: f64| {
                window
                    .pick(x, y, gtk::PickFlags::DEFAULT)
                    .map(|widget| {
                        let mut names = Vec::new();
                        let mut current = Some(widget);
                        while let Some(widget) = current {
                            names.push(widget.type_().name().to_string());
                            current = widget.parent();
                        }
                        names.join(" < ")
                    })
                    .unwrap_or_else(|| "ninguno".into())
            };
            let width = window.width() as f64;
            let height = window.height() as f64;
            let terminal = chain(width * 0.7, height * 0.5);
            let sidebar = chain(width * 0.12, height * 0.5);
            check(
                "terminal recibe el puntero",
                terminal.contains("GtkDrawingArea") && !terminal.contains("AdwTabOverview"),
            );
            check(
                "sidebar recibe el puntero",
                !sidebar.contains("AdwTabOverview"),
            );
            let right = chain(width * 0.9, height * 0.5);
            check(
                "visor cerrado no targetable",
                !right.contains("GtkTextView"),
            );
            check("overview cerrado no targetable", !tab_overview.can_target());
            overview_button.set_active(true);
            let window2 = window.clone();
            let overview_button2 = overview_button.clone();
            let tab_overview2 = tab_overview.clone();
            let app2 = app.clone();
            let mut failed2 = failed;
            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                let mut check = |label: &str, ok: bool| {
                    eprintln!(
                        "termust-pointer-test: {label} {}",
                        if ok { "ok" } else { "FAIL" }
                    );
                    failed2 |= !ok;
                };
                check(
                    "overview abierto es targetable",
                    tab_overview2.can_target() && tab_overview2.is_open(),
                );
                overview_button2.set_active(false);
                let window3 = window2.clone();
                let tab_overview3 = tab_overview2.clone();
                let app3 = app2.clone();
                let mut failed3 = failed2;
                glib::timeout_add_local_once(Duration::from_millis(500), move || {
                    let mut check = |label: &str, ok: bool| {
                        eprintln!(
                            "termust-pointer-test: {label} {}",
                            if ok { "ok" } else { "FAIL" }
                        );
                        failed3 |= !ok;
                    };
                    let chain = window3
                        .pick(
                            window3.width() as f64 * 0.7,
                            window3.height() as f64 * 0.5,
                            gtk::PickFlags::DEFAULT,
                        )
                        .map(|widget| widget.type_().name().to_string())
                        .unwrap_or_else(|| "ninguno".into());
                    check(
                        "overview cerrado devuelve el clic al terminal",
                        !chain.contains("AdwTabOverview") && !tab_overview3.can_target(),
                    );
                    eprintln!(
                        "termust-pointer-test: {}",
                        if failed3 { "FAIL" } else { "ok" }
                    );
                    if failed3 {
                        std::process::exit(1);
                    }
                    app3.quit();
                });
            });
        });
    }

    if std::env::var_os("TERMUST_EXIT_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1200), move || {
            manager.borrow().send_active(b"exit\n");
            let app = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(3500), move || {
                eprintln!("termust-exit-test: la app no se cerró FAIL");
                app.quit();
                std::process::exit(1);
            });
        });
    }

    // `--no-restore` y los hooks de test no deben pisar la sesión real.
    let read_only =
        no_restore || testing || std::env::var_os("TERMUST_SESSION_NO_RESTORE_TEST").is_some();
    if !read_only {
        let session_path = termust_store::session_path();
        let saver = SessionSaver::start(session_path.clone());
        let last: Rc<RefCell<Option<termust_store::SessionSnapshot>>> = Rc::new(RefCell::new(None));
        {
            let manager = manager.clone();
            let saver = saver.clone();
            let last = last.clone();
            glib::timeout_add_local(Duration::from_secs(1), move || {
                if let Ok(manager) = manager.try_borrow() {
                    let snapshot = manager.snapshot();
                    if last.borrow().as_ref() != Some(&snapshot) {
                        saver.schedule(snapshot.clone());
                        *last.borrow_mut() = Some(snapshot);
                    }
                }
                glib::ControlFlow::Continue
            });
        }
        {
            let manager = manager.clone();
            let path = session_path.clone();
            window.connect_close_request(move |_| {
                if let Ok(manager) = manager.try_borrow() {
                    let snapshot = manager.snapshot();
                    if let Err(e) = termust_store::save_snapshot(&path, &snapshot) {
                        eprintln!("termust: no se pudo guardar la sesión: {e}");
                    }
                }
                glib::Propagation::Proceed
            });
        }
    }

    if let Some(text) = notice.take() {
        let toast = toast_overlay.clone();
        glib::idle_add_local_once(move || {
            toast.add_toast(libadwaita::Toast::new(&text));
        });
    }

    if std::env::var_os("TERMUST_SESSION_SEED").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        let home = home.clone();
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            manager.borrow().send_active(b"cd /tmp\n");
            let manager2 = manager.clone();
            let app2 = app.clone();
            let home2 = home.clone();
            glib::timeout_add_local_once(Duration::from_millis(1200), move || {
                {
                    let mut manager = manager2.borrow_mut();
                    let _ = manager.split_active(SplitOrientation::Horizontal);
                    manager.rename_tab("seed-tab");
                    manager.set_active_tab_theme(Some("gruvbox-dark".to_string()));
                    let _ = manager.open_tab(home2.clone());
                    manager.select_index(0);
                }
                let manager3 = manager2.clone();
                let app3 = app2.clone();
                glib::timeout_add_local_once(Duration::from_millis(1200), move || {
                    let snapshot = manager3.borrow().snapshot();
                    let path = termust_store::session_path();
                    match termust_store::save_snapshot(&path, &snapshot) {
                        Ok(()) => eprintln!("termust-session-seed: ok"),
                        Err(e) => eprintln!("termust-session-seed: FAIL {e}"),
                    }
                    app3.quit();
                });
            });
        });
    }

    if std::env::var_os("TERMUST_SESSION_VERIFY").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1800), move || {
            let manager = manager.borrow();
            let tabs = manager.tab_count();
            let panes = manager.active_pane_count();
            let cwd = manager.active_cwd();
            let title = manager.active_title_override();
            let theme = manager.active_tab_theme_id();
            let ok = tabs == 2
                && panes == 2
                && theme.as_deref() == Some("gruvbox-dark")
                && cwd
                    .as_ref()
                    .map(|path| path.ends_with("tmp"))
                    .unwrap_or(false)
                && title.as_deref() == Some("seed-tab");
            eprintln!(
                "termust-session-verify: tabs={tabs} panes={panes} cwd={cwd:?} title={title:?} tema={theme:?} {}",
                if ok { "ok" } else { "FAIL" }
            );
            if !ok {
                std::process::exit(1);
            }
            app.quit();
        });
    }

    if std::env::var_os("TERMUST_SESSION_CORRUPT_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1200), move || {
            let manager = manager.borrow();
            let tabs = manager.tab_count();
            let panes = manager.active_pane_count();
            let ok = tabs == 1 && panes == 1;
            eprintln!(
                "termust-session-corrupt: tabs={tabs} panes={panes} {}",
                if ok { "ok" } else { "FAIL" }
            );
            if !ok {
                std::process::exit(1);
            }
            app.quit();
        });
    }

    if std::env::var_os("TERMUST_SESSION_NO_RESTORE_TEST").is_some() {
        let manager = manager.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            let manager = manager.borrow();
            let tabs = manager.tab_count();
            let panes = manager.active_pane_count();
            let cwd = manager.active_cwd();
            let title = manager.active_title_override();
            let ok = tabs == 1
                && panes == 1
                && !cwd
                    .as_ref()
                    .map(|path| path.ends_with("tmp"))
                    .unwrap_or(true)
                && title.is_none();
            eprintln!(
                "termust-session-no-restore: tabs={tabs} panes={panes} cwd={cwd:?} title={title:?} {}",
                if ok { "ok" } else { "FAIL" }
            );
            if !ok {
                std::process::exit(1);
            }
            app.quit();
        });
    }

    window.present();
    // Maximizar después de presentar: antes de mapear, algunos compositores
    // ignoran el pedido y la ventana restaurada queda sin maximizar.
    work_area::install(window.upcast_ref());
    if maximized {
        window.maximize();
    }

    if let Some(mode) = std::env::var_os("TERMUST_WORKAREA_TEST") {
        let restore = mode.to_string_lossy() == "restore";
        let window = window.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            if !restore {
                window.maximize();
            }
            let window = window.clone();
            let app = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(600), move || {
                let Some(monitor) = work_area::monitor_of(window.upcast_ref()) else {
                    eprintln!("termust-workarea-test: sin monitor FAIL");
                    std::process::exit(1);
                };
                let geometry = monitor.geometry();
                let reserved = work_area::measured_reservation(window.upcast_ref());
                let expected = std::env::var("TERMUST_EXPECT_RESERVED")
                    .ok()
                    .and_then(|value| value.parse::<i32>().ok());
                let mut ok = window.is_maximized()
                    && window.width() == geometry.width()
                    && window.height() + reserved == geometry.height()
                    && reserved >= 0;
                if let Some(expected) = expected {
                    ok &= reserved == expected;
                }
                eprintln!(
                    "termust-workarea-test: modo={} maximized={} monitor={}x{} escala={} reserva_vertical={}px ventana={}x{} fondo={} {}",
                    if restore { "restore" } else { "fresh" },
                    window.is_maximized(),
                    geometry.width(),
                    geometry.height(),
                    monitor.scale_factor(),
                    reserved,
                    window.width(),
                    window.height(),
                    window.height() + reserved,
                    if ok { "ok" } else { "FAIL" }
                );
                if !ok {
                    std::process::exit(1);
                }
                app.quit();
            });
        });
    }

    window
}

fn initial_tabs() -> usize {
    std::env::var("TERMUST_TABS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .clamp(1, 64)
}

const APP_CSS: &str = include_str!("../../data/termust.css");

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(APP_CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn map_key(key: gdk::Key) -> Option<CoreKey> {
    let mapped = match key {
        gdk::Key::Return => CoreKey::Enter,
        gdk::Key::Tab => CoreKey::Tab,
        gdk::Key::Escape => CoreKey::Escape,
        gdk::Key::Up => CoreKey::Up,
        gdk::Key::Down => CoreKey::Down,
        gdk::Key::Left => CoreKey::Left,
        gdk::Key::Right => CoreKey::Right,
        gdk::Key::Page_Up => CoreKey::PageUp,
        gdk::Key::Page_Down => CoreKey::PageDown,
        gdk::Key::Home => CoreKey::Home,
        gdk::Key::End => CoreKey::End,
        gdk::Key::F1 => CoreKey::F(1),
        gdk::Key::F2 => CoreKey::F(2),
        gdk::Key::F3 => CoreKey::F(3),
        gdk::Key::F4 => CoreKey::F(4),
        gdk::Key::F5 => CoreKey::F(5),
        gdk::Key::F6 => CoreKey::F(6),
        gdk::Key::F7 => CoreKey::F(7),
        gdk::Key::F8 => CoreKey::F(8),
        gdk::Key::F9 => CoreKey::F(9),
        gdk::Key::F10 => CoreKey::F(10),
        gdk::Key::F11 => CoreKey::F(11),
        gdk::Key::F12 => CoreKey::F(12),
        _ => CoreKey::Char(key.to_unicode()?),
    };
    Some(mapped)
}

fn show_error(toolbar: &ToolbarView, message: &str) {
    let page = libadwaita::StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title(i18n::tr("Could not start the terminal"))
        .description(message)
        .build();
    toolbar.set_content(Some(&page));
}

fn restore_preference() -> bool {
    setting_bool("restore-session", true)
}

fn gsettings_with_key(key: &str) -> Option<gtk::gio::Settings> {
    let source = gtk::gio::SettingsSchemaSource::default()?;
    let schema = source.lookup(termust_core::APP_ID, true)?;
    schema
        .has_key(key)
        .then(|| gtk::gio::Settings::new(termust_core::APP_ID))
}

fn setting_bool(key: &str, default: bool) -> bool {
    gsettings_with_key(key)
        .map(|settings| settings.boolean(key))
        .unwrap_or(default)
}

#[derive(Clone)]
struct SessionSaver {
    tx: mpsc::Sender<termust_store::SessionSnapshot>,
}

impl SessionSaver {
    fn start(path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("termust-session".into())
            .spawn(move || {
                for snapshot in rx {
                    if let Err(e) = termust_store::save_snapshot(&path, &snapshot) {
                        eprintln!("termust: no se pudo guardar la sesión: {e}");
                    }
                }
            });
        Self { tx }
    }

    fn schedule(&self, snapshot: termust_store::SessionSnapshot) {
        let _ = self.tx.send(snapshot);
    }
}

fn open_history(
    manager: &Rc<RefCell<TabManager>>,
    history: &Rc<history::HistoryController>,
    toast: &libadwaita::ToastOverlay,
) {
    let anchor = {
        let Ok(manager) = manager.try_borrow() else {
            return;
        };
        manager.active_view().map(|view| view.widget().clone())
    };
    let Some(anchor) = anchor else {
        return;
    };
    let manager_insert = manager.clone();
    let toast = toast.clone();
    let insert: Rc<dyn Fn(String)> = Rc::new(move |text| {
        let Ok(manager) = manager_insert.try_borrow() else {
            return;
        };
        let Some(view) = manager.active_view() else {
            return;
        };
        let bracketed = view.bracketed_paste();
        match termust_core::history::insertion_payload(&text, bracketed) {
            Ok(bytes) => {
                view.write_input(&bytes);
                view.grab_focus();
            }
            Err(_) => {
                toast.add_toast(libadwaita::Toast::new(&i18n::tr(
                    "The command is multiline and the shell does not support bracketed paste.",
                )));
            }
        }
    });
    history.open(&anchor, insert);
}

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                let value = line.strip_prefix("VmRSS:")?;
                value.split_whitespace().next()?.parse().ok()
            })
        })
        .unwrap_or(0)
}

fn file_hash(path: &std::path::Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(path).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn update_agent_bar(
    bar: &gtk::Box,
    icon: &gtk::Image,
    name: &gtk::Label,
    task: &gtk::Label,
    dot: &gtk::Label,
    agent: Option<&AgentStatus>,
) {
    let Some(agent) = agent else {
        bar.set_visible(false);
        return;
    };
    bar.set_visible(true);
    icon.set_icon_name(Some(&agent.icon));
    if icon.icon_name().is_none() {
        icon.set_icon_name(Some("avatar-default-symbolic"));
    }
    name.set_text(&agent.display_name);
    task.set_text(&agent.task);
    task.set_tooltip_text(Some(&agent.task));
    dot.set_tooltip_text(Some(&agent.state.label()));
    dot.set_css_classes(&["termust-agent-dot", agent.state.css_class()]);
}

fn pss_kb() -> u64 {
    let Ok(text) = std::fs::read_to_string("/proc/self/smaps_rollup") else {
        return 0;
    };
    text.lines()
        .find_map(|line| {
            let value = line.strip_prefix("Pss:")?;
            value.split_whitespace().next()?.parse().ok()
        })
        .unwrap_or(0)
}

fn cpu_percent_between(before: u64, elapsed: std::time::Duration) -> f64 {
    let seconds = elapsed.as_secs_f64().max(0.001);
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    (process_cpu_ticks().saturating_sub(before) as f64 / ticks_per_second) / seconds * 100.0
}

fn process_cpu_ticks() -> u64 {
    let Ok(text) = std::fs::read_to_string("/proc/self/stat") else {
        return 0;
    };
    let Some((_, rest)) = text.split_once(')') else {
        return 0;
    };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() < 13 {
        return 0;
    }
    let utime: u64 = fields[11].parse().unwrap_or(0);
    let stime: u64 = fields[12].parse().unwrap_or(0);
    utime + stime
}

fn show_shortcuts(window: &ApplicationWindow) {
    let dialog = libadwaita::ShortcutsDialog::new();
    dialog.set_title(&i18n::tr("Keyboard shortcuts"));
    let tabs = libadwaita::ShortcutsSection::new(Some(&i18n::tr("Tabs")));
    for (title, accel) in [
        ("New tab", "<Control><Shift>t"),
        ("Close tab", "<Control><Shift>w"),
        ("Next tab", "<Control>Page_Down"),
        ("Previous tab", "<Control>Page_Up"),
        ("Move tab left", "<Control><Shift>Page_Up"),
        ("Move tab right", "<Control><Shift>Page_Down"),
    ] {
        tabs.add(libadwaita::ShortcutsItem::new(&i18n::tr(title), accel));
    }
    dialog.add(tabs);
    let panes = libadwaita::ShortcutsSection::new(Some(&i18n::tr("Panes")));
    for (title, accel) in [
        ("Split pane right", "<Control><Shift>d"),
        ("Split pane down", "<Control><Shift>e"),
        ("Focus pane left", "<Control><Shift>Left"),
        ("Focus pane right", "<Control><Shift>Right"),
        ("Focus pane up", "<Control><Shift>Up"),
        ("Focus pane down", "<Control><Shift>Down"),
        ("Resize left", "<Control><Alt><Shift>Left"),
        ("Resize right", "<Control><Alt><Shift>Right"),
        ("Resize up", "<Control><Alt><Shift>Up"),
        ("Resize down", "<Control><Alt><Shift>Down"),
    ] {
        panes.add(libadwaita::ShortcutsItem::new(&i18n::tr(title), accel));
    }
    dialog.add(panes);
    let app = libadwaita::ShortcutsSection::new(Some(&i18n::tr("Application")));
    for (title, accel) in [
        ("Zoom in", "<Control>plus"),
        ("Zoom out", "<Control>minus"),
        ("Reset zoom", "<Control>0"),
        ("Fullscreen", "F11"),
        ("File panel", "F9"),
        ("Vertical tabs", "<Control><Shift>l"),
        ("Git", "<Control><Shift>g"),
        ("Preferences", "<Control>comma"),
        ("Atajos de teclado", "<Control><Shift>question"),
    ] {
        app.add(libadwaita::ShortcutsItem::new(&i18n::tr(title), accel));
    }
    dialog.add(app);
    dialog.present(Some(window));
}

fn show_about(window: &ApplicationWindow) {
    let dialog = libadwaita::AboutDialog::new();
    dialog.set_application_name("Termust");
    dialog.set_application_icon(termust_core::APP_ID);
    dialog.set_version(env!("CARGO_PKG_VERSION"));
    dialog.set_developer_name("Termust");
    dialog.set_developers(&["Termust"]);
    dialog.set_license_type(gtk::License::Gpl30);
    dialog.set_website("https://github.com/gac982/Termust");
    dialog.add_link(&i18n::tr("Website"), "https://github.com/gac982/Termust");
    dialog.add_link(
        &i18n::tr("Report an issue"),
        "https://github.com/gac982/Termust/issues",
    );
    dialog.present(Some(window));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewerKey {
    Close,
    Search,
    Pass,
}

/// La columna del visor no es modal: solo consume Escape y Ctrl+F en Archivo.
/// El resto de las teclas sigue hasta los atajos de la aplicación (F9 incluido).
fn viewer_key(is_open: bool, showing_file_page: bool, key: gdk::Key, ctrl: bool) -> ViewerKey {
    if !is_open {
        return ViewerKey::Pass;
    }
    if key == gdk::Key::Escape {
        return ViewerKey::Close;
    }
    let search = ctrl
        && key
            .to_unicode()
            .map(|ch| ch.eq_ignore_ascii_case(&'f'))
            .unwrap_or(false);
    if search && showing_file_page {
        return ViewerKey::Search;
    }
    ViewerKey::Pass
}

#[cfg(test)]
mod key_gating {
    use super::*;

    #[test]
    fn open_viewer_does_not_swallow_app_shortcuts() {
        assert_eq!(viewer_key(true, true, gdk::Key::F9, false), ViewerKey::Pass);
        assert_eq!(
            viewer_key(true, true, gdk::Key::Escape, false),
            ViewerKey::Close
        );
        assert_eq!(viewer_key(true, true, gdk::Key::f, true), ViewerKey::Search);
        assert_eq!(viewer_key(true, false, gdk::Key::f, true), ViewerKey::Pass);
        assert_eq!(
            viewer_key(false, true, gdk::Key::Escape, false),
            ViewerKey::Pass
        );
    }
}
