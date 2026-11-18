use gpui::{Modifiers, MouseButton};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MouseEventKind {
    Press,
    Release,
    Motion { button: Option<MouseButton> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalMouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl TerminalMouseButton {
    fn code(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
        }
    }
}

fn modifier_code(modifiers: Modifiers) -> u8 {
    u8::from(modifiers.shift) * 4 + u8::from(modifiers.alt) * 8 + u8::from(modifiers.control) * 16
}

fn key_modifier_code(modifiers: Modifiers) -> u8 {
    1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt) + 4 * u8::from(modifiers.control)
}

/// Encodes a key event using the terminal's xterm conventions.
pub(crate) fn encode_key(
    key: &str,
    key_char: Option<&str>,
    modifiers: Modifiers,
    application_cursor: bool,
) -> Option<Vec<u8>> {
    if modifiers.platform {
        return None;
    }
    let lower = key.to_ascii_lowercase();
    if modifiers.control {
        if let Some(control) = control_character(&lower, key_char) {
            return Some(alt_prefix(&[control], modifiers.alt));
        }
        if matches!(lower.as_str(), "up" | "down" | "left" | "right") {
            return Some(encode_cursor(
                match lower.as_str() {
                    "up" => b'A',
                    "down" => b'B',
                    "right" => b'C',
                    _ => b'D',
                },
                modifiers,
                application_cursor,
            ));
        }
    }

    let bytes = match lower.as_str() {
        "enter" => alt_prefix(b"\r", modifiers.alt),
        "backspace" => alt_prefix(&[0x7f], modifiers.alt),
        "tab" if modifiers.shift => alt_prefix(b"\x1b[Z", modifiers.alt),
        "tab" => alt_prefix(b"\t", modifiers.alt),
        "up" => encode_cursor(b'A', modifiers, application_cursor),
        "down" => encode_cursor(b'B', modifiers, application_cursor),
        "right" => encode_cursor(b'C', modifiers, application_cursor),
        "left" => encode_cursor(b'D', modifiers, application_cursor),
        "home" => encode_cursor_position(b'H', modifiers, application_cursor),
        "end" => encode_cursor_position(b'F', modifiers, application_cursor),
        "insert" => encode_tilde_key(b'~', 2, modifiers),
        "delete" => encode_tilde_key(b'~', 3, modifiers),
        "pageup" => encode_tilde_key(b'~', 5, modifiers),
        "pagedown" => encode_tilde_key(b'~', 6, modifiers),
        "f1" => encode_function_key(b'P', 1, modifiers),
        "f2" => encode_function_key(b'Q', 1, modifiers),
        "f3" => encode_function_key(b'R', 1, modifiers),
        "f4" => encode_function_key(b'S', 1, modifiers),
        "f5" => encode_tilde_key(b'~', 15, modifiers),
        "f6" => encode_tilde_key(b'~', 17, modifiers),
        "f7" => encode_tilde_key(b'~', 18, modifiers),
        "f8" => encode_tilde_key(b'~', 19, modifiers),
        "f9" => encode_tilde_key(b'~', 20, modifiers),
        "f10" => encode_tilde_key(b'~', 21, modifiers),
        "f11" => encode_tilde_key(b'~', 23, modifiers),
        "f12" => encode_tilde_key(b'~', 24, modifiers),
        "escape" => vec![0x1b],
        _ => {
            let text = key_char?;
            let mut bytes = Vec::with_capacity(text.len() + usize::from(modifiers.alt));
            if modifiers.alt {
                bytes.push(0x1b);
            }
            bytes.extend_from_slice(text.as_bytes());
            bytes
        }
    };
    Some(bytes)
}

fn control_character(key: &str, key_char: Option<&str>) -> Option<u8> {
    if key.len() == 1 {
        let byte = key.as_bytes()[0];
        if byte.is_ascii_lowercase() {
            return Some(byte - b'a' + 1);
        }
        if byte.is_ascii_uppercase() {
            return Some(byte - b'A' + 1);
        }
    }
    match key {
        "space" | "@" | "2" => Some(0),
        "[" => Some(0x1b),
        "\\" => Some(0x1c),
        "]" => Some(0x1d),
        "^" => Some(0x1e),
        "_" | "/" | "-" => Some(0x1f),
        "?" => Some(0x7f),
        _ => {
            control_character_from_byte(key_char.and_then(|text| text.as_bytes().first().copied()))
        }
    }
}

fn control_character_from_byte(byte: Option<u8>) -> Option<u8> {
    match byte {
        Some(b' ') | Some(b'@') | Some(b'2') => Some(0),
        Some(b'[') => Some(0x1b),
        Some(b'\\') => Some(0x1c),
        Some(b']') => Some(0x1d),
        Some(b'^') => Some(0x1e),
        Some(b'_') | Some(b'/') | Some(b'-') => Some(0x1f),
        Some(b'?') => Some(0x7f),
        _ => None,
    }
}

fn alt_prefix(bytes: &[u8], alt: bool) -> Vec<u8> {
    if alt {
        [vec![0x1b], bytes.to_vec()].concat()
    } else {
        bytes.to_vec()
    }
}

fn encode_cursor(key: u8, modifiers: Modifiers, application_cursor: bool) -> Vec<u8> {
    let modifier = 1
        + u8::from(modifiers.shift)
        + 2 * u8::from(modifiers.alt)
        + 4 * u8::from(modifiers.control);
    if modifier == 1 {
        if application_cursor {
            vec![0x1b, b'O', key]
        } else {
            vec![0x1b, b'[', key]
        }
    } else {
        format!("\x1b[1;{modifier}{}", key as char).into_bytes()
    }
}

fn encode_function_key(final_byte: u8, number: u8, modifiers: Modifiers) -> Vec<u8> {
    let modifier = key_modifier_code(modifiers);
    if modifier == 1 {
        vec![0x1b, b'O', final_byte]
    } else {
        format!("\x1b[{};{modifier}{}", number, final_byte as char).into_bytes()
    }
}

fn encode_cursor_position(
    final_byte: u8,
    modifiers: Modifiers,
    application_cursor: bool,
) -> Vec<u8> {
    let modifier = key_modifier_code(modifiers);
    if modifier == 1 && application_cursor {
        vec![0x1b, b'O', final_byte]
    } else if modifier == 1 {
        vec![0x1b, b'[', final_byte]
    } else {
        format!("\x1b[1;{modifier}{}", final_byte as char).into_bytes()
    }
}

fn encode_tilde_key(final_byte: u8, number: u8, modifiers: Modifiers) -> Vec<u8> {
    let modifier = key_modifier_code(modifiers);
    if modifier == 1 && final_byte == b'~' {
        format!("\x1b[{number}~").into_bytes()
    } else if modifier == 1 {
        vec![0x1b, b'[', final_byte]
    } else {
        format!("\x1b[{number};{modifier}~").into_bytes()
    }
}

/// Normalizes line endings and removes terminal escape/control bytes from a
/// clipboard payload. The bracketed wrapper is returned as one write so a
/// partially written payload cannot expose a terminator.
pub(crate) fn encode_paste(text: &str, bracketed: bool) -> Option<Vec<u8>> {
    let mut body = Vec::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\r' {
            body.push(b'\r');
            index += 1 + usize::from(bytes.get(index + 1) == Some(&b'\n'));
        } else if byte == b'\n' {
            body.push(b'\r');
            index += 1;
        } else if bracketed {
            if byte != 0x1b {
                body.push(byte);
            }
            index += 1;
        } else {
            if byte == b'\t' || byte == b'\r' || (byte >= 0x20 && byte != 0x7f) {
                body.push(byte);
            }
            index += 1;
        }
    }
    if body.is_empty() {
        return None;
    }
    if bracketed {
        Some([&b"\x1b[200~"[..], body.as_slice(), &b"\x1b[201~"[..]].concat())
    } else {
        Some(body)
    }
}

/// Encode a mouse report. Coordinates are zero-based terminal cells.
pub(crate) fn encode_mouse(
    mode: vt100::MouseProtocolMode,
    encoding: vt100::MouseProtocolEncoding,
    kind: MouseEventKind,
    button: TerminalMouseButton,
    cell: (u16, u16),
    modifiers: Modifiers,
) -> Option<Vec<u8>> {
    let motion = matches!(kind, MouseEventKind::Motion { .. });
    let pressed_button = match kind {
        MouseEventKind::Motion { button } => button.and_then(mouse_button_code),
        _ => None,
    };
    let button_code = if motion {
        pressed_button.unwrap_or(3) | 32
    } else {
        button.code()
            | if matches!(kind, MouseEventKind::Release)
                && !matches!(encoding, vt100::MouseProtocolEncoding::Sgr)
            {
                3
            } else {
                0
            }
    };
    let enabled = match mode {
        vt100::MouseProtocolMode::None => false,
        vt100::MouseProtocolMode::Press => !matches!(
            kind,
            MouseEventKind::Release | MouseEventKind::Motion { .. }
        ),
        vt100::MouseProtocolMode::PressRelease => !motion,
        vt100::MouseProtocolMode::ButtonMotion => !motion || pressed_button.is_some(),
        vt100::MouseProtocolMode::AnyMotion => true,
    };
    if !enabled {
        return None;
    }

    let code = if mode == vt100::MouseProtocolMode::Press {
        button_code
    } else {
        button_code.saturating_add(modifier_code(modifiers))
    };
    let x = cell.1.checked_add(1)?;
    let y = cell.0.checked_add(1)?;
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let suffix = if matches!(kind, MouseEventKind::Release) {
                'm'
            } else {
                'M'
            };
            Some(format!("\x1b[<{code};{x};{y}{suffix}").into_bytes())
        }
        vt100::MouseProtocolEncoding::Default => {
            if x > 223 || y > 223 || code > 223 {
                return None;
            }
            Some(vec![
                0x1b,
                b'[',
                b'M',
                32 + code,
                32 + x as u8,
                32 + y as u8,
            ])
        }
        vt100::MouseProtocolEncoding::Utf8 => {
            let mut bytes = vec![0x1b, b'[', b'M'];
            for value in [32 + code as u32, 32 + x as u32, 32 + y as u32] {
                let character = char::from_u32(value)?;
                let mut buffer = [0; 4];
                bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            }
            Some(bytes)
        }
    }
}

fn mouse_button_code(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{encode_key, encode_mouse, encode_paste, MouseEventKind, TerminalMouseButton};
    use gpui::{Modifiers, MouseButton};

    fn modifiers(control: bool, shift: bool, alt: bool) -> Modifiers {
        Modifiers {
            control,
            shift,
            alt,
            ..Default::default()
        }
    }

    #[test]
    fn control_keys_and_modified_navigation_use_terminal_encodings() {
        assert_eq!(
            encode_key("a", Some("a"), modifiers(true, false, false), false),
            Some(vec![1])
        );
        assert_eq!(
            encode_key("v", Some("v"), modifiers(true, false, false), false),
            Some(vec![0x16])
        );
        assert_eq!(
            encode_key("space", Some(" "), modifiers(true, false, false), false),
            Some(vec![0])
        );
        assert_eq!(
            encode_key("space", None, modifiers(true, false, false), false),
            Some(vec![0])
        );
        assert_eq!(
            encode_key("[", None, modifiers(true, false, false), false),
            Some(vec![0x1b])
        );
        assert_eq!(
            encode_key("/", None, modifiers(true, false, false), false),
            Some(vec![0x1f])
        );
        assert_eq!(
            encode_key("-", None, modifiers(true, false, false), false),
            Some(vec![0x1f])
        );
        assert_eq!(
            encode_key("slash", Some("/"), modifiers(true, false, false), false),
            Some(vec![0x1f])
        );
        assert_eq!(
            encode_key("minus", Some("-"), modifiers(true, false, false), false),
            Some(vec![0x1f])
        );
        assert_eq!(
            encode_key("?", Some("?"), modifiers(true, false, false), false),
            Some(vec![0x7f])
        );
        assert_eq!(
            encode_key("a", None, modifiers(true, false, true), false),
            Some(vec![0x1b, 1])
        );
        assert_eq!(
            encode_key("enter", None, modifiers(false, false, true), false),
            Some(b"\x1b\r".to_vec())
        );
        assert_eq!(
            encode_key("backspace", None, modifiers(false, false, true), false),
            Some(vec![0x1b, 0x7f])
        );
        assert_eq!(
            encode_key("tab", None, modifiers(false, false, true), false),
            Some(b"\x1b\t".to_vec())
        );
        assert_eq!(
            encode_key("up", None, modifiers(true, true, false), false),
            Some(b"\x1b[1;6A".to_vec())
        );
        assert_eq!(
            encode_key("up", None, modifiers(false, false, false), true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode_key("home", None, modifiers(false, false, false), true),
            Some(b"\x1bOH".to_vec())
        );
        assert_eq!(
            encode_key("end", None, modifiers(false, false, false), true),
            Some(b"\x1bOF".to_vec())
        );
        assert_eq!(
            encode_key("home", None, modifiers(false, false, true), true),
            Some(b"\x1b[1;3H".to_vec())
        );
        assert_eq!(
            encode_key("tab", None, modifiers(false, true, false), false),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            encode_key("insert", None, modifiers(false, true, false), false),
            Some(b"\x1b[2;2~".to_vec())
        );
        assert_eq!(
            encode_key("f12", None, modifiers(false, true, false), false),
            Some(b"\x1b[24;2~".to_vec())
        );
        assert_eq!(
            encode_key("f1", None, modifiers(false, true, false), false),
            Some(b"\x1b[1;2P".to_vec())
        );
        assert_eq!(
            encode_key("x", Some("x"), modifiers(false, false, true), false),
            Some(b"\x1bx".to_vec())
        );
    }

    #[test]
    fn platform_shortcuts_are_left_to_the_host() {
        assert_eq!(
            encode_key(
                "k",
                Some("k"),
                Modifiers {
                    platform: true,
                    ..Default::default()
                },
                false
            ),
            None
        );
        assert_eq!(
            encode_key(
                "left",
                None,
                Modifiers {
                    platform: true,
                    ..Default::default()
                },
                false
            ),
            None
        );
    }

    #[test]
    fn paste_normalizes_and_blocks_escape_controls() {
        assert_eq!(
            encode_paste("a\r\nb\nc\td", false),
            Some(b"a\rb\rc\td".to_vec())
        );
        assert_eq!(
            encode_paste("\x1b[201~safe\x07\x7f", true),
            Some(b"\x1b[200~[201~safe\x07\x7f\x1b[201~".to_vec())
        );
        assert_eq!(encode_paste("\x1b\x00\x01", false), None);
        assert_eq!(
            encode_paste("safe\x00\x07\x7f\t\n", false),
            Some(b"safe\t\r".to_vec())
        );
        assert_eq!(encode_paste("", true), None);
    }

    #[test]
    fn mouse_modes_and_encodings_follow_xterm_rules() {
        let plain = Modifiers::default();
        assert_eq!(
            encode_mouse(
                vt100::MouseProtocolMode::Press,
                vt100::MouseProtocolEncoding::Default,
                MouseEventKind::Press,
                TerminalMouseButton::Left,
                (0, 0),
                plain
            ),
            Some(b"\x1b[M !!".to_vec())
        );
        assert_eq!(
            encode_mouse(
                vt100::MouseProtocolMode::PressRelease,
                vt100::MouseProtocolEncoding::Sgr,
                MouseEventKind::Release,
                TerminalMouseButton::Right,
                (2, 3),
                plain
            ),
            Some(b"\x1b[<2;4;3m".to_vec())
        );
        assert_eq!(
            encode_mouse(
                vt100::MouseProtocolMode::PressRelease,
                vt100::MouseProtocolEncoding::Sgr,
                MouseEventKind::Release,
                TerminalMouseButton::Left,
                (2, 3),
                plain
            ),
            Some(b"\x1b[<0;4;3m".to_vec())
        );
        assert_eq!(
            encode_mouse(
                vt100::MouseProtocolMode::ButtonMotion,
                vt100::MouseProtocolEncoding::Sgr,
                MouseEventKind::Motion {
                    button: Some(MouseButton::Left)
                },
                TerminalMouseButton::Left,
                (2, 3),
                plain
            ),
            Some(b"\x1b[<32;4;3M".to_vec())
        );
        assert_eq!(
            encode_mouse(
                vt100::MouseProtocolMode::ButtonMotion,
                vt100::MouseProtocolEncoding::Sgr,
                MouseEventKind::Motion { button: None },
                TerminalMouseButton::Left,
                (2, 3),
                plain
            ),
            None
        );
        assert_eq!(
            encode_mouse(
                vt100::MouseProtocolMode::AnyMotion,
                vt100::MouseProtocolEncoding::Sgr,
                MouseEventKind::Motion { button: None },
                TerminalMouseButton::Left,
                (2, 3),
                plain
            ),
            Some(b"\x1b[<35;4;3M".to_vec())
        );
        assert_eq!(
            encode_mouse(
                vt100::MouseProtocolMode::PressRelease,
                vt100::MouseProtocolEncoding::Utf8,
                MouseEventKind::Press,
                TerminalMouseButton::Left,
                (0, 100),
                plain
            ),
            Some(b"\x1b[M \xc2\x85!".to_vec())
        );
        assert_eq!(
            encode_mouse(
                vt100::MouseProtocolMode::PressRelease,
                vt100::MouseProtocolEncoding::Utf8,
                MouseEventKind::Press,
                TerminalMouseButton::Left,
                (0, 223),
                plain
            ),
            Some(b"\x1b[M \xc4\x80!".to_vec())
        );
        assert_eq!(
            encode_mouse(
                vt100::MouseProtocolMode::Press,
                vt100::MouseProtocolEncoding::Default,
                MouseEventKind::Press,
                TerminalMouseButton::Left,
                (0, 0),
                modifiers(true, true, true)
            ),
            Some(b"\x1b[M !!".to_vec())
        );
        assert_eq!(
            encode_mouse(
                vt100::MouseProtocolMode::None,
                vt100::MouseProtocolEncoding::Sgr,
                MouseEventKind::Press,
                TerminalMouseButton::Left,
                (0, 0),
                plain
            ),
            None
        );
        assert_eq!(
            encode_mouse(
                vt100::MouseProtocolMode::Press,
                vt100::MouseProtocolEncoding::Default,
                MouseEventKind::Press,
                TerminalMouseButton::Left,
                (0, 223),
                plain
            ),
            None
        );
    }
}
