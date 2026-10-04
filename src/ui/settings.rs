//! Settings window: view-model, hotkey validation, and the winit/egui window.
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
    /// Reserved by wiflow itself (Esc cancels).
    Reserved(&'static str),
    /// Registrable in theory, but wiflow only drives its three presets
    /// (the hotkey bridge forwards known preset ids, no restart needed).
    CustomUnsupported(String),
}

/// Map a recorded (modifiers, key) press to a wiflow preset, or explain why
/// it cannot be used. Bare Fn / Right-Option map to their tap presets;
/// Ctrl+Space maps to the combo preset.
pub fn validate_hotkey(mods: ModifiersState, code: KeyCode) -> Result<HotkeyPreset, HotkeyError> {
    use winit::keyboard::KeyCode as K;
    // Esc is wiflow's own cancel key — never a PTT binding.
    if code == K::Escape {
        return Err(HotkeyError::Reserved("Esc cancels dictation"));
    }
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
}

// ── Window (winit + egui, lives on the existing event loop) ─────────────────

use super::gl::{GlWindow, GlWindowOpts};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

use crate::core::config::ModelChoice;
use crate::core::history::HistoryEntry;

/// Live settings window: GL pieces (spike-proven pattern) + egui state +
/// staged view-model. Created on demand from the tray menu on the EXISTING
/// event loop (H25: one loop per process); closed via the window X button.
pub struct SettingsWindow {
    gl: GlWindow,
    vm: SettingsViewModel,
    mic_list: Vec<String>,
    history: Vec<HistoryEntry>,
    recording_keys: bool,
    record_mods: ModifiersState,
    record_msg: Option<String>,
    save_msg: Option<String>,
    closed: bool,
}

impl SettingsWindow {
    /// Build + show the window. `history` and `mic_list` are snapshots taken
    /// by the App at open time (H20: the window renders passed-in state).
    pub fn open(
        event_loop: &ActiveEventLoop,
        history: Vec<HistoryEntry>,
        mic_list: Vec<String>,
    ) -> Result<Self, String> {
        let gl = GlWindow::open(
            event_loop,
            &GlWindowOpts {
                title: "wiflow Settings",
                width: 580.0,
                height: 660.0,
                decorations: true,
                always_on_top: false,
                position: None,
            },
        )?;
        Ok(Self {
            gl,
            vm: SettingsViewModel::load(),
            mic_list,
            history,
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

    /// Closed via the window X button — the App drops the window then.
    pub fn close_requested(&self) -> bool {
        self.closed
    }

    /// Clear the locally shown history after a ClearHistory intent executes.
    pub fn clear_history_view(&mut self) {
        self.history.clear();
    }

    /// Revert the staged launch-at-login flag (OS refused the change).
    pub fn set_launch_at_login(&mut self, enable: bool) {
        self.vm.config.launch_at_login = enable;
    }

    /// Feed a winit event to egui; capture recorder keys; collect intents.
    pub fn handle_event(&mut self, event: &WindowEvent) -> Vec<SettingsIntent> {
        let mut intents = Vec::new();
        if *event == WindowEvent::CloseRequested {
            self.closed = true;
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
                        use winit::keyboard::KeyCode as K;
                        if code == K::Escape {
                            self.recording_keys = false;
                            self.record_msg = Some("recording cancelled".into());
                        } else {
                            match validate_hotkey(self.record_mods, code) {
                                Ok(preset) => {
                                    self.recording_keys = false;
                                    self.record_msg =
                                        Some(format!("captured {preset:?} — switching"));
                                    intents.push(SettingsIntent::SwitchHotkey(preset));
                                }
                                Err(e) => {
                                    self.record_msg = Some(format!("not usable: {e:?}"));
                                }
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
    /// `test_status`: App-owned mic-probe result line, if any.
    pub fn paint(&mut self, test_status: Option<&str>) -> Vec<SettingsIntent> {
        let mut intents = Vec::new();
        // Split borrows up front: the frame closure captures locals, never
        // `self`, while `gl.paint` holds `&mut self.gl`.
        let vm = &mut self.vm;
        let recording_keys = &mut self.recording_keys;
        let record_msg = &mut self.record_msg;
        let save_msg = &mut self.save_msg;
        let mic_list = &self.mic_list;
        let history = &mut self.history;
        self.gl.paint(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                // The full panel stack exceeds the window height: scroll.
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading("wiflow Settings");
                    ui.label(
                    "Changes save to disk and apply to the next hold — never the active session.",
                );
                    ui.separator();
                    Self::hotkey_panel(ui, vm, recording_keys, record_msg);
                    ui.separator();
                    Self::audio_panel(ui, vm, mic_list);
                    ui.separator();
                    Self::provider_panel(ui, vm);
                    ui.separator();
                    Self::media_panel(ui, vm, &mut intents);
                    ui.separator();
                    Self::history_panel(ui, history, &mut intents);
                    ui.separator();
                    Self::permissions_panel(ui, test_status, &mut intents);
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            match vm.save() {
                                Ok(()) => {
                                    *save_msg = Some("saved — applies to the next hold".into());
                                }
                                Err(e) => {
                                    *save_msg = Some(format!("save failed: {e}"));
                                }
                            }
                        }
                        if ui.button("Revert").clicked() {
                            vm.revert();
                            *save_msg = Some("reverted to disk".into());
                        }
                        if let Some(msg) = &*save_msg {
                            ui.label(msg.as_str());
                        }
                    });
                });
            });
        }); // end paint closure
        self.gl.request_redraw();
        intents
    }
}

// ── Panels (thin egui views over the staged view-model) ─────────────────────

impl SettingsWindow {
    fn hotkey_panel(
        ui: &mut egui::Ui,
        vm: &SettingsViewModel,
        recording_keys: &mut bool,
        record_msg: &mut Option<String>,
    ) {
        ui.heading("Push-to-talk key");
        ui.label(format!("Current: {:?}", vm.config.hotkey_preset));
        if ui
            .button(if *recording_keys {
                "Press keys… (Esc aborts)"
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
        ui.small("Bare Fn / Right-Option ride the tap; combos need Ctrl/⌘. Single keys can't register on macOS.");
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
            ui.label("Model:");
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

    fn provider_panel(ui: &mut egui::Ui, vm: &mut SettingsViewModel) {
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
            ui.label("STT language:");
            ui.text_edit_singleline(&mut vm.config.stt_language);
        });
        ui.checkbox(&mut vm.config.cleanup_enabled, "AI cleanup chain");
        ui.horizontal(|ui| {
            ui.label("Cleanup provider:");
            ui.text_edit_singleline(&mut vm.config.cleanup_provider);
        });
        ui.checkbox(&mut vm.config.context_enabled, "Focused-app context");
    }

    fn media_panel(
        ui: &mut egui::Ui,
        vm: &mut SettingsViewModel,
        intents: &mut Vec<SettingsIntent>,
    ) {
        ui.heading("Media");
        ui.checkbox(
            &mut vm.config.duck_audio,
            "Duck competing audio while dictating (applies on Save)",
        );
        // Launch-at-login is OS state, not session state: applies at once
        // through the App (same path as the tray menu), never staged.
        let before = vm.config.launch_at_login;
        ui.checkbox(&mut vm.config.launch_at_login, "Launch at login");
        if vm.config.launch_at_login != before {
            intents.push(SettingsIntent::SetLaunchAtLogin(vm.config.launch_at_login));
        }
    }

    fn history_panel(
        ui: &mut egui::Ui,
        history: &mut Vec<HistoryEntry>,
        intents: &mut Vec<SettingsIntent>,
    ) {
        ui.heading("History (last 50)");
        if ui.button("Clear history (deletes the file)").clicked() {
            intents.push(SettingsIntent::ClearHistory);
            history.clear();
        }
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .show(ui, |ui| {
                for entry in history.iter().rev().take(50) {
                    ui.horizontal(|ui| {
                        let short = if entry.text.chars().count() > 60 {
                            format!("{}…", entry.text.chars().take(60).collect::<String>())
                        } else {
                            entry.text.clone()
                        };
                        ui.label(format!("{} ({}ms)", short, entry.duration_ms));
                        if ui.small_button("copy").clicked() {
                            intents.push(SettingsIntent::CopyHistory(entry.text.clone()));
                        }
                    });
                }
            });
    }

    fn permissions_panel(
        ui: &mut egui::Ui,
        test_status: Option<&str>,
        intents: &mut Vec<SettingsIntent>,
    ) {
        ui.heading("Permissions & mic test");
        ui.horizontal(|ui| {
            if ui.button("Open Microphone settings…").clicked() {
                intents.push(SettingsIntent::OpenMicSettings);
            }
            if ui.button("Open Accessibility settings…").clicked() {
                intents.push(SettingsIntent::OpenAccessibilitySettings);
            }
            if ui.button("Test record (1 s)").clicked() {
                intents.push(SettingsIntent::TestRecord);
            }
        });
        if let Some(status) = test_status {
            ui.label(format!("mic probe: {status}"));
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
    fn recorder_accepts_presets_and_reserves_esc() {
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
            Err(HotkeyError::Reserved(_))
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
