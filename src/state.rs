//! Shared global state, used to communicate between the core (hook/tray) thread
//! and the eframe UI thread without passing handles around explicitly.

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Mutex, OnceLock};

/// Append a line to `%TEMP%\hide_winbar.log` for field diagnostics. Cheap and
/// best-effort; never panics.
pub fn log(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("hide_winbar.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{msg}");
    }
}

/// Marker stuffed into `dwExtraInfo` of any keystroke *we* synthesize, so our
/// own low-level keyboard hook can recognize and pass through its own events
/// instead of reprocessing them. ASCII "HWIB".
pub const INJECT_SENTINEL: usize = 0x4857_4942;

/// True while hide_winbar is actively remapping Win and hiding the taskbar.
pub static APP_ENABLED: AtomicBool = AtomicBool::new(true);

/// True while taskbar-hiding is active. Flipped from the tray menu / hotkey.
pub static HIDE_ENABLED: AtomicBool = AtomicBool::new(true);

/// Set by the keyboard hook on a lone Win tap; consumed by the UI thread to
/// show the launcher.
pub static SHOW_LAUNCHER: AtomicBool = AtomicBool::new(false);

/// Set by the keyboard hook to dismiss the launcher (Win tapped while open).
pub static HIDE_LAUNCHER: AtomicBool = AtomicBool::new(false);

/// Published by the launcher so the hook knows whether a Win tap should open or
/// close it.
pub static LAUNCHER_VISIBLE: AtomicBool = AtomicBool::new(false);

/// While this deadline is in the future, any shell Start-menu window opened by
/// Explorer should be dismissed. Milliseconds since Unix epoch.
pub static START_SUPPRESS_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The egui context, published by the launcher once eframe has started. The
/// hook thread uses it to wake the (possibly hidden) UI when the Win key is
/// tapped.
pub static EGUI_CTX: OnceLock<eframe::egui::Context> = OnceLock::new();

/// A single indexed application.
#[derive(Clone)]
pub struct AppEntry {
    pub name: String,
    /// SIGDN_DESKTOPABSOLUTEPARSING string, round-tripped at launch time.
    pub parsing_name: String,
    pub lower_name: String,
    /// Extracted icon as `(width, height, tightly-packed RGBA)`, if available.
    pub icon: Option<(u32, u32, Vec<u8>)>,
}

/// The indexed app list, filled by the background indexer and read by the UI.
pub static APPS: OnceLock<Mutex<Vec<AppEntry>>> = OnceLock::new();

/// Bumped every time the app list is replaced, so the UI can invalidate its
/// icon-texture cache.
pub static APPS_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn apps() -> &'static Mutex<Vec<AppEntry>> {
    APPS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Replace the indexed app list and notify the UI to refresh.
pub fn set_apps(list: Vec<AppEntry>) {
    *apps().lock().unwrap() = list;
    APPS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if let Some(ctx) = EGUI_CTX.get() {
        ctx.request_repaint();
    }
}
