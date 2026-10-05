use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

pub const HOTKEY_ID: i32 = 0x5645;

fn virtual_key(hotkey: &str) -> Option<u16> {
    let hotkey = hotkey.trim();
    let key = match hotkey {
        "RightAlt" | "RAlt" | "ROption" => 0xA5,
        "LeftAlt" | "LAlt" | "LOption" => 0xA4,
        "RightCtrl" | "RCtrl" => 0xA3,
        "LeftCtrl" | "LCtrl" | "Ctrl" => 0xA2,
        "Key0" => b'0' as u16,
        "Key1" => b'1' as u16,
        "Key2" => b'2' as u16,
        "Key3" => b'3' as u16,
        "Key4" => b'4' as u16,
        "Key5" => b'5' as u16,
        "Key6" => b'6' as u16,
        "Key7" => b'7' as u16,
        "Key8" => b'8' as u16,
        "Key9" => b'9' as u16,
        "A" => b'A' as u16,
        "B" => b'B' as u16,
        "C" => b'C' as u16,
        "D" => b'D' as u16,
        "E" => b'E' as u16,
        "F" => b'F' as u16,
        "G" => b'G' as u16,
        "H" => b'H' as u16,
        "I" => b'I' as u16,
        "J" => b'J' as u16,
        "K" => b'K' as u16,
        "L" => b'L' as u16,
        "M" => b'M' as u16,
        "N" => b'N' as u16,
        "O" => b'O' as u16,
        "P" => b'P' as u16,
        "Q" => b'Q' as u16,
        "R" => b'R' as u16,
        "S" => b'S' as u16,
        "T" => b'T' as u16,
        "U" => b'U' as u16,
        "V" => b'V' as u16,
        "W" => b'W' as u16,
        "X" => b'X' as u16,
        "Y" => b'Y' as u16,
        "Z" => b'Z' as u16,
        "F1" => 0x70,
        "F2" => 0x71,
        "F3" => 0x72,
        "F4" => 0x73,
        "F5" => 0x74,
        "F6" => 0x75,
        "F7" => 0x76,
        "F8" => 0x77,
        "F9" => 0x78,
        "F10" => 0x79,
        "F11" => 0x7A,
        "F12" => 0x7B,
        "F13" => 0x7C,
        "F14" => 0x7D,
        "F15" => 0x7E,
        "F16" => 0x7F,
        "F17" => 0x80,
        "F18" => 0x81,
        "F19" => 0x82,
        "F20" => 0x83,
        "Escape" => 0x1B,
        "Space" => 0x20,
        "LControl" => 0xA2,
        "RControl" => 0xA3,
        "LShift" => 0xA0,
        "RShift" => 0xA1,
        "Command" | "LMeta" => 0x5B,
        "RCommand" | "RMeta" => 0x5C,
        "Enter" | "NumpadEnter" => 0x0D,
        "Up" => 0x26,
        "Down" => 0x28,
        "Left" => 0x25,
        "Right" => 0x27,
        "Backspace" => 0x08,
        "CapsLock" => 0x14,
        "Tab" => 0x09,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "Numpad0" => 0x60,
        "Numpad1" => 0x61,
        "Numpad2" => 0x62,
        "Numpad3" => 0x63,
        "Numpad4" => 0x64,
        "Numpad5" => 0x65,
        "Numpad6" => 0x66,
        "Numpad7" => 0x67,
        "Numpad8" => 0x68,
        "Numpad9" => 0x69,
        "NumpadSubtract" => 0x6D,
        "NumpadAdd" => 0x6B,
        "NumpadDivide" => 0x6F,
        "NumpadMultiply" => 0x6A,
        "NumpadEquals" => 0x92,
        "NumpadDecimal" => 0x6E,
        "Grave" => 0xC0,
        "Minus" => 0xBD,
        "Equal" => 0xBB,
        "LeftBracket" => 0xDB,
        "RightBracket" => 0xDD,
        "BackSlash" => 0xDC,
        "Semicolon" => 0xBA,
        "Apostrophe" => 0xDE,
        "Comma" => 0xBC,
        "Dot" => 0xBE,
        "Slash" => 0xBF,
        _ => return None,
    };
    Some(key)
}

fn key_is_down(virtual_key: u16) -> bool {
    unsafe { GetAsyncKeyState(virtual_key as i32) < 0 }
}

pub fn windows_hotkey_parts(hotkey: &str) -> anyhow::Result<(u32, u32)> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOD_CONTROL, MOD_NOREPEAT};

    let hotkey = hotkey.trim();
    let (modifiers, key) = if matches!(hotkey, "CtrlSpace" | "ControlSpace") {
        (MOD_CONTROL, 0x20)
    } else {
        (
            0,
            virtual_key(hotkey).ok_or_else(|| anyhow::anyhow!("Unsupported hotkey: {hotkey}"))?
                as u32,
        )
    };
    Ok((modifiers | MOD_NOREPEAT, key))
}

pub fn is_hotkey_pressed(hotkey: &str) -> bool {
    if matches!(hotkey.trim(), "CtrlSpace" | "ControlSpace") {
        return key_is_down(0xA2) && key_is_down(0x20);
    }
    virtual_key(hotkey).is_some_and(key_is_down)
}

pub fn pressed_hotkey() -> Option<String> {
    if key_is_down(0xA2) && key_is_down(0x20) {
        return Some("CtrlSpace".into());
    }

    const HOTKEYS: &[&str] = &[
        "RightAlt",
        "LeftAlt",
        "RightCtrl",
        "LeftCtrl",
        "LShift",
        "RShift",
        "Command",
        "RCommand",
        "LMeta",
        "RMeta",
        "Key0",
        "Key1",
        "Key2",
        "Key3",
        "Key4",
        "Key5",
        "Key6",
        "Key7",
        "Key8",
        "Key9",
        "A",
        "B",
        "C",
        "D",
        "E",
        "F",
        "G",
        "H",
        "I",
        "J",
        "K",
        "L",
        "M",
        "N",
        "O",
        "P",
        "Q",
        "R",
        "S",
        "T",
        "U",
        "V",
        "W",
        "X",
        "Y",
        "Z",
        "F1",
        "F2",
        "F3",
        "F4",
        "F5",
        "F6",
        "F7",
        "F8",
        "F9",
        "F10",
        "F11",
        "F12",
        "F13",
        "F14",
        "F15",
        "F16",
        "F17",
        "F18",
        "F19",
        "F20",
        "Escape",
        "Space",
        "Enter",
        "Up",
        "Down",
        "Left",
        "Right",
        "Backspace",
        "CapsLock",
        "Tab",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "Insert",
        "Delete",
        "Numpad0",
        "Numpad1",
        "Numpad2",
        "Numpad3",
        "Numpad4",
        "Numpad5",
        "Numpad6",
        "Numpad7",
        "Numpad8",
        "Numpad9",
        "NumpadSubtract",
        "NumpadAdd",
        "NumpadDivide",
        "NumpadMultiply",
        "NumpadEquals",
        "NumpadEnter",
        "NumpadDecimal",
        "Grave",
        "Minus",
        "Equal",
        "LeftBracket",
        "RightBracket",
        "BackSlash",
        "Semicolon",
        "Apostrophe",
        "Comma",
        "Dot",
        "Slash",
    ];

    HOTKEYS
        .iter()
        .find(|hotkey| virtual_key(hotkey).is_some_and(key_is_down))
        .map(|hotkey| (*hotkey).to_string())
}

pub fn is_valid_hotkey(hotkey: &str) -> bool {
    matches!(hotkey.trim(), "CtrlSpace" | "ControlSpace") || virtual_key(hotkey).is_some()
}

#[cfg(test)]
mod tests {
    use super::{is_valid_hotkey, windows_hotkey_parts};

    #[test]
    fn accepts_supported_hotkey_names_and_aliases() {
        for hotkey in [
            "RightAlt",
            "RAlt",
            "LeftAlt",
            "LAlt",
            "RightCtrl",
            "RCtrl",
            "LeftCtrl",
            "LCtrl",
            "Ctrl",
            "CtrlSpace",
            "ControlSpace",
            "Space",
            "A",
            "F20",
            "NumpadEnter",
            "Grave",
        ] {
            assert!(is_valid_hotkey(hotkey), "{hotkey} should be valid");
        }
    }

    #[test]
    fn rejects_unknown_hotkey_names() {
        assert!(!is_valid_hotkey("NotAKey"));
    }

    #[test]
    fn maps_configured_hotkeys_to_windows_virtual_keys() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            MOD_CONTROL, MOD_NOREPEAT, VK_RMENU, VK_SPACE,
        };

        assert_eq!(
            windows_hotkey_parts("CtrlSpace").unwrap(),
            (MOD_CONTROL | MOD_NOREPEAT, VK_SPACE as u32)
        );
        assert_eq!(
            windows_hotkey_parts("RightAlt").unwrap(),
            (MOD_NOREPEAT, VK_RMENU as u32)
        );
        assert_eq!(windows_hotkey_parts("A").unwrap().1, b'A' as u32);
    }
}
