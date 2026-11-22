mod appearance;
mod history;
mod preferences;
mod tabs;

use appearance::{Appearance, AUTO_THEME};
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
use tabs::TabManager;
use termust_core::history::{HistoryEntry, HistoryIndex, HistorySource};
use termust_core::keymap::{
    resolve_shortcut, Action, Focus, Key as CoreKey, KeyConfig, KeyEvent, Mods,
};
use termust_core::tabs::{Direction, Preset, SplitOrientation};
use termust_pty_unix::UnixPtySpawner;

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
    let started = std::time::Instant::now();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
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

    let header = HeaderBar::new();
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Termust")
        .default_width(win_width)
        .default_height(win_height)
        .content(&toolbar)
        .build();
    if maximized {
        window.maximize();
    }

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
                            "Sesión restaurada; algunos directorios no existían y se usó $HOME."
                                .into()
                        } else {
                            "Sesión restaurada. Los procesos no se restauran.".into()
                        });
                        Ok(manager)
                    }
                    Err(e) => {
                        eprintln!("termust: no se pudo restaurar la sesión: {e}");
                        notice =
                            Some("No se pudo restaurar la sesión; se inició una nueva.".into());
                        make_fresh()
                    }
                },
                Err(e) => {
                    eprintln!("termust: sesión inválida: {e}");
                    notice = Some("La sesión guardada no es válida; se inició una nueva.".into());
                    make_fresh()
                }
            }
        }
        termust_store::LoadResult::Corrupt(e) => {
            eprintln!("termust: sesión corrupta: {e}");
            notice = Some("No se pudo leer la sesión guardada; se inició una nueva.".into());
            make_fresh()
        }
        termust_store::LoadResult::FutureVersion(version) => {
            eprintln!("termust: sesión con schema v{version} no soportado");
            notice = Some("La sesión guardada es de una versión más nueva; se ignoró.".into());
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
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(250), move || app.quit());
    }
    for _ in 1..initial_tabs() {
        if let Err(e) = manager.borrow_mut().open_tab(home.clone()) {
            eprintln!("termust: no se pudo abrir pestaña: {e}");
            break;
        }
    }

    install_css();
    let new_tab = gtk::Button::builder()
        .icon_name("tab-new-symbolic")
        .tooltip_text("Nueva pestaña")
        .build();
    new_tab.add_css_class("flat");
    {
        let manager = manager.clone();
        let home = home.clone();
        new_tab.connect_clicked(move |_| {
            if let Err(e) = manager.borrow_mut().open_tab(home.clone()) {
                eprintln!("termust: no se pudo abrir pestaña: {e}");
            }
        });
    }
    header.pack_start(&new_tab);
    let tab_bar = manager.borrow().tab_bar().clone();
    tab_bar.set_hexpand(true);
    tab_bar.set_halign(gtk::Align::Fill);
    header.set_title_widget(Some(&tab_bar));
    let toast_overlay = libadwaita::ToastOverlay::new();
    toast_overlay.set_child(Some(manager.borrow().tab_view()));
    toolbar.set_content(Some(&toast_overlay));

    let history_index = Rc::new(RefCell::new(HistoryIndex::new()));
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
                    toast.add_toast(libadwaita::Toast::new("No hay pestaña activa"));
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
                toast.add_toast(libadwaita::Toast::new("Historial de comandos borrado"));
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
        menu.append(Some("Dividir a la derecha"), Some("pane.split-right"));
        menu.append(Some("Dividir abajo"), Some("pane.split-down"));
        let designs = gtk::gio::Menu::new();
        designs.append(Some("Dos columnas"), Some("pane.preset-two-columns"));
        designs.append(Some("Dos filas"), Some("pane.preset-two-rows"));
        designs.append(Some("Tres columnas"), Some("pane.preset-three-columns"));
        designs.append(Some("Cuadrícula 2×2"), Some("pane.preset-grid"));
        menu.append_submenu(Some("Diseños"), &designs);
        let themes_menu = gtk::gio::Menu::new();
        let global_theme = gtk::gio::MenuItem::new(Some("Tema global"), None);
        global_theme
            .set_action_and_target_value(Some("pane.tab-theme"), Some(&AUTO_THEME.to_variant()));
        themes_menu.append_item(&global_theme);
        for theme in appearance.themes() {
            let item = gtk::gio::MenuItem::new(Some(&theme.name), None);
            item.set_action_and_target_value(Some("pane.tab-theme"), Some(&theme.id.to_variant()));
            themes_menu.append_item(&item);
        }
        menu.append_submenu(Some("Tema de la pestaña"), &themes_menu);
        menu.append(Some("Preferencias"), Some("pane.preferences"));
        menu.append(Some("Historial de comandos"), Some("pane.history"));
        menu.append(Some("Borrar historial"), Some("pane.clear-history"));
        menu.append(Some("Renombrar pestaña"), Some("pane.rename-tab"));
        menu.append(Some("Cerrar panel"), Some("pane.close-pane"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Menú")
            .build();
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
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _, state| {
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
        glib::timeout_add_local(Duration::from_millis(500), move || {
            manager.borrow_mut().update_titles();
            let records = manager.borrow().take_history_records();
            if !records.is_empty() {
                let mut index = history_index.borrow_mut();
                for record in records {
                    index.insert(HistoryEntry::new(
                        record.text,
                        record.cwd,
                        record.timestamp,
                        record.exit_code,
                        HistorySource::Session,
                    ));
                }
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
                let ok = open
                    && rows >= 1
                    && text.as_deref() == Some("echo HISTORY_OK")
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
            let base = appearance.base_font().size;
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
                    1.0 - percent as f64 / 100.0
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
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            preferences.present(&window);
            let preferences2 = preferences.clone();
            let app2 = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(700), move || {
                let visible = preferences2.is_visible();
                eprintln!(
                    "termust-prefs-test: visible={visible} temas={} {}",
                    preferences2.theme_count(),
                    if visible { "ok" } else { "FAIL" }
                );
                preferences2.close();
                if !visible {
                    std::process::exit(1);
                }
                app2.quit();
            });
        });
    }

    let read_only = std::env::var_os("TERMUST_SESSION_NO_RESTORE_TEST").is_some();
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
        .title("No se pudo iniciar el terminal")
        .description(message)
        .build();
    toolbar.set_content(Some(&page));
}

fn restore_preference() -> bool {
    let Some(source) = gtk::gio::SettingsSchemaSource::default() else {
        return true;
    };
    if source.lookup(termust_core::APP_ID, true).is_none() {
        return true;
    }
    gtk::gio::Settings::new(termust_core::APP_ID).boolean("restore-session")
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
                toast.add_toast(libadwaita::Toast::new(
                    "El comando es multilínea y el shell no soporta bracketed paste.",
                ));
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
