use gtk4::glib;
use libadwaita::prelude::*;
use libadwaita::{Application, ApplicationWindow, HeaderBar, ToolbarView};
use std::mem::forget;
use std::path::PathBuf;
use std::rc::Rc;
use termust_core::pty::{PtySpawner, SpawnSpec};
use termust_core::terminal::Size;
use termust_pty_unix::UnixPtySpawner;
use termust_render_cairo::CairoRenderer;
use termust_term_engine::AlacrittyEngine;
use termust_ui::TerminalView;

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

    match start_terminal(&toolbar) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("termust: failed to start terminal: {e}");
            show_error(&toolbar, &e);
        }
    }

    window.present();
}

fn start_terminal(toolbar: &ToolbarView) -> Result<(), String> {
    let spawner = Rc::new(UnixPtySpawner::new().map_err(|e| e.to_string())?);
    let shell = termust_host_exec::default_shell();
    let integration = termust_host_exec::materialize().map_err(|e| e.to_string())?;
    let command =
        termust_host_exec::shell_command(&shell, &integration, termust_host_exec::is_flatpak());

    let cwd = std::env::var_os("HOME").map(PathBuf::from);
    let size = Size::new(80, 24);
    let spec = SpawnSpec {
        argv: command.argv,
        cwd,
        env: command.env,
        size,
    };
    let handle = spawner.spawn(spec).map_err(|e| e.to_string())?;

    let engine = AlacrittyEngine::new(size, 2_000);
    let renderer = CairoRenderer::new();
    let view = TerminalView::new(
        Box::new(engine),
        handle.session,
        Box::new(renderer),
        handle.events,
    );

    toolbar.set_content(Some(view.widget()));
    view.grab_focus();

    // El spawner posee el hilo IO y debe vivir tanto como la app.
    forget(spawner);
    Ok(())
}

fn show_error(toolbar: &ToolbarView, message: &str) {
    let page = libadwaita::StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title("No se pudo iniciar el shell")
        .description(message)
        .build();
    toolbar.set_content(Some(&page));
}
