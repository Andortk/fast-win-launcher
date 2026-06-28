//! Low-level keyboard hook that remaps the Windows key.
//!
//! Behaviour:
//!   * A lone Win **tap** toggles the launcher (tap to open, tap again to close).
//!     The Start menu never opens while the app is active.
//!   * All native Win+<key> combos (Win+D, Win+E, Win+arrows, ...) still work,
//!     reconstructed by synthesizing a real Win press only when needed.
//!
//! (The system on/off toggle is a separate `RegisterHotKey` hotkey owned by the
//! tray module — far more reliable than routing a combo through this hook.)
//!
//! Synthesized events carry `INJECT_SENTINEL` in `dwExtraInfo` so this same hook
//! passes them straight through.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_ESCAPE, VK_LWIN, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::state::{
    now_ms, APP_ENABLED, EGUI_CTX, HIDE_LAUNCHER, INJECT_SENTINEL, LAUNCHER_VISIBLE, SHOW_LAUNCHER,
    START_SUPPRESS_UNTIL_MS,
};

/// Diagnostics: total hook callbacks and Win-key events seen.
pub static HOOK_CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub static WIN_EVENTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

static WIN_DOWN: AtomicBool = AtomicBool::new(false);
/// We swallowed the physical Win down and have restored it synthetically for a
/// native Win+<key> combo.
static SYNTH_WIN_DOWN: AtomicBool = AtomicBool::new(false);
/// A non-Win key was pressed during this Win hold (so it isn't a lone tap).
static COMBO_USED: AtomicBool = AtomicBool::new(false);
/// The visible launcher was closed on Win-down; eat the matching Win-up without
/// treating it as a second lone tap.
static CLOSE_ON_UP: AtomicBool = AtomicBool::new(false);

const VK_F13_CODE: u32 = 0x7C;
const VK_F14_CODE: u32 = 0x7D;

fn is_win_key(vk: u32) -> bool {
    vk == VK_LWIN.0 as u32 || vk == VK_RWIN.0 as u32 || vk == VK_F13_CODE || vk == VK_F14_CODE
}

fn is_extended(flags: u32) -> bool {
    (flags & 0x01) != 0
}

fn wake_launcher_ui() {
    if let Some(ctx) = EGUI_CTX.get() {
        ctx.request_repaint();
    }
}

/// Synthesize a single key event. `extended` mirrors the original key's
/// extended-key flag so things like the arrow keys snap correctly.
fn send_key(vk: u16, down: bool, extended: bool) {
    let mut flags = if down {
        Default::default()
    } else {
        KEYEVENTF_KEYUP
    };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECT_SENTINEL,
            },
        },
    };
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
}

pub fn suppress_start_menu_for(duration: std::time::Duration) {
    let until = now_ms().saturating_add(duration.as_millis() as u64);
    START_SUPPRESS_UNTIL_MS.store(until, Ordering::SeqCst);
}

pub fn dismiss_start_menu_now() {
    send_key(VK_ESCAPE.0, true, false);
    send_key(VK_ESCAPE.0, false, false);
}

fn dismiss_start_menu_later() {
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(40));
        dismiss_start_menu_now();
        std::thread::sleep(std::time::Duration::from_millis(80));
        dismiss_start_menu_now();
        std::thread::sleep(std::time::Duration::from_millis(160));
        dismiss_start_menu_now();
    });
}

pub fn release_win_keys() {
    send_key(VK_LWIN.0, false, false);
    send_key(VK_RWIN.0, false, true);
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION as i32 {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    HOOK_CALLS.fetch_add(1, Ordering::Relaxed);

    let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);

    // Always let our own injected events flow through untouched.
    if kb.dwExtraInfo == INJECT_SENTINEL {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    if !APP_ENABLED.load(Ordering::SeqCst) {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let msg = wparam.0 as u32;
    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

    if is_win_key(kb.vkCode) {
        WIN_EVENTS.fetch_add(1, Ordering::Relaxed);
        suppress_start_menu_for(std::time::Duration::from_millis(1500));

        if is_down {
            if !WIN_DOWN.swap(true, Ordering::SeqCst) {
                COMBO_USED.store(false, Ordering::SeqCst);
                CLOSE_ON_UP.store(false, Ordering::SeqCst);
                SYNTH_WIN_DOWN.store(false, Ordering::SeqCst);
                release_win_keys();
                if LAUNCHER_VISIBLE.load(Ordering::SeqCst) {
                    LAUNCHER_VISIBLE.store(false, Ordering::SeqCst);
                    SHOW_LAUNCHER.store(false, Ordering::SeqCst);
                    HIDE_LAUNCHER.store(true, Ordering::SeqCst);
                    wake_launcher_ui();
                    CLOSE_ON_UP.store(true, Ordering::SeqCst);
                    dismiss_start_menu_later();
                }
            }
            return LRESULT(1);
        }

        if is_up {
            let close_on_up = CLOSE_ON_UP.swap(false, Ordering::SeqCst);
            let combo = COMBO_USED.swap(false, Ordering::SeqCst);
            let synth_win_down = SYNTH_WIN_DOWN.swap(false, Ordering::SeqCst);
            WIN_DOWN.store(false, Ordering::SeqCst);
            if synth_win_down {
                send_key(VK_LWIN.0, false, false);
            } else {
                release_win_keys();
            }
            if close_on_up {
                dismiss_start_menu_later();
            } else if !combo {
                SHOW_LAUNCHER.store(true, Ordering::SeqCst);
                wake_launcher_ui();
            }
            return LRESULT(1);
        }
    } else if WIN_DOWN.load(Ordering::SeqCst) {
        if is_down || is_up {
            COMBO_USED.store(true, Ordering::SeqCst);
            if !SYNTH_WIN_DOWN.swap(true, Ordering::SeqCst) {
                send_key(VK_LWIN.0, true, false);
            }
            send_key(kb.vkCode as u16, is_down, is_extended(kb.flags.0));
            return LRESULT(1);
        }
    }

    CallNextHookEx(None, code, wparam, lparam)
}
/// Install the keyboard hook. Must be called on a thread that pumps messages
/// (the core thread). The hook lives for the rest of the process.
pub fn install() -> windows::core::Result<HHOOK> {
    unsafe {
        let hmod = GetModuleHandleW(None)?;
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), HINSTANCE(hmod.0), 0)
    }
}
