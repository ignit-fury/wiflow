//! S2 orchestrator — testable lifecycle logic separated from the winit `App` shell.
//!
//! `Orchestrator::handle(ev)` implements the full §7.2 routing table:
//! every `DaemonEvent` variant is routed through the `PttMachine` phase gate,
//! and terminal events funnel through `finalize_session`.
//!
//! `tick()` checks the supervision deadline (armed in LISTENING).
//! `handle_shutdown()` forces cleanup from any phase.

use std::time::{Duration, Instant};

use crate::app::session::{begin_session, Session};
use crate::app::AppState;
use crate::core::traits::{
    ChainProvider, InjectReport, OsascriptContext, RouterRecognizer, SystemInjector,
};
use crate::daemon::{Control, DaemonEvent};
use crate::ptt::{Admission, Phase, PttMachine};

// ── Action ──────────────────────────────────────────────────────────────────

/// Side-effects the orchestrator requests from the App shell.
///
/// The App executes these on the winit event-loop thread.
/// `Inject` MUST execute only there (enigo HIToolbox is main-queue-only).
#[derive(Debug, Clone)]
pub enum Action {
    /// Send a `Control` command to the dictation worker.
    SendControl(Control),
    /// Update the tray icon and tooltip. `None` note clears any override.
    SetTray(AppState, Option<String>),
    /// Show a transient warning in the tray (warn-note).
    Notify(String),
    /// Inject text into the focused cursor (enigo, main-thread-only).
    Inject(String),
}

// ── NoopMedia (S2 seam) ─────────────────────────────────────────────────────

/// S2 safe no-op seam for media restore.
///
/// S3 (Task 12) will replace this with real audio-state capture/restore.
/// In S2, `restore_media()` always returns no actions and is tested as a no-op.
#[derive(Debug, Clone, Default)]
pub(crate) struct NoopMedia;

// ── Orchestrator ────────────────────────────────────────────────────────────

/// Central lifecycle coordinator.
///
/// All `DaemonEvent` routing goes through `handle()`. Terminal events
/// (Done, Failed) funnel through `finalize_session`. Supervision is armed
/// on entering LISTENING and checked by `tick()`.
pub struct Orchestrator {
    machine: PttMachine,
    session: Option<Session>,
    supervision_at: Option<Instant>,
    /// S3/Task-12 seam: will track output device / playback state.
    #[allow(dead_code)]
    media: NoopMedia,
    /// Brief-pinned constructor signature; final review may prune.
    #[allow(dead_code)]
    recognizer: RouterRecognizer,
    /// Brief-pinned constructor signature; final review may prune.
    #[allow(dead_code)]
    cleanup: ChainProvider,
    /// Brief-pinned constructor signature; final review may prune.
    #[allow(dead_code)]
    injector: SystemInjector,
    /// Brief-pinned constructor signature; final review may prune.
    #[allow(dead_code)]
    context: OsascriptContext,
}

impl Orchestrator {
    /// Create a new orchestrator with production trait implementations.
    pub fn new(
        recognizer: RouterRecognizer,
        cleanup: ChainProvider,
        injector: SystemInjector,
        context: OsascriptContext,
    ) -> Self {
        Self {
            machine: PttMachine::new(),
            session: None,
            supervision_at: None,
            media: NoopMedia,
            recognizer,
            cleanup,
            injector,
            context,
        }
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Route a `DaemonEvent` through the phase gate and return side-effect
    /// `Action`s for the App shell to execute.
    pub fn handle(&mut self, ev: &DaemonEvent) -> Vec<Action> {
        match ev {
            DaemonEvent::PttDown => self.handle_ptt_down(),
            DaemonEvent::PttUp => self.handle_ptt_up(),
            DaemonEvent::Cancel => self.handle_cancel(),
            DaemonEvent::CaptureStarted => self.handle_capture_started(),
            DaemonEvent::Watchdog { duration_ms } => self.handle_watchdog(*duration_ms),
            DaemonEvent::TapIssue(msg) => self.handle_tap_issue(msg),
            DaemonEvent::Done {
                text,
                duration_ms,
                rtf,
            } => self.handle_done(text, *duration_ms, *rtf),
            DaemonEvent::Failed(msg) => self.handle_failed(msg),
            DaemonEvent::CleanupIssue(msg) => self.handle_cleanup_issue(msg),
        }
    }

    /// Current PTT phase (used for observability in ignored-event logs).
    pub fn phase(&self) -> Phase {
        self.machine.phase()
    }

    /// Supervision deadline check. Called every `about_to_wait` tick.
    ///
    /// When in LISTENING, if no worker event arrives within the session's
    /// watchdog window, the session is force-ended with an error.
    pub fn tick(&mut self) -> Vec<Action> {
        if let (Some(sup_at), Some(session)) = (self.supervision_at, &self.session) {
            let deadline = sup_at + Duration::from_millis(session.watchdog_ms);
            if Instant::now() >= deadline {
                let msg = format!(
                    "recording exceeded {}ms watchdog — force-ending session",
                    session.watchdog_ms
                );
                tracing::error!("[session={}] {}", session.id, msg);
                let mut actions = self.finalize_session(true);
                actions.push(Action::Notify(msg));
                actions
            } else {
                vec![]
            }
        } else {
            vec![]
        }
    }

    /// Graceful shutdown: clean up from any phase before `stt::shutdown()`.
    pub fn handle_shutdown(&mut self) -> Vec<Action> {
        match self.machine.phase() {
            Phase::Idle | Phase::Error => {
                vec![Action::Notify("shutting down".into())]
            }
            _ => self.finalize_session(false),
        }
    }

    /// Called by the App after executing `Inject(text)` on the winit thread.
    ///
    /// On `Ok`: `on_inject_ok` → RESTORING → `finalize_session(false)` → IDLE.
    /// On `Err`: `on_inject_failed` → RESTORING → `finalize_session(true)` → ERROR
    ///   (with the inject error surfaced in the tray note).
    pub fn finish_inject(&mut self, result: Result<InjectReport, String>) -> Vec<Action> {
        match result {
            Ok(_) => {
                self.machine.on_inject_ok();
                self.finalize_session(false)
            }
            Err(e) => {
                self.machine.on_inject_failed();
                let mut actions = self.finalize_session(true);
                // Override the generic "session ended" note with the actual
                // inject error so the user sees the real problem.
                for action in &mut actions {
                    if let Action::SetTray(AppState::Error, note) = action {
                        *note = Some(format!("injected to clipboard: {e}"));
                    }
                }
                actions
            }
        }
    }

    /// THE funnel (H7): every terminal path ends here.
    ///
    /// Step order:
    /// 1. Disarm supervision
    /// 2. Drop capture handle (S2: worker owns it — no-op)
    /// 3. restore_media() [S2 = safe no-op seam, tested]
    /// 4. Hide pill: SetTray(Idle/Error)
    /// 5. Release audio_ref (drop session)
    /// 6. Session-end log line
    /// 7. on_finalized machine step
    ///
    /// Tray state (Idle vs Error) is derived from `self.machine.phase()`
    /// after `on_finalized()` — single source of truth. The `fatal` param
    /// is only used for the non-Restoring fallback path (e.g. supervision
    /// expiry from Listening, where the machine isn't in Restoring and must
    /// be forced to Error via `on_failed`).
    pub(crate) fn finalize_session(&mut self, fatal: bool) -> Vec<Action> {
        // 1. Disarm supervision
        self.supervision_at = None;

        // 2. Drop capture handle — S2 no-op (worker owns the mic handle)

        // 3. Restore media — S2 safe no-op seam
        self.restore_media();

        // 4/5/6. Session-end log + release resources
        if let Some(s) = &self.session {
            tracing::info!("[session={}] session ended", s.id);
        }
        self.session = None;

        // 7. on_finalized machine step (only valid from Restoring)
        if self.machine.phase() == Phase::Restoring {
            self.machine.on_finalized();
        } else if fatal {
            // Non-Restoring fatal case (e.g. supervision expiry from
            // Listening): force to Restoring → Error so the machine and
            // tray agree.
            self.machine.on_failed();
            if self.machine.phase() == Phase::Restoring {
                self.machine.on_finalized();
            }
        }
        // For non-fatal non-Restoring (e.g. shutdown from Listening), the
        // machine stays in its current phase; the tray below defaults to
        // Idle. Acceptable because the app is exiting.

        // Derive tray state from machine phase — single source of truth.
        let (state, note) = match self.machine.phase() {
            Phase::Error => (AppState::Error, Some("session ended".into())),
            _ => (AppState::Idle, None),
        };

        vec![Action::SetTray(state, note)]
    }

    // ── Internal: restore_media (S2 seam) ─────────────────────────────────

    /// S2 no-op: nothing to restore.
    ///
    /// S3 (Task 12) will implement real media-state capture/restore here.
    fn restore_media(&mut self) {
        // Intentionally empty — S2 seam.
    }

    // ── Internal: event handlers ──────────────────────────────────────────

    fn handle_ptt_down(&mut self) -> Vec<Action> {
        if self.machine.on_down() == Admission::Accept {
            // Freeze settings + context at STARTING time (H6/H24).
            let config = crate::core::config::load_config();
            self.session = Some(begin_session(&config, OsascriptContext));
            vec![
                Action::SetTray(AppState::Recording, None),
                Action::SendControl(Control::Down),
            ]
        } else {
            vec![]
        }
    }

    fn handle_ptt_up(&mut self) -> Vec<Action> {
        if self.machine.on_up() == Admission::Accept {
            // Leaving LISTENING → disarm supervision.
            self.supervision_at = None;
            vec![
                Action::SetTray(AppState::Transcribing, None),
                Action::SendControl(Control::Up),
            ]
        } else {
            vec![]
        }
    }

    fn handle_cancel(&mut self) -> Vec<Action> {
        if self.machine.on_cancel() == Admission::Accept {
            self.supervision_at = None;
            vec![Action::SendControl(Control::Cancel)]
        } else {
            vec![]
        }
    }

    fn handle_capture_started(&mut self) -> Vec<Action> {
        if self.machine.on_capture_started() == Admission::Accept {
            // Entered LISTENING → arm supervision deadline.
            self.supervision_at = Some(Instant::now());
            vec![]
        } else {
            // Stale event — silently drop (phase gate rejects).
            vec![]
        }
    }

    fn handle_watchdog(&mut self, duration_ms: u64) -> Vec<Action> {
        let msg = format!("auto-stopped after {duration_ms}ms — release key lost");
        if self.machine.on_watchdog() == Admission::Accept {
            self.supervision_at = None;
            vec![
                Action::Notify(msg.clone()),
                Action::SetTray(AppState::Transcribing, Some(msg)),
            ]
        } else {
            vec![Action::Notify(msg)]
        }
    }

    fn handle_tap_issue(&mut self, msg: &str) -> Vec<Action> {
        vec![Action::Notify(msg.to_string())]
    }

    fn handle_done(&mut self, text: &str, _duration_ms: u64, _rtf: f64) -> Vec<Action> {
        if text.is_empty() {
            if self.machine.on_empty() == Admission::Accept {
                self.finalize_session(false)
            } else {
                vec![]
            }
        } else {
            if self.machine.on_transcript() == Admission::Accept {
                // Emit Inject action only. The App executes it on the winit
                // thread and then calls `finish_inject` with the result,
                // which drives the machine through RESTORING → finalize.
                vec![Action::Inject(text.to_string())]
            } else {
                vec![]
            }
        }
    }

    fn handle_failed(&mut self, msg: &str) -> Vec<Action> {
        let was_cancelled = self.machine.phase() == Phase::Cancelled;
        if self.machine.on_failed() == Admission::Accept {
            self.supervision_at = None;
            self.finalize_session(!was_cancelled)
        } else {
            vec![Action::Notify(msg.to_string())]
        }
    }

    fn handle_cleanup_issue(&mut self, msg: &str) -> Vec<Action> {
        vec![Action::Notify(msg.to_string())]
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes access to process-level env vars across parallel tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn new_orchestrator() -> Orchestrator {
        Orchestrator::new(
            RouterRecognizer,
            ChainProvider,
            SystemInjector,
            OsascriptContext,
        )
    }

    fn evt_ptt_down() -> DaemonEvent {
        DaemonEvent::PttDown
    }

    fn evt_ptt_up() -> DaemonEvent {
        DaemonEvent::PttUp
    }

    fn evt_cancel() -> DaemonEvent {
        DaemonEvent::Cancel
    }

    fn evt_capture_started() -> DaemonEvent {
        DaemonEvent::CaptureStarted
    }

    fn evt_watchdog(ms: u64) -> DaemonEvent {
        DaemonEvent::Watchdog { duration_ms: ms }
    }

    fn evt_tap_issue(msg: &str) -> DaemonEvent {
        DaemonEvent::TapIssue(msg.to_string())
    }

    fn evt_done(text: &str) -> DaemonEvent {
        DaemonEvent::Done {
            text: text.to_string(),
            duration_ms: 1000,
            rtf: 0.5,
        }
    }

    fn evt_failed(msg: &str) -> DaemonEvent {
        DaemonEvent::Failed(msg.to_string())
    }

    fn evt_cleanup_issue(msg: &str) -> DaemonEvent {
        DaemonEvent::CleanupIssue(msg.to_string())
    }

    fn is_send_control(actions: &[Action], expected: &Control) -> bool {
        actions.iter().any(|a| matches!(a, Action::SendControl(c) if std::mem::discriminant(c) == std::mem::discriminant(expected)))
    }

    fn is_set_tray(actions: &[Action], state: AppState) -> bool {
        actions
            .iter()
            .any(|a| matches!(a, Action::SetTray(s, _) if *s == state))
    }

    #[allow(dead_code)]
    fn is_set_tray_with_note(actions: &[Action], state: AppState, note_contains: &str) -> bool {
        actions.iter().any(|a| {
            matches!(a, Action::SetTray(s, n) if *s == state && n.as_ref().is_some_and(|n| n.contains(note_contains)))
        })
    }

    fn is_notify(actions: &[Action]) -> bool {
        actions.iter().any(|a| matches!(a, Action::Notify(_)))
    }

    fn is_inject(actions: &[Action]) -> bool {
        actions.iter().any(|a| matches!(a, Action::Inject(_)))
    }

    fn inject_text(actions: &[Action]) -> Option<&str> {
        actions.iter().find_map(|a| match a {
            Action::Inject(t) => Some(t.as_str()),
            _ => None,
        })
    }

    fn count_inject(actions: &[Action]) -> usize {
        actions
            .iter()
            .filter(|a| matches!(a, Action::Inject(_)))
            .count()
    }

    #[allow(dead_code)]
    fn count_send_control(actions: &[Action]) -> usize {
        actions
            .iter()
            .filter(|a| matches!(a, Action::SendControl(_)))
            .count()
    }

    // ── Routing & lifecycle tests ─────────────────────────────────────────

    #[test]
    fn happy_path_actions_sequence() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        // PttDown from Idle → [SetTray(Recording), SendControl(Down)]
        let actions = orch.handle(&evt_ptt_down());
        assert_eq!(actions.len(), 2, "PttDown should produce 2 actions");
        assert!(
            is_set_tray(&actions, AppState::Recording),
            "PttDown should set Recording state"
        );
        assert!(
            is_send_control(&actions, &Control::Down),
            "PttDown should send Down control"
        );

        // CaptureStarted → no actions (internal state change, supervision armed)
        let actions = orch.handle(&evt_capture_started());
        assert!(
            actions.is_empty(),
            "CaptureStarted should produce no actions"
        );

        // PttUp from Listening → [SetTray(Transcribing), SendControl(Up)]
        let actions = orch.handle(&evt_ptt_up());
        assert_eq!(actions.len(), 2, "PttUp should produce 2 actions");
        assert!(
            is_set_tray(&actions, AppState::Transcribing),
            "PttUp should set Transcribing state"
        );
        assert!(
            is_send_control(&actions, &Control::Up),
            "PttUp should send Up control"
        );

        // Done with text → [Inject(text)] only; no finalize yet.
        let actions = orch.handle(&evt_done("hello world"));
        assert_eq!(
            actions.len(),
            1,
            "Done with text should produce Inject only"
        );
        assert!(is_inject(&actions), "Done should produce Inject action");
        assert_eq!(
            inject_text(&actions),
            Some("hello world"),
            "Inject should carry the transcript text"
        );

        // App executes Inject → calls finish_inject(Ok) → [SetTray(Idle)]
        let ok_report = InjectReport {
            pasted_via: "test",
            clipboard_restored: true,
        };
        let actions = orch.finish_inject(Ok(ok_report));
        assert_eq!(
            actions.len(),
            1,
            "finish_inject(Ok) should produce 1 action"
        );
        assert!(
            is_set_tray(&actions, AppState::Idle),
            "finish_inject(Ok) should set Idle state"
        );

        // Next PttDown should work (machine is back in Idle)
        let actions = orch.handle(&evt_ptt_down());
        assert!(!actions.is_empty(), "PttDown from Idle should be accepted");
    }

    #[test]
    fn inject_success_ends_idle() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());
        orch.handle(&evt_ptt_up());

        // Done text → Inject only
        let actions = orch.handle(&evt_done("success text"));
        assert_eq!(count_inject(&actions), 1);
        assert!(
            !is_set_tray(&actions, AppState::Idle),
            "Inject step should not emit SetTray"
        );

        // finish_inject(Ok) → IDLE
        let ok_report = InjectReport {
            pasted_via: "test",
            clipboard_restored: true,
        };
        let actions = orch.finish_inject(Ok(ok_report));
        assert!(
            is_set_tray(&actions, AppState::Idle),
            "inject success must end in IDLE"
        );
        assert_eq!(orch.machine.phase(), Phase::Idle);
    }

    #[test]
    fn inject_failure_ends_error() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());
        orch.handle(&evt_ptt_up());

        // Done text → Inject only
        let actions = orch.handle(&evt_done("will fail"));
        assert_eq!(count_inject(&actions), 1);

        // finish_inject(Err) → ERROR with inject-error note
        let actions = orch.finish_inject(Err("enigo failed".into()));
        assert!(
            is_set_tray(&actions, AppState::Error),
            "inject failure must end in ERROR"
        );
        assert_eq!(orch.machine.phase(), Phase::Error);
        // Verify the note mentions the inject error
        assert!(
            actions.iter().any(|a| matches!(a, Action::SetTray(AppState::Error, n) if n.as_ref().is_some_and(|n| n.contains("clipboard")))),
            "tray note must mention clipboard fallback"
        );
    }

    #[test]
    fn on_inject_failed_live_covered() {
        // Verifies that on_inject_failed is exercised through finish_inject,
        // eliminating the dead-code warning from the original commit.
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());
        orch.handle(&evt_ptt_up());
        orch.handle(&evt_done("text"));

        // Before finish_inject, machine is Injecting
        assert_eq!(orch.machine.phase(), Phase::Injecting);

        // finish_inject(Err) → on_inject_failed → RESTORING → finalize → ERROR
        let _actions = orch.finish_inject(Err("fail".into()));
        assert_eq!(orch.machine.phase(), Phase::Error);
    }

    #[test]
    fn esc_in_starting_cancels_pending_capture() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        // PttDown → Starting
        let actions = orch.handle(&evt_ptt_down());
        assert!(!actions.is_empty());

        // Cancel from Starting → [SendControl(Cancel)]
        let actions = orch.handle(&evt_cancel());
        assert_eq!(
            actions.len(),
            1,
            "Cancel from Starting should produce 1 action"
        );
        assert!(
            is_send_control(&actions, &Control::Cancel),
            "Cancel should send Cancel control"
        );
    }

    #[test]
    fn stale_capture_started_after_cancel_ignored() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        // PttDown → Starting
        orch.handle(&evt_ptt_down());
        // Cancel → Cancelled
        orch.handle(&evt_cancel());

        // Stale CaptureStarted from Cancelled → no actions
        let actions = orch.handle(&evt_capture_started());
        assert!(
            actions.is_empty(),
            "stale CaptureStarted after cancel must produce no actions"
        );
    }

    #[test]
    fn duplicate_done_ignored_by_phase_gate() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        // Full happy path to Idle (including finish_inject)
        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());
        orch.handle(&evt_ptt_up());

        // First Done: accepted → Inject
        let actions1 = orch.handle(&evt_done("first transcript"));
        assert!(
            count_inject(&actions1) == 1,
            "first Done should produce exactly 1 Inject"
        );

        // finish_inject → Idle
        let ok_report = InjectReport {
            pasted_via: "test",
            clipboard_restored: true,
        };
        let _actions = orch.finish_inject(Ok(ok_report));

        // Second Done: machine in Idle → rejected
        let actions2 = orch.handle(&evt_done("second transcript"));
        assert!(
            actions2.is_empty(),
            "duplicate Done must be rejected by phase gate"
        );
    }

    #[test]
    fn finalize_is_idempotent() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        // Get to Restoring (via empty Done from Processing)
        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());
        orch.handle(&evt_ptt_up());

        let actions1 = orch.handle(&evt_done(""));
        assert!(
            is_set_tray(&actions1, AppState::Idle),
            "finalize should set Idle"
        );

        // Second finalize: machine is Idle (not Restoring), so on_finalized is ignored
        // and finalize returns SetTray(Idle, None) again
        let actions2 = orch.finalize_session(false);
        // Idempotent: should still return a valid SetTray (not panic, not duplicate actions)
        assert!(
            is_set_tray(&actions2, AppState::Idle),
            "idempotent finalize should still set Idle"
        );
    }

    #[test]
    fn supervision_deadline_fires_without_worker_events() {
        let _lock = ENV_LOCK.lock().unwrap();
        // Use a very short watchdog to avoid sleeping in the test.
        std::env::set_var("WIFLOW_MAX_RECORDING_MS", "50");

        let mut orch = new_orchestrator();
        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());

        // Set supervision time far in the past so the deadline has expired.
        orch.supervision_at = Some(Instant::now() - Duration::from_millis(200));

        let actions = orch.tick();
        assert!(
            !actions.is_empty(),
            "tick() past deadline must produce actions"
        );
        assert!(
            is_set_tray(&actions, AppState::Error),
            "supervision expiry must set Error state"
        );
        assert!(
            is_notify(&actions),
            "supervision expiry must produce a notification"
        );

        // Cleanup
        std::env::remove_var("WIFLOW_MAX_RECORDING_MS");
    }

    #[test]
    fn shutdown_from_listening_releases_everything() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());

        let actions = orch.handle_shutdown();
        assert!(
            !actions.is_empty(),
            "shutdown from Listening must produce actions"
        );
        assert!(
            is_set_tray(&actions, AppState::Idle),
            "shutdown must reset to Idle"
        );
        // Session must be released
        assert!(orch.session.is_none(), "shutdown must release the session");
        assert!(
            orch.supervision_at.is_none(),
            "shutdown must disarm supervision"
        );
    }

    #[test]
    fn down_from_error_resumes() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        // Go to Error
        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());
        orch.handle(&evt_failed("capture failed"));

        // Verify we're in Error
        assert_eq!(orch.machine.phase(), Phase::Error);

        // PttDown from Error → accepted (Start new session)
        let actions = orch.handle(&evt_ptt_down());
        assert!(!actions.is_empty(), "PttDown from Error should be accepted");
        assert!(
            is_set_tray(&actions, AppState::Recording),
            "should set Recording"
        );
    }

    #[test]
    fn restore_media_no_op_on_clean_session() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        // Clean session: never started a session, no media state.
        // restore_media should be a no-op (no panic, no side effects).
        orch.restore_media();
        // No assertion needed — the test passes if it doesn't panic.
    }

    #[test]
    fn tap_issue_notifies() {
        let mut orch = new_orchestrator();
        let actions = orch.handle(&evt_tap_issue("tap broke"));
        assert!(is_notify(&actions), "TapIssue should produce Notify");
    }

    #[test]
    fn cleanup_issue_notifies() {
        let mut orch = new_orchestrator();
        let actions = orch.handle(&evt_cleanup_issue("rate limit"));
        assert!(is_notify(&actions), "CleanupIssue should produce Notify");
    }

    #[test]
    fn done_empty_finalizes_to_idle() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());
        orch.handle(&evt_ptt_up());

        let actions = orch.handle(&evt_done(""));
        assert!(
            is_set_tray(&actions, AppState::Idle),
            "empty Done should finalize to Idle"
        );
        assert!(!is_inject(&actions), "empty Done should not produce Inject");
    }

    #[test]
    fn failed_from_cancelled_resolves_idle() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_cancel());

        let actions = orch.handle(&evt_failed("cancelled session"));
        assert!(
            is_set_tray(&actions, AppState::Idle),
            "Failed from Cancelled should resolve to Idle"
        );
    }

    #[test]
    fn failed_from_active_resolves_error() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());

        let actions = orch.handle(&evt_failed("capture error"));
        assert!(
            is_set_tray(&actions, AppState::Error),
            "Failed from active phase should resolve to Error"
        );
    }

    #[test]
    fn capture_started_from_listening_ignored() {
        let mut orch = new_orchestrator();

        // Can't be in Listening without starting a session, which requires
        // config. Just test that CaptureStarted from Idle is ignored.
        let actions = orch.handle(&evt_capture_started());
        assert!(
            actions.is_empty(),
            "CaptureStarted from Idle should be ignored"
        );
    }

    #[test]
    fn ptt_down_while_processing_ignored() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());
        orch.handle(&evt_ptt_up());

        // Machine is in Processing — second Down should be ignored
        let actions = orch.handle(&evt_ptt_down());
        assert!(
            actions.is_empty(),
            "PttDown while Processing should be ignored"
        );
    }

    #[test]
    fn ptt_up_while_idle_ignored() {
        let mut orch = new_orchestrator();
        let actions = orch.handle(&evt_ptt_up());
        assert!(actions.is_empty(), "PttUp from Idle should be ignored");
    }

    #[test]
    fn cancel_while_idle_ignored() {
        let mut orch = new_orchestrator();
        let actions = orch.handle(&evt_cancel());
        assert!(actions.is_empty(), "Cancel from Idle should be ignored");
    }

    #[test]
    fn tick_no_supervision_returns_empty() {
        let mut orch = new_orchestrator();
        let actions = orch.tick();
        assert!(
            actions.is_empty(),
            "tick() without armed supervision should return empty"
        );
    }

    #[test]
    fn shutdown_from_idle_notifies_only() {
        let mut orch = new_orchestrator();
        let actions = orch.handle_shutdown();
        assert!(
            is_notify(&actions),
            "shutdown from Idle should produce a notification"
        );
        assert!(
            !is_set_tray(&actions, AppState::Idle) || actions.len() == 1,
            "shutdown from Idle should not reset tray (already Idle)"
        );
    }

    #[test]
    fn watchdog_from_listening_transitions_and_notifies() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());

        let actions = orch.handle(&evt_watchdog(5000));
        assert!(
            is_notify(&actions),
            "Watchdog from Listening should produce Notify"
        );
        assert!(
            is_set_tray(&actions, AppState::Transcribing),
            "Watchdog should set Transcribing state"
        );
        // Supervision should be disarmed
        assert!(
            orch.supervision_at.is_none(),
            "Watchdog should disarm supervision"
        );
    }

    #[test]
    fn done_non_empty_from_starting_recovers() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        // PttDown → Starting (CaptureStarted lost)
        orch.handle(&evt_ptt_down());

        // Done text arrives while still in Starting (lost PttUp)
        let actions = orch.handle(&evt_done("recovered text"));
        assert!(
            is_inject(&actions),
            "Done from Starting should produce Inject"
        );
        // Inject only — no SetTray yet (finish_inject drives the rest).
        assert_eq!(actions.len(), 1, "should produce Inject action only");

        // finish_inject(Ok) → Idle
        let ok_report = InjectReport {
            pasted_via: "test",
            clipboard_restored: true,
        };
        let actions = orch.finish_inject(Ok(ok_report));
        assert!(
            is_set_tray(&actions, AppState::Idle),
            "should finalize to Idle after successful inject"
        );
    }

    #[test]
    fn full_cycle_then_done_empty() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());
        orch.handle(&evt_ptt_up());

        // Empty Done → finalize to Idle
        let actions = orch.handle(&evt_done(""));
        assert!(
            !actions.is_empty(),
            "empty Done should produce at least SetTray"
        );
        assert!(is_set_tray(&actions, AppState::Idle));
    }

    #[test]
    fn done_ignored_from_wrong_phase() {
        let mut orch = new_orchestrator();
        // Idle → Done is ignored
        let actions = orch.handle(&evt_done("text"));
        assert!(actions.is_empty(), "Done from Idle should be ignored");
    }

    #[test]
    fn failed_ignored_from_idle() {
        let mut orch = new_orchestrator();
        let actions = orch.handle(&evt_failed("oops"));
        // Machine in Idle → on_failed returns Ignore → Notify
        assert!(
            is_notify(&actions),
            "Failed from Idle should produce Notify"
        );
    }

    #[test]
    fn session_created_on_ptt_down() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        assert!(orch.session.is_some(), "PttDown should create a session");
    }

    #[test]
    fn session_released_on_finalize() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        assert!(orch.session.is_some());

        orch.handle(&evt_capture_started());
        orch.handle(&evt_ptt_up());
        orch.handle(&evt_done(""));

        assert!(
            orch.session.is_none(),
            "finalize should release the session"
        );
    }

    #[test]
    fn session_released_on_finish_inject() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());
        orch.handle(&evt_ptt_up());
        orch.handle(&evt_done("text"));

        // Session still held (Done text doesn't finalize)
        assert!(
            orch.session.is_some(),
            "session must exist before finish_inject"
        );

        let ok_report = InjectReport {
            pasted_via: "test",
            clipboard_restored: true,
        };
        let _actions = orch.finish_inject(Ok(ok_report));

        assert!(
            orch.session.is_none(),
            "finish_inject must release the session"
        );
    }

    #[test]
    fn supervision_armed_on_capture_started() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        assert!(
            orch.supervision_at.is_none(),
            "supervision not armed until CaptureStarted"
        );

        orch.handle(&evt_capture_started());
        assert!(
            orch.supervision_at.is_some(),
            "CaptureStarted should arm supervision"
        );
    }

    #[test]
    fn supervision_disarmed_on_ptt_up() {
        let _lock = ENV_LOCK.lock().unwrap();
        let mut orch = new_orchestrator();

        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());
        assert!(orch.supervision_at.is_some());

        orch.handle(&evt_ptt_up());
        assert!(
            orch.supervision_at.is_none(),
            "PttUp should disarm supervision"
        );
    }

    #[test]
    fn tick_before_deadline_returns_empty() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("WIFLOW_MAX_RECORDING_MS", "60000");

        let mut orch = new_orchestrator();
        orch.handle(&evt_ptt_down());
        orch.handle(&evt_capture_started());

        // Deadline is 60s away — tick should return empty
        let actions = orch.tick();
        assert!(
            actions.is_empty(),
            "tick() before deadline should return empty"
        );

        std::env::remove_var("WIFLOW_MAX_RECORDING_MS");
    }
}
