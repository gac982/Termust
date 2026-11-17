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
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &Application) {
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
            return;
        }
    };
    let shell = termust_host_exec::default_shell();
    let integration = match termust_host_exec::materialize() {
        Ok(si) => si,
        Err(e) => {
            show_error(&toolbar, &e.to_string());
            window.present();
            return;
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
            return;
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
    toolbar.set_content(Some(manager.borrow().tab_view()));
    manager.borrow().focus_active();

    {
        let manager = manager.clone();
        let home = home.clone();
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

            let mut mgr = manager.borrow_mut();
            match action {
                Action::NewTab => {
                    if let Err(e) = mgr.open_tab(home.clone()) {
                        eprintln!("termust: no se pudo abrir pestaña: {e}");
                    }
                }
                Action::CloseTab => mgr.close_active(),
                Action::NextTab => mgr.select_next(),
                Action::PrevTab => mgr.select_prev(),
                Action::Activate(n) => mgr.select_index(n.saturating_sub(1)),
                Action::ActivateLast => mgr.select_last(),
                Action::MoveTabLeft => mgr.move_active(-1),
                Action::MoveTabRight => mgr.move_active(1),
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

    window.present();
}

fn initial_tabs() -> usize {
    std::env::var("TERMUST_TABS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .clamp(1, 64)
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        r#"
tabbar tab {
  min-width: 110px;
  margin: 3px 2px;
  padding: 0 8px;
  border: none;
  border-radius: 6px;
  background-color: alpha(currentColor, 0.08);
  box-shadow: none;
}
tabbar tab:hover {
  background-color: alpha(currentColor, 0.14);
}
tabbar tab:selected {
  background-color: alpha(@accent_bg_color, 0.45);
  color: @accent_fg_color;
}
tabbar tab > label {
  font-weight: 500;
}
tabbar separator {
  background-color: transparent;
  min-width: 0;
}
.termust-tab-strip {
  margin-left: 6px;
}
"#,
    );
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
