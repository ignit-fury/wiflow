use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub text: String,
    pub at_ms: u64,
    pub duration_ms: u64,
    pub rtf: f64,
}

pub const MAX_ENTRIES: usize = 50;
const HISTORY_NAME: &str = "history.json";

pub fn history_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(format!("Library/Application Support/wiflow/{HISTORY_NAME}"))
}

/// Missing or corrupt file → empty vec (never Err — history must not break dictation).
pub fn load_history_from(path: &Path) -> Vec<HistoryEntry> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn push_history_to(path: &Path, entry: HistoryEntry) -> Result<(), String> {
    let mut all = load_history_from(path);
    all.push(entry);
    if all.len() > MAX_ENTRIES {
        all.drain(..all.len() - MAX_ENTRIES);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir history: {e:?}"))?;
    }
    let json = serde_json::to_string_pretty(&all).map_err(|e| format!("encode history: {e:?}"))?;
    std::fs::write(path, json).map_err(|e| format!("write history: {e:?}"))
}

/// Read API for future UI; kept narrow — clippy demands without a bin caller.
#[allow(dead_code)]
pub fn load_history() -> Vec<HistoryEntry> {
    load_history_from(&history_path())
}

pub fn push_history(entry: HistoryEntry) -> Result<(), String> {
    push_history_to(&history_path(), entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(text: &str) -> HistoryEntry {
        HistoryEntry {
            text: text.into(),
            at_ms: 1_700_000_000_000,
            duration_ms: 1500,
            rtf: 0.1,
        }
    }

    #[test]
    fn test_roundtrip_single() {
        let p = std::env::temp_dir().join("wiflow_hist_test_1.json");
        let _ = std::fs::remove_file(&p);
        push_history_to(&p, sample_entry("hello")).unwrap();
        let got = load_history_from(&p);
        assert_eq!(got, vec![sample_entry("hello")]);
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_missing_file_is_empty() {
        let p = std::env::temp_dir().join("wiflow_hist_missing_xyz.json");
        let _ = std::fs::remove_file(&p);
        assert!(load_history_from(&p).is_empty());
    }

    #[test]
    fn test_corrupt_file_is_empty() {
        let p = std::env::temp_dir().join("wiflow_hist_corrupt_xyz.json");
        std::fs::write(&p, "{not json").unwrap();
        assert!(load_history_from(&p).is_empty());
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_truncates_to_50() {
        let p = std::env::temp_dir().join("wiflow_hist_trunc_xyz.json");
        let _ = std::fs::remove_file(&p);
        for i in 0..55 {
            push_history_to(&p, sample_entry(&format!("e{i}"))).unwrap();
        }
        let got = load_history_from(&p);
        assert_eq!(got.len(), MAX_ENTRIES);
        assert_eq!(got.last().unwrap().text, "e54");
        assert_eq!(got.first().unwrap().text, "e5");
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_history_path_name() {
        assert_eq!(history_path().file_name().unwrap(), "history.json");
    }
}
