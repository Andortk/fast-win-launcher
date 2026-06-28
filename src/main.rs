//! hide_winbar — hide the Windows 11 taskbar and remap the Win key to a fast,
//! fuzzy app launcher. Everything runs locally; there are no network calls.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appbar;
mod apps;
mod autostart;
mod calc;
mod fuzzy;
mod hook;
mod launcher;
mod power;
mod state;
mod taskbar;
mod time_fn;
mod topbar;
mod tray;
mod volume;

fn main() {
    // Simple CLI for parity with HideTaskbar and as a recovery valve.
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Fresh diagnostics log each launch (ignore for the short-lived CLI verbs).
    if args.is_empty() {
        let _ = std::fs::remove_file(std::env::temp_dir().join("hide_winbar.log"));
        state::log("=== hide_winbar starting ===");
        // Raise our process priority so a busy game can't starve the keyboard
        // hook thread and make the Win key leak through.
        unsafe {
            use windows::Win32::System::Threading::{
                GetCurrentProcess, SetPriorityClass, HIGH_PRIORITY_CLASS,
            };
            let _ = SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS);
        }
        // Give Windows a longer leash before it bypasses our hook under load.
        match autostart::ensure_lowlevel_hooks_timeout(3000) {
            Some(v) => state::log(&format!(
                "LowLevelHooksTimeout set to {v}ms (effective after next logon)"
            )),
            None => state::log("LowLevelHooksTimeout already sufficient"),
        }
    }
    if args.iter().any(|a| a == "--hide") {
        taskbar::set_hidden(true);
        return;
    }
    if args.iter().any(|a| a == "--show") {
        taskbar::set_hidden(false);
        return;
    }
    if args.iter().any(|a| a == "--list") {
        // Self-test: write the indexed apps to a temp file for inspection.
        let apps = apps::collect();
        let mut out = format!("indexed {} apps\n", apps.len());
        for a in apps.iter().take(20) {
            let has_icon = a.icon.is_some();
            out.push_str(&format!(
                "[{}] {}\n",
                if has_icon { "icon" } else { "    " },
                a.name
            ));
        }
        let path = std::env::temp_dir().join("hide_winbar_apps.txt");
        let _ = std::fs::write(path, out);
        return;
    }
    if args.iter().any(|a| a == "--install-win-remap") {
        let ok = autostart::install_win_key_scancode_remap();
        eprintln!(
            "install Win-key scancode remap: {}. Reboot or sign out/in for it to take effect.",
            if ok {
                "ok"
            } else {
                "failed (run as Administrator)"
            }
        );
        return;
    }
    if args.iter().any(|a| a == "--remove-win-remap") {
        let ok = autostart::remove_win_key_scancode_remap();
        eprintln!(
            "remove Win-key scancode remap: {}. Reboot or sign out/in for it to take effect.",
            if ok {
                "ok"
            } else {
                "failed (run as Administrator or value missing)"
            }
        );
        return;
    }

    // Demo mode: just show the launcher immediately (no hook, no taskbar
    // changes). Used to visually verify the UI in isolation.
    let demo = args.iter().any(|a| a == "--demo");

    if !demo {
        // Core thread: keyboard hook + tray icon, at time-critical priority and
        // kept otherwise idle so the hook callback is always serviced within
        // Windows' low-level-hook timeout (no Win-key leak to Start / Search).
        std::thread::spawn(tray::run);
        // Steady-state taskbar re-hiding runs on its own thread, off the hook
        // thread, so its EnumWindows work can't delay the hook.
        std::thread::spawn(taskbar::rehide_loop);
    }

    // Index installed apps in the background so startup stays instant.
    std::thread::spawn(apps::index_into_global);

    if demo {
        state::SHOW_LAUNCHER.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    // UI thread (this thread): the launcher window, hidden until Win is tapped.
    if let Err(e) = launcher::run() {
        eprintln!("launcher exited: {e:?}");
    }

    // Make sure we never leave the taskbar stuck hidden on exit.
    taskbar::set_hidden(false);
}
