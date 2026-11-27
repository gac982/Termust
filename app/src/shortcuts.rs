//! Panel de atajos editable y persistencia en GSettings.

use crate::i18n;
use gtk4 as gtk;
use gtk4::gdk;
use gtk4::gio;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use termust_core::keymap::{display_chord, Action, Chord, KeyMap, KeyMapError, Mods};

const SECTIONS: &[&str] = &["Tabs", "Panes", "Terminal", "Application"];

fn section_of(action: Action) -> &'static str {
    match action {
        Action::NewTab
        | Action::CloseTab
        | Action::NextTab
        | Action::PrevTab
        | Action::MoveTabLeft
        | Action::MoveTabRight => "Tabs",
        Action::SplitPane(_) | Action::FocusPane(_) | Action::ResizePane(_) => "Panes",
        Action::Copy | Action::Paste | Action::InsertNewline => "Terminal",
        _ => "Application",
    }
}

pub fn action_title(action: Action) -> String {
    i18n::tr(match action {
        Action::NewTab => "New tab",
        Action::CloseTab => "Close tab",
        Action::NextTab => "Next tab",
        Action::PrevTab => "Previous tab",
        Action::MoveTabLeft => "Move tab left",
        Action::MoveTabRight => "Move tab right",
        Action::SplitPane(termust_core::tabs::SplitOrientation::Horizontal) => "Split pane right",
        Action::SplitPane(termust_core::tabs::SplitOrientation::Vertical) => "Split pane down",
        Action::FocusPane(termust_core::tabs::Direction::Left) => "Focus pane left",
        Action::FocusPane(termust_core::tabs::Direction::Right) => "Focus pane right",
        Action::FocusPane(termust_core::tabs::Direction::Up) => "Focus pane up",
        Action::FocusPane(termust_core::tabs::Direction::Down) => "Focus pane down",
        Action::ResizePane(termust_core::tabs::Direction::Left) => "Resize left",
        Action::ResizePane(termust_core::tabs::Direction::Right) => "Resize right",
        Action::ResizePane(termust_core::tabs::Direction::Up) => "Resize up",
        Action::ResizePane(termust_core::tabs::Direction::Down) => "Resize down",
        Action::Copy => "Copy",
        Action::Paste => "Paste",
        Action::InsertNewline => "New line",
        Action::RenameTab => "Rename tab",
        Action::ZoomIn => "Zoom in",
        Action::ZoomOut => "Zoom out",
        Action::ZoomReset => "Reset zoom",
        Action::ToggleFiles => "File panel",
        Action::ToggleVerticalTabs => "Vertical tabs",
        Action::ToggleGit => "Git",
        Action::Fullscreen => "Fullscreen",
        Action::Preferences => "Preferences",
        Action::Shortcuts => "Keyboard shortcuts",
        Action::Activate(_) | Action::ActivateLast => "Activate tab",
    })
}

/// `None` cuando el esquema no está instalado (por ejemplo al correr el
/// binario de desarrollo sin GSETTINGS_SCHEMA_DIR).
fn settings() -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(termust_core::APP_ID, true))
        .map(|_| gio::Settings::new(termust_core::APP_ID))
}

pub fn load() -> KeyMap {
    let mut map = KeyMap::new();
    let Some(settings) = settings() else {
        return map;
    };
    let stored = settings.get::<HashMap<String, String>>("shortcuts");
    let mut overrides: Vec<(String, String)> = stored.into_iter().collect();
    overrides.sort();
    map.apply_overrides(&overrides);
    map
}

pub fn save(map: &KeyMap) {
    let Some(settings) = settings() else {
        return;
    };
    let stored: HashMap<String, String> = map.customized_overrides().into_iter().collect();
    let _ = settings.set("shortcuts", stored);
}

fn error_text(error: KeyMapError) -> String {
    match error {
        KeyMapError::TooPlain => i18n::tr("Use Ctrl, Alt or a function key"),
        KeyMapError::Reserved => i18n::tr("Escape can't be used as a shortcut"),
        KeyMapError::Conflict(other) => i18n::tr_args(
            "That shortcut is already used by \"{action}\"",
            &[("action", &action_title(other))],
        ),
    }
}

pub fn present(window: &gtk::Window, map: Rc<RefCell<KeyMap>>) -> adw::Dialog {
    let dialog = adw::Dialog::new();
    dialog.set_title(&i18n::tr("Keyboard shortcuts"));
    dialog.set_content_width(560);
    dialog.set_content_height(620);

    let title = gtk::Label::new(Some(&i18n::tr("Keyboard shortcuts")));
    title.add_css_class("heading");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    let reset_all = gtk::Button::with_label(&i18n::tr("Reset all"));
    reset_all.add_css_class("flat");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&title);
    header.append(&reset_all);

    let hint = gtk::Label::new(Some(&i18n::tr(
        "Click a shortcut and press the new combination",
    )));
    hint.set_xalign(0.0);
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");

    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&list)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&header);
    content.append(&hint);
    content.append(&scroll);
    dialog.set_child(Some(&content));

    let rebuild = rebuild_fn(&list, map.clone(), window.clone());
    rebuild();

    {
        let map = map.clone();
        let rebuild = rebuild.clone();
        reset_all.connect_clicked(move |_| {
            map.borrow_mut().reset_all();
            save(&map.borrow());
            rebuild();
        });
    }

    dialog.present(Some(window));
    dialog
}

fn rebuild_fn(list: &gtk::ListBox, map: Rc<RefCell<KeyMap>>, window: gtk::Window) -> Rc<dyn Fn()> {
    let list = list.clone();
    let weak = Rc::new(RefCell::new(None::<std::rc::Weak<dyn Fn()>>));
    let weak_for_closure = weak.clone();
    let rebuild: Rc<dyn Fn()> = Rc::new(move || {
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        for section in SECTIONS {
            let header = gtk::Label::new(Some(&i18n::tr(section)));
            header.set_xalign(0.0);
            header.add_css_class("heading");
            header.set_margin_top(6);
            header.set_margin_start(6);
            list.append(&header);

            let entries: Vec<(Action, String, bool)> = {
                let map = map.borrow();
                map.bindings()
                    .iter()
                    .filter(|binding| section_of(binding.action) == *section)
                    .map(|binding| {
                        (
                            binding.action,
                            display_chord(binding.chord),
                            map.is_default(binding.action),
                        )
                    })
                    .collect()
            };

            for (action, chord, is_default) in entries {
                let row = adw::ActionRow::builder()
                    .title(action_title(action))
                    .activatable(true)
                    .build();

                let suffix = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let chord_label = gtk::Label::new(Some(&chord));
                chord_label.add_css_class("monospace");
                chord_label.add_css_class("dim-label");
                suffix.append(&chord_label);
                if !is_default {
                    let reset = gtk::Button::from_icon_name("edit-undo-symbolic");
                    reset.add_css_class("flat");
                    reset.set_tooltip_text(Some(&i18n::tr("Restore default")));
                    let map_reset = map.clone();
                    let weak = weak_for_closure.clone();
                    reset.connect_clicked(move |_| {
                        map_reset.borrow_mut().reset(action);
                        save(&map_reset.borrow());
                        if let Some(rebuild) = weak.borrow().as_ref().and_then(|w| w.upgrade()) {
                            rebuild();
                        }
                    });
                    suffix.append(&reset);
                }
                let edit = gtk::Button::from_icon_name("document-edit-symbolic");
                edit.add_css_class("flat");
                edit.set_tooltip_text(Some(&i18n::tr("Change shortcut")));
                {
                    let map_edit = map.clone();
                    let window = window.clone();
                    let weak = weak_for_closure.clone();
                    edit.connect_clicked(move |_| {
                        let weak = weak.clone();
                        prompt_chord(
                            &window,
                            action,
                            map_edit.clone(),
                            Rc::new(move || {
                                if let Some(rebuild) =
                                    weak.borrow().as_ref().and_then(|w| w.upgrade())
                                {
                                    rebuild();
                                }
                            }),
                        );
                    });
                }
                suffix.append(&edit);
                row.add_suffix(&suffix);
                list.append(&row);
            }
        }
    });
    *weak.borrow_mut() = Some(Rc::downgrade(&rebuild));
    rebuild
}

fn prompt_chord(
    window: &gtk::Window,
    action: Action,
    map: Rc<RefCell<KeyMap>>,
    on_done: Rc<dyn Fn()>,
) {
    let dialog = adw::Dialog::new();
    dialog.set_title(&i18n::tr("New shortcut"));
    dialog.set_content_width(360);

    let title = gtk::Label::new(Some(&action_title(action)));
    title.add_css_class("heading");
    let hint = gtk::Label::new(Some(&i18n::tr("Press the new combination")));
    hint.add_css_class("dim-label");
    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.set_visible(false);
    error.set_wrap(true);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(&title);
    content.append(&hint);
    content.append(&error);
    dialog.set_child(Some(&content));

    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let dialog = dialog.clone();
        let error = error.clone();
        let map = map.clone();
        let on_done = on_done.clone();
        key.connect_key_pressed(move |_, keyval, _, state| {
            if keyval == gdk::Key::Escape {
                dialog.close();
                return gtk::glib::Propagation::Stop;
            }
            let Some(key) = crate::map_key(keyval) else {
                return gtk::glib::Propagation::Proceed;
            };
            let chord: Chord = termust_core::keymap::KeyEvent::new(
                key,
                Mods {
                    ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
                    shift: state.contains(gdk::ModifierType::SHIFT_MASK),
                    alt: state.contains(gdk::ModifierType::ALT_MASK),
                    logo: state.contains(gdk::ModifierType::SUPER_MASK),
                },
            );
            match map.borrow_mut().set(action, chord) {
                Ok(()) => {
                    save(&map.borrow());
                    on_done();
                    dialog.close();
                }
                Err(e) => {
                    error.set_text(&error_text(e));
                    error.set_visible(true);
                }
            }
            gtk::glib::Propagation::Stop
        });
    }
    dialog.add_controller(key);
    dialog.present(Some(window));
}
