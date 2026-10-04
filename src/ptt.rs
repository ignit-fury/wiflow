//! Explicit, idempotent PTT state machine (lifecycle rules, Phase 4).
//!
//! States: Idle → Recording → Processing → Idle, plus Cancelling for the
//! Esc path. Every transition rule from the PTT lifecycle spec is enforced
//! HERE, on the winit thread, before any event reaches the worker:
//!
//! - PttDown:  Idle → Recording (only)
//! - PttUp:    Recording → Processing (only)
//! - Esc:      Recording → Cancelling (only)
//! - duplicate PttDown / duplicate PttUp / PttUp while Idle:
//!   ignored (no transition, no worker traffic)
//! - a second PttDown while Processing: ignored
//! - Recording is only left via PttUp, Esc, watchdog (forced stop surfaced
//!   as `RecordingTimedOut`), a terminal Done/Failed, or a fatal capture
//!   error (Failed) — never silently.
//!
//! The worker keeps its own `PushToTalk` bookkeeping; this machine gates
//! EVENT ADMISSION so the tray and the worker cannot diverge.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Recording,
    Processing,
    Cancelling,
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
}

impl PttMachine {
    pub fn new() -> Self {
        Self { phase: Phase::Idle }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// PttDown: Idle → Recording. Everything else is a duplicate/early press.
    pub fn on_down(&mut self) -> Admission {
        self.transition(Phase::Idle, Phase::Recording, "PttDown")
    }

    /// PttUp: Recording → Processing.
    pub fn on_up(&mut self) -> Admission {
        self.transition(Phase::Recording, Phase::Processing, "PttUp")
    }

    /// Esc: Recording → Cancelling.
    pub fn on_cancel(&mut self) -> Admission {
        self.transition(Phase::Recording, Phase::Cancelling, "Esc")
    }

    /// Watchdog auto-stop: Recording → Processing (mirrors PttUp, with the
    /// loss already warned about at the worker).
    pub fn on_watchdog(&mut self) -> Admission {
        self.transition(Phase::Recording, Phase::Processing, "watchdog auto-stop")
    }

    /// Terminal success/empty result. Normal completions return from
    /// Processing/Cancelling; a Done/Failed arriving while the tray still
    /// says Recording means the PttUp was lost upstream and something else
    /// (watchdog, capture error) already ended the recording — accept it
    /// loudly rather than leave Recording wedged.
    pub fn on_done(&mut self) -> Admission {
        match self.phase {
            Phase::Processing | Phase::Cancelling | Phase::Idle => self.set(Phase::Idle, "Done"),
            Phase::Recording => {
                tracing::warn!(
                    "Done arrived while still Recording — PttUp lost upstream; leaving Recording"
                );
                self.set(Phase::Idle, "Done")
            }
        }
    }

    /// Terminal failure (transcribe error, cancel, fatal capture error).
    pub fn on_failed(&mut self) -> Admission {
        match self.phase {
            Phase::Recording => {
                tracing::warn!(
                    "Failed arrived while still Recording — fatal capture/recording error"
                );
                self.set(Phase::Idle, "Failed")
            }
            _ => self.set(Phase::Idle, "Failed"),
        }
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

    /// One full cycle: Down → Up → Done → Idle.
    #[test]
    fn test_down_up_done_cycle() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Recording);
        assert_eq!(m.on_up(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Processing);
        assert_eq!(m.on_done(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// Down → Esc → Failed → Idle.
    #[test]
    fn test_down_cancel_cycle() {
        let mut m = PttMachine::new();
        assert_eq!(m.on_down(), Admission::Accept);
        assert_eq!(m.on_cancel(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Cancelling);
        assert_eq!(m.on_failed(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// Duplicate Down while Recording is ignored (no double capture).
    #[test]
    fn test_duplicate_down_ignored() {
        let mut m = PttMachine::new();
        m.on_down();
        assert_eq!(m.on_down(), Admission::Ignore);
        assert_eq!(m.phase(), Phase::Recording);
    }

    /// Second PttDown while Processing is ignored.
    #[test]
    fn test_down_while_processing_ignored() {
        let mut m = PttMachine::new();
        m.on_down();
        m.on_up();
        assert_eq!(m.on_down(), Admission::Ignore);
        assert_eq!(m.phase(), Phase::Processing);
    }

    /// Duplicate Up (and Up after Done) is ignored.
    #[test]
    fn test_duplicate_up_ignored() {
        let mut m = PttMachine::new();
        m.on_down();
        m.on_up();
        assert_eq!(m.on_up(), Admission::Ignore);
        m.on_done();
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
        m.on_up();
        assert_eq!(m.on_cancel(), Admission::Ignore);
        assert_eq!(m.phase(), Phase::Processing);
    }

    /// Watchdog only fires from Recording; a late Up after it is ignored.
    #[test]
    fn test_watchdog_timeout_then_late_up() {
        let mut m = PttMachine::new();
        m.on_down();
        assert_eq!(m.on_watchdog(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Processing);
        assert_eq!(m.on_up(), Admission::Ignore, "late release must be ignored");
        m.on_done();
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// Lost PttUp: Done from the watchdog pipeline while tray still
    /// Recording must still leave Recording (never wedged).
    #[test]
    fn test_done_while_recording_recovers() {
        let mut m = PttMachine::new();
        m.on_down();
        assert_eq!(m.on_done(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// Fatal capture error ends Recording (Failed accepted from Recording).
    #[test]
    fn test_failed_while_recording_recovers() {
        let mut m = PttMachine::new();
        m.on_down();
        assert_eq!(m.on_failed(), Admission::Accept);
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// 20 rapid Down/Up/Done cycles stay in lockstep.
    #[test]
    fn test_rapid_cycles_stay_synced() {
        let mut m = PttMachine::new();
        for _ in 0..20 {
            assert_eq!(m.on_down(), Admission::Accept);
            assert_eq!(m.on_up(), Admission::Accept);
            assert_eq!(m.on_done(), Admission::Accept);
            assert_eq!(m.phase(), Phase::Idle);
        }
    }

    /// Down during Cancelling is ignored until the cancel resolves.
    #[test]
    fn test_down_while_cancelling_ignored() {
        let mut m = PttMachine::new();
        m.on_down();
        m.on_cancel();
        assert_eq!(m.on_down(), Admission::Ignore);
        m.on_failed();
        assert_eq!(m.on_down(), Admission::Accept);
    }
}
