# fast-win-launcher

**Make Windows 11 feel calmer, faster, and a little more like macOS or Linux —
without giving up what makes Windows good.**

`fast-win-launcher` is a tiny, fast, **local-only** Windows 11 utility that gets
the OS out of your way. Instead of the Start menu, the taskbar, and a dozen
right-click trips, you get one keystroke, one clean launcher, and a short top
bar — so you spend less time navigating Windows' menu systems and more time
doing the thing you opened the computer to do.

It's deliberately small and single-purpose. It doesn't replace Windows; it just
**streamlines the parts you touch most** while everything underneath — your
apps, drivers, games, and shortcuts — keeps working exactly as before.

Built in Rust. A single native `.exe`, no runtime, no installer, **no network
calls** of any kind.

## What it does

1. **A fast Win-key app launcher.** Tap **Win** to open a clean, rounded launcher
   (a nicer Win+R) with fuzzy, indexed search over every app on your PC. Type a
   few letters, press **Enter**, the app launches. It also does quick math and
   time answers, has favorites, and a power menu — so Start search becomes
   unnecessary.
2. **A short macOS-Tahoe-style top bar.** Along the top edge: the date and time,
   a **magnifying-glass** that opens the launcher, and a **settings (gear)**
   button. The gear pops a small panel in the top-right with:
   - a draggable **sound slider** (you can also just **scroll the wheel while
     hovering the bar** to change the volume),
   - an **appearance toggle** (translucent / dark),
   - a round button that opens the Windows **Settings** app.

   The strip the bar occupies is **reserved like a real screen edge**, so
   maximized windows stop below it and their tabs/buttons stay reachable.
3. **A taskbar hider.** Hide the taskbar to reclaim the screen and the clutter,
   and bring it back instantly when you want it (**Ctrl+Alt+H**, or the tray menu).

The result is a desktop that leans on **keyboard + one bar** instead of the
Start menu and taskbar — the kind of flow macOS (Spotlight + menu bar) and Linux
tiling setups are loved for — running on the Windows you already have.

## Pair it with a decluttered Windows

This tool handles the *surface* you interact with. To clean up the rest of
Windows 11 — debloat, disable telemetry, remove preinstalled junk, and tweak
sane defaults — I highly recommend **Chris Titus Tech's WinUtil**:

> https://github.com/ChrisTitusTech/winutil

Run that once to de-clutter the system, then keep `fast-win-launcher` running for
the day-to-day flow. Together they make Windows 11 feel noticeably lighter while
keeping its compatibility and hardware support.

## Why you can trust it

- **Fully local & offline.** It never touches the network. The only things it
  reads are the standard shell *AppsFolder* (the same list Start search shows)
  and your icons; the only thing it writes is one optional registry value under
  `HKCU\…\Run` when you enable "Start with Windows", plus a tiny settings file in
  `%LOCALAPPDATA%`.
- **Small, readable source.** A handful of short modules, each doing one thing.
  Every Win32 call is in plain sight.
- **Reputable dependencies only:** Microsoft's official `windows` crate for the
  OS APIs and `eframe`/`egui` (the widely-used Rust GUI) for the UI.
- **No admin rights**, no background services, no telemetry.

## Build

Requires a recent stable Rust (1.85+) with the `x86_64-pc-windows-msvc` target.

```sh
cargo build --release
# -> target\release\fast-win-launcher.exe
```

## Run

Run the **release** exe with no arguments (the debug build opens a console; the
release build runs silently):

```sh
.\target\release\fast-win-launcher.exe
```

On launch the taskbar is hidden, the Win key is remapped, and the top bar
appears. To start it automatically with Windows, right-click the tray icon →
**Start with Windows** (press **Ctrl+Alt+H** first to bring the taskbar/tray back
if it's hidden).

### Keyboard controls

| Action | What it does |
|--------|--------------|
| **Tap Win** | Open the launcher (tap again to close). The Start menu never opens. |
| **Ctrl+Alt+H** | Toggle taskbar-hiding on/off. The tray icon lives *on* the taskbar, so this is your main switch to bring it (and the tray) back. |
| **Win+D / Win+E / Win+arrows / …** | Native Windows combos still work. |

### Using the launcher

- **Type** to fuzzy-search; **↑/↓** to move the selection; **Enter** to launch.
- **Esc**, **tap Win again**, or **click away** → dismiss.
- **▾** (right side, empty query) → list every app with icons; **★** to favorite.
- Type a sum (e.g. `12*8`) or `time` for a quick answer; type `power` for a
  shutdown/restart/sleep submenu.

### Tray menu (right-click the tray icon — reachable when the taskbar is shown)

- **Hide taskbar** — toggle the taskbar on/off (same as Ctrl+Alt+H)
- **Start with Windows** — add/remove the autostart entry
- **Reindex apps** — rebuild the app list
- **Quit** — restores the taskbar (and the reserved top strip) and exits

### Command-line flags (parity / recovery)

```sh
fast-win-launcher.exe --hide   # hide the taskbar once and exit
fast-win-launcher.exe --show   # restore the taskbar and exit (use this if it
                               # ever gets stuck hidden after a crash)
fast-win-launcher.exe --list   # self-test: write the indexed app list to
                               # %TEMP%\hide_winbar_apps.txt
```

## How it works

| Piece | File | Mechanism |
|------|------|-----------|
| Hide taskbar | `src/taskbar.rs` | `EnumWindows` finds `Shell_TrayWnd` / `Shell_SecondaryTrayWnd`; `WS_EX_LAYERED` + `SetLayeredWindowAttributes(alpha=0)`, re-applied on a 1s timer |
| Win-key remap | `src/hook.rs` | `WH_KEYBOARD_LL` hook; a lone tap toggles the launcher; other combos are preserved by synthesizing a real `LWIN` only when a second key is pressed |
| Taskbar toggle hotkey | `src/tray.rs` | **Ctrl+Alt+H** via `RegisterHotKey` (handled directly by Windows, so it's immune to the low-level-hook timeout) flips taskbar-hiding on/off |
| Tray + core loop | `src/tray.rs` | message-only window, `Shell_NotifyIcon`, popup menu, the 1s timer |
| App index | `src/apps.rs` | enumerates `shell:AppsFolder` (Win32 **and** UWP): display name, parsing name, icon |
| Fuzzy match | `src/fuzzy.rs` | dependency-free subsequence scorer with prefix / word-boundary / run bonuses |
| Launcher UI | `src/launcher.rs` | `eframe`/`egui` frameless rounded window, hidden until the Win key is tapped |
| Top bar + settings | `src/topbar.rs` | always-on-top egui *immediate* viewports; date/time, magnifier → launcher, gear → settings panel; vector-drawn icons; translucent (DWM per-pixel alpha) / dark toggle |
| Reserve top strip | `src/appbar.rs` | `SHAppBarMessage` registers an AppBar at the top edge so maximized windows stop below the bar |
| Volume control | `src/volume.rs` | Core Audio `IAudioEndpointVolume` (master scalar + mute) |
| Autostart | `src/autostart.rs` | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |

## Notes & limitations

- The taskbar is made *invisible*, not removed. If it ever gets stuck hidden
  (e.g. after a crash), run `fast-win-launcher.exe --show`, or press **Ctrl+Alt+H**.
- The Win-key behaviour relies on a global low-level hook. To stay responsive it
  does the bare minimum inside the hook callback.
- "Translucent" mode uses real per-pixel alpha (your wallpaper shows through);
  "Dark" mode is a solid bar. Your choice is remembered across restarts.

## License

MIT.
