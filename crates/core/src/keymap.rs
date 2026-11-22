use crate::tabs::{Direction, SplitOrientation};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
}

impl KeyEvent {
    pub fn new(key: Key, mods: Mods) -> Self {
        Self { key, mods }
    }
}

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
    Fullscreen,
    Shortcuts,
    RenameTab,
    SplitPane(SplitOrientation),
    FocusPane(Direction),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyConfig {
    pub tab_numbers: bool,
    pub close_tab: bool,
    pub move_tabs: bool,
    pub zoom: bool,
}

impl Default for KeyConfig {
    fn default() -> Self {
        Self {
            tab_numbers: true,
            close_tab: true,
            move_tabs: true,
            zoom: true,
        }
    }
}

/// Resuelve un atajo de la app. Devuelve `None` si la tecla debe ir al terminal/TUI.
pub fn resolve_shortcut(ev: KeyEvent, focus: Focus, cfg: &KeyConfig) -> Option<Action> {
    // Sin atajos Alt-only ni Super-only (conflictos con otras apps). Ctrl+Alt
    // sí se usa para mover el foco entre paneles.
    if ev.mods.logo || (ev.mods.alt && !ev.mods.ctrl) {
        return None;
    }
    let ctrl = ev.mods.ctrl;
    let shift = ev.mods.shift;
    let alt = ev.mods.alt;

    if ctrl && alt && !shift {
        match ev.key {
            Key::Left => return Some(Action::FocusPane(Direction::Left)),
            Key::Right => return Some(Action::FocusPane(Direction::Right)),
            Key::Up => return Some(Action::FocusPane(Direction::Up)),
            Key::Down => return Some(Action::FocusPane(Direction::Down)),
            _ => {}
        }
    }

    if ctrl && shift {
        match ev.key {
            Key::Char(c) if c.eq_ignore_ascii_case(&'t') => return Some(Action::NewTab),
            Key::Char(c) if c.eq_ignore_ascii_case(&'w') && cfg.close_tab => {
                return Some(Action::CloseTab)
            }
            Key::Char(c) if c.eq_ignore_ascii_case(&'d') => {
                return Some(Action::SplitPane(SplitOrientation::Horizontal))
            }
            Key::Char(c) if c.eq_ignore_ascii_case(&'e') => {
                return Some(Action::SplitPane(SplitOrientation::Vertical))
            }
            Key::PageUp if cfg.move_tabs => return Some(Action::MoveTabLeft),
            Key::PageDown if cfg.move_tabs => return Some(Action::MoveTabRight),
            Key::Char('+') | Key::Char('=') if cfg.zoom => return Some(Action::ZoomIn),
            Key::Char('?') => return Some(Action::Shortcuts),
            _ => {}
        }
    }

    if ctrl && !shift && !alt {
        if let Key::Char(c) = ev.key {
            let c = c.to_ascii_lowercase();
            if cfg.tab_numbers {
                if ('1'..='8').contains(&c) {
                    return Some(Action::Activate((c as u8 - b'0') as usize));
                }
                if c == '9' {
                    return Some(Action::ActivateLast);
                }
            }
            if cfg.zoom {
                match c {
                    '+' | '=' => return Some(Action::ZoomIn),
                    '-' => return Some(Action::ZoomOut),
                    '0' => return Some(Action::ZoomReset),
                    _ => {}
                }
            }
            if c == '?' {
                return Some(Action::Shortcuts);
            }
        }
        match ev.key {
            Key::PageUp => return Some(Action::NextTab),
            Key::PageDown => return Some(Action::PrevTab),
            _ => {}
        }
        if focus == Focus::Chrome && cfg.close_tab {
            if let Key::Char(c) = ev.key {
                if c.eq_ignore_ascii_case(&'w') {
                    return Some(Action::CloseTab);
                }
            }
        }
    }

    if ev.mods == Mods::none() && ev.key == Key::F(11) {
        return Some(Action::Fullscreen);
    }
    if ev.mods == Mods::none() && ev.key == Key::F(2) {
        return Some(Action::RenameTab);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key, mods: Mods) -> Option<Action> {
        resolve_shortcut(
            KeyEvent::new(key, mods),
            Focus::Terminal,
            &KeyConfig::default(),
        )
    }

    #[test]
    fn ctrl_numbers_and_last() {
        assert_eq!(
            press(Key::Char('1'), Mods::ctrl()),
            Some(Action::Activate(1))
        );
        assert_eq!(
            press(Key::Char('8'), Mods::ctrl()),
            Some(Action::Activate(8))
        );
        assert_eq!(
            press(Key::Char('9'), Mods::ctrl()),
            Some(Action::ActivateLast)
        );
    }

    #[test]
    fn ctrl_shift_t_and_w() {
        assert_eq!(
            press(Key::Char('t'), Mods::ctrl_shift()),
            Some(Action::NewTab)
        );
        assert_eq!(
            press(Key::Char('T'), Mods::ctrl_shift()),
            Some(Action::NewTab)
        );
        assert_eq!(
            press(Key::Char('w'), Mods::ctrl_shift()),
            Some(Action::CloseTab)
        );
    }

    #[test]
    fn ctrl_w_passes_to_terminal_but_closes_in_chrome() {
        let ev = KeyEvent::new(Key::Char('w'), Mods::ctrl());
        assert_eq!(
            resolve_shortcut(ev, Focus::Terminal, &KeyConfig::default()),
            None
        );
        assert_eq!(
            resolve_shortcut(ev, Focus::Chrome, &KeyConfig::default()),
            Some(Action::CloseTab)
        );
    }

    #[test]
    fn page_keys_next_prev_and_move() {
        assert_eq!(press(Key::PageUp, Mods::ctrl()), Some(Action::NextTab));
        assert_eq!(press(Key::PageDown, Mods::ctrl()), Some(Action::PrevTab));
        assert_eq!(
            press(Key::PageUp, Mods::ctrl_shift()),
            Some(Action::MoveTabLeft)
        );
        assert_eq!(
            press(Key::PageDown, Mods::ctrl_shift()),
            Some(Action::MoveTabRight)
        );
    }

    #[test]
    fn zoom_and_shortcuts() {
        assert_eq!(
            press(Key::Char('+'), Mods::ctrl_shift()),
            Some(Action::ZoomIn)
        );
        assert_eq!(press(Key::Char('-'), Mods::ctrl()), Some(Action::ZoomOut));
        assert_eq!(press(Key::Char('0'), Mods::ctrl()), Some(Action::ZoomReset));
        assert_eq!(
            press(Key::Char('?'), Mods::ctrl_shift()),
            Some(Action::Shortcuts)
        );
        assert_eq!(
            press(Key::Char('?'), Mods::ctrl_shift()),
            Some(Action::Shortcuts)
        );
    }

    #[test]
    fn no_alt_only_shortcuts() {
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        assert_eq!(press(Key::Char('1'), alt), None);
        assert_eq!(press(Key::Up, alt), None);
    }

    #[test]
    fn tab_numbers_can_be_disabled() {
        let cfg = KeyConfig {
            tab_numbers: false,
            ..KeyConfig::default()
        };
        let ev = KeyEvent::new(Key::Char('1'), Mods::ctrl());
        assert_eq!(resolve_shortcut(ev, Focus::Terminal, &cfg), None);
    }

    #[test]
    fn f11_is_fullscreen() {
        assert_eq!(press(Key::F(11), Mods::none()), Some(Action::Fullscreen));
        assert_eq!(press(Key::F(11), Mods::ctrl()), None);
    }

    #[test]
    fn ctrl_shift_d_and_e_split_panes() {
        assert_eq!(
            press(Key::Char('d'), Mods::ctrl_shift()),
            Some(Action::SplitPane(SplitOrientation::Horizontal))
        );
        assert_eq!(
            press(Key::Char('e'), Mods::ctrl_shift()),
            Some(Action::SplitPane(SplitOrientation::Vertical))
        );
    }

    #[test]
    fn ctrl_alt_arrows_move_focus() {
        let mods = Mods {
            ctrl: true,
            alt: true,
            shift: false,
            logo: false,
        };
        assert_eq!(
            press(Key::Left, mods),
            Some(Action::FocusPane(Direction::Left))
        );
        assert_eq!(
            press(Key::Right, mods),
            Some(Action::FocusPane(Direction::Right))
        );
        assert_eq!(press(Key::Up, mods), Some(Action::FocusPane(Direction::Up)));
        assert_eq!(
            press(Key::Down, mods),
            Some(Action::FocusPane(Direction::Down))
        );
    }

    #[test]
    fn f2_renames() {
        assert_eq!(press(Key::F(2), Mods::none()), Some(Action::RenameTab));
    }

    #[test]
    fn plain_keys_pass_through() {
        assert_eq!(press(Key::Char('a'), Mods::none()), None);
        assert_eq!(press(Key::Enter, Mods::none()), None);
    }
}
