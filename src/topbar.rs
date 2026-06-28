//! A short, always-visible macOS-Tahoe-style top bar, rendered as its own
//! always-on-top viewport (a sibling of the launcher window). It shows the date
//! and time and hosts three controls on the right:
//!
//! * a **magnifying glass** that opens the existing app launcher,
//! * a **settings (gear)** button that pops a small panel in the top-right with a
//!   draggable **sound slider**, an **appearance toggle** (translucent / dark),
//!   and a round button that opens the Windows **Settings** app,
//! * the **clock**.
//!
//! The sound slider also responds to the scroll wheel whenever the pointer is
//! anywhere over the bar.
//!
//! In *translucent* mode the bar fill is semi-transparent so the desktop shows
//! through (the viewport itself is a transparent window); in *dark* mode it's a
//! solid near-black bar.
//!
//! The strip the bar occupies is reserved as a screen edge by `crate::appbar`,
//! so maximized apps stop below it.

use std::f32::consts::PI;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, Key, Layout, Pos2, Rect, Rounding, Sense, Shape, Stroke, Vec2, ViewportId,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HWND};
use windows::Win32::Graphics::Dwm::{DwmEnableBlurBehindWindow, DWM_BB_BLURREGION, DWM_BB_ENABLE, DWM_BLURBEHIND};
use windows::Win32::Graphics::Gdi::CreateRectRgn;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetSystemMetrics, SetWindowPos, HWND_TOPMOST, SM_CXSCREEN, SWP_NOACTIVATE,
    SWP_SHOWWINDOW,
};

use crate::state::SHOW_LAUNCHER;

/// Logical height of the bar (also used by `appbar` to size the reserved strip).
pub const BAR_H: f32 = 30.0;
/// Settings panel size.
const PANEL_W: f32 = 290.0;
const PANEL_H: f32 = 250.0;

const FG: Color32 = Color32::from_rgb(240, 240, 245);
const FG_DIM: Color32 = Color32::from_rgb(178, 178, 190);
const ACCENT: Color32 = Color32::from_rgb(72, 134, 240);

pub struct TopBar {
    settings_open: bool,
    /// Master volume scalar (0..1), refreshed from the system each frame.
    vol: f32,
    muted: bool,
    /// Semi-transparent bar when true, solid near-black when false. Persisted.
    translucent: bool,
    /// Cached handle to our bar window, so we can pin it into the reserved strip.
    bar_hwnd: Option<HWND>,
    /// Whether we've enabled per-pixel alpha on the bar / panel windows yet.
    bar_alpha_on: bool,
    panel_alpha_on: bool,
    /// Guards the settings panel's click-away auto-close.
    settings_shown_at: Instant,
    settings_focused_once: bool,
}

impl Default for TopBar {
    fn default() -> Self {
        Self {
            settings_open: false,
            vol: crate::volume::get().unwrap_or(0.5),
            muted: crate::volume::is_muted(),
            translucent: load_translucent(),
            bar_hwnd: None,
            bar_alpha_on: false,
            panel_alpha_on: false,
            settings_shown_at: Instant::now(),
            settings_focused_once: false,
        }
    }
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("hide_winbar").join("topbar.cfg")
}

fn load_translucent() -> bool {
    // Default to the translucent look; only "dark" opts out.
    fs::read_to_string(config_path())
        .map(|s| s.trim() != "dark")
        .unwrap_or(true)
}

fn save_translucent(translucent: bool) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, if translucent { "translucent" } else { "dark" });
}

fn screen_width_logical(ctx: &egui::Context) -> f32 {
    let phys = unsafe { GetSystemMetrics(SM_CXSCREEN) } as f32;
    (phys / ctx.pixels_per_point()).max(320.0)
}

fn find_window(title: &str) -> Option<HWND> {
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { FindWindowW(PCWSTR::null(), PCWSTR(wide.as_ptr())) }.ok()
}

/// Switch a window into DWM per-pixel-alpha compositing using the classic
/// "empty blur region" trick: it makes the framebuffer's alpha channel honored
/// (so our semi-transparent fill shows the desktop through) **without** blurring
/// or replacing what we draw — unlike acrylic, which erases egui's content.
fn enable_per_pixel_alpha(hwnd: HWND) {
    unsafe {
        let region = CreateRectRgn(0, 0, -1, -1); // empty => "blur nothing"
        let bb = DWM_BLURBEHIND {
            dwFlags: DWM_BB_ENABLE | DWM_BB_BLURREGION,
            fEnable: BOOL(1),
            hRgnBlur: region,
            fTransitionOnMaximized: BOOL(0),
        };
        let _ = DwmEnableBlurBehindWindow(hwnd, &bb);
    }
}

impl TopBar {
    /// Render the bar (and, when open, the settings panel). Call once per frame
    /// from the launcher's root `update`.
    pub fn render(&mut self, ctx: &egui::Context) {
        let width = screen_width_logical(ctx);
        let bar_builder = egui::ViewportBuilder::default()
            .with_title("hide_winbar_topbar")
            .with_position([0.0, 0.0])
            .with_inner_size([width, BAR_H])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(false); // never steal focus from the foreground app

        ctx.show_viewport_immediate(
            ViewportId::from_hash_of("hide_winbar_topbar"),
            bar_builder,
            |ctx, _class| self.bar_ui(ctx),
        );

        if self.settings_open {
            let x = (width - PANEL_W - 10.0).max(0.0);
            let panel_builder = egui::ViewportBuilder::default()
                .with_title("hide_winbar_settings")
                .with_position([x, BAR_H + 8.0])
                .with_inner_size([PANEL_W, PANEL_H])
                .with_decorations(false)
                .with_transparent(true)
                .with_always_on_top()
                .with_taskbar(false)
                .with_resizable(false);

            ctx.show_viewport_immediate(
                ViewportId::from_hash_of("hide_winbar_settings"),
                panel_builder,
                |ctx, _class| self.settings_ui(ctx),
            );
        }
    }

    /// Force the bar window to fill the reserved top strip. Windows otherwise
    /// nudges the (borderless) window down to the work-area origin — i.e. *below*
    /// the strip the appbar just reserved — so we re-pin it every frame.
    fn pin_bar(&mut self, ctx: &egui::Context) {
        if self.bar_hwnd.is_none() {
            self.bar_hwnd = find_window("hide_winbar_topbar");
        }
        if let Some(hwnd) = self.bar_hwnd {
            if !self.bar_alpha_on {
                enable_per_pixel_alpha(hwnd);
                self.bar_alpha_on = true;
            }
            let w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
            let h = (BAR_H * ctx.pixels_per_point()).round() as i32;
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    w,
                    h,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    }

    /// The bar itself: date/time on the right, with the magnifier + gear controls.
    fn bar_ui(&mut self, ctx: &egui::Context) {
        self.pin_bar(ctx);

        // Volume-by-scroll: any wheel motion delivered to the bar means the
        // pointer is over it. One notch ≈ 5%.
        let scroll = ctx.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > 0.0 {
            self.vol = (self.vol + scroll.signum() * 0.05).clamp(0.0, 1.0);
            crate::volume::set(self.vol);
            self.muted = self.vol == 0.0;
        }

        let fill = if self.translucent {
            Color32::from_rgba_unmultiplied(20, 20, 26, 110)
        } else {
            Color32::from_rgb(15, 15, 19)
        };
        let frame = egui::Frame {
            fill,
            inner_margin: egui::Margin::symmetric(12.0, 0.0),
            ..Default::default()
        };
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.set_height(BAR_H);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(crate::time_fn::bar_time()).size(13.0).color(FG));
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(crate::time_fn::bar_date())
                        .size(13.0)
                        .color(FG_DIM),
                );
                ui.add_space(14.0);
                if icon_button(ui, Icon::Gear).clicked() && !self.settings_open {
                    self.open_settings();
                }
                ui.add_space(4.0);
                if icon_button(ui, Icon::Search).clicked() {
                    self.settings_open = false;
                    SHOW_LAUNCHER.store(true, Ordering::SeqCst);
                    ctx.request_repaint();
                }
            });
        });
    }

    fn open_settings(&mut self) {
        self.settings_open = true;
        self.settings_shown_at = Instant::now();
        self.settings_focused_once = false;
        self.panel_alpha_on = false; // a fresh window is created each time it opens
        if let Some(v) = crate::volume::get() {
            self.vol = v;
        }
        self.muted = crate::volume::is_muted();
    }

    /// The pop-up settings panel.
    fn settings_ui(&mut self, ctx: &egui::Context) {
        if !self.panel_alpha_on {
            if let Some(hwnd) = find_window("hide_winbar_settings") {
                enable_per_pixel_alpha(hwnd);
                self.panel_alpha_on = true;
            }
        }

        // Click-away / focus-loss close, once it has genuinely held focus and a
        // brief grace period has passed.
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
        if focused {
            self.settings_focused_once = true;
        }
        let grace = self.settings_shown_at.elapsed() > Duration::from_millis(250);
        if (self.settings_focused_once && !focused && grace)
            || ctx.input(|i| i.viewport().close_requested())
            || ctx.input(|i| i.key_pressed(Key::Escape))
        {
            self.settings_open = false;
            return;
        }

        // The panel stays readable, so it's only lightly translucent.
        let fill = if self.translucent {
            Color32::from_rgba_unmultiplied(26, 26, 32, 222)
        } else {
            Color32::from_rgb(28, 28, 34)
        };
        let frame = egui::Frame {
            fill,
            rounding: Rounding::same(18.0),
            stroke: Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
            inner_margin: egui::Margin::same(16.0),
            ..Default::default()
        };
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 12.0);

            ui.label(egui::RichText::new("Sound").size(12.0).color(FG_DIM));
            ui.horizontal(|ui| {
                let speaker = ui.allocate_response(Vec2::new(26.0, 26.0), Sense::click());
                draw_speaker(ui, speaker.rect, self.muted || self.vol == 0.0);
                if speaker.clicked() {
                    self.muted = crate::volume::toggle_mute();
                    if let Some(v) = crate::volume::get() {
                        self.vol = v;
                    }
                }
                let avail = ui.available_width();
                let mut v = self.vol;
                if sound_slider(ui, avail, &mut v).changed() {
                    self.vol = v;
                    crate::volume::set(v);
                    self.muted = v == 0.0;
                }
            });

            ui.add_space(2.0);
            ui.label(egui::RichText::new("Appearance").size(12.0).color(FG_DIM));
            ui.horizontal(|ui| {
                if pill(ui, "Translucent", self.translucent).clicked() && !self.translucent {
                    self.translucent = true;
                    save_translucent(true);
                }
                if pill(ui, "Dark", !self.translucent).clicked() && self.translucent {
                    self.translucent = false;
                    save_translucent(false);
                }
            });

            ui.add_space(2.0);
            ui.separator();
            ui.vertical_centered(|ui| {
                if round_button(ui, Icon::Gear, 46.0).clicked() {
                    crate::apps::launch("ms-settings:");
                    self.settings_open = false;
                }
                ui.label(
                    egui::RichText::new("System Settings")
                        .size(12.0)
                        .color(FG_DIM),
                );
            });
        });
    }
}

// --- custom controls -----------------------------------------------------

#[derive(Clone, Copy)]
enum Icon {
    Search,
    Gear,
}

/// A square, hover-highlighted icon button used on the bar.
fn icon_button(ui: &mut egui::Ui, icon: Icon) -> egui::Response {
    let resp = ui.allocate_response(Vec2::splat(24.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(
            resp.rect.shrink(1.0),
            Rounding::same(7.0),
            Color32::from_rgba_unmultiplied(255, 255, 255, 30),
        );
    }
    draw_icon(ui, icon, resp.rect, FG, 1.7, 6);
    resp
}

/// A circular button (used for "System Settings" in the panel).
fn round_button(ui: &mut egui::Ui, icon: Icon, diameter: f32) -> egui::Response {
    let resp = ui.allocate_response(Vec2::splat(diameter), Sense::click());
    let bg = if resp.hovered() {
        Color32::from_rgb(92, 92, 104)
    } else {
        Color32::from_rgb(72, 72, 82)
    };
    ui.painter().circle_filled(resp.rect.center(), diameter / 2.0, bg);
    draw_icon(ui, icon, resp.rect, FG, 2.0, 7);
    resp
}

/// A small rounded segmented-control button; filled when selected.
fn pill(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::FontId::proportional(13.0),
        Color32::WHITE,
    );
    let size = Vec2::new(galley.size().x + 22.0, 26.0);
    let resp = ui.allocate_response(size, Sense::click());
    let (bg, fg) = if selected {
        (ACCENT, Color32::WHITE)
    } else if resp.hovered() {
        (Color32::from_rgba_unmultiplied(255, 255, 255, 26), FG)
    } else {
        (Color32::from_rgba_unmultiplied(255, 255, 255, 14), FG_DIM)
    };
    ui.painter().rect_filled(resp.rect, Rounding::same(13.0), bg);
    ui.painter()
        .galley(resp.rect.center() - galley.size() / 2.0, galley, fg);
    resp
}

// --- vector icons (drawn, so they don't depend on emoji-font glyphs) -----

fn draw_icon(ui: &egui::Ui, icon: Icon, rect: Rect, color: Color32, width: f32, teeth: usize) {
    match icon {
        Icon::Search => draw_search(ui, rect, color, width),
        Icon::Gear => draw_gear(ui, rect, color, width, teeth),
    }
}

/// Magnifying glass: a circle with a short rounded diagonal handle.
fn draw_search(ui: &egui::Ui, rect: Rect, color: Color32, width: f32) {
    let p = ui.painter();
    let c = rect.center();
    let r = rect.width() * 0.24;
    let lens = Pos2::new(c.x - r * 0.30, c.y - r * 0.30);
    p.circle_stroke(lens, r, Stroke::new(width, color));
    let dir = Vec2::new(0.707, 0.707);
    let start = lens + dir * r;
    let end = start + dir * (r * 1.05);
    p.line_segment([start, end], Stroke::new(width + 0.5, color));
    p.circle_filled(end, (width + 0.5) / 2.0, color); // round cap
}

/// A proper cog: a toothed outline with a round hole.
fn draw_gear(ui: &egui::Ui, rect: Rect, color: Color32, width: f32, n: usize) {
    let p = ui.painter();
    let c = rect.center();
    let r_out = rect.width() * 0.30;
    let r_in = r_out * 0.75;
    let step = 2.0 * PI / n as f32;
    let tw = step * 0.30; // half-width of a tooth top
    let polar = |a: f32, rad: f32| Pos2::new(c.x + a.cos() * rad, c.y + a.sin() * rad);

    let mut pts = Vec::with_capacity(n * 5);
    for i in 0..n {
        let a = i as f32 * step;
        pts.push(polar(a - step * 0.5, r_in)); // flat valley floor between teeth
        pts.push(polar(a - tw, r_in));
        pts.push(polar(a - tw, r_out));
        pts.push(polar(a + tw, r_out));
        pts.push(polar(a + tw, r_in));
    }
    p.add(Shape::closed_line(pts, Stroke::new(width, color)));
    p.circle_stroke(c, r_out * 0.42, Stroke::new(width, color));
}

/// Sample an arc into a polyline.
fn arc(center: Pos2, radius: f32, a0: f32, a1: f32, segs: usize) -> Vec<Pos2> {
    (0..=segs)
        .map(|k| {
            let t = a0 + (a1 - a0) * (k as f32 / segs as f32);
            Pos2::new(center.x + t.cos() * radius, center.y + t.sin() * radius)
        })
        .collect()
}

/// Speaker icon with two sound waves (or a slash when muted).
fn draw_speaker(ui: &egui::Ui, rect: Rect, muted: bool) {
    let p = ui.painter();
    let color = if muted { FG_DIM } else { FG };
    let c = rect.center();
    let s = rect.width();
    let bx = c.x - s * 0.30;
    let body = vec![
        Pos2::new(bx, c.y - s * 0.11),
        Pos2::new(bx + s * 0.13, c.y - s * 0.11),
        Pos2::new(bx + s * 0.30, c.y - s * 0.26),
        Pos2::new(bx + s * 0.30, c.y + s * 0.26),
        Pos2::new(bx + s * 0.13, c.y + s * 0.11),
        Pos2::new(bx, c.y + s * 0.11),
    ];
    p.add(Shape::convex_polygon(body, color, Stroke::NONE));
    if muted {
        let a = Pos2::new(c.x + s * 0.12, c.y - s * 0.16);
        let b = Pos2::new(c.x + s * 0.34, c.y + s * 0.16);
        p.line_segment([a, b], Stroke::new(1.9, color));
    } else {
        let center = Pos2::new(c.x - s * 0.02, c.y);
        let stroke = Stroke::new(1.6, color);
        p.add(Shape::line(arc(center, s * 0.20, -PI / 4.0, PI / 4.0, 10), stroke));
        p.add(Shape::line(arc(center, s * 0.32, -PI / 4.0, PI / 4.0, 12), stroke));
    }
}

/// A macOS-style horizontal volume slider: a thin track with a filled portion
/// up to a round draggable knob.
fn sound_slider(ui: &mut egui::Ui, width: f32, value: &mut f32) -> egui::Response {
    let height = 22.0;
    let (rect, mut resp) =
        ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
    let knob_r = 8.0;
    let track_y = rect.center().y;
    let x0 = rect.left() + knob_r;
    let x1 = rect.right() - knob_r;
    let span = (x1 - x0).max(1.0);

    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.dragged() || resp.clicked() {
            *value = ((pos.x - x0) / span).clamp(0.0, 1.0);
            resp.mark_changed();
        }
    }

    let p = ui.painter();
    let knob_x = x0 + span * value.clamp(0.0, 1.0);
    p.line_segment(
        [Pos2::new(x0, track_y), Pos2::new(x1, track_y)],
        Stroke::new(4.0, Color32::from_rgba_unmultiplied(255, 255, 255, 46)),
    );
    p.line_segment(
        [Pos2::new(x0, track_y), Pos2::new(knob_x, track_y)],
        Stroke::new(4.0, ACCENT),
    );
    p.circle_filled(Pos2::new(knob_x, track_y), knob_r, Color32::WHITE);
    resp
}
