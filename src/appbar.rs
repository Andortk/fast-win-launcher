//! Reserve the top strip of the screen for our top bar, using the Win32 **AppBar**
//! API (`SHAppBarMessage`) — the same mechanism the Windows taskbar uses. Once
//! registered, maximized windows stop *below* the reserved strip, so the bar no
//! longer covers their tabs / window buttons; the area acts like a screen edge.
//!
//! We register against a dedicated, invisible, click-through top-level window
//! owned by the core (tray) thread, whose message loop already pumps messages.
//! The visible bar is the separate egui viewport that floats over this strip.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, APPBARDATA, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS,
    ABN_POSCHANGED,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetSystemMetrics, MoveWindow, RegisterClassW,
    SetLayeredWindowAttributes, HMENU, LWA_ALPHA, SM_CXSCREEN, WM_DESTROY, WM_DISPLAYCHANGE,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};
use windows::Win32::Foundation::COLORREF;

use crate::topbar::BAR_H;

/// Our appbar window handle (as isize so it's a plain `static`).
static APPBAR_HWND: AtomicIsize = AtomicIsize::new(0);
/// Custom message Windows uses to notify us of appbar changes.
const APPBAR_CALLBACK: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 7;

/// Physical pixel height of the reserved strip (logical bar height × system DPI).
fn bar_height_px() -> i32 {
    let dpi = unsafe { GetDpiForSystem() }.max(96) as f32;
    (BAR_H * dpi / 96.0).round() as i32
}

fn base_data(hwnd: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        ..Default::default()
    }
}

/// (Re)assert the reserved rectangle at the top edge of the primary screen.
unsafe fn set_pos(hwnd: HWND) {
    let width = GetSystemMetrics(SM_CXSCREEN);
    let height = bar_height_px();

    let mut abd = base_data(hwnd);
    abd.uEdge = ABE_TOP;
    abd.rc = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };

    // Let Windows adjust the proposed rect for any other appbars, then pin our
    // thickness to the top edge and commit it.
    SHAppBarMessage(ABM_QUERYPOS, &mut abd);
    abd.rc.left = 0;
    abd.rc.right = width;
    abd.rc.top = 0;
    abd.rc.bottom = abd.rc.top + height;
    SHAppBarMessage(ABM_SETPOS, &mut abd);

    let _ = MoveWindow(
        hwnd,
        abd.rc.left,
        abd.rc.top,
        abd.rc.right - abd.rc.left,
        abd.rc.bottom - abd.rc.top,
        true,
    );
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        APPBAR_CALLBACK => {
            if wparam.0 as u32 == ABN_POSCHANGED {
                set_pos(hwnd);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            set_pos(hwnd);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            let mut abd = base_data(hwnd);
            SHAppBarMessage(ABM_REMOVE, &mut abd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Create the appbar window and reserve the top strip. Call once from the core
/// thread (its message loop dispatches our window's messages).
pub fn install() {
    unsafe {
        let hinstance = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(_) => return,
        };
        let hinst = HINSTANCE(hinstance.0);
        let class_name = w!("hide_winbar_appbar");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = match CreateWindowExW(
            // Invisible, click-through, never-activated helper window.
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_TOPMOST,
            class_name,
            w!("hide_winbar_appbar"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            HMENU::default(),
            hinst,
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                crate::state::log(&format!("appbar: CreateWindow failed: {e:?}"));
                return;
            }
        };
        // Fully transparent: it only exists to own the reservation.
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);

        APPBAR_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

        let mut abd = base_data(hwnd);
        abd.uCallbackMessage = APPBAR_CALLBACK;
        if SHAppBarMessage(ABM_NEW, &mut abd) == 0 {
            crate::state::log("appbar: ABM_NEW failed");
            return;
        }
        set_pos(hwnd);
        crate::state::log("appbar: reserved top strip");
    }
}

/// Release the reserved strip (restores the full work area). Call on quit.
pub fn remove() {
    let raw = APPBAR_HWND.swap(0, Ordering::SeqCst);
    if raw == 0 {
        return;
    }
    unsafe {
        let mut abd = base_data(HWND(raw as *mut _));
        SHAppBarMessage(ABM_REMOVE, &mut abd);
    }
}
