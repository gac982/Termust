mod tabs;

use gtk::gdk;
use gtk::glib;
use gtk4 as gtk;
use libadwaita::prelude::*;
use libadwaita::{Application, ApplicationWindow, HeaderBar, ToolbarView};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use tabs::TabManager;
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
    let slot: Rc<RefCell<Option<ApplicationWindow>>> = Rc::new(RefCell::new(None));
    let activate_slot = slot.clone();
    app.connect_activate(move |app| {
        if let Some(window) = activate_slot.borrow().as_ref() {
            window.present();
            return;
        }
        let window = build_ui(app);
        *activate_slot.borrow_mut() = Some(window);
    });
    app.run()
}

fn build_ui(app: &Application) -> ApplicationWindow {
    let header = HeaderBar::new();
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Termust")
        .default_width(1024)
        .default_height(700)
        .content(&toolbar)
        .build();

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
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));

    let manager = match TabManager::new(spawner, shell, integration, home.clone(), window.clone()) {
        Ok(m) => Rc::new(RefCell::new(m)),
        Err(e) => {
            show_error(&toolbar, &e);
            window.present();
            return window;
        }
    };
    TabManager::connect(&manager);
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

    {
        let group = gtk::gio::SimpleActionGroup::new();
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
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _, state| {
            let Some(key) = map_key(keyval) else {
                return glib::Propagation::Proceed;
            };
            let mods = Mods {
                ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
                shift: state.contains(gdk::ModifierType::SHIFT_MASK),
                alt: state.contains(gdk::ModifierType::ALT_MASK),
                logo: state.contains(gdk::ModifierType::SUPER_MASK),
            };
            let Some(action) = resolve_shortcut(
                KeyEvent::new(key, mods),
                Focus::Terminal,
                &KeyConfig::default(),
            ) else {
                return glib::Propagation::Proceed;
            };

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
        glib::timeout_add_local(Duration::from_millis(500), move || {
            manager.borrow_mut().update_titles();
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
                    "split horizontal 2",
                    mgr.split_active(SplitOrientation::Horizontal).is_ok(),
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
                        app4.quit();
                    });
                    if failed {
                        std::process::exit(1);
                    }
                });
            });
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
