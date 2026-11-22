//! Diálogo de preferencias (HIG de Adwaita) para tema, transparencia y zoom.

use gtk::gio;
use gtk4 as gtk;
use libadwaita::prelude::*;
use libadwaita::{
    ActionRow, ComboRow, PreferencesDialog, PreferencesGroup, PreferencesPage, SpinRow,
};
use std::cell::Cell;
use std::rc::{Rc, Weak};

use crate::appearance::{display_name, Appearance, AUTO_THEME};

pub struct PreferencesController {
    dialog: PreferencesDialog,
    appearance: Rc<Appearance>,
    theme_ids: Vec<String>,
    dark_ids: Vec<String>,
    light_ids: Vec<String>,
    theme_row: ComboRow,
    dark_row: ComboRow,
    light_row: ComboRow,
    opacity_row: ActionRow,
    opacity_scale: gtk::Scale,
    zoom_row: SpinRow,
    contrast_row: ActionRow,
    warning: gtk::Label,
    updating: Cell<bool>,
}

impl PreferencesController {
    pub fn new(appearance: Rc<Appearance>) -> Rc<Self> {
        let dialog = PreferencesDialog::new();
        dialog.set_title("Preferencias");

        let page = PreferencesPage::builder()
            .title("Apariencia")
            .icon_name("applications-graphics-symbolic")
            .build();

        let theme_group = PreferencesGroup::builder().title("Tema").build();
        let theme_ids: Vec<String> = std::iter::once(AUTO_THEME.to_string())
            .chain(appearance.themes().iter().map(|theme| theme.id.clone()))
            .collect();
        let names: Vec<String> = std::iter::once("Seguir al sistema".to_string())
            .chain(appearance.themes().iter().map(display_name))
            .collect();
        let strings = gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let theme_row = ComboRow::builder()
            .title("Tema")
            .subtitle("Sigue al sistema o fuerza una paleta")
            .model(&strings)
            .build();

        let dark_themes: Vec<_> = appearance
            .themes()
            .iter()
            .filter(|theme| theme.variant == termust_core::theme::ThemeVariant::Dark)
            .collect();
        let light_themes: Vec<_> = appearance
            .themes()
            .iter()
            .filter(|theme| theme.variant == termust_core::theme::ThemeVariant::Light)
            .collect();
        let dark_ids: Vec<String> = dark_themes.iter().map(|theme| theme.id.clone()).collect();
        let light_ids: Vec<String> = light_themes.iter().map(|theme| theme.id.clone()).collect();
        let dark_names: Vec<String> = dark_themes.iter().map(|theme| theme.name.clone()).collect();
        let light_names: Vec<String> = light_themes
            .iter()
            .map(|theme| theme.name.clone())
            .collect();
        let dark_strings =
            gtk::StringList::new(&dark_names.iter().map(String::as_str).collect::<Vec<_>>());
        let light_strings =
            gtk::StringList::new(&light_names.iter().map(String::as_str).collect::<Vec<_>>());
        let dark_row = ComboRow::builder()
            .title("Tema oscuro")
            .subtitle("Se usa cuando el sistema está en modo oscuro")
            .model(&dark_strings)
            .build();
        let light_row = ComboRow::builder()
            .title("Tema claro")
            .subtitle("Se usa cuando el sistema está en modo claro")
            .model(&light_strings)
            .build();
        dark_row.set_sensitive(false);
        light_row.set_sensitive(false);
        theme_group.add(&theme_row);
        theme_group.add(&dark_row);
        theme_group.add(&light_row);
        page.add(&theme_group);

        let contrast_group = PreferencesGroup::builder().title("Contraste").build();
        let contrast_row = ActionRow::builder().title("Contraste del tema").build();
        contrast_group.add(&contrast_row);
        page.add(&contrast_group);

        let transparency_group = PreferencesGroup::builder()
            .title("Transparencia")
            .description("Solo el fondo. Texto, cursor y paneles siguen opacos; no hay desenfoque.")
            .build();
        let opacity_row = ActionRow::builder()
            .title("Transparencia del fondo")
            .subtitle("Solo el fondo; sin desenfoque")
            .build();
        let opacity_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 5.0);
        opacity_scale.set_draw_value(true);
        opacity_scale.set_value_pos(gtk::PositionType::Right);
        opacity_scale.set_size_request(200, -1);
        opacity_scale.set_valign(gtk::Align::Center);
        opacity_row.add_suffix(&opacity_scale);
        transparency_group.add(&opacity_row);
        let warning = gtk::Label::new(None);
        warning.set_wrap(true);
        warning.set_xalign(0.0);
        warning.add_css_class("warning");
        warning.set_visible(false);
        warning.set_margin_top(6);
        warning.set_margin_bottom(6);
        warning.set_margin_start(12);
        warning.set_margin_end(12);
        transparency_group.add(&warning);
        page.add(&transparency_group);

        let text_group = PreferencesGroup::builder().title("Texto").build();
        let zoom_row = SpinRow::with_range(0.5, 2.0, 0.1);
        zoom_row.set_title("Zoom de fuente");
        zoom_row.set_subtitle("Ctrl + / Ctrl − / Ctrl 0");
        zoom_row.set_digits(1);
        text_group.add(&zoom_row);
        page.add(&text_group);
        dialog.add(&page);

        if !appearance.import_errors().is_empty() {
            let errors_page = PreferencesPage::builder()
                .title("Temas importados")
                .icon_name("dialog-warning-symbolic")
                .build();
            let group = PreferencesGroup::builder()
                .title("Errores de importación")
                .description("Los archivos de ~/.config/termust/themes se ignoraron.")
                .build();
            for error in appearance.import_errors() {
                let label = gtk::Label::new(Some(error));
                label.set_wrap(true);
                label.set_xalign(0.0);
                label.add_css_class("dim-label");
                label.set_margin_top(4);
                label.set_margin_bottom(4);
                group.add(&label);
            }
            errors_page.add(&group);
            dialog.add(&errors_page);
        }

        let controller = Rc::new(Self {
            dialog,
            appearance: appearance.clone(),
            theme_ids,
            dark_ids,
            light_ids,
            theme_row,
            dark_row,
            light_row,
            opacity_row,
            opacity_scale,
            zoom_row,
            contrast_row,
            warning,
            updating: Cell::new(false),
        });
        controller.connect();
        controller.sync();
        controller.watch(appearance);
        controller
    }

    fn watch(self: &Rc<Self>, appearance: Rc<Appearance>) {
        let weak: Weak<Self> = Rc::downgrade(self);
        appearance.on_change(move || {
            if let Some(controller) = weak.upgrade() {
                controller.sync();
            }
        });
    }

    fn connect(self: &Rc<Self>) {
        {
            let weak = Rc::downgrade(self);
            self.theme_row.connect_selected_notify(move |_| {
                if let Some(controller) = weak.upgrade() {
                    controller.theme_selected();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.dark_row.connect_selected_notify(move |_| {
                if let Some(controller) = weak.upgrade() {
                    if controller.updating.get() {
                        return;
                    }
                    let index = controller.dark_row.selected() as usize;
                    if let Some(id) = controller.dark_ids.get(index) {
                        controller.appearance.set_dark_id(id);
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.light_row.connect_selected_notify(move |_| {
                if let Some(controller) = weak.upgrade() {
                    if controller.updating.get() {
                        return;
                    }
                    let index = controller.light_row.selected() as usize;
                    if let Some(id) = controller.light_ids.get(index) {
                        controller.appearance.set_light_id(id);
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.opacity_scale.connect_value_changed(move |scale| {
                if let Some(controller) = weak.upgrade() {
                    if controller.updating.get() {
                        return;
                    }
                    controller
                        .appearance
                        .set_opacity(scale.value().round() as i32);
                    controller.update_warning();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.zoom_row.connect_value_notify(move |row| {
                if let Some(controller) = weak.upgrade() {
                    if controller.updating.get() {
                        return;
                    }
                    controller.appearance.set_font_scale(row.value());
                }
            });
        }
    }

    fn theme_selected(&self) {
        if self.updating.get() {
            return;
        }
        let index = self.theme_row.selected() as usize;
        match self.theme_ids.get(index) {
            Some(id) if id == AUTO_THEME => self.appearance.set_theme_id(None),
            Some(id) => self.appearance.set_theme_id(Some(id)),
            None => {}
        }
    }

    fn sync(&self) {
        if self.updating.replace(true) {
            return;
        }
        let auto = self.appearance.theme_id() == AUTO_THEME;
        let index = if auto {
            0
        } else {
            self.theme_ids
                .iter()
                .position(|id| *id == self.appearance.theme_id())
                .unwrap_or(0)
        };
        self.theme_row.set_selected(index as u32);

        let dark_index = self
            .dark_ids
            .iter()
            .position(|id| *id == self.appearance.dark_id())
            .unwrap_or(0);
        let light_index = self
            .light_ids
            .iter()
            .position(|id| *id == self.appearance.light_id())
            .unwrap_or(0);
        self.dark_row.set_selected(dark_index as u32);
        self.light_row.set_selected(light_index as u32);
        self.dark_row.set_sensitive(auto);
        self.light_row.set_sensitive(auto);

        self.opacity_scale
            .set_value(self.appearance.opacity() as f64);
        let high = self.appearance.high_contrast();
        self.opacity_scale.set_sensitive(!high);
        self.opacity_row.set_subtitle(if high {
            "Desactivado por alto contraste"
        } else {
            "Solo el fondo; sin desenfoque"
        });
        self.zoom_row.set_value(self.appearance.font_scale());

        let theme = self.appearance.resolved_theme();
        let ratio = self.appearance.theme_contrast(&theme);
        self.contrast_row.set_subtitle(&format!(
            "{} · {ratio:.1}:1 {}",
            theme.name,
            if ratio + f64::EPSILON >= termust_core::theme::MIN_CONTRAST {
                "✓"
            } else {
                "bajo el umbral"
            }
        ));
        self.update_warning();
        self.updating.set(false);
    }

    fn update_warning(&self) {
        let warning = self.appearance.contrast_warning();
        if let Some((ratio, high)) = warning {
            let text = if high {
                format!("Contraste efectivo {ratio:.1}:1 por debajo de 4.5:1.")
            } else {
                format!(
                    "Con esta transparencia el contraste efectivo baja a {ratio:.1}:1 \
                     (mínimo 4.5:1). Se recomienda reducirla."
                )
            };
            self.warning.set_text(&text);
            self.warning.set_visible(true);
        } else {
            self.warning.set_visible(false);
        }
    }

    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.sync();
        self.dialog.present(Some(parent));
    }

    pub fn dialog(&self) -> &PreferencesDialog {
        &self.dialog
    }

    pub fn is_visible(&self) -> bool {
        self.dialog.is_visible()
    }

    pub fn close(&self) {
        self.dialog.close();
    }

    /// Refresca las filas desde el estado actual (para hooks de test).
    pub fn sync_now(&self) {
        self.sync();
    }

    pub fn theme_count(&self) -> usize {
        self.theme_ids.len().saturating_sub(1)
    }

    pub fn opacity_value(&self) -> f64 {
        self.opacity_scale.value()
    }

    pub fn zoom_value(&self) -> f64 {
        self.zoom_row.value()
    }

    pub fn transparency_sensitive(&self) -> bool {
        self.opacity_scale.is_sensitive()
    }

    pub fn warning_visible(&self) -> bool {
        self.warning.is_visible()
    }
}

/// Acción de GSettings para restaurar la sesión, si el esquema está instalado.
pub fn restore_session_switch() -> Option<(libadwaita::SwitchRow, gio::Settings)> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup(termust_core::APP_ID, true)?;
    if !schema.has_key("restore-session") {
        return None;
    }
    let settings = gio::Settings::new(termust_core::APP_ID);
    let row = libadwaita::SwitchRow::builder()
        .title("Restaurar la sesión al iniciar")
        .subtitle("Pestañas, paneles y directorios de la última sesión")
        .active(settings.boolean("restore-session"))
        .build();
    {
        let settings = settings.clone();
        row.connect_active_notify(move |row| {
            let _ = settings.set_boolean("restore-session", row.is_active());
        });
    }
    Some((row, settings))
}

/// Agrega la página «General» al diálogo de preferencias.
pub fn add_session_page(dialog: &PreferencesDialog) {
    let Some((row, _settings)) = restore_session_switch() else {
        return;
    };
    let page = PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();
    let group = PreferencesGroup::builder().title("Sesión").build();
    group.add(&row);
    page.add(&group);
    dialog.add(&page);
}
