//! Recording pill: strictly observational recording indicator (H21).
//!
//! Displays LISTENING state (passed in, never sensed), live RMS amplitude
//! bars, elapsed time, and the cancel instruction. It holds no recording
//! state beyond render cache (`shown_at`, sample history) — the orchestrator
//! owns lifecycle truth; the pill renders what it is told.
//!
//! The window is a non-activating floating panel
//! (`platform::macos::panel`): it can never steal key focus from the
//! dictation target, and it ignores mouse events (click-through).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

use super::gl::{GlWindow, GlWindowOpts};
use crate::core::audio::recording_rms;

/// RMS smoothing factor per polled sample (exponential approach).
const SMOOTH_ALPHA: f32 = 0.35;
/// Bars of waveform history.
const BAR_COUNT: usize = 32;
/// Above digital silence. Bluetooth mics deliver exact zeros while they
/// switch profiles; a built-in mic's room noise is ~1e-4 or more.
const LIVE_RMS: f32 = 1e-5;
/// Pill window height: timer row, waveform, live-text row.
const PILL_HEIGHT: f32 = 108.0;
/// Pill window width in points.
const PILL_WIDTH: f32 = 320.0;
/// Characters of live text that fit the pill's one line.
const LIVE_CHARS: usize = 44;
/// Blend one step toward the target. Pure; unit-tested below.
pub fn smooth_rms(prev: f32, target: f32) -> f32 {
    prev + (target - prev) * SMOOTH_ALPHA
}

/// The end of `text` that fits one pill line: whole words, with "…" in front
/// when words were cut. Pure; unit-tested.
pub fn live_tail(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut len = 1; // the leading "…"
    for word in text.split_whitespace().rev() {
        let add = word.chars().count() + usize::from(!kept.is_empty());
        if len + add > max_chars {
            break;
        }
        len += add;
        kept.push(word);
    }
    if kept.is_empty() {
        // One word longer than the line: keep its end.
        let end: Vec<char> = text.chars().rev().take(max_chars - 1).collect();
        return format!("…{}", end.into_iter().rev().collect::<String>());
    }
    kept.reverse();
    format!("…{}", kept.join(" "))
}

/// Pill render state: what to show, derived ONLY from `show`/`hide` calls
/// and pushed amplitude samples. No phase machine, no daemon handles, no
/// control effects — that absence IS the H21 contract (pinned by tests).
#[derive(Debug)]
pub struct PillState {
    visible: bool,
    shown_at: Option<Instant>,
    smoothed: f32,
    history: VecDeque<f32>,
    /// A non-silent sample arrived since `show_now` (false = "connecting").
    heard: bool,
    /// Words heard so far (live preview); cleared on every show.
    live: String,
}

impl PillState {
    pub fn new() -> Self {
        Self {
            visible: false,
            shown_at: None,
            smoothed: 0.0,
            history: VecDeque::with_capacity(BAR_COUNT),
            heard: false,
            live: String::new(),
        }
    }

    /// Mark visible + start the elapsed timer. Render cache only.
    pub fn show_now(&mut self) {
        self.visible = true;
        self.shown_at = Some(Instant::now());
        self.smoothed = 0.0;
        self.history.clear();
        self.heard = false;
        self.live.clear();
    }

    /// Latest live-preview text (render cache only).
    pub fn set_live(&mut self, text: String) {
        self.live = text;
    }

    pub fn live(&self) -> &str {
        &self.live
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.shown_at = None;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Feed one polled amplitude sample (no-op while hidden).
    pub fn push_sample(&mut self, rms: f32) {
        if !self.visible {
            return;
        }
        let rms = rms.clamp(0.0, 1.0);
        self.heard |= rms > LIVE_RMS;
        self.smoothed = smooth_rms(self.smoothed, rms);
        if self.history.len() >= BAR_COUNT {
            self.history.pop_front();
        }
        self.history.push_back(self.smoothed);
    }

    /// True until the mic delivers real audio (Bluetooth cold start).
    pub fn connecting(&self) -> bool {
        self.visible && !self.heard
    }

    /// Bar heights for rendering (oldest → newest).
    pub fn bars(&self) -> Vec<f32> {
        self.history.iter().copied().collect()
    }

    /// Elapsed since `show_now` (zero while hidden).
    pub fn elapsed(&self) -> Duration {
        match (self.visible, self.shown_at) {
            (true, Some(t)) => t.elapsed(),
            _ => Duration::ZERO,
        }
    }
}

impl Default for PillState {
    fn default() -> Self {
        Self::new()
    }
}

/// Live pill: `PillState` + an optional lazily-created panel window.
/// Created on first `show` (needs the `ActiveEventLoop`); the window lives
/// until the App drops the pill. No recording decisions happen here.
pub struct Pill {
    state: PillState,
    gl: Option<GlWindow>,
}

impl Pill {
    pub fn new() -> Self {
        Self {
            state: PillState::new(),
            gl: None,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.state.is_visible()
    }

    /// Show the words heard so far; drawn on the next animation frame.
    pub fn set_live_text(&mut self, text: String) {
        self.state.set_live(text);
    }

    pub fn window_id(&self) -> Option<WindowId> {
        self.gl.as_ref().map(|g| g.window_id())
    }

    /// Show the pill (idempotent): create the panel on first call,
    /// position top-center, configure non-activating, make visible.
    /// Failures degrade to "no pill" (logged) — dictation never depends
    /// on the indicator.
    pub fn show(&mut self, event_loop: &ActiveEventLoop) {
        if self.gl.is_none() {
            let (x, y) = pill_position(event_loop);
            match GlWindow::open(
                event_loop,
                &GlWindowOpts {
                    title: "wiflow",
                    width: PILL_WIDTH,
                    height: PILL_HEIGHT,
                    decorations: false,
                    always_on_top: true,
                    position: Some((x, y)),
                },
            ) {
                Ok(gl) => {
                    crate::platform::macos::panel::configure_pill_panel(&gl.window);
                    gl.show();
                    self.gl = Some(gl);
                }
                Err(e) => {
                    tracing::warn!("pill window failed, continuing without it: {e}");
                    return;
                }
            }
        }
        self.state.show_now();
        if let Some(g) = self.gl.as_ref() {
            crate::platform::macos::panel::order_front_without_activating(&g.window);
            // Restart the frame loop: `frame` stops requesting redraws while
            // hidden, so without this every hold after the first showed a
            // frozen picture (no timer, waveform or live text).
            g.request_redraw();
        }
    }

    /// Hide the pill (idempotent). Called on every LISTENING exit and from
    /// terminal cleanup (H22) — missing a hide is a bug, double-hiding is
    /// a no-op.
    pub fn hide(&mut self) {
        self.state.hide();
        if let Some(g) = self.gl.as_ref() {
            g.set_visible(false);
        }
    }

    /// One animated frame while visible: poll amplitude, draw bars + timer
    /// + cancel hint, request the next frame. No-ops while hidden.
    pub fn frame(&mut self) {
        if !self.state.is_visible() {
            return;
        }
        let Some(g) = self.gl.as_mut() else { return };
        self.state.push_sample(recording_rms());
        let elapsed = self.state.elapsed();
        let bars = self.state.bars();
        let connecting = self.state.connecting();
        let live = self.state.live().to_string();
        g.paint(|ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(24, 24, 28)))
                .show(ctx, |ui| {
                    ui.vertical(|ui| {
                        // Row 1: recording dot + elapsed + hint.
                        ui.horizontal(|ui| {
                            let (dot_rect, _) = ui
                                .allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            // Amber until audio flows, then red.
                            let dot = if connecting {
                                egui::Color32::from_rgb(255, 180, 60)
                            } else {
                                egui::Color32::from_rgb(255, 70, 70)
                            };
                            ui.painter().circle_filled(dot_rect.center(), 7.0, dot);
                            ui.label(format!(
                                "{:02}:{:02}",
                                elapsed.as_secs() / 60,
                                elapsed.as_secs() % 60
                            ));
                            ui.small(if connecting {
                                "connecting mic, wait…"
                            } else {
                                "release to transcribe"
                            });
                        });
                        // Row 2: waveform bars across the full width.
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            for v in &bars {
                                let h = v.clamp(0.0, 1.0) * 26.0 + 3.0;
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(6.0, 30.0),
                                    egui::Sense::hover(),
                                );
                                let bar = egui::Rect::from_min_max(
                                    egui::pos2(rect.min.x, rect.max.y - h),
                                    egui::pos2(rect.max.x, rect.max.y),
                                );
                                ui.painter().rect_filled(
                                    bar,
                                    1.0,
                                    egui::Color32::from_rgb(120, 200, 255),
                                );
                            }
                        });
                        // Row 3: the words heard so far (live preview).
                        if !live.is_empty() {
                            ui.label(
                                egui::RichText::new(live_tail(&live, LIVE_CHARS))
                                    .color(egui::Color32::from_rgb(230, 230, 235)),
                            );
                        }
                    });
                });
        });
        g.request_redraw();
    }
}

impl Default for Pill {
    fn default() -> Self {
        Self::new()
    }
}

/// Top-center of the primary monitor, in physical pixels.
fn pill_position(event_loop: &ActiveEventLoop) -> (i32, i32) {
    let (w, x0, scale) = event_loop
        .primary_monitor()
        .map(|m| (m.size().width as i32, m.position().x, m.scale_factor()))
        .unwrap_or((1440, 0, 1.0));
    (centered_x(x0, w, scale), 48)
}

/// Left edge (physical px) that centres the pill on a monitor. The monitor
/// is measured in physical pixels but the pill's width is in points, so the
/// half-width must be scaled (it sat 80 pt right of centre on Retina).
fn centered_x(monitor_x: i32, monitor_width_px: i32, scale: f64) -> i32 {
    monitor_x + monitor_width_px / 2 - (PILL_WIDTH as f64 / 2.0 * scale) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothing_converges() {
        // Approach from below: monotonic rise toward the target.
        let mut v = 0.0f32;
        let mut last = v;
        for _ in 0..200 {
            v = smooth_rms(v, 1.0);
            assert!(v >= last, "monotonic rise");
            last = v;
        }
        assert!((v - 1.0).abs() < 1e-3, "converges, got {v}");

        // Decay from above: monotonic fall.
        let mut v = 1.0f32;
        for _ in 0..200 {
            let next = smooth_rms(v, 0.0);
            assert!(next <= v, "monotonic decay");
            v = next;
        }
        assert!(v.abs() < 1e-3, "decays, got {v}");

        // Step response is bounded (no overshoot past the target).
        assert!(smooth_rms(0.0, 1.0) <= 1.0);
        assert!(smooth_rms(1.0, 0.0) >= 0.0);
    }

    #[test]
    fn connecting_until_first_non_silent_sample() {
        let mut p = PillState::new();
        assert!(!p.connecting(), "hidden pill is never connecting");
        p.show_now();
        assert!(p.connecting());
        p.push_sample(0.0); // Bluetooth profile switch: digital silence
        assert!(p.connecting());
        p.push_sample(2e-4); // room noise
        assert!(!p.connecting());
        p.push_sample(0.0);
        assert!(!p.connecting(), "stays live once heard");
        p.show_now();
        assert!(p.connecting(), "reset on every hold");
    }

    #[test]
    fn pill_has_no_lifecycle_state() {
        // H21 contract, behaviorally: visibility mirrors show/hide ONLY;
        // bars derive from pushed samples ONLY; elapsed runs only while
        // visible. No phase machine, daemon, or control effects exist in
        // this API (compile-level: no such imports — see use block above).
        let mut p = PillState::new();
        assert!(!p.is_visible());
        assert_eq!(p.elapsed(), Duration::ZERO);
        assert!(p.bars().is_empty());

        // Samples while hidden are ignored (not buffered, not leaked).
        p.push_sample(0.9);
        assert!(p.bars().is_empty());

        p.show_now();
        assert!(p.is_visible());
        p.push_sample(0.0);
        p.push_sample(1.0);
        assert_eq!(p.bars().len(), 2);
        assert!(p.bars()[1] > p.bars()[0], "smoothing rises toward input");

        // History is bounded.
        for _ in 0..100 {
            p.push_sample(0.5);
        }
        assert_eq!(p.bars().len(), BAR_COUNT);

        p.hide();
        assert!(!p.is_visible());
        assert_eq!(p.elapsed(), Duration::ZERO);
        p.push_sample(0.9);
        assert_eq!(p.bars().len(), BAR_COUNT, "hidden push changes nothing");
    }

    #[test]
    fn live_text_resets_on_every_hold() {
        let mut p = PillState::new();
        p.show_now();
        p.set_live("hello there".into());
        assert_eq!(p.live(), "hello there");
        p.show_now();
        assert_eq!(p.live(), "", "a new hold starts empty");
    }

    #[test]
    fn pill_is_centred_in_points_on_retina() {
        // 1470-pt-wide Retina display = 2940 px; pill is 320 pt = 640 px.
        assert_eq!(centered_x(0, 2940, 2.0), 1150, "pill centre at 1470 px");
        assert_eq!(centered_x(0, 1440, 1.0), 560, "non-Retina unchanged");
        assert_eq!(centered_x(2940, 2940, 2.0), 4090, "second monitor offset");
    }

    #[test]
    fn live_tail_keeps_the_newest_whole_words() {
        assert_eq!(live_tail("  short text ", 20), "short text");
        let tail = live_tail("the quick brown fox jumps over the lazy dog", 20);
        assert_eq!(tail, "…over the lazy dog");
        assert!(tail.chars().count() <= 20);
        assert_eq!(
            live_tail("supercalifragilistic", 8),
            "…ilistic",
            "one long word keeps its end"
        );
    }
}
