use crate::tabs::{Direction, SplitOrientation};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    F(u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub logo: bool,
}

impl Mods {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn ctrl() -> Self {
        Self {
            ctrl: true,
            ..Self::default()
        }
    }

    pub fn ctrl_shift() -> Self {
        Self {
            ctrl: true,
            shift: true,
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
}

impl KeyEvent {
    pub fn new(key: Key, mods: Mods) -> Self {
        Self { key, mods }
    }
}

/// Combinación de teclas editable (formato propio, sin depender de GTK).
pub type Chord = KeyEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Terminal,
    Chrome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    MoveTabLeft,
    MoveTabRight,
    Activate(usize),
    ActivateLast,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Preferences,
    ToggleFiles,
    ToggleVerticalTabs,
    ToggleGit,
    Fullscreen,
    Shortcuts,
    RenameTab,
    SplitPane(SplitOrientation),
    FocusPane(Direction),
    ResizePane(Direction),
    Copy,
    Paste,
    InsertNewline,
}

/// Acciones que el usuario puede reasignar desde el panel de atajos, en el
/// orden en que se muestran. `Activate(n)`/`ActivateLast` no están: los
/// Ctrl+1…9 quedan fijos.
pub const EDITABLE_ACTIONS: &[Action] = &[
    Action::NewTab,
    Action::CloseTab,
    Action::NextTab,
    Action::PrevTab,
    Action::MoveTabLeft,
    Action::MoveTabRight,
    Action::SplitPane(SplitOrientation::Horizontal),
    Action::SplitPane(SplitOrientation::Vertical),
    Action::FocusPane(Direction::Left),
    Action::FocusPane(Direction::Right),
    Action::FocusPane(Direction::Up),
    Action::FocusPane(Direction::Down),
    Action::ResizePane(Direction::Left),
    Action::ResizePane(Direction::Right),
    Action::ResizePane(Direction::Up),
    Action::ResizePane(Direction::Down),
    Action::Copy,
    Action::Paste,
    Action::InsertNewline,
    Action::RenameTab,
    Action::ZoomIn,
    Action::ZoomOut,
    Action::ZoomReset,
    Action::ToggleFiles,
    Action::ToggleVerticalTabs,
    Action::ToggleGit,
    Action::Fullscreen,
    Action::Preferences,
    Action::Shortcuts,
];

impl Action {
    /// Identificador estable para guardar los atajos.
    pub fn id(self) -> String {
        match self {
            Action::NewTab => "new_tab".into(),
            Action::CloseTab => "close_tab".into(),
            Action::NextTab => "next_tab".into(),
            Action::PrevTab => "prev_tab".into(),
            Action::MoveTabLeft => "move_tab_left".into(),
            Action::MoveTabRight => "move_tab_right".into(),
            Action::Activate(_) | Action::ActivateLast => "activate".into(),
            Action::ZoomIn => "zoom_in".into(),
            Action::ZoomOut => "zoom_out".into(),
            Action::ZoomReset => "zoom_reset".into(),
            Action::Preferences => "preferences".into(),
            Action::ToggleFiles => "toggle_files".into(),
            Action::ToggleVerticalTabs => "toggle_vertical_tabs".into(),
            Action::ToggleGit => "toggle_git".into(),
            Action::Fullscreen => "fullscreen".into(),
            Action::Shortcuts => "shortcuts".into(),
            Action::RenameTab => "rename_tab".into(),
            Action::SplitPane(SplitOrientation::Horizontal) => "split_horizontal".into(),
            Action::SplitPane(SplitOrientation::Vertical) => "split_vertical".into(),
            Action::FocusPane(Direction::Left) => "focus_left".into(),
            Action::FocusPane(Direction::Right) => "focus_right".into(),
            Action::FocusPane(Direction::Up) => "focus_up".into(),
            Action::FocusPane(Direction::Down) => "focus_down".into(),
            Action::ResizePane(Direction::Left) => "resize_left".into(),
            Action::ResizePane(Direction::Right) => "resize_right".into(),
            Action::ResizePane(Direction::Up) => "resize_up".into(),
            Action::ResizePane(Direction::Down) => "resize_down".into(),
            Action::Copy => "copy".into(),
            Action::Paste => "paste".into(),
            Action::InsertNewline => "insert_newline".into(),
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        EDITABLE_ACTIONS
            .iter()
            .copied()
            .find(|action| action.id() == id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyMapError {
    /// El atajo no usa modificador ni es una tecla de función.
    TooPlain,
    /// Escape se reserva para cancelar la captura.
    Reserved,
    /// Ya lo usa otra acción.
    Conflict(Action),
}

impl fmt::Display for KeyMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyMapError::TooPlain => write!(f, "shortcut needs Ctrl, Alt, Super or a function key"),
            KeyMapError::Reserved => write!(f, "Escape cannot be used as a shortcut"),
            KeyMapError::Conflict(action) => write!(f, "shortcut already used by {action:?}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub action: Action,
    pub chord: Chord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyMap {
    bindings: Vec<Binding>,
}

impl Default for KeyMap {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyMap {
    pub fn new() -> Self {
        let bindings = EDITABLE_ACTIONS
            .iter()
            .copied()
            .filter_map(|action| default_chord(action).map(|chord| Binding { action, chord }))
            .collect();
        Self { bindings }
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn chord(&self, action: Action) -> Option<Chord> {
        self.bindings
            .iter()
            .find(|binding| binding.action == action)
            .map(|binding| binding.chord)
    }

    /// ¿Sigue el atajo predeterminado de fábrica?
    pub fn is_default(&self, action: Action) -> bool {
        self.chord(action) == default_chord(action)
    }

    pub fn any_customized(&self) -> bool {
        self.bindings
            .iter()
            .any(|binding| !self.is_default(binding.action))
    }

    /// Aplica overrides guardados (`id → combinación`). Ignora lo inválido.
    pub fn apply_overrides(&mut self, overrides: &[(String, String)]) {
        for (id, text) in overrides {
            let Some(action) = Action::from_id(id) else {
                continue;
            };
            let Some(chord) = parse_chord(text) else {
                continue;
            };
            if self.validate(action, chord).is_ok() {
                self.assign(action, chord);
            }
        }
    }

    /// `true` si el atajo está personalizado (para guardarlo en GSettings).
    pub fn customized_overrides(&self) -> Vec<(String, String)> {
        self.bindings
            .iter()
            .filter(|binding| !self.is_default(binding.action))
            .map(|binding| (binding.action.id(), format_chord(binding.chord)))
            .collect()
    }

    pub fn set(&mut self, action: Action, chord: Chord) -> Result<(), KeyMapError> {
        self.validate(action, chord)?;
        self.assign(action, chord);
        Ok(())
    }

    pub fn reset(&mut self, action: Action) {
        if let Some(default) = default_chord(action) {
            self.assign(action, default);
        }
    }

    pub fn reset_all(&mut self) {
        for binding in &mut self.bindings {
            if let Some(default) = default_chord(binding.action) {
                binding.chord = default;
            }
        }
    }

    pub fn validate(&self, action: Action, chord: Chord) -> Result<(), KeyMapError> {
        if chord.key == Key::Escape {
            return Err(KeyMapError::Reserved);
        }
        // Super queda para el escritorio; Alt solo tampoco se usa.
        if chord.mods.logo
            || (!chord.mods.ctrl && !chord.mods.alt && !matches!(chord.key, Key::F(_)))
        {
            // Shift+Enter es la excepción: inserta un salto de línea.
            if !(chord.mods.shift && chord.key == Key::Enter) {
                return Err(KeyMapError::TooPlain);
            }
        }
        if let Some(other) = self.binding_for(chord, action) {
            return Err(KeyMapError::Conflict(other.action));
        }
        for focus in [Focus::Terminal, Focus::Chrome] {
            if let Some(other) = fixed_binding(chord, focus) {
                if other != action {
                    return Err(KeyMapError::Conflict(other));
                }
            }
        }
        Ok(())
    }

    fn binding_for(&self, chord: Chord, except: Action) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|binding| binding.action != except && chord_matches(binding.chord, chord))
    }

    fn assign(&mut self, action: Action, chord: Chord) {
        if let Some(binding) = self
            .bindings
            .iter_mut()
            .find(|binding| binding.action == action)
        {
            binding.chord = chord;
        }
    }

    /// Resuelve una tecla. `None` significa que va al terminal/TUI.
    pub fn resolve(&self, ev: KeyEvent, focus: Focus) -> Option<Action> {
        // Sin atajos Alt-only ni Super-only (conflictos con otras apps).
        if ev.mods.logo || (ev.mods.alt && !ev.mods.ctrl) {
            return None;
        }
        if let Some(action) = fixed_binding(ev, focus) {
            return Some(action);
        }
        self.bindings
            .iter()
            .find(|binding| chord_matches(binding.chord, ev))
            .map(|binding| binding.action)
    }
}

/// Atajos que no se editan: Ctrl+1…9 y Ctrl+W en el marco de la ventana.
fn fixed_binding(ev: KeyEvent, focus: Focus) -> Option<Action> {
    if ev.mods == Mods::ctrl() {
        if let Key::Char(c) = ev.key {
            if ('1'..='8').contains(&c) {
                return Some(Action::Activate((c as u8 - b'0') as usize));
            }
            if c == '9' {
                return Some(Action::ActivateLast);
            }
        }
        if focus == Focus::Chrome && ev.key == Key::Char('w') {
            return Some(Action::CloseTab);
        }
    }
    None
}

fn chord_matches(binding: Chord, ev: KeyEvent) -> bool {
    let same_key = match (binding.key, ev.key) {
        (Key::Char(a), Key::Char(b)) => a.eq_ignore_ascii_case(&b),
        (a, b) => a == b,
    };
    if same_key && binding.mods == ev.mods {
        return true;
    }
    // En muchos teclados `+` se escribe con Shift; aceptar ambas variantes.
    let plus = |key: Key| matches!(key, Key::Char('+') | Key::Char('='));
    if plus(binding.key) && plus(ev.key) {
        let mut a = binding.mods;
        let mut b = ev.mods;
        a.shift = false;
        b.shift = false;
        return a == b;
    }
    false
}

fn default_chord(action: Action) -> Option<Chord> {
    let ctrl_shift = Mods::ctrl_shift();
    let ctrl = Mods::ctrl();
    let ctrl_shift_alt = Mods {
        ctrl: true,
        shift: true,
        alt: true,
        logo: false,
    };
    let none = Mods::none();
    let shift = Mods {
        shift: true,
        ..Mods::default()
    };
    let chord = match action {
        Action::NewTab => KeyEvent::new(Key::Char('t'), ctrl_shift),
        Action::CloseTab => KeyEvent::new(Key::Char('w'), ctrl_shift),
        Action::NextTab => KeyEvent::new(Key::PageDown, ctrl),
        Action::PrevTab => KeyEvent::new(Key::PageUp, ctrl),
        Action::MoveTabLeft => KeyEvent::new(Key::PageUp, ctrl_shift),
        Action::MoveTabRight => KeyEvent::new(Key::PageDown, ctrl_shift),
        Action::SplitPane(SplitOrientation::Horizontal) => {
            KeyEvent::new(Key::Char('d'), ctrl_shift)
        }
        Action::SplitPane(SplitOrientation::Vertical) => KeyEvent::new(Key::Char('e'), ctrl_shift),
        Action::FocusPane(Direction::Left) => KeyEvent::new(Key::Left, ctrl_shift),
        Action::FocusPane(Direction::Right) => KeyEvent::new(Key::Right, ctrl_shift),
        Action::FocusPane(Direction::Up) => KeyEvent::new(Key::Up, ctrl_shift),
        Action::FocusPane(Direction::Down) => KeyEvent::new(Key::Down, ctrl_shift),
        Action::ResizePane(Direction::Left) => KeyEvent::new(Key::Left, ctrl_shift_alt),
        Action::ResizePane(Direction::Right) => KeyEvent::new(Key::Right, ctrl_shift_alt),
        Action::ResizePane(Direction::Up) => KeyEvent::new(Key::Up, ctrl_shift_alt),
        Action::ResizePane(Direction::Down) => KeyEvent::new(Key::Down, ctrl_shift_alt),
        Action::Copy => KeyEvent::new(Key::Char('c'), ctrl_shift),
        Action::Paste => KeyEvent::new(Key::Char('v'), ctrl_shift),
        Action::InsertNewline => KeyEvent::new(Key::Enter, shift),
        Action::RenameTab => KeyEvent::new(Key::F(2), none),
        Action::ZoomIn => KeyEvent::new(Key::Char('+'), ctrl),
        Action::ZoomOut => KeyEvent::new(Key::Char('-'), ctrl),
        Action::ZoomReset => KeyEvent::new(Key::Char('0'), ctrl),
        Action::ToggleFiles => KeyEvent::new(Key::F(9), none),
        Action::ToggleVerticalTabs => KeyEvent::new(Key::Char('l'), ctrl_shift),
        Action::ToggleGit => KeyEvent::new(Key::Char('g'), ctrl_shift),
        Action::Fullscreen => KeyEvent::new(Key::F(11), none),
        Action::Preferences => KeyEvent::new(Key::Char(','), ctrl),
        Action::Shortcuts => KeyEvent::new(Key::Char('?'), ctrl_shift),
        Action::Activate(_) | Action::ActivateLast => return None,
    };
    Some(chord)
}

/// Formato de guardado: `ctrl+shift+t`, `f9`, `shift+enter`.
pub fn format_chord(chord: Chord) -> String {
    let mut parts = Vec::new();
    if chord.mods.ctrl {
        parts.push("ctrl".to_string());
    }
    if chord.mods.shift {
        parts.push("shift".to_string());
    }
    if chord.mods.alt {
        parts.push("alt".to_string());
    }
    if chord.mods.logo {
        parts.push("super".to_string());
    }
    parts.push(key_name(chord.key));
    parts.join("+")
}

pub fn parse_chord(text: &str) -> Option<Chord> {
    let mut mods = Mods::default();
    let mut key = None;
    for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" => mods.alt = true,
            "super" | "logo" => mods.logo = true,
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(parse_key(other)?);
            }
        }
    }
    Some(KeyEvent::new(key?, mods))
}

/// Texto para mostrar en el panel (`Ctrl+Shift+T`).
pub fn display_chord(chord: Chord) -> String {
    let mut parts = Vec::new();
    if chord.mods.ctrl {
        parts.push("Ctrl".to_string());
    }
    if chord.mods.shift {
        parts.push("Shift".to_string());
    }
    if chord.mods.alt {
        parts.push("Alt".to_string());
    }
    if chord.mods.logo {
        parts.push("Super".to_string());
    }
    parts.push(key_display(chord.key));
    parts.join("+")
}

fn key_name(key: Key) -> String {
    match key {
        Key::Char('+') => "plus".into(),
        Key::Char('-') => "minus".into(),
        Key::Char(',') => "comma".into(),
        Key::Char('?') => "question".into(),
        Key::Char(c) => c.to_ascii_lowercase().to_string(),
        Key::Enter => "enter".into(),
        Key::Tab => "tab".into(),
        Key::Escape => "esc".into(),
        Key::Up => "up".into(),
        Key::Down => "down".into(),
        Key::Left => "left".into(),
        Key::Right => "right".into(),
        Key::PageUp => "pageup".into(),
        Key::PageDown => "pagedown".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::F(n) => format!("f{n}"),
    }
}

fn key_display(key: Key) -> String {
    match key {
        Key::Char(c) => c.to_ascii_uppercase().to_string(),
        Key::Enter => "Enter".into(),
        Key::Tab => "Tab".into(),
        Key::Escape => "Esc".into(),
        Key::Up => "Up".into(),
        Key::Down => "Down".into(),
        Key::Left => "Left".into(),
        Key::Right => "Right".into(),
        Key::PageUp => "PageUp".into(),
        Key::PageDown => "PageDown".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
        Key::F(n) => format!("F{n}"),
    }
}

fn parse_key(name: &str) -> Option<Key> {
    let key = match name {
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "pageup" | "page_up" => Key::PageUp,
        "pagedown" | "page_down" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        "plus" => Key::Char('+'),
        "minus" => Key::Char('-'),
        "comma" => Key::Char(','),
        "question" => Key::Char('?'),
        other => {
            if let Some(n) = other.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                if (1..=12).contains(&n) {
                    Key::F(n)
                } else {
                    return None;
                }
            } else {
                let mut chars = other.chars();
                let ch = chars.next()?;
                if chars.next().is_some() || ch.is_control() {
                    return None;
                }
                Key::Char(ch)
            }
        }
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key, mods: Mods) -> Option<Action> {
        KeyMap::new().resolve(KeyEvent::new(key, mods), Focus::Terminal)
    }

    #[test]
    fn ctrl_numbers_are_fixed() {
        assert_eq!(
            press(Key::Char('1'), Mods::ctrl()),
            Some(Action::Activate(1))
        );
        assert_eq!(
            press(Key::Char('9'), Mods::ctrl()),
            Some(Action::ActivateLast)
        );
        let mut map = KeyMap::new();
        let err = map.set(Action::Copy, KeyEvent::new(Key::Char('3'), Mods::ctrl()));
        assert_eq!(err, Err(KeyMapError::Conflict(Action::Activate(3))));
    }

    #[test]
    fn defaults_resolve_like_before() {
        assert_eq!(
            press(Key::Char('T'), Mods::ctrl_shift()),
            Some(Action::NewTab)
        );
        assert_eq!(
            press(Key::Char('w'), Mods::ctrl_shift()),
            Some(Action::CloseTab)
        );
        assert_eq!(
            press(Key::Char('c'), Mods::ctrl_shift()),
            Some(Action::Copy)
        );
        assert_eq!(
            press(Key::Char('v'), Mods::ctrl_shift()),
            Some(Action::Paste)
        );
        assert_eq!(
            press(Key::Char('?'), Mods::ctrl_shift()),
            Some(Action::Shortcuts)
        );
        assert_eq!(press(Key::F(9), Mods::none()), Some(Action::ToggleFiles));
        assert_eq!(press(Key::F(11), Mods::none()), Some(Action::Fullscreen));
        assert_eq!(press(Key::F(2), Mods::none()), Some(Action::RenameTab));
        assert_eq!(
            press(
                Key::Enter,
                Mods {
                    shift: true,
                    ..Mods::default()
                }
            ),
            Some(Action::InsertNewline)
        );
    }

    #[test]
    fn ctrl_w_only_closes_in_chrome() {
        let ev = KeyEvent::new(Key::Char('w'), Mods::ctrl());
        assert_eq!(KeyMap::new().resolve(ev, Focus::Terminal), None);
        assert_eq!(
            KeyMap::new().resolve(ev, Focus::Chrome),
            Some(Action::CloseTab)
        );
    }

    #[test]
    fn custom_binding_replaces_the_default() {
        let mut map = KeyMap::new();
        map.set(Action::NewTab, KeyEvent::new(Key::Char('n'), Mods::ctrl()))
            .unwrap();
        assert_eq!(
            map.resolve(KeyEvent::new(Key::Char('n'), Mods::ctrl()), Focus::Terminal),
            Some(Action::NewTab)
        );
        assert_eq!(
            map.resolve(
                KeyEvent::new(Key::Char('t'), Mods::ctrl_shift()),
                Focus::Terminal
            ),
            None
        );
        assert!(!map.is_default(Action::NewTab));
    }

    #[test]
    fn conflicts_are_rejected() {
        let mut map = KeyMap::new();
        let err = map.set(
            Action::NewTab,
            KeyEvent::new(Key::Char('c'), Mods::ctrl_shift()),
        );
        assert_eq!(err, Err(KeyMapError::Conflict(Action::Copy)));
        // El atajo original sigue intacto.
        assert_eq!(
            map.chord(Action::NewTab),
            Some(KeyEvent::new(Key::Char('t'), Mods::ctrl_shift()))
        );
    }

    #[test]
    fn plain_and_escape_shortcuts_are_rejected() {
        let mut map = KeyMap::new();
        assert_eq!(
            map.set(Action::Copy, KeyEvent::new(Key::Char('x'), Mods::none())),
            Err(KeyMapError::TooPlain)
        );
        assert_eq!(
            map.set(Action::Copy, KeyEvent::new(Key::Escape, Mods::ctrl())),
            Err(KeyMapError::Reserved)
        );
        assert!(map
            .set(Action::Copy, KeyEvent::new(Key::F(5), Mods::none()))
            .is_ok());
    }

    #[test]
    fn reset_restores_defaults() {
        let mut map = KeyMap::new();
        map.set(Action::Copy, KeyEvent::new(Key::F(6), Mods::none()))
            .unwrap();
        assert!(map.any_customized());
        map.reset(Action::Copy);
        assert!(map.is_default(Action::Copy));
        assert!(!map.any_customized());

        map.set(Action::Copy, KeyEvent::new(Key::F(6), Mods::none()))
            .unwrap();
        map.set(Action::Paste, KeyEvent::new(Key::F(7), Mods::none()))
            .unwrap();
        map.reset_all();
        assert!(!map.any_customized());
    }

    #[test]
    fn overrides_round_trip_through_storage_format() {
        let mut map = KeyMap::new();
        map.set(Action::Copy, KeyEvent::new(Key::F(6), Mods::none()))
            .unwrap();
        map.set(
            Action::InsertNewline,
            KeyEvent::new(Key::Char('j'), Mods::ctrl()),
        )
        .unwrap();
        let saved = map.customized_overrides();
        assert_eq!(saved.len(), 2);

        let mut loaded = KeyMap::new();
        loaded.apply_overrides(&saved);
        assert_eq!(
            loaded.chord(Action::Copy),
            Some(KeyEvent::new(Key::F(6), Mods::none()))
        );
        assert_eq!(
            loaded.chord(Action::InsertNewline),
            Some(KeyEvent::new(Key::Char('j'), Mods::ctrl()))
        );
        assert!(loaded.any_customized());
    }

    #[test]
    fn storage_ignores_unknown_and_invalid_entries() {
        let mut map = KeyMap::new();
        map.apply_overrides(&[
            ("nope".into(), "ctrl+x".into()),
            ("copy".into(), "shift+enter".into()), // conflicto con InsertNewline
            ("paste".into(), "ctrl+1".into()),     // fijo
            ("copy".into(), "f8".into()),
        ]);
        assert_eq!(
            map.chord(Action::Copy),
            Some(KeyEvent::new(Key::F(8), Mods::none()))
        );
        assert_eq!(
            map.chord(Action::Paste),
            Some(KeyEvent::new(Key::Char('v'), Mods::ctrl_shift()))
        );
    }

    #[test]
    fn chord_format_round_trip() {
        let chord = KeyEvent::new(Key::Char('t'), Mods::ctrl_shift());
        assert_eq!(format_chord(chord), "ctrl+shift+t");
        assert_eq!(parse_chord("ctrl+shift+t"), Some(chord));
        assert_eq!(
            parse_chord("ctrl+shift+plus"),
            Some(KeyEvent::new(Key::Char('+'), Mods::ctrl_shift()))
        );
        let arrows = KeyEvent::new(
            Key::Left,
            Mods {
                ctrl: true,
                shift: true,
                alt: true,
                logo: false,
            },
        );
        assert_eq!(format_chord(arrows), "ctrl+shift+alt+left");
        assert_eq!(parse_chord(&format_chord(arrows)), Some(arrows));
        assert_eq!(display_chord(chord), "Ctrl+Shift+T");
        assert_eq!(parse_chord("ctrl+nope"), None);
        assert_eq!(parse_chord("ctrl+shift+t+x"), None);
    }

    #[test]
    fn plus_and_equal_are_equivalent() {
        assert_eq!(press(Key::Char('='), Mods::ctrl()), Some(Action::ZoomIn));
        assert_eq!(
            press(Key::Char('+'), Mods::ctrl_shift()),
            Some(Action::ZoomIn)
        );
    }

    #[test]
    fn alt_only_and_super_shortcuts_pass_through() {
        assert_eq!(
            press(
                Key::Char('1'),
                Mods {
                    alt: true,
                    ..Mods::default()
                }
            ),
            None
        );
        assert_eq!(
            press(
                Key::Char('t'),
                Mods {
                    logo: true,
                    ..Mods::default()
                }
            ),
            None
        );
    }

    #[test]
    fn all_editable_actions_have_unique_defaults() {
        let map = KeyMap::new();
        assert_eq!(map.bindings().len(), EDITABLE_ACTIONS.len());
        for (index, binding) in map.bindings().iter().enumerate() {
            let chord = format_chord(binding.chord);
            for other in map.bindings().iter().skip(index + 1) {
                assert_ne!(
                    chord,
                    format_chord(other.chord),
                    "{} y {}",
                    binding.action.id(),
                    other.action.id()
                );
            }
        }
    }
}
