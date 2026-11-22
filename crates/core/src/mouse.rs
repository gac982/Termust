//! Codificación de eventos de ratón para el protocolo xterm.
//!
//! Se usa SGR (1006) cuando la aplicación lo pide; si no, el formato legacy
//! `ESC [ M` con coordenadas desplazadas 32. Las coordenadas son 1-based.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    /// Sin botón: solo válido para reportar movimiento.
    None,
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

impl MouseButton {
    fn code(self) -> u8 {
        match self {
            MouseButton::None => 3,
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
            MouseButton::WheelLeft => 66,
            MouseButton::WheelRight => 67,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    Motion,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl MouseMods {
    fn bits(self) -> u8 {
        let mut b = 0;
        if self.shift {
            b |= 4;
        }
        if self.alt {
            b |= 8;
        }
        if self.ctrl {
            b |= 16;
        }
        b
    }
}

/// Máximo desplazable en el formato legacy (32 + 223 = 255).
const LEGACY_MAX: usize = 223;

pub fn encode_mouse(
    kind: MouseEventKind,
    button: MouseButton,
    col: usize,
    row: usize,
    mods: MouseMods,
    sgr: bool,
    utf8: bool,
) -> Vec<u8> {
    let x = col.max(1);
    let y = row.max(1);
    let mut b = button.code() | mods.bits();
    if kind == MouseEventKind::Motion {
        b |= 32;
    }

    if sgr {
        let final_byte = match kind {
            MouseEventKind::Release => 'm',
            _ => 'M',
        };
        return format!("\x1b[<{b};{x};{y}{final_byte}").into_bytes();
    }

    // En el protocolo legacy la liberación se codifica como botón 3.
    if kind == MouseEventKind::Release {
        b = 3 | mods.bits();
    }
    let mut out = vec![0x1b, b'[', b'M'];
    out.push(32 + b);
    if utf8 {
        push_utf8_coord(&mut out, x + 32);
        push_utf8_coord(&mut out, y + 32);
    } else {
        out.push((32 + x.min(LEGACY_MAX)) as u8);
        out.push((32 + y.min(LEGACY_MAX)) as u8);
    }
    out
}

fn push_utf8_coord(out: &mut Vec<u8>, value: usize) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(
        char::from_u32(value as u32)
            .unwrap_or('\u{7f}')
            .encode_utf8(&mut buf)
            .as_bytes(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: MouseMods = MouseMods {
        shift: false,
        alt: false,
        ctrl: false,
    };

    #[test]
    fn sgr_press_release_and_motion() {
        assert_eq!(
            encode_mouse(
                MouseEventKind::Press,
                MouseButton::Left,
                5,
                10,
                NONE,
                true,
                false
            ),
            b"\x1b[<0;5;10M"
        );
        assert_eq!(
            encode_mouse(
                MouseEventKind::Release,
                MouseButton::Left,
                5,
                10,
                NONE,
                true,
                false
            ),
            b"\x1b[<0;5;10m"
        );
        assert_eq!(
            encode_mouse(
                MouseEventKind::Motion,
                MouseButton::Left,
                2,
                2,
                NONE,
                true,
                false
            ),
            b"\x1b[<32;2;2M"
        );
    }

    #[test]
    fn sgr_wheel_and_modifiers() {
        assert_eq!(
            encode_mouse(
                MouseEventKind::Press,
                MouseButton::WheelUp,
                1,
                1,
                NONE,
                true,
                false
            ),
            b"\x1b[<64;1;1M"
        );
        assert_eq!(
            encode_mouse(
                MouseEventKind::Press,
                MouseButton::WheelDown,
                1,
                1,
                NONE,
                true,
                false
            ),
            b"\x1b[<65;1;1M"
        );
        let mods = MouseMods {
            shift: true,
            alt: false,
            ctrl: true,
        };
        assert_eq!(
            encode_mouse(
                MouseEventKind::Press,
                MouseButton::Right,
                3,
                4,
                mods,
                true,
                false
            ),
            b"\x1b[<22;3;4M"
        );
    }

    #[test]
    fn legacy_press_release_and_clamp() {
        assert_eq!(
            encode_mouse(
                MouseEventKind::Press,
                MouseButton::Left,
                1,
                1,
                NONE,
                false,
                false
            ),
            b"\x1b[M !!"
        );
        assert_eq!(
            encode_mouse(
                MouseEventKind::Release,
                MouseButton::Left,
                1,
                1,
                NONE,
                false,
                false
            ),
            b"\x1b[M#!!"
        );
        let clamped = encode_mouse(
            MouseEventKind::Press,
            MouseButton::Left,
            500,
            500,
            NONE,
            false,
            false,
        );
        assert_eq!(&clamped[3..], &[32, 255, 255]);
    }

    #[test]
    fn legacy_utf8_coordinates() {
        let out = encode_mouse(
            MouseEventKind::Press,
            MouseButton::Left,
            300,
            300,
            NONE,
            false,
            true,
        );
        assert_eq!(&out[..3], b"\x1b[M");
        assert_eq!(&out[3..4], &[32]);
        assert_eq!(std::str::from_utf8(&out[4..]).unwrap(), "ŌŌ");
    }

    #[test]
    fn coordinates_are_at_least_one() {
        assert_eq!(
            encode_mouse(
                MouseEventKind::Press,
                MouseButton::Left,
                0,
                0,
                NONE,
                true,
                false
            ),
            b"\x1b[<0;1;1M"
        );
    }
}
