//! Paced Unicode input: keep each scalar together and let the destination
//! process its key messages before submitting the next character.
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

fn deliver(
    text: &str,
    mut send: impl FnMut(&[u16]) -> anyhow::Result<()>,
    mut pause: impl FnMut(),
) -> anyhow::Result<()> {
    let mut buffer = [0u16; 2];
    for character in text.chars() {
        send(character.encode_utf16(&mut buffer))?;
        pause();
    }
    Ok(())
}

pub fn send_text(text: &str) -> anyhow::Result<()> {
    if text.is_empty() { return Ok(()); }
    let target = unsafe { GetForegroundWindow() };
    anyhow::ensure!(target != 0, "No foreground window for dictation");
    deliver(text, |units| {
        anyhow::ensure!(unsafe { GetForegroundWindow() } == target,
            "Dictation insertion stopped because the foreground window changed");
        let mut inputs = Vec::with_capacity(units.len() * 2);
        for &unit in units {
            for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
                inputs.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 { ki: KEYBDINPUT {
                        wVk: 0, wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: 0,
                    } },
                });
            }
        }
        let expected = inputs.len() as u32;
        let sent = unsafe { SendInput(expected, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
        // Never retry a partially accepted batch: that could duplicate text.
        anyhow::ensure!(sent == expected,
            "Windows accepted {sent} of {expected} character input events (error {})",
            unsafe { GetLastError() });
        Ok(())
    // Keep editor message queues responsive without visibly slowing long text.
    }, || std::thread::sleep(std::time::Duration::from_millis(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn preserves_exact_text_and_surrogate_pairs() {
        let mut batches = Vec::new();
        let input = "Hello, check check.\r\nMicrophone testing 😀 café.";
        deliver(input, |units| { batches.push(units.to_vec()); Ok(()) }, || {}).unwrap();
        assert_eq!(String::from_utf16(&batches.concat()).unwrap(), input);
        assert_eq!(batches.len(), input.chars().count());
        assert!(batches.contains(&vec![0xd83d, 0xde00]));
    }

    #[test]
    fn paces_every_character_including_repeated_letters() {
        let events = RefCell::new(Vec::new());
        deliver("aaa  z", |_| { events.borrow_mut().push("send"); Ok(()) },
            || events.borrow_mut().push("pause")).unwrap();
        assert_eq!(*events.borrow(), ["send", "pause"].repeat(6));
    }

    #[test]
    fn long_text_has_no_batch_boundary_loss() {
        let input = "ab😀 ".repeat(2000);
        let mut output = Vec::new();
        deliver(&input, |units| { output.extend_from_slice(units); Ok(()) }, || {}).unwrap();
        assert_eq!(String::from_utf16(&output).unwrap(), input);
    }

    #[test]
    fn failure_stops_without_retry_or_sending_remaining_text() {
        let mut calls = 0;
        let mut pauses = 0;
        let result = deliver("abcd", |_| {
            calls += 1;
            anyhow::ensure!(calls != 2, "partial input");
            Ok(())
        }, || pauses += 1);
        assert!(result.is_err());
        assert_eq!((calls, pauses), (2, 1));
    }

    #[test]
    fn empty_text_does_not_send_or_wait() {
        deliver("", |_| panic!("unexpected input"), || panic!("unexpected wait")).unwrap();
    }
}
