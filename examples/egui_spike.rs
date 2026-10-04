//! egui integration spike (Task 13, TEMPORARY — deleted after the decision).
//!
//! Proves the risky unknowns for S4 on this exact toolchain:
//!  1. egui 0.31 + egui-winit 0.31 + egui_glow 0.31 + glutin 0.32 resolve and
//!     compile against the repo's winit 0.30.
//!  2. A winit window hosts a glutin GL context, an egui frame renders into
//!     it via egui_glow, and window events drive egui input (counter button).
//!  3. (By code-reading, not runtime): `ActiveEventLoop::create_window` is
//!     available in `user_event`/`about_to_wait`, so a settings window can
//!     be born on the EXISTING tray loop; `Window::set_visible` covers
//!     show/hide without touching the loop.
//!
//! Run: `cargo run --example egui_spike` — a window with a counter must
//! appear; close it via the window X button.

use std::sync::Arc;

use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextAttributesBuilder, PossiblyCurrentContext};
use glutin::display::GetGlDisplay;
use glutin::prelude::*;
use glutin::surface::{SurfaceAttributesBuilder, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasWindowHandle;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

struct Spike {
    window: Option<Window>,
    gl_context: Option<PossiblyCurrentContext>,
    gl_surface: Option<glutin::surface::Surface<WindowSurface>>,
    painter: Option<egui_glow::Painter>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    counter: usize,
}

impl Spike {
    fn new() -> Self {
        Self {
            window: None,
            gl_context: None,
            gl_surface: None,
            painter: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            counter: 0,
        }
    }

    fn paint(&mut self) {
        let (window, painter, egui_state) =
            match (&self.window, &mut self.painter, &mut self.egui_state) {
                (Some(w), Some(p), Some(s)) => (w, p, s),
                _ => return,
            };
        let raw_input = egui_state.take_egui_input(window);
        let output = self.egui_ctx.run(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("wiflow egui spike: OK");
                if ui.button(format!("clicks: {}", self.counter)).clicked() {
                    self.counter += 1;
                }
            });
        });
        egui_state.handle_platform_output(window, output.platform_output);
        let clipped = self
            .egui_ctx
            .tessellate(output.shapes, output.pixels_per_point);
        let dims = window.inner_size();
        painter.paint_and_update_textures(
            [dims.width, dims.height],
            output.pixels_per_point,
            &clipped,
            &output.textures_delta,
        );
        if let (Some(ctx), Some(surf)) = (&self.gl_context, &self.gl_surface) {
            surf.swap_buffers(ctx).unwrap();
        }
    }
}

impl ApplicationHandler for Spike {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = WindowAttributes::default().with_title("wiflow egui spike");
        let template = ConfigTemplateBuilder::new();
        let display_builder = DisplayBuilder::new().with_window_attributes(Some(attrs));
        let (window, gl_config) = display_builder
            .build(event_loop, template, |mut configs| {
                configs.next().expect("no gl configs")
            })
            .expect("display build");
        let window = window.expect("window");
        let raw_handle = window.window_handle().unwrap().as_raw();
        let gl_display = gl_config.display();
        let context_attributes = ContextAttributesBuilder::new().build(Some(raw_handle));
        let fallback = ContextAttributesBuilder::new()
            .with_context_api(glutin::context::ContextApi::Gles(None))
            .build(Some(raw_handle));
        let (context, _is_gles) = unsafe {
            gl_display
                .create_context(&gl_config, &context_attributes)
                .map(|c| (c, false))
                .or_else(|_| {
                    gl_display
                        .create_context(&gl_config, &fallback)
                        .map(|c| (c, true))
                })
                .expect("gl context")
        };
        let surface_attributes = window
            .build_surface_attributes(SurfaceAttributesBuilder::new())
            .expect("surface attrs");
        let surface = unsafe {
            gl_display
                .create_window_surface(&gl_config, &surface_attributes)
                .expect("surface")
        };
        let context = context.make_current(&surface).expect("make current");
        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                let cstr = std::ffi::CString::new(s).unwrap();
                gl_display.get_proc_address(&cstr) as *const _
            })
        };
        let painter = egui_glow::Painter::new(Arc::new(gl), "", None, false).expect("painter");
        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        self.gl_context = Some(context);
        self.gl_surface = Some(surface);
        self.painter = Some(painter);
        self.egui_state = Some(egui_state);
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let resp = self.egui_state.as_mut().and_then(|s| {
            self.window
                .as_ref()
                .map(|w| s.on_window_event(w, &event).repaint)
        });
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                self.paint();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {
                if resp == Some(true) {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
        }
        let _ = resp;
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = Spike::new();
    event_loop.run_app(&mut app).expect("run");
}
