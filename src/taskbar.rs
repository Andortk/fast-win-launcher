//! Hide / show the Windows taskbar.
//!
//! Like sinjs/HideTaskbar this doesn't destroy or move the taskbar — it just
//! toggles the visibility of the `Shell_TrayWnd` / `Shell_SecondaryTrayWnd`
//! windows with `ShowWindow`. The shell can re-show it, so the core loop
//! re-applies the hidden state on a timer while hiding is enabled.

use std::cell::RefCell;

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, ShowWindow, SW_HIDE, SW_SHOWNA,
};

thread_local! {
    /// Scratch buffer used only inside `collect_taskbars` on a single thread.
    static FOUND: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "system" fn enum_proc(hwnd: HWND, _l: LPARAM) -> BOOL {
    let mut buf = [0u16; 256];
    let len = GetClassNameW(hwnd, &mut buf);
    if len > 0 {
        let class = String::from_utf16_lossy(&buf[..len as usize]);
        if class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd" {
            FOUND.with(|f| f.borrow_mut().push(hwnd));
        }
    }
    TRUE
}

fn collect_taskbars() -> Vec<HWND> {
    FOUND.with(|f| f.borrow_mut().clear());
    // EnumWindows is synchronous; enum_proc runs on this thread before it returns.
    let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(0)) };
    FOUND.with(|f| f.borrow().clone())
}

fn set_window_hidden(hwnd: HWND, hidden: bool) {
    unsafe {
        // SW_HIDE genuinely removes the taskbar; SW_SHOWNA shows it again without
        // stealing activation. More reliable on Windows 11 than fighting the
        // shell's layered-alpha management.
        let _ = ShowWindow(hwnd, if hidden { SW_HIDE } else { SW_SHOWNA });
    }
}

/// Apply the requested visibility to every taskbar window on every monitor.
pub fn set_hidden(hidden: bool) {
    let bars = collect_taskbars();
    for hwnd in bars {
        set_window_hidden(hwnd, hidden);
    }
}

/// Background loop that re-applies taskbar hiding (Windows keeps resetting the
/// alpha). Runs on its OWN thread — deliberately NOT on the keyboard-hook thread
/// — so the relatively slow `EnumWindows` work here can never delay the hook
/// callback and cause the Win key to leak through.
pub fn rehide_loop() {
    use std::sync::atomic::Ordering;
    let mut last_log = (0u32, 0u32);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(800));
        if crate::state::HIDE_ENABLED.load(Ordering::SeqCst) {
            set_hidden(true);
        }
        // Gentle heartbeat so the (idle) launcher UI notices show/hide requests.
        if let Some(ctx) = crate::state::EGUI_CTX.get() {
            ctx.request_repaint();
        }
        // Log keyboard-hook activity when it changes, for field diagnostics.
        let now = (
            crate::hook::HOOK_CALLS.load(Ordering::Relaxed),
            crate::hook::WIN_EVENTS.load(Ordering::Relaxed),
        );
        if now != last_log {
            last_log = now;
            crate::state::log(&format!(
                "hook activity: total_keys={} win_key_events={}",
                now.0, now.1
            ));
        }
    }
}
