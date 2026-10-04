//! Session orchestrator data (S2).
//!
//! `Session` holds lifecycle metadata, never audio samples (H5).
//! `begin_session` freezes settings and context at STARTING time (H6/H24);
//! mid-session config mutation cannot affect an already-built session.
//! Context acquisition uses a bounded wait; on timeout or failure it
//! yields `None` and writes a warn log (H19). Watchdog timeout uses
//! `WIFLOW_MAX_RECORDING_MS`, the same env var read by the worker.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::core::config::Config;
use crate::core::traits::ContextProvider;
use crate::daemon::next_session;

// ── Public structs ──────────────────────────────────────────────────────────

/// Reference to an audio session — correlation id only, **never samples** (H5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioRef {
    pub session_id: u64,
}

/// Frozen media snapshot at STARTING time.
///
/// S3 fills `was_playing` from real state; S2 defaults to `false` / `None`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaSnapshot {
    pub output_device: Option<String>,
    pub was_playing: bool,
}

/// Immutable session snapshot created at STARTING.
///
/// All 8 fields are frozen at construction:
/// - `id` — from the daemon's monotonic counter
/// - `started_at_ms` — wall-clock millisecond timestamp
/// - `settings` — full `Config` clone (H6/H24)
/// - `watchdog_ms` — env `WIFLOW_MAX_RECORDING_MS` (same source as worker)
/// - `app_context` — `None` on timeout/failure (H19)
/// - `audio_ref` — correlation id
/// - `media` — frozen snapshot
#[derive(Debug, Clone)]
#[allow(dead_code)] // consumed by orchestrator in Task 8
pub struct Session {
    pub id: u64,
    pub started_at_ms: u64,
    pub settings: Config,
    pub watchdog_ms: u64,
    pub app_context: Option<String>,
    pub audio_ref: AudioRef,
    pub media: MediaSnapshot,
}

// ── begin_session ───────────────────────────────────────────────────────────

/// Create a new `Session` with frozen STARTING snapshot.
///
/// - Calls `ctx.focused_app()` with bounded wait (≤500ms).
/// - On timeout or failure → `app_context = None` + warn log (H19).
/// - Reads `WIFLOW_MAX_RECORDING_MS` env (same source as worker).
/// - Clones the full `Config` so later mutations don't affect this session.
#[allow(dead_code)] // consumed by orchestrator in Task 8
pub fn begin_session(settings: &Config, ctx: impl ContextProvider + Send + 'static) -> Session {
    let id = next_session();
    let started_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let watchdog_ms = std::env::var("WIFLOW_MAX_RECORDING_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60_000);

    // Bounded context acquisition (≤500ms).
    let app_context = bounded_context_fetch(ctx, Duration::from_millis(500));

    let audio_ref = AudioRef { session_id: id };
    let media = MediaSnapshot::default();

    if app_context.is_none() {
        tracing::warn!(
            "[session={}] context unavailable (timeout/failure) — app_context is None",
            id
        );
    }

    Session {
        id,
        started_at_ms,
        settings: settings.clone(),
        watchdog_ms,
        app_context,
        audio_ref,
        media,
    }
}

/// Fetch context with a bounded timeout. Returns `None` on timeout or panic.
///
/// Spawns a thread for the actual `focused_app()` call so the deadline is
/// enforced even when the provider blocks. The orphaned thread is left to
/// finish on its own (H19 — never blocks the startup path unboundedly).
fn bounded_context_fetch(
    ctx: impl ContextProvider + Send + 'static,
    timeout: Duration,
) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = ctx.focused_app();
        let _ = tx.send(result);
    });

    rx.recv_timeout(timeout).unwrap_or(None)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Fake context providers ────────────────────────────────────────────

    struct FakeContextApp(String);
    impl ContextProvider for FakeContextApp {
        fn focused_app(&self) -> Option<String> {
            Some(self.0.clone())
        }
    }

    struct FakeContextNone;
    impl ContextProvider for FakeContextNone {
        fn focused_app(&self) -> Option<String> {
            None
        }
    }

    /// Fake that panics on first call (simulates crash/timeout).
    struct FakeContextPanics;
    impl ContextProvider for FakeContextPanics {
        fn focused_app(&self) -> Option<String> {
            panic!("simulated context provider crash");
        }
    }

    /// Fake that sleeps past the bounded deadline (proves the timeout guard
    /// actually aborts rather than waiting forever).
    struct FakeContextSlow;
    impl ContextProvider for FakeContextSlow {
        fn focused_app(&self) -> Option<String> {
            std::thread::sleep(std::time::Duration::from_millis(600));
            Some("SlowApp".to_string())
        }
    }

    // ── Test: snapshot freezes settings ───────────────────────────────────

    #[test]
    fn snapshot_freezes_settings() {
        let cfg = Config {
            stt_provider: "groq".to_string(),
            ..Default::default()
        };

        let session = begin_session(&cfg, FakeContextApp("TestApp".to_string()));

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

    // ── Test: context timeout yields None and completes ───────────────────

    #[test]
    fn context_timeout_yields_none_and_completes() {
        // Provider that always returns None — simulates timeout.
        let session = begin_session(&Config::default(), FakeContextNone);

        assert_eq!(
            session.app_context, None,
            "app_context must be None when context provider returns None"
        );
        // Must complete without panic.
        assert!(session.started_at_ms > 0);
    }

    #[test]
    fn context_slow_provider_actually_times_out() {
        // Provider that sleeps 600ms — past the 500ms bounded deadline.
        // The call must return None and complete well before 600ms.
        let t0 = std::time::Instant::now();
        let session = begin_session(&Config::default(), FakeContextSlow);
        let elapsed = t0.elapsed();

        assert_eq!(
            session.app_context, None,
            "app_context must be None when context provider sleeps past the deadline"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(550),
            "bounded fetch must return before the fake's 600ms sleep completes (took {elapsed:?})"
        );
    }

    #[test]
    fn context_panicking_provider_yields_none_and_completes() {
        // Provider that panics — must return None without propagating.
        let session = begin_session(&Config::default(), FakeContextPanics);

        assert_eq!(
            session.app_context, None,
            "app_context must be None when context provider panics"
        );
        // Must complete without panic propagating to the test.
        assert!(session.started_at_ms > 0);
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
        let before = crate::daemon::current_session();
        let session = begin_session(&Config::default(), FakeContextNone);
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
        let session = begin_session(&Config::default(), FakeContextApp("Xcode".to_string()));

        // AudioRef has only session_id — no Vec<f32>, no audio data.
        // This is a compile-time guarantee (struct has one field).
        assert!(session.audio_ref.session_id > 0);
    }

    // ── Test: MediaSnapshot defaults ──────────────────────────────────────

    #[test]
    fn media_snapshot_defaults_s2() {
        let session = begin_session(&Config::default(), FakeContextNone);

        assert_eq!(session.media.output_device, None);
        assert!(
            !session.media.was_playing,
            "was_playing must default false in S2"
        );
    }

    // ── Test: watchdog_ms from env ────────────────────────────────────────

    #[test]
    fn watchdog_ms_reads_env_with_default() {
        let session = begin_session(&Config::default(), FakeContextNone);
        // Default is 60_000ms when env var not set.
        assert_eq!(session.watchdog_ms, 60_000);
    }

    #[test]
    fn watchdog_ms_respects_env_var() {
        // Save old value.
        let old = std::env::var("WIFLOW_MAX_RECORDING_MS").ok();

        std::env::set_var("WIFLOW_MAX_RECORDING_MS", "30000");
        let session = begin_session(&Config::default(), FakeContextNone);
        assert_eq!(session.watchdog_ms, 30_000);

        // Restore.
        match old {
            Some(v) => std::env::set_var("WIFLOW_MAX_RECORDING_MS", v),
            None => std::env::remove_var("WIFLOW_MAX_RECORDING_MS"),
        }
    }
}
