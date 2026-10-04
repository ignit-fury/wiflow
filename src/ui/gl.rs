//! Shared winit-window + GL + egui hosting (S4).
//!
//! Both S4 windows (settings, pill) live on the EXISTING event loop with
//! identical plumbing (DisplayBuilder display → surface/context → glow
//! painter → egui-winit state → paint+swap). One implementation, two
//! configurations — instead of two copies of the spike pattern.

use std::sync::Arc;

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
    pub window: Window,
    surface: glutin::surface::Surface<WindowSurface>,
    context: PossiblyCurrentContext,
    painter: egui_glow::Painter,
    pub egui_ctx: egui::Context,
    pub egui_state: egui_winit::State,
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
            window,
            surface,
            context,
            painter,
            egui_ctx,
            egui_state,
        })
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// Make visible. Called AFTER any panel configuration (the pill must be
    /// non-activating before its first show) — creation never shows.
    pub fn show(&self) {
        self.window.set_visible(true);
    }

    pub fn set_visible(&self, visible: bool) {
        self.window.set_visible(visible);
    }

    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }

    /// Feed a winit event to egui. Returns true when egui wants a repaint.
    pub fn handle_event(&mut self, event: &WindowEvent) -> bool {
        self.egui_state.on_window_event(&self.window, event).repaint
    }

    /// Run one egui frame (`ui` draws) + swap. No redraw requested here —
    /// the caller decides (continuous while a pill is visible, on-demand
    /// for settings).
    pub fn paint(&mut self, ui: impl FnMut(&egui::Context)) {
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let output = self.egui_ctx.run(raw_input, ui);
        self.egui_state
            .handle_platform_output(&self.window, output.platform_output);
        let clipped = self
            .egui_ctx
            .tessellate(output.shapes, output.pixels_per_point);
        let dims = self.window.inner_size();
        self.painter.paint_and_update_textures(
            [dims.width, dims.height],
            output.pixels_per_point,
            &clipped,
            &output.textures_delta,
        );
        self.surface.swap_buffers(&self.context).unwrap();
    }
}
