//! Shared winit-window + GL + egui hosting (S4).
//!
//! Both S4 windows (settings, pill) live on the EXISTING event loop with
//! identical plumbing (DisplayBuilder display → surface/context → glow
//! painter → egui-winit state → paint+swap). One implementation, two
//! configurations — instead of two copies of the spike pattern.

use std::sync::Arc;
use std::time::{Duration, Instant};

use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextAttributesBuilder, PossiblyCurrentContext};
use glutin::display::GetGlDisplay;
use glutin::prelude::*;
use glutin::surface::{SurfaceAttributesBuilder, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow as GlWindowTrait};
use raw_window_handle::HasWindowHandle;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

/// Window configuration. Pills are frameless + non-decorated; settings is a
/// normal (decorated) window. Transparency is intentionally NOT offered —
/// both S4 windows are opaque dark surfaces (no alpha-surface plumbing).
pub struct GlWindowOpts<'a> {
    pub title: &'a str,
    pub width: f32,
    pub height: f32,
    pub decorations: bool,
    pub always_on_top: bool,
    /// Top-left in physical pixels (pill centers itself beforehand).
    pub position: Option<(i32, i32)>,
}

/// One egui-painted window on the shared event loop.
pub struct GlWindow {
    pub window: Arc<Window>,
    surface: glutin::surface::Surface<WindowSurface>,
    context: PossiblyCurrentContext,
    painter: egui_glow::Painter,
    pub egui_ctx: egui::Context,
    pub egui_state: egui_winit::State,
    repaint_at: Option<Instant>,
    surface_size: winit::dpi::PhysicalSize<u32>,
}

impl GlWindow {
    /// Create + show. `ActiveEventLoop` is available in `user_event` /
    /// `about_to_wait` — no startup work, no second loop.
    pub fn open(event_loop: &ActiveEventLoop, opts: &GlWindowOpts) -> Result<Self, String> {
        use winit::window::WindowAttributes;
        let mut attrs = WindowAttributes::default()
            .with_title(opts.title)
            .with_inner_size(LogicalSize::new(opts.width, opts.height))
            .with_resizable(true)
            .with_decorations(opts.decorations);
        if opts.always_on_top {
            attrs = attrs.with_window_level(winit::window::WindowLevel::AlwaysOnTop);
        }
        if let Some((x, y)) = opts.position {
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
        }
        let template = ConfigTemplateBuilder::new();
        let (window, gl_config) = DisplayBuilder::new()
            .with_window_attributes(Some(attrs))
            .build(event_loop, template, |mut configs| {
                configs.next().expect("no gl configs")
            })
            .map_err(|e| format!("display: {e:?}"))?;
        let (window, gl_config) = (window.ok_or("window not created")?, gl_config);
        let raw = window
            .window_handle()
            .map_err(|e| format!("window handle: {e:?}"))?
            .as_raw();
        let gl_display = gl_config.display();
        let context_attributes = ContextAttributesBuilder::new().build(Some(raw));
        let context = unsafe { gl_display.create_context(&gl_config, &context_attributes) }
            .map_err(|e| format!("gl context: {e:?}"))?;
        let surface_attributes = window
            .build_surface_attributes(SurfaceAttributesBuilder::new())
            .map_err(|e| format!("surface attrs: {e:?}"))?;
        let surface = unsafe { gl_display.create_window_surface(&gl_config, &surface_attributes) }
            .map_err(|e| format!("surface: {e:?}"))?;
        let context = context
            .make_current(&surface)
            .map_err(|e| format!("make current: {e:?}"))?;
        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                let cstr = std::ffi::CString::new(s).unwrap();
                gl_display.get_proc_address(&cstr) as *const _
            })
        };
        let painter = egui_glow::Painter::new(Arc::new(gl), "", None, false)
            .map_err(|e| format!("painter: {e:?}"))?;
        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        window.set_visible(false);
        Ok(Self {
            surface_size: window.inner_size(),
            window: Arc::new(window),
            surface,
            context,
            painter,
            egui_ctx,
            egui_state,
            repaint_at: None,
        })
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// Make visible. Called AFTER any panel configuration (the pill must be
    /// non-activating before its first show) — creation never shows.
    pub fn show(&self) {
        self.window.set_visible(true);
        self.window.request_redraw();
    }

    pub fn set_visible(&self, visible: bool) {
        self.window.set_visible(visible);
    }

    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }

    /// Feed a winit event to egui. Returns true when egui wants a repaint.
    pub fn handle_event(&mut self, event: &WindowEvent) -> bool {
        let resized = matches!(
            event,
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
        );
        // Resize on the following paint, after selecting this GL context.
        let repaint = self.egui_state.on_window_event(&self.window, event).repaint;
        redraw_for_event(event, repaint || resized)
    }

    /// Consume a due repaint once; idle egui output has no deadline.
    pub fn repaint_deadline(&mut self, now: Instant) -> Option<Instant> {
        if self.repaint_at.is_some_and(|deadline| deadline <= now) {
            self.repaint_at = None;
            self.window.request_redraw();
        }
        self.repaint_at
    }

    /// Run one egui frame (`ui` draws) + swap. No redraw requested here —
    /// the caller decides (continuous while a pill is visible, on-demand
    /// for settings).
    pub fn paint(&mut self, ui: impl FnMut(&egui::Context)) {
        self.repaint_at = None;
        let dims = self.window.inner_size();
        if dims.width == 0 || dims.height == 0 {
            return;
        }
        // Settings and the recording pill have separate GL contexts.
        // Select this window's context before resizing or painting it.
        if let Err(e) = self.context.make_current(&self.surface) {
            tracing::warn!("make GL context current: {e}");
            return;
        }
        if dims != self.surface_size {
            self.window.resize_surface(&self.surface, &self.context);
            self.surface_size = dims;
        }
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let output = self.egui_ctx.run(raw_input, ui);
        let delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |viewport| viewport.repaint_delay);
        self.repaint_at = deadline_after(Instant::now(), delay);
        self.egui_state
            .handle_platform_output(&self.window, output.platform_output);
        let clipped = self
            .egui_ctx
            .tessellate(output.shapes, output.pixels_per_point);
        self.painter.paint_and_update_textures(
            [dims.width, dims.height],
            output.pixels_per_point,
            &clipped,
            &output.textures_delta,
        );
        if let Err(e) = self.surface.swap_buffers(&self.context) {
            tracing::warn!("swap GL buffers: {e}");
        }
    }
}

fn deadline_after(now: Instant, delay: Duration) -> Option<Instant> {
    if delay == Duration::MAX {
        None
    } else {
        now.checked_add(delay)
    }
}

fn redraw_for_event(event: &WindowEvent, repaint: bool) -> bool {
    // egui-winit marks RedrawRequested as needing repaint. The App already
    // paints that event; queuing another here creates an endless redraw loop.
    repaint && !matches!(event, WindowEvent::RedrawRequested)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraw_event_does_not_queue_itself_again() {
        assert!(!redraw_for_event(&WindowEvent::RedrawRequested, true));
        assert!(redraw_for_event(
            &WindowEvent::Resized(winit::dpi::PhysicalSize::new(800, 600)),
            true
        ));
        assert!(!redraw_for_event(&WindowEvent::Focused(false), false));
    }

    #[test]
    fn idle_egui_stops_scheduling_frames() {
        let ctx = egui::Context::default();
        let mut deadline = Some(Instant::now());
        // Egui can request settling frames after its first layout.
        for _ in 0..10 {
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.label("Settings");
                });
            });
            deadline = deadline_after(
                Instant::now(),
                output.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
            );
            if deadline.is_none() {
                break;
            }
        }
        assert!(
            deadline.is_none(),
            "idle settings must let the event loop sleep"
        );
    }

    #[test]
    fn egui_delayed_repaint_preserves_its_deadline() {
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |_| {});
        }
        let delay = Duration::from_millis(500);
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            ctx.request_repaint_after(delay)
        });
        let now = Instant::now();
        // Egui subtracts its predicted next-frame time from the requested
        // delay. Preserve that adjusted deadline instead of repainting now.
        let scheduled = deadline_after(
            now,
            output.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
        )
        .unwrap();
        assert!(scheduled > now + Duration::from_millis(450));
        assert!(scheduled <= now + delay);
    }
}
