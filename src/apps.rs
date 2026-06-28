//! Indexing and launching of installed applications.
//!
//! We enumerate the shell **AppsFolder** namespace, which is exactly the set of
//! apps shown by Start / `Get-StartApps` — covering both classic Win32 programs
//! and UWP/Store apps. For each we capture the display name, an absolute
//! parsing name (used to relaunch), and the icon pixels.

use std::ffi::c_void;

use windows::core::w;
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, IShellItem, IShellItemImageFactory,
    SHCreateItemFromParsingName, ShellExecuteW, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
    SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::state::{set_apps, AppEntry};

const ICON_PX: i32 = 40;

unsafe fn pwstr_into_string(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = p.to_string().unwrap_or_default();
    windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const c_void));
    s
}

/// Pull the pixels out of an `HBITMAP` as top-down RGBA.
unsafe fn bitmap_to_rgba(
    hbm: windows::Win32::Graphics::Gdi::HBITMAP,
) -> Option<(u32, u32, Vec<u8>)> {
    let mut bm = BITMAP::default();
    let got = GetObjectW(
        HGDIOBJ(hbm.0),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut c_void),
    );
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
        return None;
    }
    let w = bm.bmWidth;
    let h = bm.bmHeight;

    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // negative => top-down rows
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut buf = vec![0u8; (w * h * 4) as usize];
    let hdc = GetDC(None);
    let lines = GetDIBits(
        hdc,
        hbm,
        0,
        h as u32,
        Some(buf.as_mut_ptr() as *mut c_void),
        &mut bmi,
        DIB_RGB_COLORS,
    );
    ReleaseDC(None, hdc);
    if lines == 0 {
        return None;
    }

    // DIB is BGRA; convert to RGBA. If the icon carries no alpha at all, make it
    // fully opaque so it doesn't render invisible.
    let mut any_alpha = false;
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
        if px[3] != 0 {
            any_alpha = true;
        }
    }
    if !any_alpha {
        for px in buf.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    Some((w as u32, h as u32, buf))
}

unsafe fn extract_icon(item: &IShellItem) -> Option<(u32, u32, Vec<u8>)> {
    let factory: IShellItemImageFactory = item.cast().ok()?;
    let size = SIZE {
        cx: ICON_PX,
        cy: ICON_PX,
    };
    let hbm = factory
        .GetImage(size, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK)
        .ok()?;
    let result = bitmap_to_rgba(hbm);
    let _ = DeleteObject(HGDIOBJ(hbm.0));
    result
}

unsafe fn enumerate() -> windows::core::Result<Vec<AppEntry>> {
    let apps_folder: IShellItem =
        SHCreateItemFromParsingName(windows::core::w!("shell:AppsFolder"), None)?;
    let enumerator: IEnumShellItems = apps_folder.BindToHandler(None, &BHID_EnumItems)?;

    let mut out = Vec::new();
    loop {
        let mut fetched: [Option<IShellItem>; 1] = [None];
        let mut count: u32 = 0;
        enumerator.Next(&mut fetched, Some(&mut count))?;
        if count == 0 {
            break;
        }
        let Some(item) = fetched[0].take() else {
            break;
        };

        let name = pwstr_into_string(item.GetDisplayName(SIGDN_NORMALDISPLAY)?);
        // Parent-relative parsing name == the AppsFolder ID (AUMID for UWP, the
        // app id for Win32). Usable as `shell:AppsFolder\<id>` to launch either.
        let parsing = pwstr_into_string(item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING)?);
        if name.is_empty() || parsing.is_empty() {
            continue;
        }
        let icon = extract_icon(&item);
        out.push(AppEntry {
            lower_name: name.to_lowercase(),
            name,
            parsing_name: parsing,
            icon,
        });
    }

    out.push(AppEntry {
        name: "Volume mixer".to_owned(),
        lower_name: "volume mixer sound audio settings".to_owned(),
        parsing_name: "ms-settings:apps-volume".to_owned(),
        icon: None,
    });

    out.sort_by(|a, b| a.lower_name.cmp(&b.lower_name));
    Ok(out)
}

/// Enumerate all apps on the calling thread (initializes COM for this thread).
pub fn collect() -> Vec<AppEntry> {
    unsafe {
        // Apartment-threaded: required for the shell namespace COM objects.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        enumerate().unwrap_or_default()
    }
}

/// Build the index on the calling (background) thread and publish it.
pub fn index_into_global() {
    set_apps(collect());
}

/// Launch an app by its AppsFolder id. We delegate to `explorer.exe` so the app
/// starts as the normal user even when *we* are running elevated (an elevated
/// process otherwise can't launch Store apps, and would launch Win32 apps
/// elevated). This works for both Win32 and UWP/Store apps.
pub fn launch(app_id: &str) {
    let (file, params, target) = if app_id.starts_with("ms-settings:") {
        (app_id.to_owned(), None, app_id.to_owned())
    } else {
        let target = format!("shell:AppsFolder\\{app_id}");
        ("explorer.exe".to_owned(), Some(target.clone()), target)
    };
    let file_w: Vec<u16> = file.encode_utf16().chain(std::iter::once(0)).collect();
    let params_w: Option<Vec<u16>> = params
        .as_ref()
        .map(|s| s.encode_utf16().chain(std::iter::once(0)).collect());
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(file_w.as_ptr()),
            params_w
                .as_ref()
                .map(|s| PCWSTR(s.as_ptr()))
                .unwrap_or_else(PCWSTR::null),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    crate::state::log(&format!(
        "launch '{target}' -> ShellExecute ret={}",
        result.0 as isize
    ));
}
