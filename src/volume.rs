//! System master-volume control via Core Audio (`IAudioEndpointVolume`).
//!
//! Used by the top bar's sound slider and by scroll-over-the-bar volume changes.
//! The COM endpoint is cached per-thread and lazily (re)created; every call is
//! best-effort and never panics, so a transient device change can't take us down.

use std::cell::RefCell;

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};

thread_local! {
    /// Cached default-render endpoint. Dropped and rebuilt if a call fails.
    static ENDPOINT: RefCell<Option<IAudioEndpointVolume>> = const { RefCell::new(None) };
}

/// Create the master-volume interface for the current default output device.
unsafe fn create_endpoint() -> Option<IAudioEndpointVolume> {
    // Match winit's apartment so CoCreateInstance is happy; ignore the result
    // (it's almost always already initialized on the UI thread).
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
    device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok()
}

/// Run `f` against a live endpoint, rebuilding it once if the cached one fails.
fn with_endpoint<T>(f: impl Fn(&IAudioEndpointVolume) -> windows::core::Result<T>) -> Option<T> {
    ENDPOINT.with(|cell| {
        // Try the cached endpoint first.
        if let Some(ep) = cell.borrow().as_ref() {
            if let Ok(v) = f(ep) {
                return Some(v);
            }
        }
        // Rebuild and retry once.
        let fresh = unsafe { create_endpoint() }?;
        let result = f(&fresh).ok();
        *cell.borrow_mut() = Some(fresh);
        result
    })
}

/// Current master volume as a scalar in `0.0..=1.0`.
pub fn get() -> Option<f32> {
    with_endpoint(|ep| unsafe { ep.GetMasterVolumeLevelScalar() })
}

/// Set the master volume; `v` is clamped to `0.0..=1.0`.
pub fn set(v: f32) {
    let v = v.clamp(0.0, 1.0);
    let _ = with_endpoint(|ep| unsafe { ep.SetMasterVolumeLevelScalar(v, std::ptr::null()) });
    // Setting a non-zero level should also unmute, matching the OS slider.
    if v > 0.0 {
        let _ = with_endpoint(|ep| unsafe { ep.SetMute(false, std::ptr::null()) });
    }
}

/// Whether the default output is muted.
pub fn is_muted() -> bool {
    with_endpoint(|ep| unsafe { ep.GetMute().map(|m| m.as_bool()) }).unwrap_or(false)
}

/// Flip the mute state; returns the new muted state.
pub fn toggle_mute() -> bool {
    let next = !is_muted();
    let _ = with_endpoint(|ep| unsafe { ep.SetMute(next, std::ptr::null()) });
    next
}
