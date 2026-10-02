//! Audio prioritization for push-to-talk: while the mic is hot, competing
//! laptop audio is ducked (and scriptable players paused), then everything
//! is restored exactly.
//!
//! Platform reality (verified against the macOS SDK headers on this machine):
//! `AVAudioSession` duck-others is `API_UNAVAILABLE(macos)` and per-app
//! volume control has no public API, so no third party can "pause all
//! audio" the way Apple Dictation does (private entitlements + system
//! service). Two levers remain, both used here:
//!
//! 1. Output-volume duck via CoreAudio (no OSD popup, no subprocess): works
//!    on EVERY source including browsers — same acoustic effect Apple gets.
//! 2. Best-effort pause of scriptable players (Music, Spotify) via
//!    AppleScript: only when already running AND playing, resumed only when
//!    we paused them. Browsers can never be paused (documented, not claimed).
//!
//! Never touched: the input device, mic selection, mute state of anything.
//! All backend failures degrade to "ducking skipped" (warn-only).

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Hold must outlive this before players pause: accidental <300ms taps
/// (and fast re-taps) never stutter music — only sustained dictation does.
pub const PAUSE_DELAY: Duration = Duration::from_millis(600);
/// Duck factor: output drops to 20% while listening.
const DUCK_FACTOR: f32 = 0.2;
/// Below this the output is effectively silent — nothing worth ducking.
const SILENCE_FLOOR: f32 = 0.005;

/// Scriptable players we may pause (never launched, never force-resumed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerApp {
    Music,
    Spotify,
}

impl PlayerApp {
    fn process_name(self) -> &'static str {
        match self {
            PlayerApp::Music => "Music",
            PlayerApp::Spotify => "Spotify",
        }
    }
}

const PLAYERS: &[PlayerApp] = &[PlayerApp::Music, PlayerApp::Spotify];

/// Hardware/OS media access. Trait (not inline syscalls) so the state
/// machine is fully testable with a fake.
pub trait MediaBackend: Send + Sync + 'static {
    /// Per-channel output scalars (master first when present). Empty when
    /// the device exposes no volume control. Saved and restored as a vector
    /// so stereo balance can never drift.
    fn output_volumes(&self) -> Vec<f32>;
    fn set_output_volumes(&self, v: &[f32]);
    fn output_muted(&self) -> bool;
    /// True only when the app is RUNNING and its player state is playing.
    /// Must never launch the app as a side effect.
    fn is_playing(&self, app: PlayerApp) -> bool;
    fn pause(&self, app: PlayerApp);
    fn resume(&self, app: PlayerApp);
}

#[derive(Debug)]
struct Inner {
    enabled: bool,
    active: bool,
    saved_volumes: Vec<f32>,
    paused: Vec<PlayerApp>,
    hold_id: u64,
}

/// Owns duck state across holds. Idempotent on both ends; stale delayed
/// pauses are invalidated by `hold_id`, so rapid start/stop cycles cannot
/// corrupt volume or leave apps paused.
pub struct AudioDuck<B: MediaBackend> {
    backend: Arc<B>,
    pause_delay: Duration,
    inner: Arc<Mutex<Inner>>,
}

// Shared between the worker (duck at key-down) and the main thread
// (restore after injection completes). No `B: Clone` bound needed.
impl<B: MediaBackend> Clone for AudioDuck<B> {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            pause_delay: self.pause_delay,
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<B: MediaBackend> AudioDuck<B> {
    pub fn new(backend: B, enabled: bool, pause_delay: Duration) -> Self {
        Self {
            backend: Arc::new(backend),
            pause_delay,
            inner: Arc::new(Mutex::new(Inner {
                enabled,
                active: false,
                saved_volumes: Vec::new(),
                paused: Vec::new(),
                hold_id: 0,
            })),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        let mut inner = self.lock();
        inner.enabled = enabled;
        // Toggling off mid-hold restores immediately (no stuck duck).
        if !enabled && inner.active {
            inner.hold_id = inner.hold_id.wrapping_add(1);
            inner.active = false;
            let paused = std::mem::take(&mut inner.paused);
            let saved = std::mem::take(&mut inner.saved_volumes);
            drop(inner);
            for app in paused {
                self.backend.resume(app);
            }
            if !saved.is_empty() {
                self.backend.set_output_volumes(&saved);
            }
        }
    }

    /// Begin prioritization for a new hold. Cheap + idempotent: re-entry
    /// without restore is a no-op (same hold continuing).
    pub fn duck(&self) {
        let mut inner = self.lock();
        if !inner.enabled || inner.active {
            return;
        }
        if self.backend.output_muted() {
            return; // Nothing audible: leave everything alone.
        }
        let saved = self.backend.output_volumes();
        if saved.is_empty() {
            // No volume control: pause path still valuable.
        } else if saved.iter().all(|v| *v < SILENCE_FLOOR) {
            return; // Effectively silent.
        } else {
            let ducked: Vec<f32> = saved.iter().map(|v| v * DUCK_FACTOR).collect();
            self.backend.set_output_volumes(&ducked);
            inner.saved_volumes = saved;
        }
        inner.active = true;
        inner.paused.clear();
        inner.hold_id = inner.hold_id.wrapping_add(1);
        let hold_id = inner.hold_id;
        drop(inner);
        // Delayed player pause off-thread (never blocks capture start).
        let backend = Arc::clone(&self.backend);
        let inner_ref = Arc::clone(&self.inner);
        let delay = self.pause_delay;
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            Self::maybe_pause_for_hold(&backend, &inner_ref, hold_id);
        });
    }

    /// Delayed-pause body, extracted for tests. Revalidates the hold after
    /// EVERY slow backend call: osascript round-trips take 100-500ms cold,
    /// so the hold can end mid-sequence. Interleavings that slip through
    /// still converge on the correct end state:
    /// - restore() between check and pause → we skip (fresh check fails).
    /// - restore() between record and pause completing → post-pause check
    ///   fails → immediate resume + unrecord (never stuck paused).
    /// - restore() after pause completing → it resumes from the list.
    fn maybe_pause_for_hold(backend: &Arc<B>, inner_ref: &Arc<Mutex<Inner>>, hold_id: u64) {
        {
            let inner = inner_ref.lock().unwrap_or_else(|e| e.into_inner());
            if !inner.active || inner.hold_id != hold_id {
                return; // Hold ended (or superseded) before the delay.
            }
        }
        for app in PLAYERS {
            if !backend.is_playing(*app) {
                continue;
            }
            {
                let inner = inner_ref.lock().unwrap_or_else(|e| e.into_inner());
                if !inner.active || inner.hold_id != hold_id {
                    return; // Ended during the state query.
                }
            }
            // Optimistic record BEFORE the slow pause call so a concurrent
            // restore() sees (and resumes) this app no matter when it lands.
            {
                let mut inner = inner_ref.lock().unwrap_or_else(|e| e.into_inner());
                if !inner.paused.contains(app) {
                    inner.paused.push(*app);
                }
            }
            backend.pause(*app);
            {
                let mut inner = inner_ref.lock().unwrap_or_else(|e| e.into_inner());
                if !inner.active || inner.hold_id != hold_id {
                    // Hold ended mid-pause: undo immediately, unrecord.
                    inner.paused.retain(|a| a != app);
                    drop(inner);
                    backend.resume(*app);
                    return;
                }
            }
        }
    }

    /// End prioritization: resume what WE paused, restore the exact saved
    /// volume. Idempotent — safe on every hold-exit path.
    pub fn restore(&self) {
        let mut inner = self.lock();
        if !inner.active {
            return;
        }
        inner.hold_id = inner.hold_id.wrapping_add(1); // Cancel pending pause.
        inner.active = false;
        let paused = std::mem::take(&mut inner.paused);
        let saved = std::mem::take(&mut inner.saved_volumes);
        drop(inner);
        for app in paused {
            self.backend.resume(app);
        }
        if !saved.is_empty() {
            self.backend.set_output_volumes(&saved);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Test probe: is a hold currently ducked?
    #[cfg(test)]
    fn is_active(&self) -> bool {
        self.lock().active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeState {
        volumes: Vec<f32>,
        muted: bool,
        running: Vec<PlayerApp>,
        playing: Vec<PlayerApp>,
        writes: Vec<Vec<f32>>,
        pauses: Vec<PlayerApp>,
        resumes: Vec<PlayerApp>,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    struct Fake {
        state: Mutex<FakeState>,
    }

    impl Fake {
        fn new(volumes: Vec<f32>) -> Self {
            Self {
                state: Mutex::new(FakeState {
                    volumes,
                    muted: false,
                    running: vec![PlayerApp::Music, PlayerApp::Spotify],
                    playing: Vec::new(),
                    writes: Vec::new(),
                    pauses: Vec::new(),
                    resumes: Vec::new(),
                    hook: None,
                }),
            }
        }
    }

    impl MediaBackend for Fake {
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
            let s = self.state.lock().unwrap();
            s.running.contains(&app) && s.playing.contains(&app)
        }
        fn pause(&self, app: PlayerApp) {
            let hook = self.state.lock().unwrap().hook.clone();
            if let Some(h) = hook {
                h(); // Test-only: simulate key-up landing mid-pause.
            }
            let mut s = self.state.lock().unwrap();
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

    fn ducked(delay: Duration) -> AudioDuck<Fake> {
        AudioDuck::new(Fake::new(vec![0.8]), true, delay)
    }

    #[test]
    fn test_duck_lowers_restore_exact() {
        let d = ducked(Duration::from_millis(5));
        d.duck();
        assert!(d.is_active());
        let vol = d.backend.state.lock().unwrap().volumes.clone();
        assert!((vol[0] - 0.16).abs() < 1e-6, "ducked to 20%, got {vol:?}");
        d.restore();
        assert!(!d.is_active());
        let s = d.backend.state.lock().unwrap();
        assert!((s.volumes[0] - 0.8).abs() < 1e-9, "exact restore");
        assert_eq!(s.writes.len(), 2, "one duck write + one restore write");
    }

    #[test]
    fn test_stereo_balance_preserved() {
        let d = AudioDuck::new(Fake::new(vec![0.9, 0.5]), true, Duration::from_millis(5));
        d.duck();
        d.restore();
        assert_eq!(d.backend.state.lock().unwrap().volumes, vec![0.9, 0.5]);
    }

    #[test]
    fn test_muted_and_silent_noop() {
        let d = ducked(Duration::from_millis(5));
        d.backend.state.lock().unwrap().muted = true;
        d.duck();
        assert!(!d.is_active());
        assert!(d.backend.state.lock().unwrap().writes.is_empty());

        let d2 = AudioDuck::new(Fake::new(vec![0.0]), true, Duration::from_millis(5));
        d2.duck();
        assert!(!d2.is_active());
    }

    #[test]
    fn test_idempotent_both_ends() {
        let d = ducked(Duration::from_millis(5));
        d.duck();
        d.duck();
        d.restore();
        d.restore();
        assert_eq!(d.backend.state.lock().unwrap().writes.len(), 2);
    }

    #[test]
    fn test_delayed_pause_and_resume() {
        let d = ducked(Duration::from_millis(5));
        d.backend
            .state
            .lock()
            .unwrap()
            .playing
            .push(PlayerApp::Music);
        d.duck();
        // Before the delay: volume ducked, nothing paused yet.
        assert!(d.backend.state.lock().unwrap().pauses.is_empty());
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            d.backend.state.lock().unwrap().pauses,
            vec![PlayerApp::Music]
        );
        d.restore();
        assert_eq!(
            d.backend.state.lock().unwrap().resumes,
            vec![PlayerApp::Music]
        );
    }

    #[test]
    fn test_stale_hold_never_pauses() {
        // Short tap restored before the delay: the sleeper must no-op.
        let d = ducked(Duration::from_millis(80));
        d.backend
            .state
            .lock()
            .unwrap()
            .playing
            .push(PlayerApp::Spotify);
        d.duck();
        d.restore();
        std::thread::sleep(Duration::from_millis(150));
        let s = d.backend.state.lock().unwrap();
        assert!(s.pauses.is_empty(), "stale hold paused: {:?}", s.pauses);
        assert!(s.resumes.is_empty());
        // Volume still restored exactly once.
        assert!((s.volumes[0] - 0.8).abs() < 1e-9);
    }

    #[test]
    fn test_resume_only_what_we_paused() {
        let d = ducked(Duration::from_millis(5));
        {
            let mut s = d.backend.state.lock().unwrap();
            s.playing.push(PlayerApp::Music); // playing → we will pause it
            s.running.retain(|a| a != &PlayerApp::Spotify); // not running
        }
        d.duck();
        std::thread::sleep(Duration::from_millis(60));
        d.restore();
        let s = d.backend.state.lock().unwrap();
        assert_eq!(s.pauses, vec![PlayerApp::Music]);
        assert_eq!(s.resumes, vec![PlayerApp::Music]);
    }

    #[test]
    fn test_no_volume_device_still_pauses() {
        let d = AudioDuck::new(Fake::new(Vec::new()), true, Duration::from_millis(5));
        d.backend
            .state
            .lock()
            .unwrap()
            .playing
            .push(PlayerApp::Music);
        d.duck();
        assert!(d.is_active());
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            d.backend.state.lock().unwrap().pauses,
            vec![PlayerApp::Music]
        );
        d.restore();
        assert!(d.backend.state.lock().unwrap().writes.is_empty());
    }

    #[test]
    fn test_disable_mid_hold_restores() {
        let d = ducked(Duration::from_millis(5));
        d.duck();
        d.set_enabled(false);
        assert!(!d.is_active());
        assert!((d.backend.state.lock().unwrap().volumes[0] - 0.8).abs() < 1e-9);
        // Re-enabled duck works again.
        d.set_enabled(true);
        d.duck();
        assert!(d.is_active());
        d.restore();
    }

    #[test]
    fn test_stale_body_never_pauses() {
        // The exact reported bug: hold ends (restore) while the delayed
        // thread is still sleeping — the body must no-op entirely.
        let d = AudioDuck::new(Fake::new(vec![0.8]), true, Duration::from_secs(3600));
        d.backend
            .state
            .lock()
            .unwrap()
            .playing
            .push(PlayerApp::Music);
        d.duck();
        let stale_id = d.inner.lock().unwrap().hold_id;
        d.restore(); // key-up before the delay elapses
        AudioDuck::maybe_pause_for_hold(&d.backend, &d.inner, stale_id);
        let s = d.backend.state.lock().unwrap();
        assert!(s.pauses.is_empty());
        assert!(s.resumes.is_empty());
        assert!((s.volumes[0] - 0.8).abs() < 1e-9);
    }

    #[test]
    fn test_restore_mid_pause_self_heals() {
        // Worse interleaving: restore lands between our pause call and its
        // completion. End state must still be: volume exact, app playing.
        let d = AudioDuck::new(Fake::new(vec![0.8]), true, Duration::from_secs(3600));
        d.backend
            .state
            .lock()
            .unwrap()
            .playing
            .push(PlayerApp::Music);
        let restore_duck = d.clone();
        d.backend.state.lock().unwrap().hook = Some(Arc::new(move || restore_duck.restore()));
        d.duck();
        let id = d.inner.lock().unwrap().hold_id;
        AudioDuck::maybe_pause_for_hold(&d.backend, &d.inner, id);
        let s = d.backend.state.lock().unwrap();
        assert!(!d.is_active());
        assert!((s.volumes[0] - 0.8).abs() < 1e-9, "volume exact");
        assert!(s.playing.contains(&PlayerApp::Music), "app left playing");
    }

    #[test]
    fn test_disabled_duck_noop() {
        let d = AudioDuck::new(Fake::new(vec![0.8]), false, Duration::from_millis(5));
        d.duck();
        assert!(!d.is_active());
        assert!(d.backend.state.lock().unwrap().writes.is_empty());
    }
}

// --- macOS backend: CoreAudio volume (no OSD) + AppleScript players --------

// CoreAudio FFI — same zero-dep pattern as `tap.rs`.
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        id: u32,
        addr: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const std::ffi::c_void,
        size: *mut u32,
        data: *mut std::ffi::c_void,
    ) -> i32;
    fn AudioObjectSetPropertyData(
        id: u32,
        addr: *const AudioObjectPropertyAddress,
        qualifier_size: u32,
        qualifier: *const std::ffi::c_void,
        size: u32,
        data: *const std::ffi::c_void,
    ) -> i32;
    fn AudioObjectHasProperty(id: u32, addr: *const AudioObjectPropertyAddress) -> u8;
}

#[repr(C)]
struct AudioObjectPropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

const SYSTEM_OBJECT: u32 = 1;
const PROP_DEFAULT_OUTPUT: u32 = 0x644F_7574; // 'dOut'
const SCOPE_GLOBAL: u32 = 0x676C_6F62; // 'glob'
const SCOPE_OUTPUT: u32 = 0x6F75_7470; // 'outp'
const ELEMENT_MASTER: u32 = 0;
const PROP_VOLUME_SCALAR: u32 = 0x766F_6C6D; // 'volm'
const PROP_MUTE: u32 = 0x6D75_7465; // 'mute'

fn default_output_device() -> Option<u32> {
    let addr = AudioObjectPropertyAddress {
        selector: PROP_DEFAULT_OUTPUT,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MASTER,
    };
    let mut id: u32 = 0;
    let mut size = 4u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut id as *mut u32 as *mut _,
        )
    };
    (status == 0 && id != 0).then_some(id)
}

/// Real macOS backend.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsBackend;

/// Volume elements: master (0) when the device has one (most speakers),
/// else per-channel elements (AirPods-style devices). Saved/restored as a
/// vector so channel balance can never drift.
fn volume_elements(dev: u32) -> Vec<u32> {
    let master = AudioObjectPropertyAddress {
        selector: PROP_VOLUME_SCALAR,
        scope: SCOPE_OUTPUT,
        element: ELEMENT_MASTER,
    };
    if unsafe { AudioObjectHasProperty(dev, &master) } != 0 {
        return vec![ELEMENT_MASTER];
    }
    (1..=8u32)
        .filter(|el| {
            let addr = AudioObjectPropertyAddress {
                selector: PROP_VOLUME_SCALAR,
                scope: SCOPE_OUTPUT,
                element: *el,
            };
            (unsafe { AudioObjectHasProperty(dev, &addr) }) != 0
        })
        .collect()
}

fn read_channel(dev: u32, element: u32) -> Option<f32> {
    let addr = AudioObjectPropertyAddress {
        selector: PROP_VOLUME_SCALAR,
        scope: SCOPE_OUTPUT,
        element,
    };
    let mut v: f32 = 0.0;
    let mut size = 4u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            dev,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut v as *mut f32 as *mut _,
        )
    };
    (status == 0).then_some(v.clamp(0.0, 1.0))
}

fn write_channel(dev: u32, element: u32, v: f32) {
    let addr = AudioObjectPropertyAddress {
        selector: PROP_VOLUME_SCALAR,
        scope: SCOPE_OUTPUT,
        element,
    };
    let v = v.clamp(0.0, 1.0);
    unsafe {
        AudioObjectSetPropertyData(
            dev,
            &addr,
            0,
            std::ptr::null(),
            4,
            &v as *const f32 as *const _,
        );
    }
}

impl MediaBackend for OsBackend {
    fn output_volumes(&self) -> Vec<f32> {
        let Some(dev) = default_output_device() else {
            return Vec::new();
        };
        volume_elements(dev)
            .into_iter()
            .filter_map(|el| read_channel(dev, el))
            .collect()
    }

    fn set_output_volumes(&self, v: &[f32]) {
        let Some(dev) = default_output_device() else {
            return;
        };
        for (el, val) in volume_elements(dev).into_iter().zip(v.iter()) {
            write_channel(dev, el, *val);
        }
    }

    fn output_muted(&self) -> bool {
        let Some(dev) = default_output_device() else {
            return false;
        };
        let addr = AudioObjectPropertyAddress {
            selector: PROP_MUTE,
            scope: SCOPE_OUTPUT,
            element: ELEMENT_MASTER,
        };
        if unsafe { AudioObjectHasProperty(dev, &addr) } == 0 {
            return false;
        }
        let mut m: u32 = 0;
        let mut size = 4u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                dev,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                &mut m as *mut u32 as *mut _,
            )
        };
        status == 0 && m != 0
    }

    fn is_playing(&self, app: PlayerApp) -> bool {
        let name = app.process_name();
        // Existence check first: querying a non-running app would LAUNCH it.
        let running = std::process::Command::new("osascript")
            .args([
                "-e",
                &format!("tell application \"System Events\" to get exists process \"{name}\""),
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("true"))
            .unwrap_or(false);
        if !running {
            return false;
        }
        std::process::Command::new("osascript")
            .args([
                "-e",
                &format!("tell application \"{name}\" to get player state as text"),
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "playing")
            .unwrap_or(false)
    }

    fn pause(&self, app: PlayerApp) {
        let name = app.process_name();
        let _ = std::process::Command::new("osascript")
            .args(["-e", &format!("tell application \"{name}\" to pause")])
            .output();
    }

    fn resume(&self, app: PlayerApp) {
        let name = app.process_name();
        let _ = std::process::Command::new("osascript")
            .args(["-e", &format!("tell application \"{name}\" to play")])
            .output();
    }
}
