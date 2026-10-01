use muda::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use winit::{
    event::{DeviceEvent, ElementState},
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    keyboard::{KeyCode, PhysicalKey},
};

use crate::config::{Config, ModelChoice};
use crate::daemon::{preset_hint, preset_hotkey, Control, DaemonEvent, HotkeyPreset};
use crate::history::HistoryEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Idle,
    Recording,
    Transcribing,
    Error,
}

impl AppState {
    /// Idle text names the WINNING preset — never a hardcoded hotkey.
    pub fn tooltip(&self, preset: HotkeyPreset) -> String {
        match self {
            AppState::Idle => format!("Wiflow — {} to dictate", preset_hint(preset)),
            AppState::Recording => "Wiflow — recording… release to transcribe".to_string(),
            AppState::Transcribing => "Wiflow — transcribing…".to_string(),
            AppState::Error => "Wiflow — error (see menu)".to_string(),
        }
    }

    pub fn status_label(&self) -> &'static str {
        match self {
            AppState::Idle => "Idle",
            AppState::Recording => "Recording",
            AppState::Transcribing => "Transcribing",
            AppState::Error => "Error",
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

#[derive(Debug, Clone)]
pub struct MenuIds {
    status: MenuId,
    mic_items: Vec<(String, MenuId)>,
    model_tiny: MenuId,
    model_base: MenuId,
    model_small: MenuId,
    hk_right: MenuId,
    hk_fn: MenuId,
    hk_ctrl: MenuId,
    launch_login: MenuId,
    history_items: Vec<(String, MenuId)>,
    perm_mic: MenuId,
    perm_a11y: MenuId,
    edit_vocab: MenuId,
    cleanup_toggle: MenuId,
    quit: MenuId,
}

/// Pure id scheme for the whole menu tree: deterministic strings, no
/// platform objects — testable off the main thread (muda forbids
/// `Menu::new` elsewhere on macOS). `build_menu` stamps these ids via
/// `with_id`, so the handler and the test see the same values.
fn ids_for(devices: &[String], history: &[HistoryEntry]) -> MenuIds {
    MenuIds {
        status: MenuId::new("wiflow:status"),
        mic_items: devices
            .iter()
            .enumerate()
            .map(|(i, d)| (d.clone(), MenuId::new(format!("wiflow:mic:{i}"))))
            .collect(),
        model_tiny: MenuId::new("wiflow:model:tiny"),
        model_base: MenuId::new("wiflow:model:base"),
        model_small: MenuId::new("wiflow:model:small"),
        hk_right: MenuId::new("wiflow:hk:right"),
        hk_fn: MenuId::new("wiflow:hk:fn"),
        hk_ctrl: MenuId::new("wiflow:hk:ctrl"),
        launch_login: MenuId::new("wiflow:launch"),
        history_items: history
            .iter()
            .rev()
            .take(8)
            .enumerate()
            .map(|(i, e)| (e.text.clone(), MenuId::new(format!("wiflow:hist:{i}"))))
            .collect(),
        perm_mic: MenuId::new("wiflow:perm:mic"),
        perm_a11y: MenuId::new("wiflow:perm:a11y"),
        edit_vocab: MenuId::new("wiflow:edit:vocab"),
        cleanup_toggle: MenuId::new("wiflow:cleanup:toggle"),
        quit: MenuId::new("wiflow:quit"),
    }
}

fn truncate_label(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut s: String = flat.chars().take(max).collect();
        s.push('…');
        s
    }
}

/// Build the full tray menu tree. Pure inputs (config + device list +
/// history snapshot + state label) so tests can pass fakes.
pub fn build_menu(
    config: &Config,
    devices: &[String],
    history: &[HistoryEntry],
    state_label: &str,
) -> (Menu, MenuIds) {
    let menu = Menu::new();
    let ids = ids_for(devices, history);

    let status = MenuItem::with_id(
        ids.status.clone(),
        format!("Status: {state_label}"),
        false,
        None,
    );

    let mic_menu = Submenu::new("Microphone", true);
    for (i, dev) in devices.iter().enumerate() {
        let checked = match &config.mic_name {
            Some(n) => n == dev,
            None => i == 0,
        };
        let item = CheckMenuItem::with_id(ids.mic_items[i].1.clone(), dev, true, checked, None);
        mic_menu.append(&item).expect("menu append");
    }

    let model_menu = Submenu::new("Model", true);
    let model_tiny = CheckMenuItem::with_id(
        ids.model_tiny.clone(),
        "Tiny — English (~75 MB, fastest)",
        true,
        config.model == ModelChoice::TinyEn,
        None,
    );
    let model_base = CheckMenuItem::with_id(
        ids.model_base.clone(),
        "Base — English (~140 MB)",
        true,
        config.model == ModelChoice::BaseEn,
        None,
    );
    let model_small = CheckMenuItem::with_id(
        ids.model_small.clone(),
        "Small — English (~465 MB)",
        true,
        config.model == ModelChoice::SmallEn,
        None,
    );
    model_menu
        .append_items(&[&model_tiny, &model_base, &model_small])
        .expect("menu append");

    let hk_menu = Submenu::new("Push-to-talk hotkey", true);
    // Bare-modifier presets (Right Option/Fn) ride a listen-only CGEventTap —
    // RegisterEventHotKey can't see bare modifiers. Tap failure (no Input
    // Monitoring permission) falls back to CtrlSpace + warn.
    let hk_right = CheckMenuItem::with_id(
        ids.hk_right.clone(),
        "Right Option",
        true,
        config.hotkey_preset == HotkeyPreset::RightOption,
        None,
    );
    let hk_fn = CheckMenuItem::with_id(
        ids.hk_fn.clone(),
        "Fn",
        true,
        config.hotkey_preset == HotkeyPreset::Fn,
        None,
    );
    let hk_ctrl = CheckMenuItem::with_id(
        ids.hk_ctrl.clone(),
        "Ctrl+Space",
        true,
        config.hotkey_preset == HotkeyPreset::CtrlSpace,
        None,
    );
    hk_menu
        .append_items(&[&hk_right, &hk_fn, &hk_ctrl])
        .expect("menu append");

    let launch_login = CheckMenuItem::with_id(
        ids.launch_login.clone(),
        "Launch at Login",
        true,
        config.launch_at_login,
        None,
    );

    // LLM cleanup (literal dictation cleanup layer via Ollama, $0 local).
    let cleanup_toggle = CheckMenuItem::with_id(
        ids.cleanup_toggle.clone(),
        "AI Cleanup (Ollama)",
        true,
        config.cleanup_enabled,
        None,
    );

    let hist_menu = Submenu::new("History", true);
    if history.is_empty() {
        let empty = MenuItem::new("(empty)", false, None);
        hist_menu.append(&empty).expect("menu append");
    } else {
        for (text, hid) in &ids.history_items {
            let item = MenuItem::with_id(hid.clone(), truncate_label(text, 40), true, None);
            hist_menu.append(&item).expect("menu append");
        }
    }

    let perm_menu = Submenu::new("Permissions", true);
    let perm_mic = MenuItem::with_id(ids.perm_mic.clone(), "Microphone…", true, None);
    let perm_a11y = MenuItem::with_id(ids.perm_a11y.clone(), "Accessibility…", true, None);
    perm_menu
        .append_items(&[&perm_mic, &perm_a11y])
        .expect("menu append");

    // Vocabulary (Whisper initial prompt): opens prompt.txt in the default
    // editor — plain text so hand-editing can't corrupt JSON config.
    let edit_vocab = MenuItem::with_id(ids.edit_vocab.clone(), "Edit Vocabulary…", true, None);

    let quit = MenuItem::with_id(ids.quit.clone(), "Quit Wiflow", true, None);

    menu.append(&status).expect("menu append");
    menu.append(&PredefinedMenuItem::separator())
        .expect("menu append");
    menu.append(&mic_menu).expect("menu append");
    menu.append(&model_menu).expect("menu append");
    menu.append(&hk_menu).expect("menu append");
    menu.append(&launch_login).expect("menu append");
    menu.append(&cleanup_toggle).expect("menu append");
    menu.append(&PredefinedMenuItem::separator())
        .expect("menu append");
    menu.append(&hist_menu).expect("menu append");
    menu.append(&edit_vocab).expect("menu append");
    menu.append(&perm_menu).expect("menu append");
    menu.append(&PredefinedMenuItem::separator())
        .expect("menu append");
    menu.append(&quit).expect("menu append");

    (menu, ids)
}

struct DaemonApp {
    tray: TrayIcon,
    // Kept alive: dropping the manager unregisters the global hotkey.
    hotkey_manager: global_hotkey::GlobalHotKeyManager,
    // None when the PTT rides the CGEventTap (bare modifier presets).
    hotkey: Option<global_hotkey::hotkey::HotKey>,
    // Live tap handle for bare-modifier presets; Drop stops listening.
    tap: Option<crate::tap::ModifierTap>,
    preset: HotkeyPreset,
    config: Config,
    devices: Vec<String>,
    proxy: EventLoopProxy<DaemonEvent>,
    tx: std::sync::mpsc::Sender<Control>,
    state: AppState,
    /// Warn override (e.g. inject-fail, cancel): shown instead of the state tooltip.
    note: Option<String>,
    applied_state: AppState,
    applied_tooltip: String,
    menu_ids: MenuIds,
    menu_dirty: bool,
}

impl DaemonApp {
    fn set_state(&mut self, state: AppState, note: Option<String>) {
        self.state = state;
        self.note = note;
        // Status item shows the state → menu needs a rebuild.
        self.menu_dirty = true;
    }

    fn current_tooltip(&self) -> String {
        self.note
            .clone()
            .unwrap_or_else(|| self.state.tooltip(self.preset))
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

    fn rebuild_menu(&mut self) {
        let history = crate::history::load_history();
        let (menu, ids) = build_menu(
            &self.config,
            &self.devices,
            &history,
            self.state.status_label(),
        );
        self.tray.set_menu(Some(Box::new(menu)));
        self.menu_ids = ids;
        self.menu_dirty = false;
    }

    fn save(&mut self) {
        if let Err(e) = crate::config::save_config(&self.config) {
            tracing::warn!("save config failed: {e}");
        }
    }

    /// Note only when it won't clobber the recording indicator.
    fn warn_note(&mut self, msg: String) {
        tracing::warn!("{msg}");
        if self.state != AppState::Recording {
            self.note = Some(msg);
        }
    }

    fn switch_hotkey(&mut self, want: HotkeyPreset) {
        if want == self.preset {
            return;
        }
        let want_is_bare = matches!(want, HotkeyPreset::RightOption | HotkeyPreset::Fn);
        let old_preset = self.preset;
        if want_is_bare {
            // Combo → bare: unregister the global hotkey, spawn the tap.
            if let Some(old) = self.hotkey.take() {
                let _ = self.hotkey_manager.unregister(old);
            }
            match crate::tap::spawn(want, self.proxy.clone()) {
                Ok(t) => {
                    self.tap = Some(t);
                    self.preset = want;
                    self.config.hotkey_preset = want;
                    self.save();
                    self.menu_dirty = true;
                    tracing::info!("ptt hotkey switched to {want:?} (CGEventTap)");
                }
                Err(e) => {
                    // Rollback: re-register the old combo so PTT survives.
                    let _ = self.hotkey_manager.register(preset_hotkey(old_preset));
                    self.hotkey = Some(preset_hotkey(old_preset));
                    self.warn_note(format!("hotkey switch failed: {e}"));
                }
            }
        } else {
            // Bare → combo (or combo → combo): stop the tap, register hotkey.
            if let Some(t) = self.tap.as_mut() {
                t.stop();
            }
            self.tap = None;
            match self.hotkey_manager.register(preset_hotkey(want)) {
                Ok(()) => {
                    self.hotkey = Some(preset_hotkey(want));
                    self.preset = want;
                    self.config.hotkey_preset = want;
                    self.save();
                    self.menu_dirty = true;
                    tracing::info!("ptt hotkey switched to {want:?}");
                }
                Err(e) => {
                    // Rollback: restore the old mechanism.
                    if Self::old_is_bare_preset(old_preset) {
                        match crate::tap::spawn(old_preset, self.proxy.clone()) {
                            Ok(t) => self.tap = Some(t),
                            Err(e2) => tracing::warn!("tap restore failed: {e2:?}"),
                        }
                    } else if let Some(old) = self.hotkey.take() {
                        let _ = self.hotkey_manager.unregister(old);
                        if let Err(e2) = self.hotkey_manager.register(preset_hotkey(old_preset)) {
                            tracing::warn!("hotkey restore failed: {e2:?}");
                        }
                        self.hotkey = Some(preset_hotkey(old_preset));
                    }
                    self.warn_note(format!("hotkey switch failed: {e:?}"));
                }
            }
        }
    }

    fn old_is_bare_preset(preset: HotkeyPreset) -> bool {
        matches!(preset, HotkeyPreset::RightOption | HotkeyPreset::Fn)
    }

    fn handle_menu_event(&mut self, id: &MenuId) {
        let ids = self.menu_ids.clone();
        if *id == ids.status {
            return;
        }
        if *id == ids.model_tiny {
            self.config.model = ModelChoice::TinyEn;
            self.save();
            self.menu_dirty = true;
            tracing::info!("model set to tiny.en (takes effect next hold)");
            return;
        }
        if *id == ids.model_base {
            self.config.model = ModelChoice::BaseEn;
            self.save();
            self.menu_dirty = true;
            tracing::info!("model set to base.en (takes effect next hold)");
            return;
        }
        if *id == ids.model_small {
            self.config.model = ModelChoice::SmallEn;
            self.save();
            self.menu_dirty = true;
            tracing::info!("model set to small.en (takes effect next hold)");
            return;
        }
        if *id == ids.hk_right {
            self.switch_hotkey(HotkeyPreset::RightOption);
            return;
        }
        if *id == ids.hk_fn {
            self.switch_hotkey(HotkeyPreset::Fn);
            return;
        }
        if *id == ids.hk_ctrl {
            self.switch_hotkey(HotkeyPreset::CtrlSpace);
            return;
        }
        if *id == ids.launch_login {
            let enable = !self.config.launch_at_login;
            let exe = std::env::current_exe()
                .unwrap_or_else(|_| std::path::PathBuf::from("wiflow-dictation"));
            match crate::config::set_launch_at_login(enable, &exe, true) {
                Ok(()) => {
                    self.config.launch_at_login = enable;
                    self.save();
                    self.menu_dirty = true;
                    tracing::info!("launch at login: {enable}");
                }
                Err(e) => self.warn_note(format!("launch-at-login failed: {e}")),
            }
            return;
        }
        if *id == ids.edit_vocab {
            // Ensure prompt.txt exists (empty), then open in default editor.
            let path = crate::config::prompt_path();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if !path.exists() {
                let _ = std::fs::write(
                    &path,
                    "One line: names/jargon Whisper mishears, e.g. Playboy Carti, Rather Lie\n",
                );
            }
            match open::that(&path) {
                Ok(()) => tracing::info!("vocabulary editor opened"),
                Err(e) => self.warn_note(format!("cannot open editor: {e:?}")),
            }
            return;
        }
        if *id == ids.cleanup_toggle {
            self.config.cleanup_enabled = !self.config.cleanup_enabled;
            self.save();
            self.menu_dirty = true;
            tracing::info!(
                "ai cleanup (ollama): {}",
                if self.config.cleanup_enabled {
                    "on"
                } else {
                    "off"
                }
            );
            return;
        }
        if *id == ids.perm_mic {
            crate::config::permissions::open_mic_settings();
            return;
        }
        if *id == ids.perm_a11y {
            crate::config::permissions::open_accessibility_settings();
            return;
        }
        if *id == ids.quit {
            tracing::info!("quit via menu");
            crate::stt::shutdown();
            std::process::exit(0);
        }
        if let Some((dev, _)) = ids.mic_items.iter().find(|(_, mid)| mid == id) {
            self.config.mic_name = Some(dev.clone());
            self.save();
            self.menu_dirty = true;
            tracing::info!("mic set to {dev} (takes effect next hold)");
            return;
        }
        if let Some((text, _)) = ids.history_items.iter().find(|(_, hid)| hid == id) {
            crate::inject::leave_on_clipboard(text);
            self.note = Some("history copied to clipboard".to_string());
            tracing::info!("history entry copied to clipboard");
            return;
        }
        tracing::debug!("menu event for unknown id: {id:?}");
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
                    // Main-thread-only: enigo HIToolbox TIS calls trap off-main
                    // (crash report 2026-09-30). The 200ms restore sleep inside
                    // inject_text briefly blocks this thread — accepted for v1.
                    match crate::inject::inject_text(&text) {
                        Ok(r) => tracing::info!(
                            "injected via {} (clipboard restored: {})",
                            r.pasted_via,
                            r.clipboard_restored
                        ),
                        Err(e) => {
                            tracing::warn!(
                                "inject failed ({e}) — text left on clipboard, press Cmd+V"
                            );
                            crate::inject::leave_on_clipboard(&text);
                            self.set_state(
                                AppState::Error,
                                Some(format!("injected to clipboard: {e}")),
                            );
                            self.sync_tray();
                            return;
                        }
                    }
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
        while let Ok(ev) = muda::MenuEvent::receiver().try_recv() {
            self.handle_menu_event(&ev.id);
        }
        if self.menu_dirty {
            self.rebuild_menu();
        }
        self.sync_tray();
    }
}

fn app_main(
    event_loop: EventLoop<DaemonEvent>,
    proxy: winit::event_loop::EventLoopProxy<DaemonEvent>,
    mut config: Config,
) -> ! {
    let devices = crate::audio::list_devices();
    let history = crate::history::load_history();
    let (menu, ids) = build_menu(&config, &devices, &history, AppState::Idle.status_label());
    let tray = TrayIconBuilder::new()
        .with_tooltip(AppState::Idle.tooltip(config.hotkey_preset))
        .with_icon(make_icon(AppState::Idle))
        .with_menu(Box::new(menu))
        .build()
        .expect("tray icon");
    tracing::info!(
        "tray built (idle, {} mics, {} history)",
        devices.len(),
        history.len()
    );

    let is_bare = matches!(
        config.hotkey_preset,
        HotkeyPreset::RightOption | HotkeyPreset::Fn
    );
    let (manager, ptt_hotkey, won, tap) = if is_bare {
        // Bare-modifier presets ride a listen-only CGEventTap (raw
        // flagsChanged) — RegisterEventHotKey cannot see them.
        match crate::tap::spawn(config.hotkey_preset, proxy.clone()) {
            Ok(t) => {
                let manager = global_hotkey::GlobalHotKeyManager::new().unwrap_or_else(|e| {
                    eprintln!("hotkey manager: {e:?}");
                    std::process::exit(1);
                });
                (manager, None, config.hotkey_preset, Some(t))
            }
            Err(e) => {
                tracing::warn!("modifier tap unavailable ({e}) — falling back to CtrlSpace");
                let (manager, hk, w) = crate::daemon::register_ptt_hotkey(HotkeyPreset::CtrlSpace)
                    .unwrap_or_else(|e| {
                        eprintln!("no push-to-talk hotkey: {e}");
                        std::process::exit(1);
                    });
                (manager, Some(hk), w, None)
            }
        }
    } else {
        let (manager, hk, w) = crate::daemon::register_ptt_hotkey(config.hotkey_preset)
            .unwrap_or_else(|e| {
                eprintln!("no push-to-talk hotkey: {e}");
                std::process::exit(1);
            });
        (manager, Some(hk), w, None)
    };
    tracing::info!(
        "ptt hotkey registered: {won:?} ({})",
        if ptt_hotkey.is_some() {
            "global-hotkey"
        } else {
            "CGEventTap"
        }
    );
    // Esc cancel: winit device_event never delivers Key events to a
    // zero-window tray app on macOS (proven Task 3), so Esc rides the
    // same global-hotkey bridge as PTT. Registration failure degrades
    // to "cancel disabled" — the app stays usable.
    let esc_hotkey = match crate::daemon::register_cancel_hotkey(&manager) {
        Ok(hk) => {
            tracing::info!("esc cancel hotkey registered (id {})", hk.id());
            hk
        }
        Err(e) => {
            tracing::warn!("esc cancel hotkey NOT registered ({e}) — cancel disabled");
            global_hotkey::hotkey::HotKey::new(None, global_hotkey::hotkey::Code::Escape)
        }
    };
    // Persist the actual winner so tooltip + next launch agree.
    config.hotkey_preset = won;
    if let Err(e) = crate::config::save_config(&config) {
        tracing::warn!("save config failed: {e}");
    }
    crate::daemon::spawn_hotkey_bridge(proxy.clone(), esc_hotkey.id());

    let (tx, rx) = std::sync::mpsc::channel::<Control>();
    let worker_proxy = proxy.clone();
    std::thread::spawn(move || crate::daemon::worker_main(worker_proxy, rx));

    let mut app = DaemonApp {
        tray,
        hotkey_manager: manager,
        hotkey: ptt_hotkey,
        tap,
        preset: won,
        config,
        devices,
        proxy,
        tx,
        state: AppState::Idle,
        note: None,
        applied_state: AppState::Idle,
        applied_tooltip: AppState::Idle.tooltip(won),
        menu_ids: ids,
        menu_dirty: false,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("event loop exited: {e:?}");
        std::process::exit(1);
    }
    std::process::exit(0);
}

pub fn run(initial: Config) -> ! {
    let event_loop = EventLoop::<DaemonEvent>::with_user_event()
        .build()
        .expect("winit event loop");
    let proxy = event_loop.create_proxy();
    app_main(event_loop, proxy, initial)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tooltips_cover_all_states() {
        let p = HotkeyPreset::CtrlSpace;
        assert!(AppState::Idle.tooltip(p).contains("hold"));
        assert!(AppState::Recording.tooltip(p).contains("recording"));
        assert!(AppState::Transcribing.tooltip(p).contains("transcribing"));
        assert!(AppState::Error.tooltip(p).contains("error"));
    }

    #[test]
    fn test_idle_tooltip_reflects_winning_preset() {
        assert!(AppState::Idle
            .tooltip(HotkeyPreset::CtrlSpace)
            .contains("Ctrl+Space"));
        assert!(AppState::Idle
            .tooltip(HotkeyPreset::RightOption)
            .contains("Right Option"));
        assert!(AppState::Idle.tooltip(HotkeyPreset::Fn).contains("Fn"));
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

    fn sample_history() -> Vec<HistoryEntry> {
        vec![HistoryEntry {
            text: "hello world".into(),
            at_ms: 1_700_000_000_000,
            duration_ms: 1500,
            rtf: 0.1,
        }]
    }

    #[test]
    fn test_menu_ids_distinct() {
        // Pure id scheme — no platform menus (muda forbids Menu::new off
        // the main thread on macOS). build_menu stamps these same ids via
        // with_id, so distinctness here covers the handler dispatch.
        let devices = vec!["Mic A".to_string(), "Mic B".to_string()];
        let ids = ids_for(&devices, &sample_history());
        let mut all = vec![
            ids.status,
            ids.model_tiny,
            ids.model_base,
            ids.model_small,
            ids.hk_right,
            ids.hk_fn,
            ids.hk_ctrl,
            ids.launch_login,
            ids.perm_mic,
            ids.perm_a11y,
            ids.edit_vocab,
            ids.cleanup_toggle,
            ids.quit,
        ];
        all.extend(ids.mic_items.into_iter().map(|(_, id)| id));
        all.extend(ids.history_items.into_iter().map(|(_, id)| id));
        let mut seen = std::collections::HashSet::new();
        for id in &all {
            assert!(seen.insert(id.clone()), "duplicate menu id: {id:?}");
        }
    }

    #[test]
    fn test_menu_ids_stable_across_rebuilds() {
        // Rebuilds (state change, history push) must keep ids so in-flight
        // clicks still dispatch instead of falling to "unknown id".
        let devices = vec!["Mic A".to_string()];
        let a = ids_for(&devices, &sample_history());
        let b = ids_for(&devices, &sample_history());
        assert_eq!(a.status, b.status);
        assert_eq!(a.quit, b.quit);
        assert_eq!(a.hk_ctrl, b.hk_ctrl);
    }
}
