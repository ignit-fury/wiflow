use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use serde::{Deserialize, Serialize};
use std::sync::mpsc;
use winit::event_loop::EventLoopProxy;

use crate::hotkey::{PttEvent, PushToTalk};

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
    Done {
        text: String,
        duration_ms: u64,
        rtf: f64,
    },
    Failed(String),
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
                if proxy.send_event(out).is_err() {
                    break;
                }
            }
        }
    });
}

/// Model-switch-failure semantics (via `stt::transcribe_shared`): a failed
/// new-model load returns Err and keeps the previously loaded model cached,
/// so the caller only needs to warn and can retry with a fixed path.
fn pipeline_on_worker(
    capture: crate::audio::AudioCapture,
    duration_ms: u64,
    proxy: &EventLoopProxy<DaemonEvent>,
) {
    let out = capture.stop();
    let mut vad = crate::vad::Vad::new();
    let kept = crate::vad::transcribe_ready(&out.samples_mono, out.sample_rate, &mut vad);
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
    let model_path = match ensure_model_for_config() {
        Ok(p) => p,
        Err(e) => {
            let _ = proxy.send_event(DaemonEvent::Failed(format!("model unavailable: {e}")));
            return;
        }
    };
    let t0 = std::time::Instant::now();
    let text = match crate::stt::transcribe_shared(&model_path, &kept, &crate::stt::read_prompt()) {
        Ok(t) => t,
        Err(e) => {
            let _ = proxy.send_event(DaemonEvent::Failed(format!("transcribe failed: {e}")));
            return;
        }
    };
    let ms = t0.elapsed().as_millis();
    let kept_ms = kept.len() as f64 / crate::vad::VAD_SAMPLE_RATE as f64 * 1000.0;
    let rtf = ms as f64 / kept_ms.max(1.0);
    tracing::info!("transcribed in {ms}ms (RTF {rtf:.2})");
    // LLM cleanup (literal dictation cleanup layer, Ollama local $0): skips
    // instantly when Ollama is unreachable — deterministic output stands.
    let cfg = crate::config::load_config();
    let cleaned = crate::cleanup::clean(
        &text,
        cfg.cleanup_enabled,
        &cfg.cleanup_model,
        crate::cleanup::DEFAULT_ENDPOINT,
    );
    if crate::cleanup::is_filler_result(&cleaned) {
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
    if let Err(e) = crate::history::push_history(crate::history::HistoryEntry {
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

/// Model variant follows the live menu config (read per cycle, never cached):
/// SmallEn downloads small.en on first use, BaseEn uses base.en.
fn ensure_model_for_config() -> Result<std::path::PathBuf, String> {
    match crate::config::load_config().model {
        crate::config::ModelChoice::TinyEn => crate::stt::ensure_model_variant("tiny"),
        crate::config::ModelChoice::SmallEn => crate::stt::ensure_model_variant("small"),
        crate::config::ModelChoice::BaseEn => crate::stt::ensure_model_variant("base"),
    }
}
/// Worker entry: owns `PushToTalk` + the live `AudioCapture`, so every
/// `Duration`-blocking call (device open, capture stop, model download,
/// transcribe, inject) runs here, never on the winit thread.
pub fn worker_main(proxy: EventLoopProxy<DaemonEvent>, rx: mpsc::Receiver<Control>) {
    let mut ptt = PushToTalk::new(300, 60_000);
    let mut capture: Option<crate::audio::AudioCapture> = None;
    for ctl in rx {
        match ctl {
            Control::Down => match ptt.on_key_down(now_ms()) {
                PttEvent::Started => {
                    // Menu-selected mic, re-read per hold (never cached).
                    let mic = crate::config::load_config().mic_name;
                    match crate::audio::AudioCapture::start(mic) {
                        Ok(cap) => capture = Some(cap),
                        Err(e) => {
                            tracing::warn!("capture failed: {e}");
                            capture = None;
                            let _ = proxy
                                .send_event(DaemonEvent::Failed(format!("capture failed: {e}")));
                        }
                    }
                }
                e => tracing::debug!("ptt down ignored: {e:?}"),
            },
            Control::Up => match ptt.on_key_up(now_ms()) {
                PttEvent::Transcribe { duration_ms } => match capture.take() {
                    Some(cap) => pipeline_on_worker(cap, duration_ms, &proxy),
                    None => {
                        let _ = proxy
                            .send_event(DaemonEvent::Failed("no capture for transcribe".into()));
                    }
                },
                // <300ms discards never touch the model: drop audio, reset tray.
                PttEvent::DiscardedShort { duration_ms } => {
                    capture = None;
                    tracing::info!("discarded short hold ({duration_ms}ms)");
                    let _ = proxy.send_event(DaemonEvent::Done {
                        text: String::new(),
                        duration_ms,
                        rtf: 0.0,
                    });
                }
                e => {
                    capture = None;
                    tracing::debug!("ptt up ignored: {e:?}");
                    // Reset the tray: the winit side optimistically shows
                    // Transcribing on every PttUp, so a stray release (no
                    // prior press) must still resolve, never stick.
                    let _ = proxy.send_event(DaemonEvent::Done {
                        text: String::new(),
                        duration_ms: 0,
                        rtf: 0.0,
                    });
                }
            },
            Control::Cancel => match ptt.on_cancel() {
                PttEvent::Cancelled => {
                    capture = None;
                    tracing::info!("dictation cancelled (Esc)");
                    let _ = proxy.send_event(DaemonEvent::Failed("cancelled (Esc)".into()));
                }
                e => tracing::debug!("cancel ignored: {e:?}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
