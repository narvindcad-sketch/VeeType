use std::mem::size_of;

use anyhow::Context;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "VeeType";
const LEGACY_VALUE_NAME: &str = "VoiceDictation";

pub fn set_enabled(enabled: bool) -> anyhow::Result<()> {
    let key_path = wide(RUN_KEY);
    let value_name = wide(VALUE_NAME);
    let legacy_value_name = wide(LEGACY_VALUE_NAME);
    let command = if enabled {
        let executable = std::env::current_exe().context("Finding the VeeType executable")?;
        Some(wide(&format!("\"{}\"", executable.display())))
    } else {
        None
    };

    let mut key = 0;
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_path.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        anyhow::bail!("Could not open the Windows startup registry key (error {status})");
    }

    let result = if enabled {
        let command = command
            .as_ref()
            .expect("command is present when enabling startup");
        let byte_len = u32::try_from(command.len() * size_of::<u16>())
            .context("The VeeType startup command is too long")?;
        let status = unsafe {
            RegSetValueExW(
                key,
                value_name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                byte_len,
            )
        };
        if status != 0 {
            Err(anyhow::anyhow!(
                "Could not register Windows auto-start (error {status})"
            ))
        } else {
            delete_value_if_present(key, &legacy_value_name)
                .context("Removing the legacy VoiceDictation auto-start entry")
        }
    } else {
        delete_value_if_present(key, &value_name)
            .context("Removing the VeeType auto-start entry")
            .and_then(|()| {
                delete_value_if_present(key, &legacy_value_name)
                    .context("Removing the legacy VoiceDictation auto-start entry")
            })
    };

    unsafe {
        RegCloseKey(key);
    }
    result
}

fn delete_value_if_present(key: isize, value_name: &[u16]) -> anyhow::Result<()> {
    let status = unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
    if status == 0 || status == 2 {
        Ok(())
    } else {
        anyhow::bail!("Could not remove a Windows startup registry entry (error {status})");
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
