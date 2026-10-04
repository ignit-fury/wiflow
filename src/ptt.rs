//! Explicit, idempotent PTT state machine (lifecycle rules, Phase 8).
//!
//! The worker keeps its own `PushToTalk` bookkeeping; this machine gates
//! EVENT ADMISSION so the tray and the worker cannot diverge.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Starting,
    Listening,
    Processing,
    Injecting,
    Restoring,
    Cancelled,
    Error,
}

/// What the app should do with an accepted event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Apply the transition (send Control, move the tray).
    Accept,
    /// Lifecycle rule says ignore this event right now.
    Ignore,
}

#[derive(Debug)]
pub struct PttMachine {
    phase: Phase,
    restoring_error_target: bool,
}

impl PttMachine {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            restoring_error_target: false,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// PttDown: Idle|Error → Starting.
    pub fn on_down(&mut self) -> Admission {
        match self.phase {
            Phase::Idle | Phase::Error => self.set(Phase::Starting, "PttDown"),
            _ => Admission::Ignore,
        }
    }

    /// PttUp: Listening → Processing.
    pub fn on_up(&mut self) -> Admission {
        self.transition(Phase::Listening, Phase::Processing, "PttUp")
    }

    /// Esc: Listening|Starting → Cancelled.
    pub fn on_cancel(&mut self) -> Admission {
        match self.phase {
            Phase::Listening | Phase::Starting => self.set(Phase::Cancelled, "Esc"),
            _ => Admission::Ignore,
        }
    }

    /// Watchdog auto-stop: Listening → Processing.
    pub fn on_watchdog(&mut self) -> Admission {
        self.transition(Phase::Listening, Phase::Processing, "watchdog auto-stop")
    }

    /// Capture request accepted: STARTING → LISTENING.
    ///
    /// Stale `CaptureStarted` events are ignored outside STARTING.
    #[allow(dead_code)]
    pub fn on_capture_started(&mut self) -> Admission {
        self.transition(Phase::Starting, Phase::Listening, "CaptureStarted")
    }

    /// Worker transcript ready: PROCESSING → INJECTING.
    pub fn on_transcript(&mut self) -> Admission {
        // No-stuck recovery: allow transcript arrival even if we never saw PttUp,
        // matching the old "Done while Recording" behavior.
        match self.phase {
            Phase::Processing => self.set(Phase::Injecting, "transcript"),
            Phase::Starting | Phase::Listening => {
                tracing::warn!(
                    "Done text arrived in unexpected phase {:?} — recovering into INJECTING",
                    self.phase
                );
                self.set(Phase::Injecting, "transcript")
            }
            _ => Admission::Ignore,
        }
    }

    /// Empty transcript: PROCESSING → RESTORING (success: no error target).
    pub fn on_empty(&mut self) -> Admission {
        match self.phase {
            Phase::Processing => {
                self.restoring_error_target = false;
                self.set(Phase::Restoring, "empty transcript")
            }
            Phase::Starting | Phase::Listening => {
                tracing::warn!(
                    "Done (empty) arrived in unexpected phase {:?} — recovering into RESTORING",
                    self.phase
                );
                self.restoring_error_target = false;
                self.set(Phase::Restoring, "empty transcript")
            }
            _ => Admission::Ignore,
        }
    }

    /// Injection succeeded: INJECTING → RESTORING.
    pub fn on_inject_ok(&mut self) -> Admission {
        if self.phase == Phase::Injecting {
            self.restoring_error_target = false;
            self.set(Phase::Restoring, "inject ok")
        } else {
            Admission::Ignore
        }
    }

    /// Injection failed: INJECTING → RESTORING.
    #[allow(dead_code)]
    pub fn on_inject_failed(&mut self) -> Admission {
        if self.phase == Phase::Injecting {
            self.restoring_error_target = true;
            self.set(Phase::Restoring, "inject failed")
        } else {
            Admission::Ignore
        }
    }

    /// Terminal failure.
    ///
    /// Any active phase → RESTORING. Target after finalize is:
    /// - IDLE if we were in CANCELLED
    /// - ERROR otherwise
    pub fn on_failed(&mut self) -> Admission {
        match self.phase {
            Phase::Starting | Phase::Listening | Phase::Processing | Phase::Injecting => {
                self.restoring_error_target = true;
                self.set(Phase::Restoring, "Failed")
            }
            Phase::Cancelled => {
                self.restoring_error_target = false;
                self.set(Phase::Restoring, "Failed")
            }
            _ => Admission::Ignore,
        }
    }

    /// Central finalize step.
    pub fn on_finalized(&mut self) -> Admission {
        if self.phase != Phase::Restoring {
            return Admission::Ignore;
        }

        let to = if self.restoring_error_target {
            Phase::Error
        } else {
            Phase::Idle
        };

        // Clear for the next session (consumed by finalize).
        self.restoring_error_target = false;
        self.set(to, "finalized")
    }

    fn transition(&mut self, want: Phase, to: Phase, ev: &str) -> Admission {
        if self.phase == want {
            self.set(to, ev);
            Admission::Accept
        } else {
            tracing::debug!("{ev} ignored (phase {:?} — expected {want:?})", self.phase);
            Admission::Ignore
        }
    }

    fn set(&mut self, to: Phase, ev: &str) -> Admission {
        if self.phase != to {
            tracing::info!("state {:?} -> {:?} ({ev})", self.phase, to);
            self.phase = to;
        }
        Admission::Accept
    }
}

impl Default for PttMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Down → CaptureStarted → Up → transcript → inject_ok → finalized → Idle.
    #[test]
    fn test_down_up_done_cycle() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Starting);
        assert_eq!(m.on_capture_started(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Listening);

        assert_eq!(m.on_up(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Processing);

        assert_eq!(m.on_transcript(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Injecting);

        assert_eq!(m.on_inject_ok(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Restoring);

        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// Down (pending capture) → Esc → Failed → finalized → Idle.
    #[test]
    fn test_down_cancel_cycle() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Starting);

        assert_eq!(m.on_cancel(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Cancelled);

        assert_eq!(m.on_failed(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Restoring);

        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// Duplicate Down while Starting is ignored (no double capture).
    #[test]
    fn test_duplicate_down_ignored() {
        let mut m = PttMachine::new();
        m.on_down();
        assert_eq!(m.on_down(), Admission::Ignore);
        assert_eq!(m.phase(), Phase::Starting);
    }

    /// Second PttDown while Processing is ignored.
    #[test]
    fn test_down_while_processing_ignored() {
        let mut m = PttMachine::new();
        m.on_down();
        m.on_capture_started();
        m.on_up();
        assert_eq!(m.on_down(), Admission::Ignore);
        assert_eq!(m.phase(), Phase::Processing);
    }

    /// Duplicate Up (and Up after finalize) is ignored.
    #[test]
    fn test_duplicate_up_ignored() {
        let mut m = PttMachine::new();
        m.on_down();
        m.on_capture_started();
        m.on_up();
        assert_eq!(m.on_up(), Admission::Ignore);

        // finalize happy path
        m.on_transcript();
        m.on_inject_ok();
        m.on_finalized();

        assert_eq!(m.on_up(), Admission::Ignore);
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// PttUp while Idle is ignored (stray release never touches the worker).
    #[test]
    fn test_up_while_idle_ignored() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_up(), Admission::Ignore);
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// Esc while Idle/Processing is ignored.
    #[test]
    fn test_cancel_outside_recording_ignored() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_cancel(), Admission::Ignore);
        m.on_down();
        m.on_capture_started();
        m.on_up();
        assert_eq!(m.on_cancel(), Admission::Ignore);
        assert_eq!(m.phase(), Phase::Processing);
    }

    /// Watchdog only fires from Listening; a late Up after it is ignored.
    #[test]
    fn test_watchdog_timeout_then_late_up() {
        let mut m = PttMachine::new();
        m.on_down();
        m.on_capture_started();

        assert_eq!(m.on_watchdog(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Processing);

        assert_eq!(m.on_up(), Admission::Ignore, "late release must be ignored");

        m.on_transcript();
        m.on_inject_ok();
        m.on_finalized();
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// Lost PttUp: terminal success arrives while still earlier than Processing.
    #[test]
    fn test_done_while_recording_recovers() {
        let mut m = PttMachine::new();
        m.on_down();
        assert_eq!(m.on_transcript(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Injecting);
        assert_eq!(m.on_inject_ok(), Admission::Accept);
        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// Fatal capture error ends session: fatal Failed now lands in ERROR.
    #[test]
    fn test_failed_while_recording_recovers() {
        let mut m = PttMachine::new();
        m.on_down();
        assert_eq!(m.on_failed(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Restoring);
        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Error);
    }

    /// 20 rapid Down/CaptureStarted/Up/transcript/inject_ok/finalize cycles stay in lockstep.
    #[test]
    fn test_rapid_cycles_stay_synced() {
        let mut m = PttMachine::new();
        for _ in 0..20 {
            assert_eq!(m.on_down(), Admission::Accept);
            assert_eq!(m.on_capture_started(), Admission::Accept);
            assert_eq!(m.on_up(), Admission::Accept);
            assert_eq!(m.on_transcript(), Admission::Accept);
            assert_eq!(m.on_inject_ok(), Admission::Accept);
            assert_eq!(m.on_finalized(), Admission::Accept);
            assert_eq!(m.phase(), Phase::Idle);
        }
    }

    /// Down during Cancelled is ignored until the cancel resolves.
    #[test]
    fn test_down_while_cancelling_ignored() {
        let mut m = PttMachine::new();
        m.on_down();
        m.on_cancel();
        assert_eq!(m.on_down(), Admission::Ignore);
        m.on_failed();
        m.on_finalized();
        assert_eq!(m.on_down(), Admission::Accept);
    }

    // --- New §13.1 machine-level tests ---

    #[test]
    fn starting_requires_capture_started() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Starting);

        // stray Up while STARTING is ignored
        assert_eq!(m.on_up(), Admission::Ignore);

        // CaptureStarted only accepted from STARTING
        assert_eq!(m.on_capture_started(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Listening);

        // cancellation from LISTENING
        assert_eq!(m.on_cancel(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Cancelled);

        // stale CaptureStarted is ignored outside STARTING
        assert_eq!(m.on_capture_started(), Admission::Ignore);

        assert_eq!(m.on_failed(), Admission::Accept);
        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn inject_failure_records_error_target() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.on_capture_started(), Admission::Accept);
        assert_eq!(m.on_up(), Admission::Accept);
        assert_eq!(m.on_transcript(), Admission::Accept);
        assert_eq!(m.on_inject_failed(), Admission::Accept);
        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Error);
    }

    #[test]
    fn cancel_failed_resolves_idle() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.on_cancel(), Admission::Accept);
        assert_eq!(m.on_failed(), Admission::Accept);
        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn fatal_failed_resolves_error() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.on_capture_started(), Admission::Accept);
        assert_eq!(m.on_up(), Admission::Accept);
        assert_eq!(m.on_failed(), Admission::Accept);
        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Error);
    }

    #[test]
    fn down_from_error_admitted() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.on_capture_started(), Admission::Accept);
        assert_eq!(m.on_up(), Admission::Accept);
        assert_eq!(m.on_transcript(), Admission::Accept);
        assert_eq!(m.on_inject_failed(), Admission::Accept);
        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Error);

        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Starting);
    }

    #[test]
    fn empty_from_processing_to_idle() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.on_capture_started(), Admission::Accept);
        assert_eq!(m.on_up(), Admission::Accept);

        assert_eq!(m.on_empty(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Restoring);
        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn empty_from_starting_recovers() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Starting);

        assert_eq!(m.on_empty(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Restoring);
        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn empty_from_listening_recovers() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.on_capture_started(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Listening);

        assert_eq!(m.on_empty(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Restoring);
        assert_eq!(m.on_finalized(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }
}
