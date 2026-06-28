//! The core thread: a hidden message-only window that owns the keyboard hook,
//! the system-tray icon + menu, and the 1-second taskbar re-hide timer.

use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_ALT, MOD_CONTROL, VK_H};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindowTextW, LoadIconW, PeekMessageW,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterShellHookWindow, RegisterWindowMessageW,
    SetForegroundWindow, TrackPopupMenu, TranslateMessage, HMENU, HSHELL_WINDOWACTIVATED,
    HSHELL_WINDOWCREATED, HWND_MESSAGE, IDI_APPLICATION, MF_CHECKED, MF_STRING, MF_UNCHECKED, MSG,
    PM_REMOVE, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE,
    WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW,
};

use crate::state::{now_ms, APP_ENABLED, HIDE_ENABLED, HIDE_LAUNCHER, START_SUPPRESS_UNTIL_MS};
use crate::{autostart, taskbar};

const TRAY_UID: u32 = 1;
const CALLBACK_MSG: u32 = WM_APP + 1;
static SHELL_HOOK_MSG: AtomicU32 = AtomicU32::new(0);
/// Global hotkey id for the Ctrl+Alt+H system on/off toggle.
const HOTKEY_TOGGLE: i32 = 1;

const ID_TOGGLE: usize = 10;
const ID_AUTOSTART: usize = 11;
const ID_REINDEX: usize = 12;
const ID_QUIT: usize = 13;

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    }
}

fn append(hmenu: HMENU, id: usize, text: PCWSTR, checked: bool) {
    let flags = MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED };
    unsafe {
        let _ = AppendMenuW(hmenu, flags, id, text);
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(_) => return,
    };
    append(
        menu,
        ID_TOGGLE,
        w!("Hide taskbar"),
        HIDE_ENABLED.load(Ordering::SeqCst),
    );
    append(
        menu,
        ID_AUTOSTART,
        w!("Start with Windows"),
        autostart::is_enabled(),
    );
    append(menu, ID_REINDEX, w!("Reindex apps"), false);
    append(menu, ID_QUIT, w!("Quit"), false);

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // Required so the menu dismisses correctly when clicking elsewhere.
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        None,
    );
    let _ = DestroyMenu(menu);
}

fn apply_hide(enabled: bool) {
    crate::state::log(&format!("apply_hide({enabled})"));
    APP_ENABLED.store(enabled, Ordering::SeqCst);
    HIDE_ENABLED.store(enabled, Ordering::SeqCst);
    if !enabled {
        crate::hook::release_win_keys();
        HIDE_LAUNCHER.store(true, Ordering::SeqCst);
        if let Some(ctx) = crate::state::EGUI_CTX.get() {
            ctx.request_repaint();
        }
    }
    taskbar::set_hidden(enabled);
}

unsafe fn hwnd_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = GetWindowTextW(hwnd, &mut buf);
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

unsafe fn hwnd_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = GetClassNameW(hwnd, &mut buf);
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn start_suppression_active() -> bool {
    let until = START_SUPPRESS_UNTIL_MS.load(Ordering::SeqCst);
    until != 0 && now_ms() <= until
}

fn looks_like_start_surface(class: &str, title: &str) -> bool {
    let class_l = class.to_ascii_lowercase();
    let title_l = title.to_ascii_lowercase();
    title_l == "start"
        || title_l.contains("start menu")
        || title_l.contains("search")
        || class_l.contains("start")
        || class_l.contains("search")
        || class_l.contains("shellexperience")
        || class_l == "windows.ui.core.corewindow"
        || class_l == "applicationframewindow"
}

unsafe fn poll_foreground_for_start() {
    if !APP_ENABLED.load(Ordering::SeqCst) || !start_suppression_active() {
        return;
    }
    let hwnd = GetForegroundWindow();
    if hwnd.0.is_null() {
        return;
    }
    let class = hwnd_class(hwnd);
    let title = hwnd_text(hwnd);
    crate::state::log(&format!(
        "fg during Start suppress: hwnd={:?} class=''{class}'' title=''{title}''",
        hwnd.0
    ));
    if looks_like_start_surface(&class, &title) {
        crate::state::log("dismissing probable foreground Start surface");
        crate::hook::dismiss_start_menu_now();
        let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}
unsafe fn maybe_dismiss_start_window(hwnd: HWND, event: usize) {
    if !APP_ENABLED.load(Ordering::SeqCst) || !start_suppression_active() {
        return;
    }
    let class = hwnd_class(hwnd);
    let title = hwnd_text(hwnd);
    crate::state::log(&format!(
        "shell hook during Start suppress: event={event} hwnd={:?} class='{class}' title='{title}'",
        hwnd.0
    ));

    if looks_like_start_surface(&class, &title) {
        crate::state::log("dismissing probable Start menu window");
        crate::hook::dismiss_start_menu_now();
        let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let shell_msg = SHELL_HOOK_MSG.load(Ordering::Relaxed);
    if shell_msg != 0 && msg == shell_msg {
        let event = wparam.0;
        if event == HSHELL_WINDOWCREATED as usize || event == HSHELL_WINDOWACTIVATED as usize {
            maybe_dismiss_start_window(HWND(lparam.0 as *mut std::ffi::c_void), event);
        }
        return LRESULT(0);
    }

    match msg {
        WM_HOTKEY => {
            // Ctrl+Alt+H: flip taskbar-hiding. Turning it off brings the taskbar
            // (and this tray icon) back so the menu is reachable again.
            if wparam.0 as i32 == HOTKEY_TOGGLE {
                let enabled = !APP_ENABLED.load(Ordering::SeqCst);
                crate::state::log(&format!("Ctrl+Alt+H -> enabled={enabled}"));
                apply_hide(enabled);
            }
            LRESULT(0)
        }
        CALLBACK_MSG => {
            let event = (lparam.0 as u32) & 0xFFFF;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP || event == WM_CONTEXTMENU {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match (wparam.0 & 0xFFFF) as usize {
                ID_TOGGLE => apply_hide(!HIDE_ENABLED.load(Ordering::SeqCst)),
                ID_AUTOSTART => autostart::set_enabled(!autostart::is_enabled()),
                ID_REINDEX => {
                    std::thread::spawn(crate::apps::index_into_global);
                }
                ID_QUIT => {
                    crate::appbar::remove();
                    taskbar::set_hidden(false);
                    let mut nid = tray_data(hwnd);
                    let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
                    std::process::exit(0);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let mut nid = tray_data(hwnd);
            let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Run the core thread. Never returns (until Quit calls `process::exit`).
pub fn run() {
    unsafe {
        // Time-critical so the keyboard hook (installed below, on this thread)
        // is always serviced within Windows' low-level-hook timeout, even when a
        // game and our own UI are loading the CPU — otherwise the Win key leaks
        // through and opens Start / Search.
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);

        let hinstance = GetModuleHandleW(None).expect("module handle");
        let class_name = w!("hide_winbar_core");

        let hinst = HINSTANCE(hinstance.0);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!(""), // distinct from the launcher window's title
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            HMENU::default(),
            hinst,
            None,
        )
        .expect("create message window");

        crate::state::log(&format!("core window created, hwnd={:?}", hwnd.0));

        // Install the Win-key hook on this (time-critical, message-pumping) thread.
        match crate::hook::install() {
            Ok(h) => crate::state::log(&format!("keyboard hook installed: {:?}", h.0)),
            Err(e) => crate::state::log(&format!("HOOK INSTALL FAILED: {e:?}")),
        }

        let shell_msg = RegisterWindowMessageW(w!("SHELLHOOK"));
        SHELL_HOOK_MSG.store(shell_msg, Ordering::Relaxed);
        if RegisterShellHookWindow(hwnd).as_bool() {
            crate::state::log(&format!("registered shell hook message={shell_msg}"));
        } else {
            crate::state::log("RegisterShellHookWindow failed");
        }

        // Register Ctrl+Alt+H as the system on/off toggle. RegisterHotKey is
        // handled directly by Windows, so it's immune to the low-level-hook
        // timeout that makes a Win-combo unreliable.
        match RegisterHotKey(hwnd, HOTKEY_TOGGLE, MOD_CONTROL | MOD_ALT, VK_H.0 as u32) {
            Ok(()) => crate::state::log("registered Ctrl+Alt+H toggle hotkey"),
            Err(e) => crate::state::log(&format!("RegisterHotKey failed: {e:?}")),
        }

        // Tray icon.
        let hicon = LoadIconW(None, IDI_APPLICATION).unwrap_or_default();
        let mut nid = tray_data(hwnd);
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = CALLBACK_MSG;
        nid.hIcon = hicon;
        let tip = "hide_winbar\0".encode_utf16().collect::<Vec<u16>>();
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        let _ = Shell_NotifyIconW(NIM_ADD, &mut nid);

        // Hide immediately at startup; the steady-state re-hiding runs on the
        // separate `taskbar::rehide_loop` thread (kept off this hook thread).
        if HIDE_ENABLED.load(Ordering::SeqCst) {
            taskbar::set_hidden(true);
        }

        // Reserve the top strip for our bar so maximized windows stop below it
        // (this window's messages are pumped by the loop below).
        crate::appbar::install();

        // Message loop. This thread now does almost nothing between rare tray /
        // toggle messages, so the keyboard hook callback is always serviced fast.
        let mut message = MSG::default();
        loop {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                if message.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
                    return;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            poll_foreground_for_start();
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}
