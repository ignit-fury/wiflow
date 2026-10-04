pub mod headless;
pub mod orchestrator;
pub mod session;

pub use orchestrator::{Action, Orchestrator};

use muda::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use winit::{
    event::{DeviceEvent, ElementState},
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    keyboard::{KeyCode, PhysicalKey},
};

use crate::core::config::{Config, ModelChoice};
use crate::core::history::HistoryEntry;
use crate::core::traits::TextInjector;
use crate::daemon::{
    current_session, preset_hint, preset_hotkey, Control, DaemonEvent, HotkeyPreset,
};

fn daemon_event_variant_name(ev: &DaemonEvent) -> &'static str {
    match ev {
        DaemonEvent::PttDown => "PttDown",
        DaemonEvent::PttUp => "PttUp",
        DaemonEvent::Cancel => "Cancel",
        DaemonEvent::CaptureStarted { .. } => "CaptureStarted",
        DaemonEvent::Watchdog { .. } => "Watchdog",
        DaemonEvent::TapIssue { .. } => "TapIssue",
        DaemonEvent::Done { .. } => "Done",
        DaemonEvent::Failed { .. } => "Failed",
        DaemonEvent::CleanupIssue { .. } => "CleanupIssue",
    }
}

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

/// Transcribing spinner frames: the state dot stays Transcribing-blue while
/// a satellite dot orbits (4 positions, one per 250 ms tick). `tick` usually
/// comes from `about_to_wait`'s animation driver; any wraparound works
/// (`wrapping` arithmetic — frame N matches frame N mod 4).
pub fn spinner_rgba(tick: u64) -> Vec<u8> {
    // Satellite centers for the 4 frames: E, S, W, N at radius 11.
    const SAT: [(i32, i32); 4] = [(27, 16), (16, 27), (5, 16), (16, 5)];
    let (sx, sy) = SAT[(tick % 4) as usize];
    let mut px = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32i32 {
        for x in 0..32i32 {
            let dx = x - 16;
            let dy = y - 16;
            let sdx = x - sx;
            let sdy = y - sy;
            let (r, g, b) = if dx * dx + dy * dy <= 49 {
                (60, 180, 255)
            } else if sdx * sdx + sdy * sdy <= 4 {
                (240, 240, 240)
            } else {
                (24, 24, 24)
            };
            px.extend_from_slice(&[r, g, b, 255]);
        }
    }
    px
}

pub fn spinner_frame(tick: u64) -> Icon {
    Icon::from_rgba(spinner_rgba(tick), 32, 32).expect("generated spinner is valid RGBA")
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
    ctx_toggle: MenuId,
    duck_toggle: MenuId,
    prov_auto: MenuId,
    prov_groq: MenuId,
    prov_openrouter: MenuId,
    prov_ollama: MenuId,
    stt_local: MenuId,
    stt_groq: MenuId,
    key_groq: MenuId,
    key_openrouter: MenuId,
    key_groq_clear: MenuId,
    key_or_clear: MenuId,
    ollama_models: Vec<(String, MenuId)>,
    ollama_refresh: MenuId,
    show_logs: MenuId,
    settings_window: MenuId,
    quit: MenuId,
}

/// Pure id scheme for the whole menu tree: deterministic strings, no
/// platform objects — testable off the main thread (muda forbids
/// `Menu::new` elsewhere on macOS). `build_menu` stamps these ids via
/// `with_id`, so the handler and the test see the same values.
fn ids_for(devices: &[String], history: &[HistoryEntry], ollama_models: &[String]) -> MenuIds {
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
        ctx_toggle: MenuId::new("wiflow:ctx:toggle"),
        duck_toggle: MenuId::new("wiflow:duck:toggle"),
        prov_auto: MenuId::new("wiflow:prov:auto"),
        prov_groq: MenuId::new("wiflow:prov:groq"),
        prov_openrouter: MenuId::new("wiflow:prov:openrouter"),
        prov_ollama: MenuId::new("wiflow:prov:ollama"),
        stt_local: MenuId::new("wiflow:stt:local"),
        stt_groq: MenuId::new("wiflow:stt:groq"),
        key_groq: MenuId::new("wiflow:key:groq"),
        key_openrouter: MenuId::new("wiflow:key:openrouter"),
        key_groq_clear: MenuId::new("wiflow:key:groq:clear"),
        key_or_clear: MenuId::new("wiflow:key:or:clear"),
        ollama_models: ollama_models
            .iter()
            .enumerate()
            .map(|(i, m)| (m.clone(), MenuId::new(format!("wiflow:ollama:{i}"))))
            .collect(),
        ollama_refresh: MenuId::new("wiflow:ollama:refresh"),
        show_logs: MenuId::new("wiflow:show:logs"),
        settings_window: MenuId::new("wiflow:settings:open"),
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
    ollama_models: &[String],
) -> (Menu, MenuIds) {
    let menu = Menu::new();
    let ids = ids_for(devices, history, ollama_models);

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
        "AI Cleanup (Groq→OpenRouter→Ollama)",
        true,
        config.cleanup_enabled,
        None,
    );

    // Context inference: focused-app → 2-sentence context hint fed to the
    // cleanup model (on by default; skipped instantly when cleanup is off).
    let ctx_toggle = CheckMenuItem::with_id(
        ids.ctx_toggle.clone(),
        "Context Inference",
        true,
        config.context_enabled,
        None,
    );

    // Audio prioritization: duck competing output + pause scriptable players
    // while the mic is hot, restored exactly afterwards.
    let duck_toggle = CheckMenuItem::with_id(
        ids.duck_toggle.clone(),
        "Duck Audio While Dictating",
        true,
        config.duck_audio,
        None,
    );

    // Cleanup provider: auto chain or a single provider (Ollama fallback on
    // quota errors only). Empty config value counts as auto.
    let cp = if config.cleanup_provider.is_empty() {
        "auto"
    } else {
        config.cleanup_provider.as_str()
    };
    let prov_menu = Submenu::new("Cleanup Provider", true);
    let prov_auto = CheckMenuItem::with_id(
        ids.prov_auto.clone(),
        "Auto (Groq→OpenRouter→Ollama)",
        true,
        cp == "auto",
        None,
    );
    let prov_groq =
        CheckMenuItem::with_id(ids.prov_groq.clone(), "Groq only", true, cp == "groq", None);
    let prov_openrouter = CheckMenuItem::with_id(
        ids.prov_openrouter.clone(),
        "OpenRouter only",
        true,
        cp == "openrouter",
        None,
    );
    let prov_ollama = CheckMenuItem::with_id(
        ids.prov_ollama.clone(),
        "Ollama only",
        true,
        cp == "ollama",
        None,
    );
    prov_menu
        .append_items(&[&prov_auto, &prov_groq, &prov_openrouter, &prov_ollama])
        .expect("menu append");

    // Speech-to-text provider: local (on-device) by default — audio leaves
    // the device only when Groq cloud is explicitly chosen (privacy rule).
    // stt_language stays config.json-only (not in the menu).
    let stt_menu = Submenu::new("Transcription", true);
    let stt_local = CheckMenuItem::with_id(
        ids.stt_local.clone(),
        "Transcription: Local whisper",
        true,
        config.stt_provider != "groq",
        None,
    );
    let stt_groq = CheckMenuItem::with_id(
        ids.stt_groq.clone(),
        "Transcription: Groq cloud (whisper-large-v3-turbo)",
        true,
        config.stt_provider == "groq",
        None,
    );
    stt_menu
        .append_items(&[&stt_local, &stt_groq])
        .expect("menu append");

    // AI keys + local model picker. Key state shown inline (✓/not set);
    // entry via native secure dialog (tray app has no windows). Model list
    // is the live Ollama inventory passed in by the caller.
    let ai_menu = Submenu::new("AI Keys && Models", true);
    let groq_set = crate::core::cleanup::groq_key().is_some();
    let or_set = crate::core::cleanup::openrouter_key().is_some();
    let key_groq = MenuItem::with_id(
        ids.key_groq.clone(),
        format!(
            "Groq API Key{}…",
            if groq_set { " ✓" } else { " (not set)" }
        ),
        true,
        None,
    );
    let key_openrouter = MenuItem::with_id(
        ids.key_openrouter.clone(),
        format!(
            "OpenRouter API Key{}…",
            if or_set { " ✓" } else { " (not set)" }
        ),
        true,
        None,
    );
    let key_groq_clear =
        MenuItem::with_id(ids.key_groq_clear.clone(), "Clear Groq Key", groq_set, None);
    let key_or_clear = MenuItem::with_id(
        ids.key_or_clear.clone(),
        "Clear OpenRouter Key",
        or_set,
        None,
    );
    ai_menu
        .append_items(&[&key_groq, &key_openrouter, &key_groq_clear, &key_or_clear])
        .expect("menu append");
    ai_menu
        .append(&PredefinedMenuItem::separator())
        .expect("menu append");
    let ollama_menu = Submenu::new("Ollama Model", true);
    if ollama_models.is_empty() {
        let off = MenuItem::new("(Ollama not running)", false, None);
        ollama_menu.append(&off).expect("menu append");
    } else {
        for (name, mid) in &ids.ollama_models {
            let item = CheckMenuItem::with_id(
                mid.clone(),
                name,
                true,
                *name == config.cleanup_model,
                None,
            );
            ollama_menu.append(&item).expect("menu append");
        }
    }
    let ollama_refresh =
        MenuItem::with_id(ids.ollama_refresh.clone(), "Refresh Models…", true, None);
    ollama_menu.append(&ollama_refresh).expect("menu append");
    ai_menu.append(&ollama_menu).expect("menu append");

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

    // Lifecycle log (wiflow.log): opens in Console — the diagnosis trail
    // for PTT issues must be reachable without a terminal.
    let show_logs = MenuItem::with_id(ids.show_logs.clone(), "Show Logs…", true, None);

    let settings_window = MenuItem::with_id(ids.settings_window.clone(), "Settings…", true, None);

    let quit = MenuItem::with_id(ids.quit.clone(), "Quit Wiflow", true, None);

    menu.append(&status).expect("menu append");
    menu.append(&PredefinedMenuItem::separator())
        .expect("menu append");
    menu.append(&mic_menu).expect("menu append");
    menu.append(&model_menu).expect("menu append");
    menu.append(&hk_menu).expect("menu append");
    menu.append(&launch_login).expect("menu append");
    menu.append(&cleanup_toggle).expect("menu append");
    menu.append(&ctx_toggle).expect("menu append");
    menu.append(&duck_toggle).expect("menu append");
    menu.append(&prov_menu).expect("menu append");
    menu.append(&stt_menu).expect("menu append");
    menu.append(&ai_menu).expect("menu append");
    menu.append(&PredefinedMenuItem::separator())
        .expect("menu append");
    menu.append(&hist_menu).expect("menu append");
    menu.append(&edit_vocab).expect("menu append");
    menu.append(&show_logs).expect("menu append");
    menu.append(&settings_window).expect("menu append");
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
    tap: Option<crate::platform::macos::tap::ModifierTap>,
    preset: HotkeyPreset,
    config: Config,
    devices: Vec<String>,
    proxy: EventLoopProxy<DaemonEvent>,
    tx: std::sync::mpsc::Sender<Control>,
    /// Central lifecycle coordinator — replaces the old inline PttMachine.
    orchestrator: Orchestrator,
    state: AppState,
    /// Warn override (e.g. inject-fail, cancel): shown instead of the state tooltip.
    note: Option<String>,
    applied_state: AppState,
    applied_tooltip: String,
    menu_ids: MenuIds,
    menu_dirty: bool,
    /// Live settings window (S4, pure Rust): created on demand from the
    /// tray menu on the existing event loop; closed via its X button.
    settings_window: Option<crate::ui::settings::SettingsWindow>,
    /// Set by the tray "Settings…" item; consumed in `about_to_wait` (which
    /// has the `ActiveEventLoop` needed to create the window).
    open_settings_requested: bool,
    /// Mic-probe result for the settings window's Test-record button:
    /// written by an App-owned 1 s capture thread, polled per frame (H20:
    /// the window renders App-owned state, never captures itself).
    test_record_result: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    test_record_running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Recording pill (S4, pure Rust): observational indicator owned by the
    /// App shell, driven by ShowPill/HidePill actions (H21/H22).
    pill: crate::ui::pill::Pill,
    /// Transcribing spinner position (about_to_wait animation driver).
    spinner_tick: u64,
}

impl DaemonApp {
    fn set_state(&mut self, state: AppState, note: Option<String>) {
        self.state = state;
        self.note = note;
        // Status item shows the state → menu needs a rebuild.
        self.menu_dirty = true;
    }

    /// Queue a worker command. A closed channel used to be swallowed by
    /// `let _ =`, wedging the tray with no trace — now it's a visible error.
    fn send_control(&mut self, ctl: Control) {
        if let Err(e) = self.tx.send(ctl) {
            // SendError carries the undelivered control back (e.0).
            tracing::error!("worker channel closed, control {:?} dropped", e.0);
            self.set_state(
                AppState::Error,
                Some("dictation worker is not running — restart Wiflow".into()),
            );
            self.sync_tray();
        }
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
        let history = crate::core::history::load_history();
        // Live Ollama inventory (≤500ms, empty when down). Only fetched on
        // rebuilds, never per event-loop tick.
        let models = crate::core::cleanup::list_ollama_models();
        let (menu, ids) = build_menu(
            &self.config,
            &self.devices,
            &history,
            self.state.status_label(),
            &models,
        );
        self.tray.set_menu(Some(Box::new(menu)));
        self.menu_ids = ids;
        self.menu_dirty = false;
    }

    fn save(&mut self) {
        if let Err(e) = crate::core::config::save_config(&self.config) {
            tracing::warn!("save config failed: {e}");
        }
    }

    /// Launch-at-login behind one path for the tray menu AND the settings
    /// window intent (H20). On failure the tray note explains; the caller
    /// that staged the value (settings window) reverts its own copy.
    fn apply_launch_at_login(&mut self, enable: bool) -> bool {
        let exe = std::env::current_exe()
            .unwrap_or_else(|_| std::path::PathBuf::from("wiflow-dictation"));
        match crate::core::config::set_launch_at_login(enable, &exe, true) {
            Ok(()) => {
                self.config.launch_at_login = enable;
                self.save();
                self.menu_dirty = true;
                tracing::info!("launch at login: {enable}");
                true
            }
            Err(e) => {
                self.warn_note(format!("launch-at-login failed: {e}"));
                false
            }
        }
    }

    /// Execute one settings-window intent (H20: the window proposes, the
    /// App disposes — tray menu paths reused wherever they exist).
    fn execute_settings_intent(&mut self, intent: crate::ui::settings::SettingsIntent) {
        use crate::ui::settings::SettingsIntent as I;
        match intent {
            I::Save(cfg) => {
                self.config = cfg;
                self.save();
                self.menu_dirty = true;
                tracing::info!("settings saved (apply to the next hold)");
            }
            I::SwitchHotkey(want) => self.switch_hotkey(want),
            I::CopyHistory(text) => match arboard::Clipboard::new() {
                Ok(mut cb) => match cb.set_text(text) {
                    Ok(()) => tracing::info!("history entry copied"),
                    Err(e) => self.warn_note(format!("copy failed: {e:?}")),
                },
                Err(e) => self.warn_note(format!("clipboard unavailable: {e:?}")),
            },
            I::ClearHistory => {
                let path = crate::core::history::history_path();
                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        tracing::info!("history file deleted");
                        if let Some(w) = self.settings_window.as_mut() {
                            w.clear_history_view();
                        }
                    }
                    Err(e) => self.warn_note(format!("clear history failed: {e}")),
                }
            }
            I::TestRecord => self.run_test_record(),
            I::SetLaunchAtLogin(enable) => {
                if !self.apply_launch_at_login(enable) {
                    // OS refused: revert the window's staged copy so disk,
                    // OS, and window agree again.
                    if let Some(w) = self.settings_window.as_mut() {
                        w.set_launch_at_login(!enable);
                    }
                }
            }
            I::OpenMicSettings => crate::core::config::permissions::open_mic_settings(),
            I::OpenAccessibilitySettings => {
                crate::core::config::permissions::open_accessibility_settings()
            }
        }
    }

    /// 1 s mic probe for the settings window's Test-record button. Runs in
    /// an App-owned thread (never on the event loop, never in the window);
    /// the result is polled per frame via `test_record_status` (H20).
    /// Amplitude only — no audio stored, no STT.
    fn run_test_record(&mut self) {
        use std::sync::atomic::Ordering;
        if self.test_record_running.swap(true, Ordering::SeqCst) {
            return; // A probe is already running.
        }
        let out = self.test_record_result.clone();
        let running = self.test_record_running.clone();
        let mic = self.config.mic_name.clone();
        std::thread::spawn(move || {
            let line = (|| {
                let cap = crate::core::audio::AudioCapture::start(mic)
                    .map_err(|e| format!("mic unavailable: {e}"))?;
                std::thread::sleep(std::time::Duration::from_millis(1000));
                let got = cap.stop();
                let rms = crate::core::audio::rms(&got.samples_mono);
                Ok::<String, String>(crate::ui::settings::format_test_record_result(
                    rms,
                    got.samples_mono.len(),
                    got.sample_rate,
                ))
            })();
            *out.lock().unwrap() = Some(line.unwrap_or_else(|e| e));
            running.store(false, Ordering::SeqCst);
        });
    }

    /// Latest mic-probe result for the settings window to render (if any).
    fn test_record_status(&self) -> Option<String> {
        self.test_record_result.lock().unwrap().clone()
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
            match crate::platform::macos::tap::spawn(want, self.proxy.clone()) {
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
                        match crate::platform::macos::tap::spawn(old_preset, self.proxy.clone()) {
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
            self.apply_launch_at_login(enable);
            return;
        }
        if *id == ids.edit_vocab {
            // Ensure prompt.txt exists (empty), then open in default editor.
            let path = crate::core::config::prompt_path();
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
        if *id == ids.ctx_toggle {
            self.config.context_enabled = !self.config.context_enabled;
            self.save();
            self.menu_dirty = true;
            tracing::info!(
                "context inference: {}",
                if self.config.context_enabled {
                    "on"
                } else {
                    "off"
                }
            );
            return;
        }
        if *id == ids.duck_toggle {
            self.config.duck_audio = !self.config.duck_audio;
            self.save();
            self.menu_dirty = true;
            tracing::info!(
                "audio ducking: {}",
                if self.config.duck_audio { "on" } else { "off" }
            );
            return;
        }
        if *id == ids.prov_auto {
            self.config.cleanup_provider = "auto".into();
            self.save();
            self.menu_dirty = true;
            tracing::info!("cleanup provider: auto (Groq→OpenRouter→Ollama chain)");
            return;
        }
        if *id == ids.prov_groq {
            self.config.cleanup_provider = "groq".into();
            self.save();
            self.menu_dirty = true;
            tracing::info!("cleanup provider: groq (Ollama fallback on quota)");
            return;
        }
        if *id == ids.prov_openrouter {
            self.config.cleanup_provider = "openrouter".into();
            self.save();
            self.menu_dirty = true;
            tracing::info!("cleanup provider: openrouter (Ollama fallback on quota)");
            return;
        }
        if *id == ids.prov_ollama {
            self.config.cleanup_provider = "ollama".into();
            self.save();
            self.menu_dirty = true;
            tracing::info!("cleanup provider: ollama only");
            return;
        }
        if *id == ids.stt_local {
            self.config.stt_provider = "local".into();
            self.save();
            self.menu_dirty = true;
            tracing::info!("stt provider: local (on-device whisper)");
            return;
        }
        if *id == ids.stt_groq {
            self.config.stt_provider = "groq".into();
            self.save();
            self.menu_dirty = true;
            tracing::info!("stt provider: groq cloud (whisper-large-v3-turbo)");
            return;
        }
        if *id == ids.key_groq {
            match crate::core::cleanup::prompt_for_key("Wiflow", "Paste your Groq API key:") {
                Some(v) => match crate::core::cleanup::save_key("groq_api_key", &v) {
                    Ok(()) => {
                        self.menu_dirty = true;
                        tracing::info!("groq api key saved");
                    }
                    Err(e) => self.warn_note(format!("save groq key failed: {e}")),
                },
                None => tracing::debug!("groq key dialog cancelled"),
            }
            return;
        }
        if *id == ids.key_openrouter {
            match crate::core::cleanup::prompt_for_key("Wiflow", "Paste your OpenRouter API key:") {
                Some(v) => match crate::core::cleanup::save_key("openrouter_api_key", &v) {
                    Ok(()) => {
                        self.menu_dirty = true;
                        tracing::info!("openrouter api key saved");
                    }
                    Err(e) => self.warn_note(format!("save openrouter key failed: {e}")),
                },
                None => tracing::debug!("openrouter key dialog cancelled"),
            }
            return;
        }
        if *id == ids.key_groq_clear {
            match crate::core::cleanup::clear_key("groq_api_key") {
                Ok(()) => {
                    self.menu_dirty = true;
                    tracing::info!("groq api key cleared");
                }
                Err(e) => self.warn_note(format!("clear groq key failed: {e}")),
            }
            return;
        }
        if *id == ids.key_or_clear {
            match crate::core::cleanup::clear_key("openrouter_api_key") {
                Ok(()) => {
                    self.menu_dirty = true;
                    tracing::info!("openrouter api key cleared");
                }
                Err(e) => self.warn_note(format!("clear openrouter key failed: {e}")),
            }
            return;
        }
        if *id == ids.ollama_refresh {
            // Rebuild refetches the live inventory (≤500ms localhost call).
            self.menu_dirty = true;
            return;
        }
        if let Some((name, _)) = ids.ollama_models.iter().find(|(_, mid)| mid == id) {
            self.config.cleanup_model = name.clone();
            self.save();
            self.menu_dirty = true;
            tracing::info!("ollama cleanup model: {name} (takes effect next hold)");
            return;
        }
        if *id == ids.settings_window {
            // Created in about_to_wait (needs the ActiveEventLoop).
            self.open_settings_requested = true;
            return;
        }
        if *id == ids.show_logs {
            let path = crate::logfile::log_path();
            let opened = std::process::Command::new("open")
                .args(["-a", "Console"])
                .arg(&path)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if opened {
                tracing::info!("log opened in Console ({})", path.display());
            } else if open::that(&path).is_err() {
                self.warn_note(format!("cannot open log file ({})", path.display()));
            }
            return;
        }
        if *id == ids.perm_mic {
            crate::core::config::permissions::open_mic_settings();
            return;
        }
        if *id == ids.perm_a11y {
            crate::core::config::permissions::open_accessibility_settings();
            return;
        }
        if *id == ids.quit {
            tracing::info!("quit via menu");
            // Orchestrator cleanup before worker teardown.
            let _ = self.orchestrator.handle_shutdown();
            crate::core::stt::shutdown();
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
            crate::platform::macos::inject::leave_on_clipboard(text);
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
        window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        // The process owns at most two winit Windows (settings + pill).
        // Unknown ids are ignored defensively. Borrows are statement-local
        // (NLL): each `as_mut` ends before App methods run.
        if self
            .settings_window
            .as_ref()
            .is_some_and(|w| w.window_id() == window_id)
        {
            let intents = self
                .settings_window
                .as_mut()
                .map(|w| w.handle_event(&event))
                .unwrap_or_default();
            for intent in intents {
                self.execute_settings_intent(intent);
            }
            if matches!(event, winit::event::WindowEvent::RedrawRequested) {
                let status = self.test_record_status();
                let intents = self
                    .settings_window
                    .as_mut()
                    .map(|w| w.paint(status.as_deref()))
                    .unwrap_or_default();
                for intent in intents {
                    self.execute_settings_intent(intent);
                }
            }
            let closed = self
                .settings_window
                .as_ref()
                .map(|w| w.close_requested())
                .unwrap_or(false);
            if closed {
                self.settings_window = None;
                tracing::info!("settings window closed");
            }
            return;
        }
        if self.pill.window_id() == Some(window_id)
            && matches!(event, winit::event::WindowEvent::RedrawRequested)
        {
            self.pill.frame();
        }
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
                if let Err(e) = self.proxy.send_event(DaemonEvent::Cancel) {
                    tracing::warn!("Esc: event loop closed, cancel lost: {e:?}");
                }
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: DaemonEvent) {
        let before = self.orchestrator.phase();
        let actions = self.orchestrator.handle(&event);
        if actions.is_empty() {
            // Empty actions ≠ rejected: pure-transition events (e.g. the
            // first CaptureStarted) apply a phase change with no side
            // effects. Only an unchanged phase means the machine ignored it.
            let after = self.orchestrator.phase();
            tracing::info!(
                "[session={}] {} produced no actions (phase {:?} -> {:?}){}",
                current_session(),
                daemon_event_variant_name(&event),
                before,
                after,
                if before == after { " — ignored" } else { "" }
            );
        }
        for action in actions {
            match action {
                Action::SendControl(ctl) => self.send_control(ctl),
                Action::SetTray(state, note) => {
                    self.set_state(state, note);
                    self.sync_tray();
                }
                Action::Notify(msg) => self.warn_note(msg),
                Action::ShowPill => self.pill.show(event_loop),
                Action::HidePill => self.pill.hide(),
                Action::Inject(text) => {
                    let injector = crate::core::traits::SystemInjector;
                    let result = match injector.inject(&text) {
                        Ok(r) => {
                            tracing::info!(
                                "injected via {} (clipboard restored: {})",
                                r.pasted_via,
                                r.clipboard_restored
                            );
                            Ok(r)
                        }
                        Err(e) => {
                            tracing::warn!(
                                "inject failed ({e}) — text left on clipboard, press Cmd+V"
                            );
                            injector.leave_on_clipboard(&text);
                            // No early return: the result flows into
                            // finish_inject below (INJECTING → RESTORING →
                            // finalize → ERROR). Returning here used to wedge
                            // the machine in INJECTING (next press ignored).
                            Err(e)
                        }
                    };
                    // Feed inject result back to the orchestrator, which drives
                    // the machine through RESTORING → finalize (restoring
                    // media) and returns the appropriate tray action.
                    // NOTE: finalize always appends HidePill — handle it
                    // (never unreachable!: a panic here kills the event
                    // loop, as a live run proved).
                    for fi_action in self.orchestrator.finish_inject(result) {
                        match fi_action {
                            Action::SetTray(state, note) => {
                                self.set_state(state, note);
                                self.sync_tray();
                            }
                            Action::Notify(msg) => self.warn_note(msg),
                            Action::HidePill => self.pill.hide(),
                            Action::SendControl(_) | Action::Inject(_) | Action::ShowPill => {
                                unreachable!("finish_inject only emits SetTray/Notify/HidePill")
                            }
                        }
                    }
                    // Injection completed: the orchestrator's finalize (via
                    // finish_inject above) already restored media.
                }
            }
        }
        // Native notifications, best-effort (H23): failures fall back to the
        // tray note the handlers above already set — the `let _` is load
        // bearing, never "fix" it into a `?`. CleanupIssue stays tray-only
        // (a broken provider would banner-spam every cycle).
        match &event {
            DaemonEvent::Done { text, .. } if !text.trim().is_empty() => {
                match crate::ui::notify::notify("wiflow", text) {
                    Ok(()) => tracing::info!("notification posted (transcript preview)"),
                    Err(e) => tracing::info!("notification failed ({e}) — tray note stands"),
                }
            }
            DaemonEvent::Failed(msg) => {
                match crate::ui::notify::notify("wiflow", &format!("Dictation failed: {msg}")) {
                    Ok(()) => tracing::info!("notification posted (failure)"),
                    Err(e) => tracing::info!("notification failed ({e}) — tray note stands"),
                }
            }
            DaemonEvent::TapIssue(msg) => match crate::ui::notify::notify("wiflow", msg) {
                Ok(()) => tracing::info!("notification posted (tap issue)"),
                Err(e) => tracing::info!("notification failed ({e}) — tray note stands"),
            },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(ev) = muda::MenuEvent::receiver().try_recv() {
            self.handle_menu_event(&ev.id);
        }
        // Open the settings window on demand (needs the ActiveEventLoop).
        if self.open_settings_requested {
            self.open_settings_requested = false;
            if self.settings_window.is_none() {
                let history = crate::core::history::load_history();
                let mics = crate::core::audio::list_devices();
                match crate::ui::settings::SettingsWindow::open(event_loop, history, mics) {
                    Ok(w) => {
                        self.settings_window = Some(w);
                        tracing::info!("settings window opened");
                    }
                    Err(e) => self.warn_note(format!("settings window failed: {e}")),
                }
            }
        }
        // Supervision deadline check.
        for action in self.orchestrator.tick() {
            match action {
                Action::SendControl(ctl) => self.send_control(ctl),
                Action::SetTray(state, note) => {
                    self.set_state(state, note);
                    self.sync_tray();
                }
                Action::Notify(msg) => self.warn_note(msg),
                Action::ShowPill => unreachable!("tick never emits ShowPill"),
                Action::HidePill => self.pill.hide(),
                Action::Inject(_) => unreachable!("tick never emits Inject"),
            }
        }
        if self.menu_dirty {
            self.rebuild_menu();
        }
        self.sync_tray();
        // Transcribing spinner: animate the tray icon on a 250 ms cadence
        // while the worker transcribes (main thread would otherwise sit
        // idle with a static icon). Any other state → plain Wait: no wakeups,
        // no battery cost. State exit repaints the static icon via sync_tray.
        if self.state == AppState::Transcribing {
            self.spinner_tick = self.spinner_tick.wrapping_add(1);
            if let Err(e) = self.tray.set_icon(Some(spinner_frame(self.spinner_tick))) {
                tracing::warn!("tray spinner failed: {e:?}");
            }
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(250),
            ));
        } else {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
        }
    }
}

fn app_main(
    event_loop: EventLoop<DaemonEvent>,
    proxy: winit::event_loop::EventLoopProxy<DaemonEvent>,
    mut config: Config,
) -> ! {
    let devices = crate::core::audio::list_devices();
    let history = crate::core::history::load_history();
    let models = crate::core::cleanup::list_ollama_models();
    let (menu, ids) = build_menu(
        &config,
        &devices,
        &history,
        AppState::Idle.status_label(),
        &models,
    );
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
        match crate::platform::macos::tap::spawn(config.hotkey_preset, proxy.clone()) {
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
    if let Err(e) = crate::core::config::save_config(&config) {
        tracing::warn!("save config failed: {e}");
    }
    crate::daemon::spawn_hotkey_bridge(proxy.clone(), esc_hotkey.id());

    // Background startup check: when cleanup is enabled, verify Ollama is
    // running + the model is pulled; alert the user when not (off the UI
    // thread, once per launch).
    {
        let check_proxy = proxy.clone();
        let check_model = config.cleanup_model.clone();
        let check_enabled = config.cleanup_enabled;
        std::thread::spawn(move || {
            if !check_enabled {
                return;
            }
            match crate::core::cleanup::check_ollama_ready(&check_model) {
                Ok(()) => tracing::info!("ollama cleanup ready ({check_model})"),
                Err(e) => {
                    tracing::warn!("ollama check: {e}");
                    let _ = check_proxy.send_event(DaemonEvent::CleanupIssue(e));
                }
            }
        });
    }

    // One shared duck instance: worker ducks at capture-Ok on the same
    // One shared duck instance: the worker ducks at capture-Ok on the same
    // Arc state the orchestrator records and restores (Task 12 wiring).
    let media = crate::platform::macos::media::CoreAudioDuck::new(
        crate::platform::macos::duck::OsBackend,
        config.duck_audio,
        crate::platform::macos::duck::PAUSE_DELAY,
    );
    let (tx, rx) = std::sync::mpsc::channel::<Control>();
    let worker_proxy = proxy.clone();
    let worker_duck = media.shared_inner();
    std::thread::spawn(move || crate::daemon::worker_main(worker_proxy, rx, worker_duck));

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
        orchestrator: Orchestrator::new(
            crate::core::traits::RouterRecognizer,
            crate::core::traits::ChainProvider,
            crate::core::traits::SystemInjector,
            crate::core::traits::OsascriptContext,
            media,
        ),
        state: AppState::Idle,
        note: None,
        applied_state: AppState::Idle,
        applied_tooltip: AppState::Idle.tooltip(won),
        menu_ids: ids,
        menu_dirty: false,
        settings_window: None,
        open_settings_requested: false,
        test_record_result: std::sync::Arc::new(std::sync::Mutex::new(None)),
        test_record_running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        pill: crate::ui::pill::Pill::new(),
        spinner_tick: 0,
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
    fn spinner_frames_cycle() {
        // 4 distinct frames, then wraparound: frame N matches frame N mod 4.
        let frames: Vec<Vec<u8>> = (0..4).map(spinner_rgba).collect();
        for (i, a) in frames.iter().enumerate() {
            assert_eq!(a.len(), 32 * 32 * 4, "frame {i} is 32x32 RGBA");
            for (j, b) in frames.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "frames {i} and {j} must differ");
                }
            }
        }
        assert_eq!(spinner_rgba(4), frames[0], "frame 4 wraps to frame 0");
        assert_eq!(spinner_rgba(102), frames[2], "large ticks wrap");
        // Center dot stays Transcribing-blue in every frame.
        for (i, f) in frames.iter().enumerate() {
            let c = (16 * 32 + 16) * 4;
            assert!(
                f[c] < 100 && f[c + 1] > 150 && f[c + 2] > 200,
                "frame {i} center stays blue"
            );
        }
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
        let models = vec!["llama3.2:1b".to_string(), "qwen3:8b".to_string()];
        let ids = ids_for(&devices, &sample_history(), &models);
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
            ids.ctx_toggle,
            ids.duck_toggle,
            ids.prov_auto,
            ids.prov_groq,
            ids.prov_openrouter,
            ids.prov_ollama,
            ids.stt_local,
            ids.stt_groq,
            ids.key_groq,
            ids.key_openrouter,
            ids.key_groq_clear,
            ids.key_or_clear,
            ids.ollama_refresh,
            ids.show_logs,
            ids.settings_window,
            ids.quit,
        ];
        all.extend(ids.mic_items.into_iter().map(|(_, id)| id));
        all.extend(ids.history_items.into_iter().map(|(_, id)| id));
        all.extend(ids.ollama_models.into_iter().map(|(_, id)| id));
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
        let models = vec!["llama3.2:1b".to_string()];
        let a = ids_for(&devices, &sample_history(), &models);
        let b = ids_for(&devices, &sample_history(), &models);
        assert_eq!(a.status, b.status);
        assert_eq!(a.quit, b.quit);
        assert_eq!(a.hk_ctrl, b.hk_ctrl);
        assert_eq!(a.key_groq, b.key_groq);
        assert_eq!(a.ollama_refresh, b.ollama_refresh);
        assert_eq!(a.settings_window, b.settings_window);
    }

    #[test]
    fn test_ollama_model_ids_map_back_to_names() {
        // Click dispatch finds the model name by id (handler contract).
        let models = vec!["llama3.2:1b".to_string(), "qwen3:8b".to_string()];
        let ids = ids_for(&[], &[], &models);
        assert_eq!(ids.ollama_models.len(), 2);
        let hit = ids
            .ollama_models
            .iter()
            .find(|(_, mid)| mid == &ids.ollama_models[1].1)
            .map(|(name, _)| name.clone());
        assert_eq!(hit.as_deref(), Some("qwen3:8b"));
        // No models → no dynamic ids (menu shows the offline item instead).
        let empty = ids_for(&[], &[], &[]);
        assert!(empty.ollama_models.is_empty());
    }
}
