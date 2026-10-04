use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use winit::event_loop::EventLoopProxy;

use crate::core::hotkey::{PttEvent, PushToTalk};
use crate::core::traits::{CleanupProvider, ContextProvider, SpeechRecognizer};

/// Monotonic PTT session counter: every PttDown starts a new session and
/// all lifecycle log lines between that Down and its Up carry the same id,
/// so a wedged recording can be traced end-to-end through the log file.
static SESSION: AtomicU64 = AtomicU64::new(0);

/// Start a new PTT session (called on every PttDown) and return its id.
pub fn next_session() -> u64 {
    SESSION.fetch_add(1, Ordering::Relaxed) + 1
}

/// Current session id (0 before the first press).
pub fn current_session() -> u64 {
    SESSION.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HotkeyPreset {
    /// Bare Right Option — rides the CGEventTap (RegisterEventHotKey cannot
    /// see bare modifiers).
    RightOption,
    /// Bare Fn — rides the CGEventTap.
    Fn,
    #[default]
    CtrlSpace,
}

/// Idle-tooltip hint for the winning preset (no hardcoded hotkey anywhere else).
pub fn preset_hint(preset: HotkeyPreset) -> &'static str {
    match preset {
        HotkeyPreset::RightOption => "hold Right Option",
        HotkeyPreset::Fn => "hold Fn",
        HotkeyPreset::CtrlSpace => "hold Ctrl+Space",
    }
}

pub fn preset_hotkey(preset: HotkeyPreset) -> HotKey {
    match preset {
        HotkeyPreset::RightOption => HotKey::new(None, Code::AltRight),
        HotkeyPreset::Fn => HotKey::new(None, Code::Fn),
        HotkeyPreset::CtrlSpace => HotKey::new(Some(Modifiers::CONTROL), Code::Space),
    }
}

#[derive(Debug)]
pub enum DaemonEvent {
    PttDown,
    PttUp,
    Cancel,
    /// Worker reports that `AudioCapture::start(...)` succeeded.
    ///
    /// Fieldless: ordering is the whole contract (sent immediately after
    /// capture start-ok, before any transcription).
    CaptureStarted,
    /// Safety watchdog fired: the recording ran past the max duration
    /// because PttUp never arrived. The worker force-stopped the mic and is
    /// transcribing what it captured; the app must leave Recording.
    Watchdog {
        duration_ms: u64,
    },
    /// Event-tap health problem needing user attention (tap dead after a
    /// re-enable attempt, permission lost mid-run).
    TapIssue(String),
    Done {
        text: String,
        duration_ms: u64,
        rtf: f64,
    },
    Failed(String),
    /// Cleanup-chain alert (rate-limit exhausted, missing Ollama model):
    /// surfaced as the tray warn-note so the user can act on it.
    CleanupIssue(String),
}

/// Register the preferred preset; fall back when the OS swallows it.
/// macOS rejects single-key holds (AltRight/Fn report "Unknown scancode"),
/// so a CtrlSpace default registers immediately; Fn-only fallback otherwise.
/// Returns the manager (must be kept alive), the hotkey, and which preset won.
pub fn register_ptt_hotkey(
    prefer: HotkeyPreset,
) -> Result<(GlobalHotKeyManager, HotKey, HotkeyPreset), String> {
    // NOTE: if/else (not match) — rustc's dead-code pass does not count a
    // variant as constructed when its only constructor sits inside a match
    // arm on the same enum, which falsely flags `CtrlSpace` under -D warnings.
    // Single-key presets (RightOption/Fn) fall back to WORKING combos, so
    // stale configs still get a usable hotkey at startup.
    let order = if prefer == HotkeyPreset::Fn {
        [HotkeyPreset::Fn, HotkeyPreset::CtrlSpace]
    } else if prefer == HotkeyPreset::CtrlSpace {
        [HotkeyPreset::CtrlSpace, HotkeyPreset::Fn]
    } else {
        [HotkeyPreset::RightOption, HotkeyPreset::CtrlSpace]
    };
    let manager = GlobalHotKeyManager::new().map_err(|e| format!("hotkey manager: {e:?}"))?;
    for preset in order {
        let hk = preset_hotkey(preset);
        match manager.register(hk) {
            Ok(()) => return Ok((manager, hk, preset)),
            Err(e) => tracing::warn!("hotkey register failed for {preset:?}: {e:?}"),
        }
    }
    Err("no push-to-talk hotkey registered".into())
}

/// Register Esc as a second global hotkey on the SAME manager as PTT.
/// winit `device_event` never delivers Key events to a zero-window tray app
/// on macOS (proven Task 3), so Esc must arrive through the hotkey bridge.
pub fn register_cancel_hotkey(manager: &GlobalHotKeyManager) -> Result<HotKey, String> {
    let hk = HotKey::new(None, Code::Escape);
    manager
        .register(hk)
        .map_err(|e| format!("esc hotkey register failed: {e:?}"))?;
    Ok(hk)
}

/// Commands from the winit thread to the dictation worker.
/// The winit thread never blocks: it only `send()`s these and renders
/// `DaemonEvent::Done/Failed` results.
#[derive(Debug)]
pub(crate) enum Control {
    Down,
    Up,
    Cancel,
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Forward global-hotkey presses to the winit loop as `DaemonEvent`s.
/// Runs on its own thread; `receiver().recv()` blocks here, never on winit.
/// Forwards ANY known PTT preset id (not just the startup winner) so a
/// menu-driven hotkey switch needs no bridge restart and loses no events.
/// The Esc id (second hotkey on the same manager) forwards as Cancel on
/// Pressed only — the release is meaningless for a cancel.
pub fn spawn_hotkey_bridge(proxy: EventLoopProxy<DaemonEvent>, esc_id: u32) {
    let ids = [
        preset_hotkey(HotkeyPreset::RightOption).id(),
        preset_hotkey(HotkeyPreset::Fn).id(),
        preset_hotkey(HotkeyPreset::CtrlSpace).id(),
    ];
    std::thread::spawn(move || {
        while let Ok(ev) = GlobalHotKeyEvent::receiver().recv() {
            let out = if ev.id == esc_id {
                match ev.state {
                    HotKeyState::Pressed => Some(DaemonEvent::Cancel),
                    HotKeyState::Released => None,
                }
            } else if ids.contains(&ev.id) {
                Some(match ev.state {
                    HotKeyState::Pressed => DaemonEvent::PttDown,
                    HotKeyState::Released => DaemonEvent::PttUp,
                })
            } else {
                None
            };
            if let Some(out) = out {
                // Session-tagged so combo-preset cycles are traceable in the
                // log file the same way tap-preset cycles are.
                let name = match &out {
                    DaemonEvent::PttDown => "PttDown",
                    DaemonEvent::PttUp => "PttUp",
                    DaemonEvent::Cancel => "Cancel",
                    _ => "other",
                };
                let session = match out {
                    DaemonEvent::PttDown => next_session(),
                    _ => current_session(),
                };
                tracing::info!("[session={session}] hotkey bridge -> {name}");
                if proxy.send_event(out).is_err() {
                    tracing::error!(
                        "[session={session}] hotkey bridge: event loop gone, {name} lost"
                    );
                    break;
                }
            }
        }
        tracing::warn!("hotkey bridge receiver closed — global hotkey events stop here");
    });
}

/// Model-switch-failure semantics (via `stt::transcribe_shared`): a failed
/// new-model load returns Err and keeps the previously loaded model cached,
/// so the caller only needs to warn and can retry with a fixed path.
fn pipeline_on_worker(
    capture: crate::core::audio::AudioCapture,
    duration_ms: u64,
    proxy: &EventLoopProxy<DaemonEvent>,
) {
    let out = capture.stop();
    let mut vad = crate::core::vad::Vad::new();
    let kept = crate::core::vad::transcribe_ready(&out.samples_mono, out.sample_rate, &mut vad);
    tracing::info!(
        "captured {} vad-ready samples ({} raw @ {}Hz)",
        kept.len(),
        out.samples_mono.len(),
        out.sample_rate
    );
    if kept.is_empty() {
        tracing::info!("no speech detected, nothing to transcribe");
        let _ = proxy.send_event(DaemonEvent::Done {
            text: String::new(),
            duration_ms,
            rtf: 0.0,
        });
        return;
    }
    let cfg = crate::core::config::load_config();
    let t0 = std::time::Instant::now();
    // STT via trait (groq→local fallback, same logic).
    let recognizer = crate::core::traits::RouterRecognizer;
    let transcript = match recognizer.transcribe(&kept, crate::core::vad::VAD_SAMPLE_RATE, &cfg) {
        Ok(t) => t,
        Err(e) => {
            let _ = proxy.send_event(DaemonEvent::Failed(format!("transcribe failed: {e}")));
            return;
        }
    };
    // Surface provider warnings as tray alerts (exact pre-task message text).
    for warning in &transcript.warnings {
        let _ = proxy.send_event(DaemonEvent::CleanupIssue(warning.clone()));
    }
    let text = transcript.text;
    let ms = t0.elapsed().as_millis();
    let kept_ms = kept.len() as f64 / crate::core::vad::VAD_SAMPLE_RATE as f64 * 1000.0;
    let rtf = ms as f64 / kept_ms.max(1.0);
    tracing::info!("transcribed in {ms}ms (RTF {rtf:.2})");
    // Cleanup via trait (route decision + context gate + chain).
    let cleanup_provider = crate::core::traits::ChainProvider;
    let context_provider = crate::core::traits::OsascriptContext;
    // Context gate: transcript must carry value AND app must be
    // context-sensitive — otherwise the ~200ms osascript query and the
    // model call are skipped.
    let route = crate::core::analyze::decide_route(&text, &cfg);
    let ctx = match &route {
        crate::core::analyze::CleanupRoute::Llm(a) => {
            let key_present = crate::core::cleanup::groq_key().is_some();
            if a.wants_context() && cfg.cleanup_enabled && cfg.context_enabled && key_present {
                let app = context_provider.focused_app();
                if crate::core::analyze::context_allowed(
                    app.as_deref(),
                    a,
                    cfg.cleanup_enabled,
                    cfg.context_enabled,
                    key_present,
                ) {
                    crate::core::cleanup::synthesize_context(app.as_deref(), &cfg)
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        }
        _ => String::new(),
    };
    let outcome =
        cleanup_provider.clean(&text, if ctx.is_empty() { None } else { Some(&ctx) }, &cfg);
    let cleaned = outcome.text;
    let issues = outcome.issues;
    for issue in &issues {
        let _ = proxy.send_event(DaemonEvent::CleanupIssue(issue.clone()));
    }
    if crate::core::cleanup::is_filler_result(&cleaned) {
        // Filler-only transcript (or "EMPTY" sentinel) — nothing to inject.
        tracing::info!("transcript empty or filler-only after cleanup");
        let _ = proxy.send_event(DaemonEvent::Done {
            text: String::new(),
            duration_ms,
            rtf,
        });
        return;
    }
    let text = cleaned;
    if text.trim().is_empty() {
        let _ = proxy.send_event(DaemonEvent::Done {
            text: String::new(),
            duration_ms,
            rtf,
        });
        return;
    }
    // History BEFORE inject: a paste failure must not lose the transcript.
    if let Err(e) = crate::core::history::push_history(crate::core::history::HistoryEntry {
        text: text.clone(),
        at_ms: now_ms(),
        duration_ms,
        rtf,
    }) {
        tracing::warn!("history push failed: {e}");
    }
    // Inject MUST run on the MAIN thread (user_event Done handler): enigo's
    // HIToolbox keycode mapping is main-queue-only — dispatch_assert_queue
    // traps (EXC_BREAKPOINT) on background threads (crash report 2026-09-30).
    let _ = proxy.send_event(DaemonEvent::Done {
        text,
        duration_ms,
        rtf,
    });
}

trait DaemonEventSender {
    fn send_event(&self, event: DaemonEvent) -> Result<(), ()>;
}

impl DaemonEventSender for EventLoopProxy<DaemonEvent> {
    fn send_event(&self, event: DaemonEvent) -> Result<(), ()> {
        self.send_event(event).map_err(|_| ())
    }
}

fn worker_on_control_down<S: DaemonEventSender>(
    sender: &S,
    ptt: &mut PushToTalk,
    capture: &mut Option<crate::core::audio::AudioCapture>,
    capture_start: &mut Option<Instant>,
) {
    let now = now_ms();
    match ptt.on_key_down(now) {
        PttEvent::Started => {
            // Menu-selected mic, re-read per hold (never cached).
            let mic = crate::core::config::load_config().mic_name;
            match crate::core::audio::AudioCapture::start(mic) {
                Ok(cap) => {
                    *capture = Some(cap);
                    *capture_start = Some(Instant::now());
                    tracing::info!("[session={}] capture started — mic open", current_session());

                    // Emit before any transcription begins.
                    if sender.send_event(DaemonEvent::CaptureStarted).is_err() {
                        tracing::error!(
                            "[session={}] capture-start notice lost — dropping capture",
                            current_session()
                        );
                        // Mirror capture-failure semantics: clear hold
                        // bookkeeping so the NEXT press is a fresh cycle.
                        let _ = ptt.on_cancel();
                        *capture = None;
                        *capture_start = None;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[session={}] capture failed: {e} — recording cannot start",
                        current_session()
                    );
                    *capture = None;
                    *capture_start = None;
                    // Failed leaves the app's Recording phase and
                    // this clears the hold bookkeeping, so the
                    // NEXT press is a fresh cycle (not dead).
                    let _ = ptt.on_cancel();
                    if sender
                        .send_event(DaemonEvent::Failed(format!("capture failed: {e}")))
                        .is_err()
                    {
                        tracing::error!(
                            "[session={}] capture-failure notice lost",
                            current_session()
                        );
                    }
                }
            }
        }
        e => tracing::debug!("ptt down ignored: {e:?}"),
    }
}

/// Safety watchdog (PRD §6): the mic is open but the PttUp that should
/// have stopped it never arrived. Force-stop the capture (mic dies here),
/// warn loudly, transcribe what was captured, and tell the app so the
/// tray leaves Recording. This is a SAFETY NET — a watchdog fire always
/// means the PttUp path broke somewhere above, and the WARN line says so.
fn watchdog(
    ptt: &mut PushToTalk,
    capture: &mut Option<crate::core::audio::AudioCapture>,
    capture_start: &mut Option<Instant>,
    proxy: &EventLoopProxy<DaemonEvent>,
    max_ms: u64,
) {
    let duration_ms = capture_start
        .map(|s| s.elapsed().as_millis() as u64)
        .unwrap_or(max_ms)
        .min(max_ms);
    *capture_start = None;
    // Clear the hold bookkeeping: the key-up will never be processed for
    // this cycle (and a late one is dropped by app-side admission anyway).
    let _ = ptt.on_cancel();
    tracing::warn!(
        "[session={}] WATCHDOG: PttUp LOST — force-stopping recording after {duration_ms}ms (mic would have stayed open forever)",
        current_session()
    );
    match capture.take() {
        Some(cap) => {
            if proxy
                .send_event(DaemonEvent::Watchdog { duration_ms })
                .is_err()
            {
                tracing::error!(
                    "[session={}] watchdog notice lost (event loop gone)",
                    current_session()
                );
            }
            pipeline_on_worker(cap, duration_ms, proxy);
        }
        None => {
            tracing::warn!(
                "[session={}] watchdog fired with no live capture — nothing to stop",
                current_session()
            );
        }
    }
}

/// Worker entry: owns `PushToTalk` + the live `AudioCapture`, so every
/// `Duration`-blocking call (device open, capture stop, model download,
/// transcribe, inject) runs here, never on the winit thread.
///
/// While the mic is open the receive is armed with the watchdog deadline
/// (`WIFLOW_MAX_RECORDING_MS`, default 60s per PRD §6), so a lost PttUp
/// can never leave the microphone running indefinitely.
pub fn worker_main(proxy: EventLoopProxy<DaemonEvent>, rx: mpsc::Receiver<Control>) {
    let mut ptt = PushToTalk::new(300, 60_000);
    let mut capture: Option<crate::core::audio::AudioCapture> = None;
    let mut capture_start: Option<Instant> = None;
    let max_ms: u64 = std::env::var("WIFLOW_MAX_RECORDING_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60_000);
    tracing::info!(
        "[session={}] worker started (recording watchdog max {max_ms}ms)",
        current_session()
    );
    loop {
        // Mic closed → block normally. Mic open → block only until the
        // watchdog deadline; on expiry the watchdog fires instead.
        let ctl = match capture_start {
            Some(start) => {
                let deadline = start + Duration::from_millis(max_ms);
                let now = Instant::now();
                if now >= deadline {
                    watchdog(&mut ptt, &mut capture, &mut capture_start, &proxy, max_ms);
                    continue;
                }
                match rx.recv_timeout(deadline - now) {
                    Ok(c) => c,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        watchdog(&mut ptt, &mut capture, &mut capture_start, &proxy, max_ms);
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match rx.recv() {
                Ok(c) => c,
                Err(_) => break,
            },
        };
        match ctl {
            Control::Down => {
                tracing::info!("[session={}] worker Control::Down", current_session());
                worker_on_control_down(&proxy, &mut ptt, &mut capture, &mut capture_start);
            }
            Control::Up => {
                tracing::info!("[session={}] worker Control::Up", current_session());
                match ptt.on_key_up(now_ms()) {
                    PttEvent::Transcribe { duration_ms } => {
                        capture_start = None;
                        match capture.take() {
                            Some(cap) => {
                                tracing::info!(
                                    "[session={}] capture stop requested ({duration_ms}ms hold)",
                                    current_session()
                                );
                                pipeline_on_worker(cap, duration_ms, &proxy);
                            }
                            None => {
                                tracing::warn!(
                                    "[session={}] no live capture to stop for this Up",
                                    current_session()
                                );
                                if proxy
                                    .send_event(DaemonEvent::Failed(
                                        "no capture for transcribe".into(),
                                    ))
                                    .is_err()
                                {
                                    tracing::error!(
                                        "[session={}] failure notice lost",
                                        current_session()
                                    );
                                }
                            }
                        }
                    }
                    // <300ms discards never touch the model: drop audio, reset tray.
                    PttEvent::DiscardedShort { duration_ms } => {
                        capture = None;
                        capture_start = None;
                        tracing::info!(
                            "[session={}] discarded short hold ({duration_ms}ms)",
                            current_session()
                        );
                        if proxy
                            .send_event(DaemonEvent::Done {
                                text: String::new(),
                                duration_ms,
                                rtf: 0.0,
                            })
                            .is_err()
                        {
                            tracing::error!("[session={}] discard notice lost", current_session());
                        }
                    }
                    e => {
                        // App-side admission (PttMachine) already drops stray
                        // releases before they reach here; this only logs if
                        // one slips through. NEVER fake a Done from here —
                        // that masked the very lost-PttUp bug we're hunting.
                        tracing::warn!(
                            "[session={}] worker Control::Up ignored: {e:?}",
                            current_session()
                        );
                    }
                }
            }
            Control::Cancel => {
                tracing::info!("[session={}] worker Control::Cancel", current_session());
                match ptt.on_cancel() {
                    PttEvent::Cancelled => {
                        capture = None;
                        capture_start = None;
                        tracing::info!(
                            "[session={}] dictation cancelled (Esc) — mic released",
                            current_session()
                        );
                        if proxy
                            .send_event(DaemonEvent::Failed("cancelled (Esc)".into()))
                            .is_err()
                        {
                            tracing::error!("[session={}] cancel notice lost", current_session());
                        }
                    }
                    e => tracing::debug!("cancel ignored: {e:?}"),
                }
            }
        }
    }
    tracing::warn!(
        "[session={}] worker control channel closed — worker exiting",
        current_session()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_started_not_emitted_on_capture_failure() {
        // CI runs without a real mic; still, make the failure deterministic
        // by pointing the config at a known-missing device.
        let home = std::env::temp_dir().join(format!(
            "wiflow-test-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::env::set_var("HOME", &home);

        let support_dir = crate::core::config::app_support_dir();
        std::fs::create_dir_all(&support_dir).unwrap();
        std::fs::write(
            support_dir.join("config.json"),
            r#"{"mic_name":"no-such-device-xyz"}"#,
        )
        .unwrap();

        let cfg = crate::core::config::load_config();
        assert_eq!(cfg.mic_name.as_deref(), Some("no-such-device-xyz"));

        struct FakeSender {
            failed_msg: std::sync::Mutex<Option<String>>,
            capture_started_seen: std::sync::Mutex<bool>,
        }

        impl DaemonEventSender for FakeSender {
            fn send_event(&self, event: DaemonEvent) -> Result<(), ()> {
                match event {
                    DaemonEvent::Failed(msg) => {
                        *self.failed_msg.lock().unwrap() = Some(msg);
                    }
                    DaemonEvent::CaptureStarted => {
                        *self.capture_started_seen.lock().unwrap() = true;
                    }
                    _ => {}
                }
                Ok(())
            }
        }

        let sender = FakeSender {
            failed_msg: Default::default(),
            capture_started_seen: Default::default(),
        };

        let mut ptt = PushToTalk::new(300, 60_000);
        let mut capture: Option<crate::core::audio::AudioCapture> = None;
        let mut capture_start: Option<Instant> = None;
        worker_on_control_down(&sender, &mut ptt, &mut capture, &mut capture_start);

        let failed = sender.failed_msg.lock().unwrap().clone();
        let started = *sender.capture_started_seen.lock().unwrap();
        assert!(failed.is_some());
        let msg = failed.unwrap();
        assert!(msg.starts_with("capture failed:"), "{msg}");
        assert!(!started);
    }

    #[test]
    fn test_presets_are_distinct_ids() {
        let a = preset_hotkey(HotkeyPreset::RightOption);
        let b = preset_hotkey(HotkeyPreset::Fn);
        let c = preset_hotkey(HotkeyPreset::CtrlSpace);
        assert_ne!(a.id(), b.id());
        assert_ne!(a.id(), c.id());
        assert_ne!(b.id(), c.id());
    }
}
