use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use winit::{
    event::{DeviceEvent, ElementState},
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    keyboard::{KeyCode, PhysicalKey},
};

use crate::daemon::{Control, DaemonEvent, HotkeyPreset};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Idle,
    Recording,
    Transcribing,
    Error,
}

impl AppState {
    pub fn tooltip(&self) -> &'static str {
        match self {
            AppState::Idle => "Wiflow — hold Right Option to dictate",
            AppState::Recording => "Wiflow — recording… release to transcribe",
            AppState::Transcribing => "Wiflow — transcribing…",
            AppState::Error => "Wiflow — error (see menu)",
        }
    }
}

/// 32x32 status icon: dark square, center dot colored by state.
pub fn icon_rgba(state: AppState) -> Vec<u8> {
    let dot: (u8, u8, u8) = match state {
        AppState::Idle => (140, 140, 140),
        AppState::Recording => (230, 40, 40),
        AppState::Transcribing => (60, 180, 255),
        AppState::Error => (230, 150, 0),
    };
    let mut px = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32i32 {
        for x in 0..32i32 {
            let dx = x - 16;
            let dy = y - 16;
            let (r, g, b) = if dx * dx + dy * dy <= 49 {
                dot
            } else {
                (24, 24, 24)
            };
            px.extend_from_slice(&[r, g, b, 255]);
        }
    }
    px
}

pub fn make_icon(state: AppState) -> Icon {
    Icon::from_rgba(icon_rgba(state), 32, 32).expect("generated icon is valid RGBA")
}

struct DaemonApp {
    tray: TrayIcon,
    // Kept alive: dropping the manager unregisters the global hotkey.
    _hotkey_manager: global_hotkey::GlobalHotKeyManager,
    proxy: EventLoopProxy<DaemonEvent>,
    tx: std::sync::mpsc::Sender<Control>,
    state: AppState,
    /// Warn override (e.g. inject-fail, cancel): shown instead of the state tooltip.
    note: Option<String>,
    applied_state: AppState,
    applied_tooltip: String,
}

impl DaemonApp {
    fn set_state(&mut self, state: AppState, note: Option<String>) {
        self.state = state;
        self.note = note;
    }

    fn current_tooltip(&self) -> String {
        self.note
            .clone()
            .unwrap_or_else(|| self.state.tooltip().to_string())
    }

    /// Push icon/tooltip to the tray only when something changed.
    /// Runs in `about_to_wait` (winit thread); never blocks.
    fn sync_tray(&mut self) {
        let tooltip = self.current_tooltip();
        if self.state != self.applied_state {
            if let Err(e) = self.tray.set_icon(Some(make_icon(self.state))) {
                tracing::warn!("tray set_icon failed: {e:?}");
            }
            self.applied_state = self.state;
        }
        if tooltip != self.applied_tooltip {
            if let Err(e) = self.tray.set_tooltip(Some(&tooltip)) {
                tracing::warn!("tray set_tooltip failed: {e:?}");
            }
            self.applied_tooltip = tooltip;
        }
    }
}

impl winit::application::ApplicationHandler<DaemonEvent> for DaemonApp {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _event: winit::event::WindowEvent,
    ) {
        // Tray-only app: no windows exist.
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: DeviceEvent,
    ) {
        // Esc cancels an in-flight recording (routed via user_event).
        if let DeviceEvent::Key(key) = event {
            if key.state == ElementState::Pressed
                && matches!(key.physical_key, PhysicalKey::Code(KeyCode::Escape))
            {
                let _ = self.proxy.send_event(DaemonEvent::Cancel);
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: DaemonEvent) {
        match event {
            DaemonEvent::PttDown => {
                self.set_state(AppState::Recording, None);
                self.sync_tray();
                let _ = self.tx.send(Control::Down);
            }
            DaemonEvent::PttUp => {
                self.set_state(AppState::Transcribing, None);
                self.sync_tray();
                let _ = self.tx.send(Control::Up);
            }
            DaemonEvent::Cancel => {
                let _ = self.tx.send(Control::Cancel);
            }
            DaemonEvent::Done {
                text,
                duration_ms,
                rtf,
            } => {
                if text.is_empty() {
                    tracing::debug!("cycle done, no text (discard/silence)");
                } else {
                    tracing::info!("dictated {duration_ms}ms (RTF {rtf:.2}): {text:?}");
                }
                self.set_state(AppState::Idle, None);
                self.sync_tray();
            }
            DaemonEvent::Failed(msg) => {
                tracing::warn!("dictation failed: {msg}");
                self.set_state(AppState::Error, Some(msg));
                self.sync_tray();
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Task 3 fills menu items; drain here so clicks never pile up.
        while let Ok(ev) = muda::MenuEvent::receiver().try_recv() {
            tracing::debug!("menu event (unhandled until Task 3): {ev:?}");
        }
        self.sync_tray();
    }
}

fn app_main(
    event_loop: EventLoop<DaemonEvent>,
    proxy: winit::event_loop::EventLoopProxy<DaemonEvent>,
) -> ! {
    // Task 3 fills menu items; the skeleton call lives here.
    let menu = muda::Menu::new();
    let tray = TrayIconBuilder::new()
        .with_tooltip(AppState::Idle.tooltip())
        .with_icon(make_icon(AppState::Idle))
        .with_menu(Box::new(menu))
        .build()
        .expect("tray icon");
    tracing::info!("tray built (idle)");

    let (manager, hotkey, won) = crate::daemon::register_ptt_hotkey(HotkeyPreset::RightOption)
        .unwrap_or_else(|e| {
            eprintln!("no push-to-talk hotkey: {e}");
            std::process::exit(1);
        });
    tracing::info!("ptt hotkey registered: {won:?} (id {})", hotkey.id());
    crate::daemon::spawn_hotkey_bridge(proxy.clone(), hotkey.id());

    let (tx, rx) = std::sync::mpsc::channel::<Control>();
    let worker_proxy = proxy.clone();
    std::thread::spawn(move || crate::daemon::worker_main(worker_proxy, rx));

    let mut app = DaemonApp {
        tray,
        _hotkey_manager: manager,
        proxy,
        tx,
        state: AppState::Idle,
        note: None,
        applied_state: AppState::Idle,
        applied_tooltip: AppState::Idle.tooltip().to_string(),
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("event loop exited: {e:?}");
        std::process::exit(1);
    }
    std::process::exit(0);
}

pub fn run() -> ! {
    let event_loop = EventLoop::<DaemonEvent>::with_user_event()
        .build()
        .expect("winit event loop");
    let proxy = event_loop.create_proxy();
    app_main(event_loop, proxy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tooltips_cover_all_states() {
        assert!(AppState::Idle.tooltip().contains("hold"));
        assert!(AppState::Recording.tooltip().contains("recording"));
        assert!(AppState::Transcribing.tooltip().contains("transcribing"));
        assert!(AppState::Error.tooltip().contains("error"));
    }

    #[test]
    fn test_icon_bytes_are_32x32_rgba() {
        for state in [
            AppState::Idle,
            AppState::Recording,
            AppState::Transcribing,
            AppState::Error,
        ] {
            let rgba = icon_rgba(state);
            assert_eq!(rgba.len(), 32 * 32 * 4);
        }
    }
    #[test]
    fn test_recording_dot_is_red_center() {
        let rgba = icon_rgba(AppState::Recording);
        let i = (16 * 32 + 16) * 4;
        assert!(
            rgba[i] > 200 && rgba[i + 1] < 80 && rgba[i + 2] < 80,
            "center must be red"
        );
    }

    #[test]
    fn test_each_state_has_distinct_center_dot() {
        let states = [
            AppState::Idle,
            AppState::Recording,
            AppState::Transcribing,
            AppState::Error,
        ];
        let mut dots = std::collections::HashSet::new();
        for s in states {
            let rgba = icon_rgba(s);
            let i = (16 * 32 + 16) * 4;
            dots.insert([rgba[i], rgba[i + 1], rgba[i + 2]]);
        }
        assert_eq!(dots.len(), states.len());
    }

    #[test]
    fn test_make_icon_never_panics_on_generated_bytes() {
        for state in [
            AppState::Idle,
            AppState::Recording,
            AppState::Transcribing,
            AppState::Error,
        ] {
            let _ = make_icon(state);
        }
    }
}
