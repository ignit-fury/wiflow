//! `MediaController` implementation over the cherry-picked `AudioDuck`
//! machine (`platform::macos::duck`, left byte-identical apart from test
//! probes). This wrapper owns the spec §8.2 session state model and the
//! trait boundary — the orchestrator (Task 12) talks to `CoreAudioDuck`,
//! never to CoreAudio details.
//!
//! Staleness note (H12/H13): the player pause fires ASYNC (600 ms gate on a
//! spawned thread inside `AudioDuck`). `MediaSessionState` is therefore a
//! point-in-time view, refreshed by `refresh()` at lifecycle points
//! (LISTENING entry after `duck()`, RESTORING/session-end before logging).
//! The epoch (`hold_id`) is what invalidates stale timers — the state view
//! never is the invalidation mechanism.

use std::time::Duration;

use crate::core::traits::MediaController;

#[cfg(test)]
use super::duck::PlayerApp;
use super::duck::{AudioDuck, DuckSnapshot, MediaBackend, OsBackend};

/// Explicit media session state (spec §8.2 / H10). Makes "restore only what
/// Wiflow changed" executable: `restore()` touches exactly flagged state.
/// Wired into the orchestrator in Task 12; unused until then.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaSessionState {
    /// Was anything pause-worthy playing when the session ducked?
    pub was_playing_before: bool,
    /// Output device at duck time (re-checked at restore for the warn path).
    pub output_device: Option<String>,
    /// Did WE dip the output volume (and not yet restore it)?
    pub ducked_by_wiflow: bool,
    /// Did WE pause players (and not yet resume them)?
    pub paused_by_wiflow: bool,
    /// Outstanding modifications: `ducked_by_wiflow || paused_by_wiflow`.
    pub restoration_required: bool,
    /// Epoch guard: bumped by every real duck/restore transition; stale
    /// pause timers and duplicate restores compare against it (H12/H13).
    pub epoch: u64,
}

/// `MediaController` over `AudioDuck`. Generic over the backend so the
/// §13.2 scenarios run headless against a fake; production uses `OsBackend`.
/// Time control is constructor-injected `pause_delay` (short delays in
/// tests) — no separate clock trait needed.
/// Wired into the orchestrator in Task 12; unused until then.
pub struct CoreAudioDuck<B: MediaBackend = OsBackend> {
    inner: AudioDuck<B>,
    state: MediaSessionState,
    /// Mirrors the inner enabled flag for the probe gate: a disabled
    /// controller must not touch the backend at all — hermetic tests AND
    /// no per-press osascript cost when toggled off.
    enabled: bool,
}

impl<B: MediaBackend> CoreAudioDuck<B> {
    pub fn new(backend: B, enabled: bool, pause_delay: Duration) -> Self {
        Self {
            inner: AudioDuck::new(backend, enabled, pause_delay),
            state: MediaSessionState::default(),
            enabled,
        }
    }

    /// Share the inner machine (same Arc state) with another owner —
    /// used once at the composition root: the worker ducks at capture-Ok
    /// on this handle while the orchestrator records/restores through the
    /// wrapper. Idempotent on both ends, so the pair is always coherent.
    pub fn shared_inner(&self) -> AudioDuck<B> {
        self.inner.clone()
    }

    /// Current state view (refresh first at decision points). Read by
    /// tests and (future) diagnostics; the orchestrator drives the machine
    /// through the trait and never needs the view to act. Covered by 13
    /// wrapper scenarios, so this is reserved-not-dead.
    #[allow(dead_code)]
    pub fn state(&self) -> &MediaSessionState {
        &self.state
    }

    /// Pull live bookkeeping into the state view.
    pub fn refresh(&mut self) {
        let snap = self.inner.snapshot();
        self.apply_snapshot(snap);
    }

    /// Pre-duck truth for the state view. Private: only `duck()` needs it
    /// (the orchestrator fills session snapshots from worker-sent event
    /// fields — R17). Disabled controllers probe NOTHING.
    fn probe_pre_state(&self) -> (Option<String>, bool) {
        if !self.enabled {
            return (None, false);
        }
        (
            self.inner.current_device().map(|d| d.to_string()),
            self.inner.any_playing(),
        )
    }

    fn apply_snapshot(&mut self, snap: DuckSnapshot) {
        self.state.ducked_by_wiflow = snap.active && snap.volumes_saved;
        self.state.paused_by_wiflow = snap.active && snap.paused > 0;
        // A hold that ends with nothing outstanding still advanced the
        // epoch — restoration_required tracks live flags, not history.
        self.state.restoration_required =
            self.state.ducked_by_wiflow || self.state.paused_by_wiflow;
        self.state.epoch = snap.hold_id;
    }
}

impl<B: MediaBackend> MediaController for CoreAudioDuck<B> {
    fn duck(&mut self) {
        // Snapshot pre-duck truth first: the session record must describe
        // what WE found, not what a concurrent change left behind.
        let (device, playing) = self.probe_pre_state();
        self.state.was_playing_before = playing;
        self.state.output_device = device;
        self.inner.duck();
        self.refresh();
    }

    fn restore(&mut self) {
        // Idempotent by construction (inner early-returns when inactive):
        // safe on every hold-exit path, including duplicates (H3).
        self.inner.restore();
        self.refresh();
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.inner.set_enabled(enabled);
    }
}

/// Headless test backend shared by `media` and `orchestrator` tests.
/// Records everything it sees; knobs for playing/mute/device/pause-fail.
/// Test-only: never compiled into the app binary.
#[cfg(test)]
#[derive(Clone, Debug)]
pub struct FakeMediaBackend {
    state: std::sync::Arc<std::sync::Mutex<FakeMediaState>>,
}

#[cfg(test)]
#[derive(Debug)]
struct FakeMediaState {
    volumes: Vec<f32>,
    muted: bool,
    device: Option<u32>,
    playing: Vec<PlayerApp>,
    writes: Vec<Vec<f32>>,
    pauses: Vec<PlayerApp>,
    resumes: Vec<PlayerApp>,
    fail_pause: bool,
}

#[cfg(test)]
impl FakeMediaBackend {
    pub fn new() -> Self {
        Self {
            state: std::sync::Arc::new(std::sync::Mutex::new(FakeMediaState {
                volumes: vec![0.8],
                muted: false,
                device: Some(7),
                playing: Vec::new(),
                writes: Vec::new(),
                pauses: Vec::new(),
                resumes: Vec::new(),
                fail_pause: false,
            })),
        }
    }

    pub fn playing_music() -> Self {
        let f = Self::new();
        f.state.lock().unwrap().playing = vec![PlayerApp::Music];
        f
    }

    pub fn volumes(&self) -> Vec<f32> {
        self.state.lock().unwrap().volumes.clone()
    }
    pub fn pauses(&self) -> Vec<PlayerApp> {
        self.state.lock().unwrap().pauses.clone()
    }
    pub fn resumes(&self) -> Vec<PlayerApp> {
        self.state.lock().unwrap().resumes.clone()
    }
    pub fn write_count(&self) -> usize {
        self.state.lock().unwrap().writes.len()
    }
    pub fn set_device(&self, dev: Option<u32>) {
        self.state.lock().unwrap().device = dev;
    }
    pub fn set_playing(&self, app: PlayerApp) {
        let mut s = self.state.lock().unwrap();
        if !s.playing.contains(&app) {
            s.playing.push(app);
        }
    }
    pub fn set_fail_pause(&self, fail: bool) {
        self.state.lock().unwrap().fail_pause = fail;
    }
}

#[cfg(test)]
impl MediaBackend for FakeMediaBackend {
    fn output_device_id(&self) -> Option<u32> {
        self.state.lock().unwrap().device
    }
    fn output_volumes(&self) -> Vec<f32> {
        self.state.lock().unwrap().volumes.clone()
    }
    fn set_output_volumes(&self, v: &[f32]) {
        let mut s = self.state.lock().unwrap();
        s.volumes = v.to_vec();
        s.writes.push(v.to_vec());
    }
    fn output_muted(&self) -> bool {
        self.state.lock().unwrap().muted
    }
    fn is_playing(&self, app: PlayerApp) -> bool {
        self.state.lock().unwrap().playing.contains(&app)
    }
    fn pause(&self, app: PlayerApp) {
        let mut s = self.state.lock().unwrap();
        if s.fail_pause {
            return; // Simulated pause failure: nothing happens.
        }
        s.playing.retain(|a| a != &app);
        s.pauses.push(app);
    }
    fn resume(&self, app: PlayerApp) {
        let mut s = self.state.lock().unwrap();
        if !s.playing.contains(&app) {
            s.playing.push(app);
        }
        s.resumes.push(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::macos::duck::PlayerApp;

    /// Alias so the scenario tests below read unchanged.
    type Fake = super::FakeMediaBackend;

    fn ducked(fake: Fake) -> CoreAudioDuck<Fake> {
        CoreAudioDuck::new(fake, true, Duration::from_millis(5))
    }

    fn wait_gate() {
        std::thread::sleep(Duration::from_millis(80));
    }

    #[test]
    fn already_paused_players_claim_nothing() {
        let fake = Fake::new();
        let probe = fake.clone();
        let mut d = ducked(fake);
        d.duck();
        wait_gate();
        d.refresh();
        assert!(probe.pauses().is_empty(), "nothing playing: no pause");
        d.restore();
        d.refresh();
        assert!(!d.state().restoration_required);
        assert!((probe.volumes()[0] - 0.8).abs() < 1e-9, "exact restore");
    }

    #[test]
    fn playing_duck_pause_resume() {
        let fake = Fake::playing_music();
        let probe = fake.clone();
        let mut d = ducked(fake);
        d.duck();
        wait_gate();
        d.refresh();
        assert!(d.state().was_playing_before);
        assert!(d.state().paused_by_wiflow, "Music paused after gate");
        assert_eq!(probe.pauses(), vec![PlayerApp::Music]);
        d.restore();
        d.refresh();
        assert_eq!(probe.resumes(), vec![PlayerApp::Music], "resume ours");
        assert!((probe.volumes()[0] - 0.8).abs() < 1e-9, "exact restore");
        assert!(!d.state().restoration_required);
    }

    #[test]
    fn short_hold_duck_only_no_pause() {
        let fake = Fake::playing_music();
        let probe = fake.clone();
        let mut d = ducked(fake);
        d.duck();
        d.restore(); // before the 5ms gate
        wait_gate();
        assert!(probe.pauses().is_empty(), "restored before gate: no pause");
        assert!((probe.volumes()[0] - 0.8).abs() < 1e-9);
    }

    #[test]
    fn pause_failure_restore_still_safe() {
        let fake = Fake::playing_music();
        fake.set_fail_pause(true);
        let probe = fake.clone();
        let mut d = ducked(fake);
        d.duck();
        wait_gate();
        d.restore(); // must be safe despite the failed pause
        d.refresh();
        assert!((probe.volumes()[0] - 0.8).abs() < 1e-9, "volumes restored");
        assert!(!d.state().restoration_required, "flags cleared");
        assert!(!d.state().paused_by_wiflow, "nothing outstanding");
    }

    #[test]
    fn cancel_during_delay_no_pause() {
        // H12: Esc during the 600ms gate invalidates the pending pause.
        let fake = Fake::playing_music();
        let probe = fake.clone();
        let mut d = ducked(fake);
        d.duck();
        std::thread::sleep(Duration::from_millis(1));
        d.restore(); // the cancel
        wait_gate();
        assert!(probe.pauses().is_empty(), "cancelled gate must not fire");
    }

    #[test]
    fn duplicate_duck_is_noop() {
        let fake = Fake::new();
        let probe = fake.clone();
        let mut d = ducked(fake);
        d.duck();
        let epoch = d.state().epoch;
        d.duck();
        d.refresh();
        assert_eq!(d.state().epoch, epoch, "no epoch bump on duplicate");
        assert_eq!(probe.write_count(), 1, "single dip write");
    }

    #[test]
    fn duplicate_restore_is_noop() {
        let fake = Fake::new();
        let probe = fake.clone();
        let mut d = ducked(fake);
        d.duck();
        d.restore();
        assert_eq!(probe.write_count(), 2, "dip + restore");
        d.restore();
        assert_eq!(probe.write_count(), 2, "second restore writes nothing");
    }

    #[test]
    fn device_change_mid_session_completes() {
        let fake = Fake::new();
        let probe = fake.clone();
        let mut d = ducked(fake);
        d.duck();
        probe.set_device(Some(9)); // Bluetooth-style switch mid-hold
        d.restore(); // must complete (warn path), not trap
        d.refresh();
        assert!((probe.volumes()[0] - 0.8).abs() < 1e-9);
        assert!(!d.state().restoration_required);
        assert_eq!(
            d.state().output_device.as_deref(),
            Some("7"),
            "snapshot kept"
        );
    }

    #[test]
    fn vanished_device_at_restore_completes() {
        let fake = Fake::new();
        let probe = fake.clone();
        let mut d = ducked(fake);
        d.duck();
        probe.set_device(None); // unplugged before restore
        d.restore();
        d.refresh();
        assert!((probe.volumes()[0] - 0.8).abs() < 1e-9, "volumes restored");
        assert!(!d.state().restoration_required);
    }

    #[test]
    fn stale_timer_from_previous_epoch_ignored() {
        // H13: hold A's timer firing during hold B must not pause for A.
        let fake = Fake::playing_music();
        let probe = fake.clone();
        let mut d = ducked(fake);
        d.duck(); // hold A
        d.restore();
        d.duck(); // hold B (new epoch)
        wait_gate(); // both timers elapse; only B's may pause
        d.refresh();
        assert_eq!(probe.pauses(), vec![PlayerApp::Music], "exactly B's pause");
    }

    #[test]
    fn restore_after_pause_resumes_and_restores() {
        // Unit halves of "STT/injection failure after media pause": once the
        // pause has fired, restore() resumes + restores exactly. The
        // orchestrator paths (finalize → restore) land in Task 12.
        let fake = Fake::playing_music();
        let probe = fake.clone();
        let mut d = ducked(fake);
        d.duck();
        wait_gate();
        assert_eq!(probe.pauses().len(), 1);
        d.restore();
        d.refresh();
        assert_eq!(probe.resumes(), vec![PlayerApp::Music]);
        assert!((probe.volumes()[0] - 0.8).abs() < 1e-9);
        assert!(!d.state().restoration_required);
    }

    #[test]
    fn state_view_tracks_epoch_and_flags() {
        let fake = Fake::playing_music();
        let probe = fake.clone();
        let mut d = ducked(fake);
        assert_eq!(d.state().epoch, 0);
        assert!(!d.state().restoration_required);
        d.duck();
        d.refresh();
        assert!(d.state().epoch > 0, "epoch advanced on duck");
        assert!(d.state().ducked_by_wiflow, "volumes dipped");
        assert!(d.state().restoration_required);
        assert_eq!(probe.write_count(), 1);
        d.restore();
        d.refresh();
        assert!(!d.state().ducked_by_wiflow);
        assert!(!d.state().restoration_required);
    }

    #[test]
    fn disabled_duck_leaves_view_default() {
        // Disabled controllers probe nothing and dip nothing: the state
        // view stays at default through duck + restore.
        let fake = Fake::playing_music();
        let probe_handle = fake.clone();
        let mut d = ducked(fake);
        d.set_enabled(false);
        d.duck();
        d.refresh();
        assert!(!d.state().was_playing_before);
        assert_eq!(d.state().output_device, None);
        assert_eq!(probe_handle.write_count(), 0, "disabled: never dips");
        d.restore();
        d.refresh();
        assert!(!d.state().restoration_required);
    }
}
