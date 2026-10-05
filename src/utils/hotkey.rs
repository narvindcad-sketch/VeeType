use std::str::FromStr;

use device_query::{DeviceQuery, DeviceState, Keycode};

pub fn is_hotkey_pressed(hotkey: &str, device_state: &DeviceState) -> bool {
    let keys = device_state.get_keys();
    let hotkey = hotkey.trim();

    match hotkey {
        "RightAlt" | "RAlt" => keys.contains(&Keycode::RAlt),
        "LeftAlt" | "LAlt" => keys.contains(&Keycode::LAlt),
        "RightCtrl" | "RCtrl" => keys.contains(&Keycode::RControl),
        "LeftCtrl" | "LCtrl" | "Ctrl" => keys.contains(&Keycode::LControl),
        "CtrlSpace" | "ControlSpace" => {
            keys.contains(&Keycode::LControl) && keys.contains(&Keycode::Space)
        }
        _ => {
            let keycode = match hotkey {
                "RightAlt" => Ok(Keycode::RAlt),
                "LeftAlt" => Ok(Keycode::LAlt),
                "RightCtrl" => Ok(Keycode::RControl),
                "LeftCtrl" | "Ctrl" => Ok(Keycode::LControl),
                _ => Keycode::from_str(hotkey),
            };
            keycode.is_ok_and(|keycode| keys.contains(&keycode))
        }
    }
}

pub fn is_valid_hotkey(hotkey: &str) -> bool {
    matches!(
        hotkey,
        "RightAlt"
            | "RAlt"
            | "LeftAlt"
            | "LAlt"
            | "RightCtrl"
            | "RCtrl"
            | "LeftCtrl"
            | "LCtrl"
            | "Ctrl"
            | "CtrlSpace"
            | "ControlSpace"
    ) || Keycode::from_str(hotkey).is_ok()
}

#[cfg(test)]
mod tests {
    use super::is_valid_hotkey;

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
        ] {
            assert!(is_valid_hotkey(hotkey), "{hotkey} should be valid");
        }
    }

    #[test]
    fn rejects_unknown_hotkey_names() {
        assert!(!is_valid_hotkey("NotAKey"));
    }
}
