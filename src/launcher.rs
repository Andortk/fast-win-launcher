//! The launcher window: a small rounded box (a nicer Win+R) with fuzzy app
//! search and an icon list. Runs on the main thread under eframe; stays hidden
//! until the Win key is tapped.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Key, Margin, Pos2, Rounding, Stroke, Vec2};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetActiveWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, FindWindowW, GetForegroundWindow, GetShellWindow, GetSystemMetrics,
    GetWindowLongPtrW, GetWindowRect, SetForegroundWindow, SetLayeredWindowAttributes,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW, GWL_EXSTYLE, HWND_TOPMOST,
    LWA_ALPHA, SM_CXSCREEN, SM_CYSCREEN, SPI_SETFOREGROUNDLOCKTIMEOUT, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_SHOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WS_EX_LAYERED, WS_EX_TOOLWINDOW,
};

use crate::state::{
    apps, AppEntry, APPS_GENERATION, EGUI_CTX, HIDE_LAUNCHER, LAUNCHER_VISIBLE, SHOW_LAUNCHER,
};
use crate::{apps as appsmod, fuzzy};

const WIDTH: f32 = 640.0;
const HEADER_H: f32 = 64.0;
const ROW_H: f32 = 44.0;
const MAX_ROWS: usize = 9;

/// Where the window lives while "hidden": far outside any monitor. We keep the
/// window OS-visible (so winit keeps running our update loop) but parked here.
const OFFSCREEN: [f32; 2] = [-32000.0, -32000.0];

pub struct Launcher {
    query: String,
    /// Indices into `self.cache`, best match first.
    results: Vec<usize>,
    /// Quick-math result for the current query, shown as a row above the apps.
    math: Option<f64>,
    /// Local-time result for time queries, shown alongside quick math.
    time: Option<String>,
    power_menu: bool,
    selected: usize,
    show_all: bool,
    visible: bool,
    focus_input: bool,
    just_shown: u32,
    /// True once the window has actually held OS focus — guards against hiding
    /// before Windows grants foreground.
    focused_once: bool,
    cache: Vec<AppEntry>,
    generation: u64,
    textures: HashMap<usize, egui::TextureHandle>,
    favorites: HashSet<String>,
    cur_height: f32,
    hwnd: Option<HWND>,
    styled: bool,
    shown_at: Instant,
    /// The always-visible macOS-style top bar (its own viewport).
    topbar: crate::topbar::TopBar,
}

/// Best-effort: pull our window to the foreground even though we consumed the
/// Win key (so Windows doesn't consider us the input owner). Uses the standard
/// AttachThreadInput trick.
unsafe fn force_foreground(hwnd: HWND) {
    // With the foreground-lock timeout set to 0 (see `run`), a plain activation
    // sequence is honored. We deliberately avoid AttachThreadInput here — in
    // testing it made activation *less* reliable, not more.
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = BringWindowToTop(hwnd);
    let _ = SetForegroundWindow(hwnd);
    let _ = SetActiveWindow(hwnd);
    let _ = SetFocus(hwnd);
}

fn screen_size() -> (f32, f32) {
    unsafe {
        (
            GetSystemMetrics(SM_CXSCREEN) as f32,
            GetSystemMetrics(SM_CYSCREEN) as f32,
        )
    }
}

fn favorites_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("hide_winbar").join("favorites.txt")
}

fn load_favorites() -> HashSet<String> {
    fs::read_to_string(favorites_path())
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn save_favorites(favorites: &HashSet<String>) {
    let path = favorites_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut entries: Vec<&str> = favorites.iter().map(String::as_str).collect();
    entries.sort_unstable();
    let _ = fs::write(path, entries.join("\n"));
}
impl Launcher {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        // Publish the context so the hook thread can wake us.
        let _ = EGUI_CTX.set(ctx.clone());

        let mut style = (*ctx.style()).clone();
        for (_, font) in style.text_styles.iter_mut() {
            font.size *= 1.15;
        }
        ctx.set_style(style);

        Self {
            query: String::new(),
            results: Vec::new(),
            math: None,
            time: None,
            power_menu: false,
            selected: 0,
            show_all: false,
            visible: false,
            focus_input: false,
            just_shown: 0,
            focused_once: false,
            cache: Vec::new(),
            generation: u64::MAX,
            textures: HashMap::new(),
            favorites: load_favorites(),
            cur_height: HEADER_H,
            hwnd: None,
            styled: false,
            shown_at: Instant::now(),
            topbar: crate::topbar::TopBar::default(),
        }
    }

    /// Move the window on/off screen via Win32 (eframe's root-viewport
    /// OuterPosition is a no-op on Windows). Returns false if the HWND isn't
    /// known yet, so the caller can fall back to a viewport command.
    fn move_window(&mut self, center: bool) -> bool {
        let Some(hwnd) = self.window_handle() else {
            return false;
        };
        unsafe {
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            let w = r.right - r.left;
            let (x, y, flags) = if center {
                let sw = GetSystemMetrics(SM_CXSCREEN);
                let sh = GetSystemMetrics(SM_CYSCREEN);
                (
                    ((sw - w) / 2).max(0),
                    (sh as f32 * 0.22) as i32,
                    SWP_NOSIZE | SWP_SHOWWINDOW,
                )
            } else {
                (
                    OFFSCREEN[0] as i32,
                    OFFSCREEN[1] as i32,
                    SWP_NOSIZE | SWP_NOACTIVATE,
                )
            };
            let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, 0, 0, flags);
        }
        true
    }

    /// Locate our own top-level window by title (cached).
    fn window_handle(&mut self) -> Option<HWND> {
        if self.hwnd.is_none() {
            let title: Vec<u16> = "hide_winbar"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let hwnd = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) }.ok();
            self.hwnd = hwnd;
        }
        self.hwnd
    }

    fn refresh_cache_if_needed(&mut self) {
        let gen = APPS_GENERATION.load(Ordering::SeqCst);
        if gen != self.generation {
            self.cache = apps().lock().unwrap().clone();
            self.generation = gen;
            self.textures.clear();
            self.recompute();
        }
    }

    fn recompute(&mut self) {
        let q = self.query.trim();
        self.math = crate::calc::eval(q);
        self.time = crate::time_fn::eval(q);
        if !q.eq_ignore_ascii_case("power") {
            self.power_menu = false;
        }
        let ranked = fuzzy::rank(&self.query, &self.cache);
        let mut starred = Vec::new();
        let mut others = Vec::new();
        for idx in ranked {
            if self.is_favorite(idx) {
                starred.push(idx);
            } else {
                others.push(idx);
            }
        }
        starred.extend(others);
        self.results = starred;
        self.selected = 0;
    }

    fn is_favorite(&self, idx: usize) -> bool {
        self.cache
            .get(idx)
            .map(|app| self.favorites.contains(&app.parsing_name))
            .unwrap_or(false)
    }

    fn toggle_favorite(&mut self, idx: usize) {
        let Some(app_id) = self.cache.get(idx).map(|app| app.parsing_name.clone()) else {
            return;
        };
        if !self.favorites.insert(app_id.clone()) {
            self.favorites.remove(&app_id);
        }
        save_favorites(&self.favorites);
        self.recompute();
    }

    fn set_window_alpha(&mut self, alpha: u8) -> bool {
        let Some(hwnd) = self.window_handle() else {
            return false;
        };
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if (ex & WS_EX_LAYERED.0 as isize) == 0 {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
            }
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
        }
        true
    }

    fn apply_round_region(&mut self) -> bool {
        let Some(hwnd) = self.window_handle() else {
            return false;
        };
        unsafe {
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            let w = r.right - r.left;
            let h = r.bottom - r.top;
            if w <= 0 || h <= 0 {
                return false;
            }
            let radius = 32;
            let region = CreateRoundRectRgn(0, 0, w + 1, h + 1, radius, radius);
            if region.0.is_null() {
                return false;
            }
            let _ = SetWindowRgn(hwnd, region, true);
        }
        true
    }

    fn show(&mut self, ctx: &egui::Context) {
        crate::state::log("launcher: show()");
        self.visible = true;
        self.shown_at = Instant::now();
        self.just_shown = 8;
        self.focus_input = true;
        self.focused_once = false;
        self.query.clear();
        self.show_all = false;
        self.power_menu = false;
        self.refresh_cache_if_needed();
        self.recompute();
        let _ = self.set_window_alpha(255);
        let _ = self.apply_round_region();
        LAUNCHER_VISIBLE.store(true, Ordering::SeqCst);

        if !self.move_window(true) {
            // HWND not resolved yet: fall back to viewport commands.
            let (sw, sh) = screen_size();
            let x = ((sw - WIDTH) / 2.0).max(0.0);
            let y = (sh * 0.22).max(0.0);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(Pos2::new(x, y)));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn hide(&mut self, ctx: &egui::Context) {
        self.visible = false;
        self.query.clear();
        self.show_all = false;
        self.power_menu = false;
        LAUNCHER_VISIBLE.store(false, Ordering::SeqCst);
        let _ = self.set_window_alpha(0);
        // Park off-screen rather than truly hiding, so winit keeps delivering
        // frames and we still notice the next show request.
        if !self.move_window(false) {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(Pos2::new(
                OFFSCREEN[0],
                OFFSCREEN[1],
            )));
        }
        if let Some(hwnd) = self.window_handle() {
            unsafe {
                if GetForegroundWindow() == hwnd {
                    let shell = GetShellWindow();
                    if shell.0 != std::ptr::null_mut() {
                        let _ = SetForegroundWindow(shell);
                    }
                }
            }
        }
    }

    fn texture(&mut self, ctx: &egui::Context, idx: usize) -> Option<egui::TextureHandle> {
        if let Some(t) = self.textures.get(&idx) {
            return Some(t.clone());
        }
        let (w, h, rgba) = self.cache.get(idx)?.icon.as_ref()?;
        let img = egui::ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], rgba);
        let tex = ctx.load_texture(format!("icon{idx}"), img, egui::TextureOptions::LINEAR);
        self.textures.insert(idx, tex.clone());
        Some(tex)
    }

    fn power_root_visible(&self) -> bool {
        !self.power_menu && self.query.trim().eq_ignore_ascii_case("power")
    }

    /// Number of leading non-app rows (quick answers before app results).
    fn math_rows(&self) -> usize {
        usize::from(self.math.is_some())
            + usize::from(self.time.is_some())
            + usize::from(self.power_root_visible())
            + if self.power_menu {
                crate::power::ACTIONS.len()
            } else {
                0
            }
    }

    /// Activate the row at display position `pos`: copy a quick answer, open a
    /// submenu, run a power action, or launch the corresponding app.
    fn activate(&mut self, pos: usize, ctx: &egui::Context) {
        let mut row = pos;
        if let Some(v) = self.math {
            if row == 0 {
                let text = crate::calc::format_result(v);
                ctx.output_mut(|o| o.copied_text = text); // to clipboard
                self.hide(ctx);
                return;
            }
            row -= 1;
        }
        if let Some(text) = self.time.clone() {
            if row == 0 {
                ctx.output_mut(|o| o.copied_text = text); // to clipboard
                self.hide(ctx);
                return;
            }
            row -= 1;
        }
        if self.power_root_visible() {
            if row == 0 {
                self.power_menu = true;
                self.selected = 0;
                return;
            }
            row -= 1;
        }
        if self.power_menu {
            if let Some(action) = crate::power::ACTIONS.get(row).copied() {
                action.run();
                self.hide(ctx);
                return;
            }
            row = row.saturating_sub(crate::power::ACTIONS.len());
        }
        if let Some(&idx) = self.results.get(row) {
            if let Some(app) = self.cache.get(idx) {
                appsmod::launch(&app.parsing_name);
            }
        }
        self.hide(ctx);
    }
}

impl eframe::App for Launcher {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // transparent window background
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        {
            use std::sync::atomic::AtomicU32;
            static U: AtomicU32 = AtomicU32::new(0);
            let n = U.fetch_add(1, Ordering::Relaxed);
            if n == 0 || n == 50 {
                crate::state::log(&format!(
                    "launcher: update() running (frame {n}), SHOW={}",
                    SHOW_LAUNCHER.load(Ordering::Relaxed)
                ));
            }
        }
        // Poll at a modest rate (typing still repaints reactively via egui).
        // Keeping this slow minimizes CPU/GPU load so it can't contend with the
        // time-critical keyboard-hook thread and make the Win key leak through.
        ctx.request_repaint_after(Duration::from_millis(150));

        // Once, mark the window as a tool window so it stays out of Alt+Tab.
        if !self.styled {
            if let Some(hwnd) = self.window_handle() {
                unsafe {
                    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(
                        hwnd,
                        GWL_EXSTYLE,
                        ex | WS_EX_TOOLWINDOW.0 as isize | WS_EX_LAYERED.0 as isize,
                    );
                }
                let _ = self.set_window_alpha(if self.visible { 255 } else { 0 });
                let _ = self.apply_round_region();
                self.styled = true;
            }
        }

        // The top bar is always visible; render it every frame regardless of
        // whether the launcher itself is shown.
        self.topbar.render(ctx);

        // SHOW before HIDE so a simultaneous toggle ends hidden.
        if SHOW_LAUNCHER.swap(false, Ordering::SeqCst) {
            self.show(ctx);
        }
        if HIDE_LAUNCHER.swap(false, Ordering::SeqCst) {
            self.hide(ctx);
        }
        if !self.visible {
            return;
        }
        self.refresh_cache_if_needed();

        // Focus tracking from two angles: the OS foreground window, and egui's
        // own viewport focus (winit's WM_*FOCUS events). Either counts as
        // focused; we only treat it as *lost* when both agree it's gone.
        let fg_focused = match self.window_handle() {
            Some(h) => unsafe { GetForegroundWindow() == h },
            None => false,
        };
        let egui_focused = ctx.input(|i| i.viewport().focused);
        if fg_focused || egui_focused == Some(true) {
            self.focused_once = true;
        }
        let lost_focus = !fg_focused && egui_focused != Some(true);

        // Force ourselves to the foreground for the first few frames after
        // showing (we consumed the Win key, so we aren't the input owner and a
        // plain focus request can be refused). Bounded by `just_shown` so we
        // don't spam SetForegroundWindow every frame (which loads the CPU).
        if self.just_shown > 0 {
            self.just_shown -= 1;
            if !self.focused_once {
                if let Some(hwnd) = self.window_handle() {
                    unsafe { force_foreground(hwnd) };
                }
                self.focus_input = true;
            }
        }

        // Auto-hide on focus loss, but only once we've genuinely held focus and
        // after a short grace period (so a transient blip during the show
        // transition doesn't instantly dismiss it).
        if self.focused_once && lost_focus && self.shown_at.elapsed() > Duration::from_millis(300) {
            crate::state::log(&format!(
                "launcher: auto-hide (fg_focused={fg_focused} egui_focused={egui_focused:?})"
            ));
            self.hide(ctx);
            return;
        }

        // Global key handling (works regardless of which widget has focus).
        let (esc, up, down, enter) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::Enter),
            )
        });
        if esc {
            self.hide(ctx);
            return;
        }

        let mrows = self.math_rows();
        let query_active = !self.query.trim().is_empty();
        let app_count = if self.power_menu || self.power_root_visible() {
            0
        } else if query_active || self.show_all {
            self.results.len()
        } else {
            self.results
                .iter()
                .take_while(|&&idx| self.is_favorite(idx))
                .count()
        };
        // Total selectable rows = optional math row + app rows.
        let visible_count = mrows + app_count;
        if visible_count > 0 {
            if down {
                self.selected = (self.selected + 1) % visible_count;
            }
            if up {
                self.selected = (self.selected + visible_count - 1) % visible_count;
            }
        }
        if enter && visible_count > 0 {
            self.activate(self.selected.min(visible_count - 1), ctx);
            return;
        }

        // --- draw ---------------------------------------------------------
        let frame = egui::Frame {
            fill: Color32::from_rgba_unmultiplied(26, 26, 30, 246),
            rounding: Rounding::same(16.0),
            stroke: Stroke::new(1.0, Color32::from_rgb(60, 60, 68)),
            inner_margin: Margin::symmetric(14.0, 12.0),
            ..Default::default()
        };

        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);

            // Search row: text field + down-arrow toggle.
            ui.horizontal(|ui| {
                let arrow = if self.show_all { "▴" } else { "▾" };
                let btn = ui.add_sized(
                    [34.0, 34.0],
                    egui::Button::new(egui::RichText::new(arrow).size(18.0)).rounding(8.0),
                );
                if btn.clicked() {
                    self.show_all = !self.show_all;
                    self.focus_input = true;
                }
                let avail = ui.available_width();
                let edit = ui.add_sized(
                    [avail, 34.0],
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search apps…")
                        .frame(false)
                        .font(egui::TextStyle::Heading),
                );
                if edit.changed() {
                    self.recompute();
                }
                // Keep the caret in the search box the whole time it's open,
                // so keystrokes always land here.
                edit.request_focus();
                self.focus_input = false;
            });

            if visible_count == 0 {
                return;
            }

            ui.add_space(4.0);
            let mrows = self.math_rows();
            let math_text = self
                .math
                .map(|v| format!("{} = {}", self.query.trim(), crate::calc::format_result(v)));
            let time_text = self.time.clone().map(|v| format!("time = {v}"));
            let app_indices: Vec<usize> = self.results.iter().take(app_count).copied().collect();

            egui::ScrollArea::vertical()
                .max_height(MAX_ROWS as f32 * ROW_H)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    // Quick-math result row (Enter copies the value).
                    if let Some(text) = math_text {
                        let selected = self.selected == 0;
                        let bg = if selected {
                            Color32::from_rgb(58, 104, 196)
                        } else {
                            Color32::TRANSPARENT
                        };
                        let row = egui::Frame::none()
                            .fill(bg)
                            .rounding(8.0)
                            .inner_margin(Margin::symmetric(8.0, 6.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [28.0, 28.0],
                                        egui::Label::new(
                                            egui::RichText::new("=")
                                                .size(20.0)
                                                .color(Color32::from_rgb(120, 200, 140)),
                                        ),
                                    );
                                    ui.add_space(6.0);
                                    ui.label(
                                        egui::RichText::new(text)
                                            .size(17.0)
                                            .color(Color32::from_rgb(235, 235, 240)),
                                    );
                                    ui.add_space(8.0);
                                    ui.label(
                                        egui::RichText::new("⏎ copy")
                                            .size(12.0)
                                            .color(Color32::from_rgb(140, 140, 150)),
                                    );
                                });
                            });
                        let resp = ui.interact(
                            row.response.rect,
                            ui.id().with("mathrow"),
                            egui::Sense::click(),
                        );
                        if resp.clicked() {
                            self.activate(0, ctx);
                        }
                    }

                    if let Some(text) = time_text {
                        let pos = usize::from(self.math.is_some());
                        let selected = self.selected == pos;
                        let bg = if selected {
                            Color32::from_rgb(58, 104, 196)
                        } else {
                            Color32::TRANSPARENT
                        };
                        let row = egui::Frame::none()
                            .fill(bg)
                            .rounding(8.0)
                            .inner_margin(Margin::symmetric(8.0, 6.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [28.0, 28.0],
                                        egui::Label::new(
                                            egui::RichText::new("T")
                                                .size(18.0)
                                                .color(Color32::from_rgb(120, 190, 230)),
                                        ),
                                    );
                                    ui.add_space(6.0);
                                    ui.label(
                                        egui::RichText::new(text)
                                            .size(17.0)
                                            .color(Color32::from_rgb(235, 235, 240)),
                                    );
                                    ui.add_space(8.0);
                                    ui.label(
                                        egui::RichText::new("⏎ copy")
                                            .size(12.0)
                                            .color(Color32::from_rgb(140, 140, 150)),
                                    );
                                });
                            });
                        let resp = ui.interact(
                            row.response.rect,
                            ui.id().with("timerow"),
                            egui::Sense::click(),
                        );
                        if resp.clicked() {
                            self.activate(pos, ctx);
                        }
                    }

                    if self.power_root_visible() {
                        let pos =
                            usize::from(self.math.is_some()) + usize::from(self.time.is_some());
                        let selected = self.selected == pos;
                        let bg = if selected {
                            Color32::from_rgb(58, 104, 196)
                        } else {
                            Color32::TRANSPARENT
                        };
                        let row = egui::Frame::none()
                            .fill(bg)
                            .rounding(8.0)
                            .inner_margin(Margin::symmetric(8.0, 6.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [28.0, 28.0],
                                        egui::Label::new(
                                            egui::RichText::new("P")
                                                .size(18.0)
                                                .color(Color32::from_rgb(235, 175, 110)),
                                        ),
                                    );
                                    ui.add_space(6.0);
                                    ui.label(
                                        egui::RichText::new("Power")
                                            .size(17.0)
                                            .color(Color32::from_rgb(235, 235, 240)),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new("submenu")
                                                    .size(12.0)
                                                    .color(Color32::from_rgb(140, 140, 150)),
                                            );
                                        },
                                    );
                                });
                            });
                        let resp = ui.interact(
                            row.response.rect,
                            ui.id().with("powerroot"),
                            egui::Sense::click(),
                        );
                        if resp.clicked() {
                            self.activate(pos, ctx);
                        }
                    }

                    if self.power_menu {
                        let base =
                            usize::from(self.math.is_some()) + usize::from(self.time.is_some());
                        for (i, action) in crate::power::ACTIONS.iter().copied().enumerate() {
                            let pos = base + i;
                            let selected = self.selected == pos;
                            let bg = if selected {
                                Color32::from_rgb(58, 104, 196)
                            } else {
                                Color32::TRANSPARENT
                            };
                            let label = action.label();
                            let row = egui::Frame::none()
                                .fill(bg)
                                .rounding(8.0)
                                .inner_margin(Margin::symmetric(8.0, 6.0))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.add_sized(
                                            [28.0, 28.0],
                                            egui::Label::new(
                                                egui::RichText::new("P")
                                                    .size(18.0)
                                                    .color(Color32::from_rgb(235, 175, 110)),
                                            ),
                                        );
                                        ui.add_space(6.0);
                                        ui.label(
                                            egui::RichText::new(label)
                                                .size(17.0)
                                                .color(Color32::from_rgb(235, 235, 240)),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(Align::Center),
                                            |ui| {
                                                ui.label(
                                                    egui::RichText::new("run")
                                                        .size(12.0)
                                                        .color(Color32::from_rgb(140, 140, 150)),
                                                );
                                            },
                                        );
                                    });
                                });
                            let resp = ui.interact(
                                row.response.rect,
                                ui.id().with(("poweraction", i)),
                                egui::Sense::click(),
                            );
                            if resp.clicked() {
                                self.activate(pos, ctx);
                            }
                        }
                    }
                    for (i, idx) in app_indices.into_iter().enumerate() {
                        let pos = mrows + i;
                        let selected = pos == self.selected;
                        let name = self.cache[idx].name.clone();
                        let favorite = self.is_favorite(idx);
                        let tex = self.texture(ctx, idx);
                        let mut star_clicked = false;
                        let mut star_rect = None;

                        let bg = if selected {
                            Color32::from_rgb(58, 104, 196)
                        } else {
                            Color32::TRANSPARENT
                        };
                        let row = egui::Frame::none()
                            .fill(bg)
                            .rounding(8.0)
                            .inner_margin(Margin::symmetric(8.0, 6.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    if let Some(tex) = tex {
                                        ui.add(egui::Image::new(egui::load::SizedTexture::new(
                                            tex.id(),
                                            Vec2::new(28.0, 28.0),
                                        )));
                                    } else {
                                        ui.add_space(28.0);
                                    }
                                    ui.add_space(6.0);
                                    ui.label(
                                        egui::RichText::new(name)
                                            .size(17.0)
                                            .color(Color32::from_rgb(235, 235, 240)),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(Align::Center),
                                        |ui| {
                                            let star = if favorite { "★" } else { "☆" };
                                            let color = if favorite {
                                                Color32::from_rgb(245, 190, 80)
                                            } else {
                                                Color32::from_rgb(140, 140, 150)
                                            };
                                            let resp = ui.add_sized(
                                                [30.0, 28.0],
                                                egui::Button::new(
                                                    egui::RichText::new(star)
                                                        .size(18.0)
                                                        .color(color),
                                                )
                                                .frame(false),
                                            );
                                            star_rect = Some(resp.rect);
                                            if resp.clicked() {
                                                star_clicked = true;
                                            }
                                        },
                                    );
                                });
                            });

                        let resp = ui.interact(
                            row.response.rect,
                            ui.id().with(("row", idx)),
                            egui::Sense::click(),
                        );
                        let star_area_clicked = resp.clicked()
                            && star_rect
                                .zip(ctx.input(|i| i.pointer.interact_pos()))
                                .map(|(rect, pos)| rect.contains(pos))
                                .unwrap_or(false);
                        if star_clicked || star_area_clicked {
                            self.toggle_favorite(idx);
                        } else if resp.clicked() {
                            self.activate(pos, ctx);
                        }
                        if selected && (up || down) {
                            resp.scroll_to_me(Some(Align::Center));
                        }
                    }
                });
        });

        // Resize the window to fit the visible rows (driven via Win32, since the
        // root viewport ignores InnerSize on Windows).
        let visible_rows = visible_count.min(MAX_ROWS);
        let target = if visible_rows == 0 {
            HEADER_H
        } else {
            HEADER_H + visible_rows as f32 * ROW_H + 16.0
        };
        if (target - self.cur_height).abs() > 0.5 {
            self.cur_height = target;
            let scale = ctx.pixels_per_point();
            if let Some(hwnd) = self.window_handle() {
                unsafe {
                    let mut r = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut r);
                    let w = r.right - r.left;
                    let h = (target * scale).round() as i32;
                    let _ = SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        0,
                        0,
                        w,
                        h,
                        SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                    let _ = self.apply_round_region();
                }
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(WIDTH, target)));
            }
        }
    }
}

/// Build the eframe window (parked off-screen initially) and run the UI loop.
pub fn run() -> eframe::Result<()> {
    crate::state::log("launcher::run() entered (starting eframe)");
    // Allow us to take the foreground when the Win key is tapped: with the lock
    // timeout at 0, SetForegroundWindow is honored. (Standard launcher trick.)
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETFOREGROUNDLOCKTIMEOUT,
            0,
            Some(std::ptr::null_mut()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }

    let viewport = egui::ViewportBuilder::default()
        .with_title("hide_winbar")
        .with_inner_size([WIDTH, HEADER_H])
        .with_min_inner_size([WIDTH, HEADER_H])
        .with_position(OFFSCREEN) // start parked off-screen, but OS-visible
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_resizable(false)
        .with_taskbar(false);

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "hide_winbar",
        options,
        Box::new(|cc| Ok(Box::new(Launcher::new(cc)))),
    )
}
