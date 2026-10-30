//! Main app window (Home / History / Settings / Permissions): view-model,
//! hotkey validation, and the winit/egui window.
//!
//! Layering (H20): this module renders App state and emits [`SettingsIntent`]
//! commands. It never touches the daemon, the PTT machine, or the tray
//! directly — the App shell executes every intent (which is also how the
//! next-session rule, H24, is enforced: sessions snapshot at STARTING).

use std::path::Path;

use winit::keyboard::{KeyCode, ModifiersState};

use crate::core::config::Config;
use crate::daemon::HotkeyPreset;

// ── View-model (pure, headless-testable) ────────────────────────────────────

/// Staged settings edit: widgets mutate `config` in memory; `save()`
/// persists to disk. Sessions snapshot at STARTING, so saved changes apply
/// to the next hold, never the active session (H24).
#[derive(Debug, Clone)]
pub struct SettingsViewModel {
    pub config: Config,
}

impl SettingsViewModel {
    /// Load from the live config file.
    pub fn load() -> Self {
        Self {
            config: crate::core::config::load_config(),
        }
    }

    /// Reload from disk, discarding staged edits.
    pub fn revert(&mut self) {
        *self = Self::load();
    }

    /// Persist staged edits to the live config file.
    pub fn save(&self) -> Result<(), String> {
        crate::core::config::save_config(&self.config)
    }

    /// Test seam: explicit paths (never touches the live file).
    pub fn load_from(path: &Path) -> Self {
        Self {
            config: crate::core::config::load_config_from(path),
        }
    }

    /// Test seam: explicit paths (never touches the live file).
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        crate::core::config::save_config_to(path, &self.config)
    }
}

// ── Hotkey validation (pure) ────────────────────────────────────────────────

/// Why a recorded key press cannot become the push-to-talk hotkey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyError {
    /// Bare single keys can't be registered (macOS limitation for the paths
    /// wiflow uses; bare Fn/Right-Option ride the tap as presets instead).
    SingleKey(KeyCode),
    /// Registrable in theory, but wiflow only drives its three presets
    /// (the hotkey bridge forwards known preset ids, no restart needed).
    CustomUnsupported(String),
}

/// Map a recorded (modifiers, key) press to a wiflow preset, or explain why
/// it cannot be used. Bare Fn / Right-Option map to their tap presets;
/// Ctrl+Space maps to the combo preset.
pub fn validate_hotkey(mods: ModifiersState, code: KeyCode) -> Result<HotkeyPreset, HotkeyError> {
    use winit::keyboard::KeyCode as K;
    let only_ctrl = mods.control_key() && !mods.shift_key() && !mods.alt_key() && !mods.super_key();
    let bare = mods.is_empty();
    match (bare, only_ctrl, code) {
        (true, _, K::Fn) => Ok(HotkeyPreset::Fn),
        (true, _, K::AltRight) => Ok(HotkeyPreset::RightOption),
        (_, true, K::Space) => Ok(HotkeyPreset::CtrlSpace),
        // Cmd/Ctrl+Space is the macOS Spotlight default — call it out.
        (_, _, K::Space) if mods.super_key() || mods.control_key() => {
            Err(HotkeyError::CustomUnsupported(
                "that combo is taken by Spotlight — use Ctrl+Space".into(),
            ))
        }
        (true, _, single) => Err(HotkeyError::SingleKey(single)),
        (_, _, _) => Err(HotkeyError::CustomUnsupported(
            "wiflow drives its three presets (Fn / Right-Option / Ctrl+Space)".into(),
        )),
    }
}

// ── Test-record formatting (pure) ───────────────────────────────────────────

/// One-line human summary of a 1 s mic probe (no audio stored, RMS only).
pub fn format_test_record_result(rms: f32, samples: usize, sample_rate: u32) -> String {
    let secs = samples as f64 / sample_rate.max(1) as f64;
    if rms < 0.005 {
        format!("silent (rms {rms:.3}, {samples} samples) — check mic input")
    } else {
        format!("mic OK (rms {rms:.3}, {samples} samples, {secs:.1}s)")
    }
}

// ── Intents (window → App) ──────────────────────────────────────────────────

/// Commands the settings window emits; the App shell executes them (H20).
#[derive(Debug, Clone)]
pub enum SettingsIntent {
    /// Persist the staged config (H24: applies to the next hold).
    Save(Config),
    /// Switch the PTT preset (existing menu path).
    SwitchHotkey(HotkeyPreset),
    /// Copy a history entry to the clipboard (App owns arboard).
    CopyHistory(String),
    /// Delete the history file (must actually delete — rules.md).
    ClearHistory,
    /// 1 s mic probe in an App-owned thread (App owns capture).
    TestRecord,
    /// Enable/disable launch at login (existing menu path).
    SetLaunchAtLogin(bool),
    /// Open System Settings panels (existing menu paths).
    OpenMicSettings,
    OpenAccessibilitySettings,
    OpenInputMonitoringSettings,
}

// ── Window (winit + egui, lives on the existing event loop) ─────────────────

use super::gl::{GlWindow, GlWindowOpts};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

use crate::core::config::ModelChoice;
use crate::core::history::HistoryEntry;

/// Sidebar pages of the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    History,
    Settings,
    Permissions,
}

/// App-owned live state the window renders each frame (H20: the window
/// never reads the daemon or the OS itself).
pub struct LiveStatus {
    /// One line, e.g. "Ready — hold Right Option to dictate".
    pub headline: String,
    /// Status dot color (same as the tray icon dot).
    pub color: [u8; 3],
    /// Latest 1 s mic-probe result, if any.
    pub test_status: Option<String>,
    /// Accessibility permission granted (needed to paste).
    pub accessibility_ok: bool,
    /// Input Monitoring granted (the hotkey works in every app).
    pub input_monitoring_ok: bool,
}

/// Dictation count, total words, average processing time (ms).
pub fn history_stats(history: &[HistoryEntry]) -> (usize, usize, u64) {
    let words = history
        .iter()
        .map(|e| e.text.split_whitespace().count())
        .sum();
    let avg = if history.is_empty() {
        0
    } else {
        history.iter().map(|e| e.duration_ms).sum::<u64>() / history.len() as u64
    };
    (history.len(), words, avg)
}

/// Short relative time: "just now", "5 min ago", "3 h ago", "2 d ago".
pub fn time_ago(now_ms: u64, at_ms: u64) -> String {
    let secs = now_ms.saturating_sub(at_ms) / 1000;
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Main app window: sidebar (Home / History / Settings / Permissions) over
/// the staged settings view-model. Created on the EXISTING event loop
/// (H25: one loop per process); closed via the window X button.
pub struct SettingsWindow {
    gl: GlWindow,
    vm: SettingsViewModel,
    mic_list: Vec<String>,
    ollama_models: Vec<String>,
    history: Vec<HistoryEntry>,
    tab: Tab,
    search: String,
    recording_keys: bool,
    record_mods: ModifiersState,
    record_msg: Option<String>,
    save_msg: Option<String>,
    closed: bool,
}

impl SettingsWindow {
    /// Build + show the window. `history`, `mic_list` and `ollama_models`
    /// are snapshots taken by the App at open time (H20).
    pub fn open(
        event_loop: &ActiveEventLoop,
        history: Vec<HistoryEntry>,
        mic_list: Vec<String>,
        ollama_models: Vec<String>,
    ) -> Result<Self, String> {
        let gl = GlWindow::open(
            event_loop,
            &GlWindowOpts {
                title: "Wiflow",
                width: 780.0,
                height: 560.0,
                decorations: true,
                always_on_top: false,
                position: None,
            },
        )?;
        gl.show();
        Ok(Self {
            gl,
            vm: SettingsViewModel::load(),
            mic_list,
            ollama_models,
            history,
            tab: Tab::Home,
            search: String::new(),
            recording_keys: false,
            record_mods: ModifiersState::empty(),
            record_msg: None,
            save_msg: None,
            closed: false,
        })
    }

    pub fn window_id(&self) -> WindowId {
        self.gl.window_id()
    }

    /// Bring the window to the front (tray item / relaunch while open).
    pub fn focus(&self) {
        self.gl.show();
    }

    pub fn repaint_deadline(&mut self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.gl.repaint_deadline(now)
    }

    /// A background mic probe can wake this window without polling frames.
    pub fn redraw_handle(&self) -> std::sync::Arc<winit::window::Window> {
        self.gl.window.clone()
    }

    /// Closed via the window X button — the App drops the window then.
    pub fn close_requested(&self) -> bool {
        self.closed
    }

    /// Live state changed (new dictation, recording...): refresh + repaint.
    pub fn set_history(&mut self, history: Vec<HistoryEntry>) {
        self.history = history;
        self.gl.request_redraw();
    }

    /// Clear the locally shown history after a ClearHistory intent executes.
    pub fn clear_history_view(&mut self) {
        self.set_history(Vec::new());
    }

    /// Keep the staged copy in line with a hotkey the App switched live, so
    /// a later Save never writes the old preset back.
    pub fn set_hotkey(&mut self, preset: HotkeyPreset) {
        self.vm.config.hotkey_preset = preset;
        self.gl.request_redraw();
    }

    /// Revert the staged launch-at-login flag (OS refused the change).
    pub fn set_launch_at_login(&mut self, enable: bool) {
        self.vm.config.launch_at_login = enable;
        self.gl.request_redraw();
    }

    /// Feed a winit event to egui; capture recorder keys; collect intents.
    pub fn handle_event(&mut self, event: &WindowEvent) -> Vec<SettingsIntent> {
        let mut intents = Vec::new();
        if *event == WindowEvent::CloseRequested {
            self.closed = true;
            return intents;
        }
        // Leave Escape unassigned, including egui's popup/recorder behavior.
        if matches!(event, WindowEvent::KeyboardInput { event: key, .. }
            if key.physical_key == winit::keyboard::PhysicalKey::Code(KeyCode::Escape))
        {
            return intents;
        }
        // Hotkey recorder captures raw winit keys (bare Fn/AltRight are
        // invisible to egui's modifier model).
        match event {
            WindowEvent::ModifiersChanged(state) => {
                self.record_mods = state.state();
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                use winit::event::ElementState;
                use winit::keyboard::PhysicalKey;
                if self.recording_keys && key_event.state == ElementState::Pressed {
                    if let PhysicalKey::Code(code) = key_event.physical_key {
                        match validate_hotkey(self.record_mods, code) {
                            Ok(preset) => {
                                self.recording_keys = false;
                                self.record_msg = Some(format!("captured {preset:?} — switching"));
                                intents.push(SettingsIntent::SwitchHotkey(preset));
                            }
                            Err(e) => {
                                self.record_msg = Some(format!("not usable: {e:?}"));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        let repaint = self.gl.handle_event(event);
        if repaint {
            self.gl.request_redraw();
        }
        intents
    }

    /// Render one egui frame; returns intents (Save, copies, toggles...).
    pub fn paint(&mut self, live: &LiveStatus) -> Vec<SettingsIntent> {
        let mut intents = Vec::new();
        // Split borrows up front: the frame closure captures locals, never
        // `self`, while `gl.paint` holds `&mut self.gl`.
        let vm = &mut self.vm;
        let tab = &mut self.tab;
        let search = &mut self.search;
        let recording_keys = &mut self.recording_keys;
        let record_msg = &mut self.record_msg;
        let save_msg = &mut self.save_msg;
        let mic_list = &self.mic_list;
        let ollama_models = &self.ollama_models;
        let history = &self.history;
        self.gl.paint(|ctx| {
            egui::SidePanel::left("nav")
                .resizable(false)
                .exact_width(160.0)
                .show(ctx, |ui| {
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("Wiflow").size(22.0).strong());
                    ui.small("push-to-talk dictation");
                    ui.add_space(16.0);
                    for (t, name) in [
                        (Tab::Home, "Home"),
                        (Tab::History, "History"),
                        (Tab::Settings, "Settings"),
                        (Tab::Permissions, "Permissions"),
                    ] {
                        let text = egui::RichText::new(name).size(15.0);
                        if ui
                            .add_sized(
                                [ui.available_width(), 28.0],
                                egui::SelectableLabel::new(*tab == t, text),
                            )
                            .clicked()
                        {
                            *tab = t;
                        }
                    }
                });
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| match *tab {
                    Tab::Home => Self::home_page(ui, live, history, tab, &mut intents),
                    Tab::History => Self::history_page(ui, history, search, &mut intents),
                    Tab::Settings => {
                        ui.heading("Settings");
                        ui.label("Changes apply to the next dictation, never the active one.");
                        ui.separator();
                        Self::hotkey_panel(ui, vm, recording_keys, record_msg, &mut intents);
                        ui.separator();
                        Self::audio_panel(ui, vm, mic_list);
                        ui.separator();
                        Self::provider_panel(ui, vm, ollama_models);
                        ui.separator();
                        Self::media_panel(ui, vm, &mut intents);
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                intents.push(SettingsIntent::Save(vm.config.clone()));
                                *save_msg = Some("saved — applies to the next hold".into());
                            }
                            if ui.button("Revert").clicked() {
                                vm.revert();
                                *save_msg = Some("reverted to disk".into());
                            }
                            if let Some(msg) = &*save_msg {
                                ui.label(msg.as_str());
                            }
                        });
                    }
                    Tab::Permissions => Self::permissions_page(ui, live, &mut intents),
                });
            });
        }); // end paint closure
        intents
    }
}

// ── Pages (thin egui views over App-owned state + the staged view-model) ────

impl SettingsWindow {
    fn home_page(
        ui: &mut egui::Ui,
        live: &LiveStatus,
        history: &[HistoryEntry],
        tab: &mut Tab,
        intents: &mut Vec<SettingsIntent>,
    ) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            let [r, g, b] = live.color;
            ui.painter()
                .circle_filled(rect.center(), 8.0, egui::Color32::from_rgb(r, g, b));
            ui.label(egui::RichText::new(&live.headline).size(20.0).strong());
        });
        for (ok, msg) in [
            (
                live.input_monitoring_ok,
                "Input Monitoring is off — the hotkey only works while Wiflow is in front.",
            ),
            (
                live.accessibility_ok,
                "Accessibility is off — Wiflow can't paste text.",
            ),
        ] {
            if ok {
                continue;
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(230, 150, 0), msg);
                if ui.button("Fix in Permissions").clicked() {
                    *tab = Tab::Permissions;
                }
            });
        }
        ui.add_space(14.0);
        let (count, words, avg_ms) = history_stats(history);
        ui.columns(3, |cols| {
            for (col, (value, label)) in cols.iter_mut().zip([
                (count.to_string(), "dictations"),
                (words.to_string(), "words"),
                (format!("{avg_ms} ms"), "avg processing"),
            ]) {
                egui::Frame::group(col.style()).show(col, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(egui::RichText::new(value).size(24.0).strong());
                    ui.small(label);
                });
            }
        });
        ui.add_space(14.0);
        ui.heading("Last dictation");
        match history.last() {
            Some(entry) => {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(egui::RichText::new(&entry.text).size(15.0));
                    ui.horizontal(|ui| {
                        ui.small(time_ago(now_ms(), entry.at_ms));
                        if ui.small_button("Copy").clicked() {
                            intents.push(SettingsIntent::CopyHistory(entry.text.clone()));
                        }
                    });
                });
            }
            None => {
                ui.label("Nothing yet. Hold your push-to-talk key, speak, and release.");
            }
        }
    }

    fn history_page(
        ui: &mut egui::Ui,
        history: &[HistoryEntry],
        search: &mut String,
        intents: &mut Vec<SettingsIntent>,
    ) {
        ui.heading("History");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(search).hint_text("Search…"));
            if ui.button("Clear history").clicked() {
                intents.push(SettingsIntent::ClearHistory);
            }
        });
        ui.small(format!(
            "Last {} dictations, kept on this Mac only.",
            crate::core::history::MAX_ENTRIES
        ));
        ui.separator();
        let needle = search.to_lowercase();
        let now = now_ms();
        let mut shown = 0;
        for entry in history.iter().rev() {
            if !needle.is_empty() && !entry.text.to_lowercase().contains(&needle) {
                continue;
            }
            shown += 1;
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(&entry.text);
                ui.horizontal(|ui| {
                    ui.small(format!(
                        "{} · {} ms",
                        time_ago(now, entry.at_ms),
                        entry.duration_ms
                    ));
                    if ui.small_button("Copy").clicked() {
                        intents.push(SettingsIntent::CopyHistory(entry.text.clone()));
                    }
                });
            });
        }
        if shown == 0 {
            ui.label(if history.is_empty() {
                "No dictations yet."
            } else {
                "No matches."
            });
        }
    }

    fn permissions_page(ui: &mut egui::Ui, live: &LiveStatus, intents: &mut Vec<SettingsIntent>) {
        ui.heading("Permissions");
        ui.add_space(6.0);
        ui.label(egui::RichText::new("1. Microphone").strong());
        ui.label("Needed to hear you. Use Test record to check it works.");
        ui.horizontal(|ui| {
            if ui.button("Open Microphone settings…").clicked() {
                intents.push(SettingsIntent::OpenMicSettings);
            }
            if ui.button("Test record (1 s)").clicked() {
                intents.push(SettingsIntent::TestRecord);
            }
        });
        if let Some(status) = &live.test_status {
            ui.label(format!("Mic test: {status}"));
        }
        ui.add_space(12.0);
        let rows = [
            (
                "2. Input Monitoring",
                "Needed to hear your hotkey while you work in other apps.",
                live.input_monitoring_ok,
                "Open Input Monitoring settings…",
                SettingsIntent::OpenInputMonitoringSettings,
            ),
            (
                "3. Accessibility",
                "Needed to paste text into the app you're typing in.",
                live.accessibility_ok,
                "Open Accessibility settings…",
                SettingsIntent::OpenAccessibilitySettings,
            ),
        ];
        for (title, why, ok, button, intent) in rows {
            ui.label(egui::RichText::new(title).strong());
            ui.label(why);
            ui.horizontal(|ui| {
                if ok {
                    ui.colored_label(egui::Color32::from_rgb(60, 180, 90), "✔ granted");
                } else {
                    ui.colored_label(egui::Color32::from_rgb(230, 150, 0), "not granted");
                }
                if ui.button(button).clicked() {
                    intents.push(intent);
                }
            });
            ui.add_space(12.0);
        }
        ui.small(
            "After turning a switch on, quit Wiflow from the menu bar and open it again. \
             A rebuilt Wiflow.app counts as a new app, so grant these again after a rebuild.",
        );
    }

    fn hotkey_panel(
        ui: &mut egui::Ui,
        vm: &SettingsViewModel,
        recording_keys: &mut bool,
        record_msg: &mut Option<String>,
        intents: &mut Vec<SettingsIntent>,
    ) {
        ui.heading("Push-to-talk key");
        ui.horizontal(|ui| {
            for (preset, name) in [
                (HotkeyPreset::RightOption, "Right Option"),
                (HotkeyPreset::Fn, "Fn"),
                (HotkeyPreset::CtrlSpace, "Ctrl+Space"),
            ] {
                // Applies at once (same path as the tray menu), not on Save.
                if ui.radio(vm.config.hotkey_preset == preset, name).clicked()
                    && vm.config.hotkey_preset != preset
                {
                    intents.push(SettingsIntent::SwitchHotkey(preset));
                }
            }
        });
        if ui
            .button(if *recording_keys {
                "Cancel hotkey recording"
            } else {
                "Record new hotkey…"
            })
            .clicked()
        {
            *recording_keys = !*recording_keys;
            *record_msg = None;
        }
        if let Some(msg) = record_msg.as_deref() {
            ui.label(msg);
        }
    }

    fn audio_panel(ui: &mut egui::Ui, vm: &mut SettingsViewModel, mic_list: &[String]) {
        ui.heading("Audio");
        let current = vm
            .config
            .mic_name
            .clone()
            .unwrap_or_else(|| "(system default)".into());
        egui::ComboBox::from_label("Microphone")
            .selected_text(current.as_str())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut vm.config.mic_name, None, "(system default)");
                for dev in mic_list {
                    ui.selectable_value(&mut vm.config.mic_name, Some(dev.clone()), dev.as_str());
                }
            });
        ui.horizontal(|ui| {
            ui.label("Speech model:");
            ui.radio_value(
                &mut vm.config.model,
                ModelChoice::TinyEn,
                "tiny.en (fastest)",
            );
            ui.radio_value(&mut vm.config.model, ModelChoice::BaseEn, "base.en");
            ui.radio_value(
                &mut vm.config.model,
                ModelChoice::SmallEn,
                "small.en (best)",
            );
        });
    }

    fn provider_panel(ui: &mut egui::Ui, vm: &mut SettingsViewModel, ollama_models: &[String]) {
        ui.heading("Providers ($0 default: everything local)");
        ui.horizontal(|ui| {
            ui.label("Speech:");
            ui.radio_value(
                &mut vm.config.stt_provider,
                "local".to_string(),
                "local whisper",
            );
            ui.radio_value(
                &mut vm.config.stt_provider,
                "groq".to_string(),
                "Groq cloud (opt-in)",
            );
        });
        ui.horizontal(|ui| {
            ui.label("Language:");
            ui.text_edit_singleline(&mut vm.config.stt_language);
        });
        ui.checkbox(&mut vm.config.cleanup_enabled, "AI cleanup");
        ui.add_enabled_ui(vm.config.cleanup_enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Cleanup with:");
                for (value, name) in [
                    ("auto", "Auto"),
                    ("groq", "Groq"),
                    ("openrouter", "OpenRouter"),
                    ("ollama", "Ollama"),
                ] {
                    ui.radio_value(&mut vm.config.cleanup_provider, value.to_string(), name);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Ollama model:");
                if ollama_models.is_empty() {
                    ui.text_edit_singleline(&mut vm.config.cleanup_model);
                    ui.small("(Ollama not running)");
                } else {
                    egui::ComboBox::from_id_salt("ollama_model")
                        .selected_text(vm.config.cleanup_model.as_str())
                        .show_ui(ui, |ui| {
                            for m in ollama_models {
                                ui.selectable_value(
                                    &mut vm.config.cleanup_model,
                                    m.clone(),
                                    m.as_str(),
                                );
                            }
                        });
                }
            });
        });
        ui.checkbox(&mut vm.config.context_enabled, "Use focused-app context");
    }

    fn media_panel(
        ui: &mut egui::Ui,
        vm: &mut SettingsViewModel,
        intents: &mut Vec<SettingsIntent>,
    ) {
        ui.heading("General");
        ui.checkbox(
            &mut vm.config.duck_audio,
            "Lower other audio while dictating",
        );
        // Launch-at-login is OS state, not session state: applies at once
        // through the App (same path as the tray menu), never staged.
        let before = vm.config.launch_at_login;
        ui.checkbox(&mut vm.config.launch_at_login, "Launch at login");
        if vm.config.launch_at_login != before {
            intents.push(SettingsIntent::SetLaunchAtLogin(vm.config.launch_at_login));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_mods() -> ModifiersState {
        ModifiersState::empty()
    }

    fn ctrl() -> ModifiersState {
        let mut m = ModifiersState::empty();
        m.set(ModifiersState::CONTROL, true);
        m
    }

    #[test]
    fn recorder_rejects_single_keys() {
        // macOS can't register these paths as bare singles; the recorder
        // must explain, not silently accept.
        assert!(matches!(
            validate_hotkey(no_mods(), KeyCode::KeyA),
            Err(HotkeyError::SingleKey(KeyCode::KeyA))
        ));
        assert!(matches!(
            validate_hotkey(no_mods(), KeyCode::F13),
            Err(HotkeyError::SingleKey(_))
        ));
        assert!(matches!(
            validate_hotkey(ctrl(), KeyCode::KeyK),
            Err(HotkeyError::CustomUnsupported(_))
        ));
    }

    #[test]
    fn recorder_accepts_presets_and_rejects_bare_escape() {
        assert_eq!(
            validate_hotkey(no_mods(), KeyCode::Fn),
            Ok(HotkeyPreset::Fn)
        );
        assert_eq!(
            validate_hotkey(no_mods(), KeyCode::AltRight),
            Ok(HotkeyPreset::RightOption)
        );
        assert_eq!(
            validate_hotkey(ctrl(), KeyCode::Space),
            Ok(HotkeyPreset::CtrlSpace)
        );
        assert!(matches!(
            validate_hotkey(no_mods(), KeyCode::Escape),
            Err(HotkeyError::SingleKey(KeyCode::Escape))
        ));
    }

    #[test]
    fn save_then_reload_roundtrip() {
        let p = std::env::temp_dir().join("wiflow_settings_vm_test.json");
        let _ = std::fs::remove_file(&p);
        let mut vm = SettingsViewModel::load_from(&p);
        assert_eq!(vm.config, Config::default());
        vm.config.duck_audio = !vm.config.duck_audio;
        vm.config.mic_name = Some("Test Mic".into());
        vm.save_to(&p).unwrap();
        let back = SettingsViewModel::load_from(&p);
        assert_eq!(back.config.duck_audio, vm.config.duck_audio);
        assert_eq!(back.config.mic_name, Some("Test Mic".into()));
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn duck_toggle_persists() {
        let p = std::env::temp_dir().join("wiflow_settings_duck_test.json");
        let _ = std::fs::remove_file(&p);
        let mut vm = SettingsViewModel::load_from(&p);
        let before = vm.config.duck_audio;
        vm.config.duck_audio = !before;
        vm.save_to(&p).unwrap();
        assert_eq!(SettingsViewModel::load_from(&p).config.duck_audio, !before);
        std::fs::remove_file(&p).unwrap();
    }

    fn entry(text: &str, duration_ms: u64) -> HistoryEntry {
        HistoryEntry {
            text: text.into(),
            at_ms: 0,
            duration_ms,
            rtf: 0.1,
        }
    }

    #[test]
    fn stats_count_words_and_average() {
        assert_eq!(history_stats(&[]), (0, 0, 0));
        let h = [entry("hello there", 100), entry("one two three", 300)];
        assert_eq!(history_stats(&h), (2, 5, 200));
    }

    #[test]
    fn time_ago_buckets() {
        let now = 10 * 86_400_000;
        assert_eq!(time_ago(now, now - 5_000), "just now");
        assert_eq!(time_ago(now, now - 5 * 60_000), "5 min ago");
        assert_eq!(time_ago(now, now - 3 * 3_600_000), "3 h ago");
        assert_eq!(time_ago(now, now - 2 * 86_400_000), "2 d ago");
        assert_eq!(time_ago(now, now + 1_000), "just now", "clock skew");
    }

    #[test]
    fn test_record_formats_rms_result() {
        let s = format_test_record_result(0.123, 44100, 44100);
        assert!(s.contains("0.123"), "shows RMS, got: {s}");
        assert!(s.contains("44100"), "shows sample count, got: {s}");
        assert!(
            s.contains("1.0s") || s.contains("1s"),
            "shows duration, got: {s}"
        );
        let silent = format_test_record_result(0.0, 44100, 44100);
        assert!(
            silent.to_lowercase().contains("silent") || silent.contains("0.000"),
            "silence is recognizable, got: {silent}"
        );
    }
}
