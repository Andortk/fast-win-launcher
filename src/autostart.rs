//! "Start with Windows" toggle, backed by the per-user Run registry key.
//! No admin rights required; nothing is written outside HKCU.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_BINARY, REG_DWORD, REG_SZ,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("hide_winbar");
const KEYBOARD_LAYOUT_KEY: PCWSTR = w!("SYSTEM\\CurrentControlSet\\Control\\Keyboard Layout");
const SCANCODE_MAP_VALUE: PCWSTR = w!("Scancode Map");

fn open_keyboard_layout_key() -> Option<HKEY> {
    let mut hkey = HKEY::default();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            KEYBOARD_LAYOUT_KEY,
            0,
            KEY_READ | KEY_WRITE,
            &mut hkey,
        )
    };
    (rc == ERROR_SUCCESS).then_some(hkey)
}

/// Remap physical LWin/RWin to F13/F14 at the Windows keyboard-layout layer.
/// This prevents Explorer from ever receiving a Win key. Requires admin and a
/// reboot/logoff to take effect.
pub fn install_win_key_scancode_remap() -> bool {
    let Some(hkey) = open_keyboard_layout_key() else {
        return false;
    };

    let mut bytes = Vec::with_capacity(28);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&3u32.to_le_bytes());
    // DWORD format: low word = replacement scancode, high word = original.
    // LWin E0_5B -> F13 00_64, RWin E0_5C -> F14 00_65.
    bytes.extend_from_slice(&0xE05B_0064u32.to_le_bytes());
    bytes.extend_from_slice(&0xE05C_0065u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let ok = unsafe { RegSetValueExW(hkey, SCANCODE_MAP_VALUE, 0, REG_BINARY, Some(&bytes)) }
        == ERROR_SUCCESS;
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    ok
}

/// Remove the Scancode Map value installed above. Requires admin and a reboot.
pub fn remove_win_key_scancode_remap() -> bool {
    let Some(hkey) = open_keyboard_layout_key() else {
        return false;
    };
    let ok = unsafe { RegDeleteValueW(hkey, SCANCODE_MAP_VALUE) } == ERROR_SUCCESS;
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    ok
}

/// Raise the per-user low-level-hook timeout so Windows waits for our keyboard
/// hook instead of bypassing it under load (which leaks the Win key to Start).
/// Takes effect after the next logon. Returns the value if it changed.
pub fn ensure_lowlevel_hooks_timeout(ms: u32) -> Option<u32> {
    let mut hkey = HKEY::default();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Control Panel\\Desktop"),
            0,
            KEY_READ | KEY_WRITE,
            &mut hkey,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    // Read current value; only write if lower than what we want.
    let mut cur = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let q = unsafe {
        RegQueryValueExW(
            hkey,
            w!("LowLevelHooksTimeout"),
            None,
            None,
            Some(&mut cur as *mut u32 as *mut u8),
            Some(&mut size),
        )
    };
    let changed = if q != ERROR_SUCCESS || cur < ms {
        let bytes = ms.to_ne_bytes();
        let s =
            unsafe { RegSetValueExW(hkey, w!("LowLevelHooksTimeout"), 0, REG_DWORD, Some(&bytes)) };
        s == ERROR_SUCCESS
    } else {
        false
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    changed.then_some(ms)
}

fn open_run_key() -> Option<HKEY> {
    let mut hkey = HKEY::default();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            0,
            KEY_READ | KEY_WRITE,
            &mut hkey,
        )
    };
    (rc == ERROR_SUCCESS).then_some(hkey)
}

/// Is the launcher currently registered to start at login?
pub fn is_enabled() -> bool {
    let Some(hkey) = open_run_key() else {
        return false;
    };
    let rc = unsafe { RegQueryValueExW(hkey, VALUE_NAME, None, None, None, None) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    rc == ERROR_SUCCESS
}

/// Add or remove the autostart entry. The command is the quoted path to this
/// executable.
pub fn set_enabled(enabled: bool) {
    let Some(hkey) = open_run_key() else {
        return;
    };
    if enabled {
        if let Ok(exe) = std::env::current_exe() {
            let command = format!("\"{}\"", exe.display());
            let mut wide: Vec<u16> = command.encode_utf16().collect();
            wide.push(0);
            let bytes =
                unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
            unsafe {
                let _ = RegSetValueExW(hkey, VALUE_NAME, 0, REG_SZ, Some(bytes));
            }
        }
    } else {
        unsafe {
            let _ = RegDeleteValueW(hkey, VALUE_NAME);
        }
    }
    unsafe {
        let _ = RegCloseKey(hkey);
    }
}
