use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// Variant names intentionally match the model filenames (ggml-tiny.en.bin etc.).
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ModelChoice {
    TinyEn,
    #[default]
    BaseEn,
    SmallEn,
}

// HotkeyPreset owned by daemon.rs (single-key holds rejected by macOS live);
// re-exported here so config serializes the same type the daemon registers.
pub use crate::daemon::HotkeyPreset;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey_preset: HotkeyPreset,
    #[serde(default)]
    pub mic_name: Option<String>,
    #[serde(default)]
    pub model: ModelChoice,
    #[serde(default)]
    pub launch_at_login: bool,
    /// LLM cleanup (provider chain: Groq → OpenRouter → Ollama local): on by
    /// default, skips instantly when nothing is configured/reachable so the
    /// deterministic output stands.
    #[serde(default = "default_true")]
    pub cleanup_enabled: bool,
    /// Ollama model for the local fallback.
    #[serde(default = "default_ollama_model")]
    pub cleanup_model: String,
    #[serde(default = "default_groq_model")]
    pub cleanup_groq_model: String,
    #[serde(default = "default_openrouter_model")]
    pub cleanup_openrouter_model: String,
}

fn default_true() -> bool {
    true
}
fn default_ollama_model() -> String {
    "llama3.2:1b".into()
}
fn default_groq_model() -> String {
    "llama-3.3-70b-versatile".into()
}
fn default_openrouter_model() -> String {
    "meta-llama/llama-3.3-70b:free".into()
}

// Manual Default (not derive): cleanup_enabled must default ON (user intent),
// matching the serde missing-field default.
impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey_preset: HotkeyPreset::default(),
            mic_name: None,
            model: ModelChoice::default(),
            launch_at_login: false,
            cleanup_enabled: true,
            cleanup_model: "llama3.2:1b".into(),
            cleanup_groq_model: "llama-3.3-70b-versatile".into(),
            cleanup_openrouter_model: "meta-llama/llama-3.3-70b:free".into(),
        }
    }
}

pub fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/wiflow/config.json")
}

/// Whisper initial-prompt vocabulary file: plain text, one line (artist
/// names, jargon the model mishears). Plain .txt instead of a JSON config
/// field so hand-editing can never corrupt the real config (corrupt config
/// falls back to defaults and loses hotkey/mic/model settings).
pub fn prompt_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/wiflow/prompt.txt")
}

pub fn load_config_from(path: &Path) -> Config {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn load_config() -> Config {
    load_config_from(&config_path())
}

pub fn save_config_to(path: &Path, cfg: &Config) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir config: {e:?}"))?;
    }
    atomic_write_json(path, cfg)
}

pub fn save_config(cfg: &Config) -> Result<(), String> {
    save_config_to(&config_path(), cfg)
}

/// Crash-safe write for small JSON docs: tmp + rename (never partial).
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let json = serde_json::to_string_pretty(value).map_err(|e| format!("encode: {e:?}"))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write tmp: {e:?}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e:?}"))
}

pub const LAUNCH_AGENT_LABEL: &str = "com.wiflow.dictation";

pub fn launch_agent_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(format!("Library/LaunchAgents/{LAUNCH_AGENT_LABEL}.plist"))
}

/// Enable/disable launch-at-login. Applies immediately via launchctl when asked.
pub fn set_launch_at_login(enable: bool, exe_path: &Path, apply_now: bool) -> Result<(), String> {
    let path = launch_agent_path();
    if !enable {
        let _ = std::process::Command::new("launchctl")
            .args([
                "bootout",
                &format!("gui/{}", current_uid()),
                path.to_string_lossy().as_ref(),
            ])
            .status();
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir agents: {e:?}"))?;
    }
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>{LAUNCH_AGENT_LABEL}</string>\n\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{}</string>\n\t\t<string>--app</string>\n\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n</dict>\n</plist>\n",
        exe_path.display()
    );
    atomic_write_raw(&path, &plist)?;
    if apply_now {
        let _ = std::process::Command::new("launchctl")
            .args([
                "bootstrap",
                &format!("gui/{}", current_uid()),
                path.to_string_lossy().as_ref(),
            ])
            .status();
    }
    Ok(())
}

fn atomic_write_raw(path: &Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("write tmp: {e:?}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e:?}"))
}

/// uid without a libc dep: `id -u`, fallback 501.
fn current_uid() -> String {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".into())
}

pub mod permissions {
    /// System Settings deep links (macOS 13+). Fall back to the root pane when they fail.
    pub const MIC_PRIVACY: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";
    pub const ACCESSIBILITY_PRIVACY: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
    // Space for a future Input Monitoring menu row (brief reserves the URL).
    #[allow(dead_code)]
    pub const INPUT_MONITORING_PRIVACY: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";

    pub fn open_mic_settings() {
        if open::that(MIC_PRIVACY).is_err() {
            let _ = open::that("x-apple.systempreferences:com.apple.preference.security");
        }
    }
    pub fn open_accessibility_settings() {
        if open::that(ACCESSIBILITY_PRIVACY).is_err() {
            let _ = open::that("x-apple.systempreferences:com.apple.preference.security");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            hotkey_preset: HotkeyPreset::Fn,
            mic_name: Some("Test Mic".into()),
            model: ModelChoice::SmallEn,
            launch_at_login: true,
            cleanup_enabled: true,
            cleanup_model: "llama3.2:1b".into(),
            cleanup_groq_model: "llama-3.3-70b-versatile".into(),
            cleanup_openrouter_model: "meta-llama/llama-3.3-70b:free".into(),
        }
    }

    #[test]
    fn test_config_roundtrip() {
        let p = std::env::temp_dir().join("wiflow_cfg_test.json");
        let _ = std::fs::remove_file(&p);
        save_config_to(&p, &sample()).unwrap();
        assert_eq!(load_config_from(&p), sample());
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_config_missing_is_default() {
        let p = std::env::temp_dir().join("wiflow_cfg_missing_xyz.json");
        let _ = std::fs::remove_file(&p);
        assert_eq!(load_config_from(&p), Config::default());
    }

    #[test]
    fn test_config_corrupt_is_default() {
        let p = std::env::temp_dir().join("wiflow_cfg_corrupt_xyz.json");
        std::fs::write(&p, "{nope").unwrap();
        assert_eq!(load_config_from(&p), Config::default());
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_default_preset_is_ctrl_space() {
        assert_eq!(Config::default().hotkey_preset, HotkeyPreset::CtrlSpace);
    }

    #[test]
    fn test_atomic_write_leaves_no_partial() {
        let p = std::env::temp_dir().join("wiflow_cfg_atomic_xyz.json");
        let tmp = p.with_extension("tmp");
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&tmp);
        save_config_to(&p, &sample()).unwrap();
        // Winner file parses to the same value; no tmp residue left behind.
        assert_eq!(load_config_from(&p), sample());
        assert!(!tmp.exists(), "tmp residue must not remain");
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_launch_agent_path_name() {
        assert_eq!(
            launch_agent_path().file_name().unwrap().to_string_lossy(),
            format!("{LAUNCH_AGENT_LABEL}.plist")
        );
    }
}
