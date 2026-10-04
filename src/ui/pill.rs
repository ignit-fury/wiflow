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
/// Blend one step toward the target. Pure; unit-tested below.
pub fn smooth_rms(prev: f32, target: f32) -> f32 {
    prev + (target - prev) * SMOOTH_ALPHA
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
}

impl PillState {
    pub fn new() -> Self {
        Self {
            visible: false,
            shown_at: None,
            smoothed: 0.0,
            history: VecDeque::with_capacity(BAR_COUNT),
        }
    }

    /// Mark visible + start the elapsed timer. Render cache only.
    pub fn show_now(&mut self) {
        self.visible = true;
        self.shown_at = Some(Instant::now());
        self.smoothed = 0.0;
        self.history.clear();
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
        self.smoothed = smooth_rms(self.smoothed, rms);
        if self.history.len() >= BAR_COUNT {
            self.history.pop_front();
        }
        self.history.push_back(self.smoothed);
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
                    width: 320.0,
                    height: 84.0,
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
        if let Some(g) = self.gl.as_ref() {
            crate::platform::macos::panel::order_front_without_activating(&g.window);
        }
        self.state.show_now();
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
        g.paint(|ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(24, 24, 28)))
                .show(ctx, |ui| {
                    ui.vertical(|ui| {
                        // Row 1: recording dot + elapsed + hint.
                        ui.horizontal(|ui| {
                            let (dot_rect, _) = ui
                                .allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            ui.painter().circle_filled(
                                dot_rect.center(),
                                7.0,
                                egui::Color32::from_rgb(255, 70, 70),
                            );
                            ui.label(format!(
                                "{:02}:{:02}",
                                elapsed.as_secs() / 60,
                                elapsed.as_secs() % 60
                            ));
                            ui.small("release to transcribe · esc cancels");
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
    let (w, x0) = event_loop
        .primary_monitor()
        .map(|m| {
            let s = m.size();
            let p = m.position();
            (s.width as i32, p.x)
        })
        .unwrap_or((1440, 0));
    (x0 + w / 2 - 160, 48)
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
}
