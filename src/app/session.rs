//! Session orchestrator data (S2).
//!
//! `Session` holds lifecycle metadata, never audio samples (H5).
//! `begin_session` freezes settings at STARTING time (H6/H24);
//! mid-session config mutation cannot affect an already-built session.
//! It never queries the focused app: that osascript call blocked the main
//! thread for up to 500 ms before the mic opened, and nothing read the
//! result (the cleanup step reads the app itself). Watchdog timeout uses
//! `WIFLOW_MAX_RECORDING_MS`, the same env var read by the worker.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::config::Config;
use crate::daemon::next_session;

// ── Public structs ──────────────────────────────────────────────────────────

/// Reference to an audio session — correlation id only, **never samples** (H5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioRef {
    pub session_id: u64,
}

/// Frozen media snapshot at STARTING time.
///
/// Filled by the orchestrator from the controller's pre-duck probe
/// (device + playing as found, before this session ducks).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaSnapshot {
    pub output_device: Option<String>,
    pub was_playing: bool,
}

/// Immutable session snapshot created at STARTING.
///
/// All fields are frozen at construction:
/// - `id` — from the daemon's monotonic counter
/// - `started_at_ms` — wall-clock millisecond timestamp
/// - `settings` — full `Config` clone (H6/H24)
/// - `watchdog_ms` — env `WIFLOW_MAX_RECORDING_MS` (same source as worker)
/// - `audio_ref` — correlation id
/// - `media` — frozen snapshot
#[derive(Debug, Clone)]
#[allow(dead_code)] // consumed by orchestrator in Task 8
pub struct Session {
    pub id: u64,
    pub started_at_ms: u64,
    pub settings: Config,
    pub watchdog_ms: u64,
    pub audio_ref: AudioRef,
    pub media: MediaSnapshot,
}

// ── begin_session ───────────────────────────────────────────────────────────

/// Create a new `Session` with frozen STARTING snapshot.
///
/// - Reads `WIFLOW_MAX_RECORDING_MS` env (same source as worker).
/// - Clones the full `Config` so later mutations don't affect this session.
#[allow(dead_code)] // consumed by orchestrator in Task 8
pub fn begin_session(settings: &Config) -> Session {
    let id = next_session();
    let started_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let watchdog_ms = std::env::var("WIFLOW_MAX_RECORDING_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60_000);

    let audio_ref = AudioRef { session_id: id };
    let media = MediaSnapshot::default();

    Session {
        id,
        started_at_ms,
        settings: settings.clone(),
        watchdog_ms,
        audio_ref,
        media,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes access to `WIFLOW_MAX_RECORDING_MS` across parallel tests.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // ── Test: snapshot freezes settings ───────────────────────────────────

    #[test]
    fn snapshot_freezes_settings() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = Config {
            stt_provider: "groq".to_string(),
            ..Default::default()
        };

        let session = begin_session(&cfg);

        // Session must have a full clone, not a reference.
        assert_eq!(session.settings.stt_provider, "groq");

        // Mutate a clone of the config — session must NOT see the change.
        let mut mutated = cfg.clone();
        mutated.stt_provider = "local".to_string();

        assert_eq!(
            session.settings.stt_provider, "groq",
            "Session settings must be frozen at STARTING — mutation must not affect it"
        );
    }

    // ── Test: session IDs are monotonic ───────────────────────────────────

    #[test]
    fn session_ids_monotonic() {
        let id_a = next_session();
        let id_b = next_session();
        let id_c = next_session();

        assert!(
            id_a < id_b && id_b < id_c,
            "session IDs must be strictly monotonically increasing: {id_a}, {id_b}, {id_c}"
        );
    }

    #[test]
    fn session_uses_next_session_for_id() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = crate::daemon::current_session();
        let session = begin_session(&Config::default());
        let after = crate::daemon::current_session();

        // Session id must be strictly after what we observed before
        // creation, and the daemon counter must have advanced.
        assert!(
            session.id > before,
            "session id ({}) must be > counter before ({})",
            session.id,
            before
        );
        assert!(
            after >= session.id,
            "counter after ({}) must be >= session id ({})",
            after,
            session.id
        );
    }

    // ── Test: AudioRef holds no samples ───────────────────────────────────

    #[test]
    fn audio_ref_is_correlation_only() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let session = begin_session(&Config::default());

        // AudioRef has only session_id — no Vec<f32>, no audio data.
        // This is a compile-time guarantee (struct has one field).
        assert!(session.audio_ref.session_id > 0);
    }

    // ── Test: MediaSnapshot defaults ──────────────────────────────────────

    #[test]
    fn media_snapshot_defaults_s2() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let session = begin_session(&Config::default());

        assert_eq!(session.media.output_device, None);
        assert!(
            !session.media.was_playing,
            "was_playing must default false in S2"
        );
    }

    // ── Test: watchdog_ms from env ────────────────────────────────────────

    #[test]
    fn watchdog_ms_reads_env_with_default() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let session = begin_session(&Config::default());
        // Default is 60_000ms when env var not set.
        assert_eq!(session.watchdog_ms, 60_000);
    }

    #[test]
    fn watchdog_ms_respects_env_var() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Save old value.
        let old = std::env::var("WIFLOW_MAX_RECORDING_MS").ok();

        std::env::set_var("WIFLOW_MAX_RECORDING_MS", "30000");
        let session = begin_session(&Config::default());
        assert_eq!(session.watchdog_ms, 30_000);

        // Restore.
        match old {
            Some(v) => std::env::set_var("WIFLOW_MAX_RECORDING_MS", v),
            None => std::env::remove_var("WIFLOW_MAX_RECORDING_MS"),
        }
    }
}
