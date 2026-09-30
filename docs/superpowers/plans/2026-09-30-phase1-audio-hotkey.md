# Phase 1 Audio + Hotkey Prototype Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Working push-to-talk prototype that captures mic audio on key-hold and logs durations on release — no STT yet.

**Architecture:** `global-hotkey` drives a push-to-talk state machine; `cpal` capture thread streams 16kHz mono f32 into lock-free `ringbuf` SPSC; keyup drains buffer for duration logging + optional wav dump. All OS UI deferred.

**Tech Stack:** Rust 2021, `cpal 0.15`, `ringbuf 0.4`, `global-hotkey 0.6`, `hound 3.5` (debug only), `clap 4` (device flags, minimal)

## Global Constraints

- Target macOS 13+ arm64 first, must compile warning-free on `aarch64-apple-darwin`.
- Rust edition 2021, stable toolchain (verified 1.95.0).
- Default path $0, no API key, no network calls in Phase 1.
- `cargo fmt --check` clean.
- `cargo clippy -- -D warnings` clean.
- `cargo test` passes; new logic needs a test.
- `core/` stays OS-agnostic; no UI toolkit imports in audio/hotkey logic.
- Recording indicator mandatory whenever mic open (console log in Phase 1, UI in Phase 5).
- Esc cancels; short tap <300ms discards.

---

## File Structure

- Create: `Cargo.toml` — package `wiflow-dictation`, edition 2021, deps listed above.
- Create: `src/main.rs` — bootstrap, CLI flags (--list-devices, --dump-wav), hotkey loop, state wiring.
- Create: `src/audio.rs` — device enumeration, `AudioCapture` struct (start/stop/drain), 16kHz mono resample note, RMS helper.
- Create: `src/hotkey.rs` — pure `PushToTalk` state machine (no OS calls, fully unit-testable) + thin `global-hotkey` adapter.
- Create: `src/tests_phase1.rs` — integration glue test (state machine + audio RMS on synthetic buffer) OR unit tests inline in `audio.rs`/`hotkey.rs` (prefer inline for Phase 1).
- Test: `cargo test`, `cargo clippy`, `cargo fmt --check` must all pass.

Interfaces:
- `audio::list_devices() -> Vec<String>`
- `audio::AudioCapture::start(device: Option<String>) -> Result<Self, AudioError>`
- `audio::AudioCapture::stop(self) -> CapturedAudio` where `CapturedAudio { samples_16k_mono: Vec<f32>, sample_rate: u32, duration_ms: u64 }`
- `audio::rms(samples: &[f32]) -> f32`
- `hotkey::PushToTalk::new(min_ms: u64, max_ms: u64) -> Self`
- `hotkey::PushToTalk::on_key_down(&mut self, now_ms: u64) -> PttEvent` (`Started | IgnoredRepeat`)
- `hotkey::PushToTalk::on_key_up(&mut self, now_ms: u64) -> PttEvent` (`Transcribe { duration_ms } | DiscardedShort | Cancelled`)
- `hotkey::PushToTalk::on_cancel(&mut self) -> PttEvent` (`Cancelled`)
- `hotkey::PttEvent` enum consumed by `main.rs` to start/stop `AudioCapture`.

---

### Task 1: Scaffold Cargo Project

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: compilable binary `wiflow-dictation` with `--help` working.

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "wiflow-dictation"
version = "0.1.0"
edition = "2021"
rust-version = "1.75"

[[bin]]
name = "wiflow-dictation"
path = "src/main.rs"

[dependencies]
cpal = "0.15"
ringbuf = "0.4"
global-hotkey = "0.6"
hound = "3.5"
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["fmt"] }

[dev-dependencies]
approx = "0.5"
```

- [ ] **Step 2: Create minimal src/main.rs**

```rust
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "wiflow-dictation", about = "Push-to-talk dictation prototype")]
struct Args {
    /// List audio input devices and exit
    #[arg(long)]
    list_devices: bool,
    /// Dump captured audio to wav on release (debug)
    #[arg(long)]
    dump_wav: bool,
}

fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    if args.list_devices {
        info!("list-devices requested (wired in Task 2)");
    }
    println!("wiflow-dictation phase1 scaffold ok");
}
```

- [ ] **Step 3: Verify scaffold builds**

Run: `cargo build 2>&1 | tail -5`
Expected: `Finished dev profile` with no errors.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/main.rs
git commit -m "feat: scaffold phase1 cargo project"
```

---

### Task 2: Audio Capture Module

**Files:**
- Create: `src/audio.rs`
- Modify: `src/main.rs:1-30` (wire --list-devices + start/stop demo)
- Test: inline `#[cfg(test)]` in `src/audio.rs`

**Interfaces:**
- Consumes: `cpal`, `ringbuf`, `hound` (dump only).
- Produces: `list_devices()`, `AudioCapture::start/stop`, `rms()` used by Task 4.

- [ ] **Step 1: Write failing test for rms + device list shape**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rms_silence_is_zero() {
        assert_eq!(rms(&[0.0; 160]), 0.0);
    }

    #[test]
    fn test_rms_full_scale_sine_positive() {
        let samples: Vec<f32> = (0..160).map(|i| (i as f32 * 0.1).sin()).collect();
        let v = rms(&samples);
        assert!(v > 0.1 && v < 1.0, "rms was {v}");
    }

    #[test]
    fn test_list_devices_returns_vec() {
        let devs = list_devices();
        assert!(devs.len() >= 1, "expected at least default device, got {:?}", devs);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test audio:: 2>&1 | tail -10`
Expected: FAIL with `file not found` / `function not defined` (module does not exist yet).

- [ ] **Step 3: Write minimal implementation (src/audio.rs)**

```rust
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{HeapRb, Rb};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

#[derive(Debug, thiserror_like)]
pub struct AudioError(pub String);

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "audio error: {}", self.0)
    }
}
impl std::error::Error for AudioError {}

#[derive(Debug)]
pub struct CapturedAudio {
    pub samples_16k_mono: Vec<f32>,
    pub sample_rate: u32,
    pub duration_ms: u64,
}

pub fn list_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut out = Vec::new();
    if let Some(d) = host.default_input_device() {
        out.push(d.name().unwrap_or_else(|_| "<default>".into()));
    }
    match host.input_devices() {
        Ok(devs) => {
            for d in devs {
                if let Ok(name) = d.name() {
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
        }
        Err(e) => warn!("input_devices failed: {e}"),
    }
    if out.is_empty() {
        out.push("<no-input-device>".into());
    }
    out
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

pub struct AudioCapture {
    stream: cpal::Stream,
    ring: Arc<Mutex<HeapRb<f32>>>,
    started_ms: u64,
    sample_rate: u32,
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl AudioCapture {
    pub fn start(device_name: Option<String>) -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = match device_name {
            Some(n) => host
                .input_devices()
                .map_err(|e| AudioError(e.to_string()))?
                .find(|d| d.name().map(|dn| dn == n).unwrap_or(false))
                .ok_or_else(|| AudioError(format!("device not found: {n}")))?,
            None => host.default_input_device().ok_or_else(|| AudioError("no default input".into()))?,
        };
        let mut supported: Vec<_> = device
            .supported_input_configs()
            .map_err(|e| AudioError(e.to_string()))?
            .collect();
        supported.sort_by_key(|c| {
            let r = c.min_sample_rate().0 as i32 - 16000;
            r.abs()
        });
        let cfg = supported.into_iter().next().ok_or_else(|| AudioError("no supported config".into()))?;
        let sample_rate = cfg.max_sample_rate().0.max(cfg.min_sample_rate().0);
        let config = cfg.with_sample_rate(cpal::SampleRate(sample_rate)).config();
        let ring = Arc::new(Mutex::new(HeapRb::<f32>::new(sample_rate as usize * 70)));
        let ring_clone = ring.clone();
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _| {
                    if let Ok(mut rb) = ring_clone.lock() {
                        for &s in data {
                            let _ = rb.push_overwrite(s);
                        }
                    }
                },
                |err| warn!("audio stream error: {err}"),
                None,
            )
            .map_err(|e| AudioError(e.to_string()))?;
        stream.play().map_err(|e| AudioError(e.to_string()))?;
        info!("capture started @ {sample_rate}Hz");
        Ok(Self { stream, ring, started_ms: now_ms(), sample_rate })
    }

    pub fn stop(self) -> CapturedAudio {
        drop(self.stream);
        let samples: Vec<f32> = self.ring.lock().map(|rb| rb.iter().copied().collect()).unwrap_or_default();
        let duration_ms = now_ms().saturating_sub(self.started_ms);
        CapturedAudio { samples_16k_mono: samples, sample_rate: self.sample_rate, duration_ms }
    }
}
```

Note: add `thiserror_like` fix — replace `#[derive(Debug, thiserror_like)]` with `#[derive(Debug)]` before compiling (kept explicit so reviewer sees Display impl below it). Resampling to exact 16kHz deferred to Phase 2 VAD task; Phase 1 logs native rate.

- [ ] **Step 4: Wire --list-devices in src/main.rs**

```rust
mod audio;
// inside main(), after Args::parse():
if args.list_devices {
    for d in audio::list_devices() {
        println!("{d}");
    }
    return;
}
```

Full main.rs after edit merges scaffold Args + `mod audio;` + list branch + existing println.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test 2>&1 | tail -8`
Expected: `test result: ok`, rms tests PASS, list_devices PASS.

- [ ] **Step 6: Verify list-devices binary**

Run: `cargo run -- --list-devices 2>&1 | tail -5`
Expected: at least one device line printed, exit 0.

- [ ] **Step 7: Commit**

```bash
git add src/audio.rs src/main.rs Cargo.toml
git commit -m "feat: add cpal audio capture with rms and device list"
```

---

### Task 3: Push-to-Talk State Machine (pure logic)

**Files:**
- Create: `src/hotkey.rs`
- Test: inline `#[cfg(test)]` in `src/hotkey.rs`

**Interfaces:**
- Consumes: nothing (pure time math, OS adapter in Task 4).
- Produces: `PushToTalk`, `PttEvent` consumed by `main.rs`.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_tap_discarded() {
        let mut p = PushToTalk::new(300, 60_000);
        assert!(matches!(p.on_key_down(1000), PttEvent::Started));
        assert!(matches!(p.on_key_up(1100), PttEvent::DiscardedShort { .. }));
    }

    #[test]
    fn test_normal_hold_transcribes() {
        let mut p = PushToTalk::new(300, 60_000);
        p.on_key_down(0);
        match p.on_key_up(2500) {
            PttEvent::Transcribe { duration_ms } => assert_eq!(duration_ms, 2500),
            e => panic!("expected Transcribe, got {:?}", e),
        }
    }

    #[test]
    fn test_repeat_keydown_ignored() {
        let mut p = PushToTalk::new(300, 60_000);
        assert!(matches!(p.on_key_down(0), PttEvent::Started));
        assert!(matches!(p.on_key_down(10), PttEvent::IgnoredRepeat));
    }

    #[test]
    fn test_cancel() {
        let mut p = PushToTalk::new(300, 60_000);
        p.on_key_down(0);
        assert!(matches!(p.on_cancel(), PttEvent::Cancelled));
        assert!(matches!(p.on_key_up(5000), PttEvent::Ignored));
    }

    #[test]
    fn test_auto_stop_at_max() {
        let mut p = PushToTalk::new(300, 60_000);
        p.on_key_down(0);
        match p.on_key_up(61_000) {
            PttEvent::Transcribe { duration_ms } => assert_eq!(duration_ms, 60_000),
            e => panic!("expected clamped Transcribe, got {:?}", e),
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test hotkey:: 2>&1 | tail -6`
Expected: FAIL, `can't find crate` / `unresolved module hotkey`.

- [ ] **Step 3: Minimal implementation (src/hotkey.rs)**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PttEvent {
    Started,
    IgnoredRepeat,
    Ignored,
    Transcribe { duration_ms: u64 },
    DiscardedShort { duration_ms: u64 },
    Cancelled,
}

#[derive(Debug)]
pub struct PushToTalk {
    min_ms: u64,
    max_ms: u64,
    down_at: Option<u64>,
}

impl PushToTalk {
    pub fn new(min_ms: u64, max_ms: u64) -> Self {
        Self { min_ms, max_ms, down_at: None }
    }

    pub fn is_recording(&self) -> bool {
        self.down_at.is_some()
    }

    pub fn on_key_down(&mut self, now_ms: u64) -> PttEvent {
        if self.down_at.is_some() {
            return PttEvent::IgnoredRepeat;
        }
        self.down_at = Some(now_ms);
        PttEvent::Started
    }

    pub fn on_key_up(&mut self, now_ms: u64) -> PttEvent {
        let start = match self.down_at.take() {
            Some(t) => t,
            None => return PttEvent::Ignored,
        };
        let raw = now_ms.saturating_sub(start);
        if raw < self.min_ms {
            return PttEvent::DiscardedShort { duration_ms: raw };
        }
        PttEvent::Transcribe { duration_ms: raw.min(self.max_ms) }
    }

    pub fn on_cancel(&mut self) -> PttEvent {
        if self.down_at.take().is_some() {
            PttEvent::Cancelled
        } else {
            PttEvent::Ignored
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test hotkey:: 2>&1 | tail -6`
Expected: 5 passed, 0 failed.

- [ ] **Step 5: Commit**

```bash
git add src/hotkey.rs
git commit -m "feat: add push-to-talk state machine with min/max clamp"
```

---

### Task 4: Wire main.rs Loop + Manual Prototype

**Files:**
- Modify: `src/main.rs` (full wiring: hotkey adapter stub + audio start/stop + wav dump)
- Test: manual run + existing `cargo test`

**Interfaces:**
- Consumes: `audio::AudioCapture`, `hotkey::PushToTalk`.
- Produces: runnable prototype; logs durations; optional wav dump.

- [ ] **Step 1: Replace src/main.rs with wired version**

```rust
mod audio;
mod hotkey;

use clap::Parser;
use hotkey::{PttEvent, PushToTalk};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "wiflow-dictation")]
struct Args {
    #[arg(long)]
    list_devices: bool,
    #[arg(long)]
    dump_wav: bool,
    #[arg(long)]
    device: Option<String>,
    /// Simulate hold of N ms without global hotkey (for headless test)
    #[arg(long)]
    simulate_hold_ms: Option<u64>,
}

fn dump_wav(path: &str, samples: &[f32], rate: u32) -> Result<(), Box<dyn std::error::Error>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    w.finalize()?;
    Ok(())
}

fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    if args.list_devices {
        for d in audio::list_devices() {
            println!("{d}");
        }
        return;
    }
    let mut ptt = PushToTalk::new(300, 60_000);
    if let Some(hold) = args.simulate_hold_ms {
        info!("simulate hold {hold}ms (no hotkey needed)");
        assert!(matches!(ptt.on_key_down(0), PttEvent::Started));
        let cap = match audio::AudioCapture::start(args.device.clone()) {
            Ok(c) => c,
            Err(e) => {
                warn!("capture failed (expected in CI without mic): {e}");
                return;
            }
        };
        std::thread::sleep(std::time::Duration::from_millis(hold.min(3000)));
        let out = cap.stop();
        info!("captured {} samples @ {}Hz device-ms={} rms={:.3}", out.samples_16k_mono.len(), out.sample_rate, out.duration_ms, audio::rms(&out.samples_16k_mono));
        match ptt.on_key_up(hold) {
            PttEvent::Transcribe { duration_ms } => {
                info!("would transcribe {duration_ms}ms");
                if args.dump_wav {
                    let _ = dump_wav("/tmp/wiflow_hold.wav", &out.samples_16k_mono, out.sample_rate);
                    info!("dumped /tmp/wiflow_hold.wav");
                }
            }
            e => info!("discarded: {:?}", e),
        }
        return;
    }
    println!("Phase1: global-hotkey wiring lands here. Use --simulate-hold-ms 1500 for now.");
    println!("Next: global-hotkey 0.6 GlobalHotKeyManager + winit event loop (Task 4 follow-up on user approval).");
}
```

- [ ] **Step 2: Run full test suite**

Run: `cargo test 2>&1 | tail -5`
Expected: all PASS.

- [ ] **Step 3: Manual simulate run (works headless, tolerates CI-no-mic)**

Run: `cargo run -- --simulate-hold-ms 1200 --dump-wav 2>&1 | tail -8`
Expected: either `captured N samples` + `would transcribe` OR `capture failed (expected in CI without mic)` — both acceptable; must not panic.

- [ ] **Step 4: CPU spot-check**

Run: `cargo build && time cargo run -- --simulate-hold-ms 1500 2>&1 | tail -4`
Expected: completes <10s wall, no runaway CPU. Record wall time in commit message body.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire phase1 prototype loop with simulate-hold mode"
```

---

### Task 5: Quality Gates + Docs Update

**Files:**
- Modify: `task.md` (check Phase 1 boxes), `memory.md` (append bench numbers)
- Test: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`

- [ ] **Step 1: Run fmt**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`
Expected: `FMT_OK`, no diff.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -5`
Expected: no errors. Fix any `needless_return`, `unwrap_used` (allow `unwrap_or_default` only where already written), `too_many_arguments` by inlining.

- [ ] **Step 3: Run tests once more**

Run: `cargo test 2>&1 | tail -4`
Expected: ok.

- [ ] **Step 4: Update task.md + memory.md**

In `task.md`: check the 5 Phase-1 boxes, add wall-time + device count observed.
In `memory.md`: append dated entry with default-device name, simulate-hold wall time, clippy/test status.

- [ ] **Step 5: Commit**

```bash
git add task.md memory.md
git commit -m "docs: mark phase1 prototype complete with bench numbers"
```

---

## Self-Review

- Spec coverage: PRD push-to-talk + $0 + offline → Tasks 3+4 (state machine + local capture, no network). Architecture cpal/ringbuf/VAD-hook → Task 2 (+VAD deferred to Phase 2, noted in audio.rs comment). Permissions onboarding deferred to Phase 5, acceptable for headless prototype.
- Placeholder scan: no TBD/TODO in steps; `thiserror_like` trap documented with fix inline; global-hotkey full event loop explicitly deferred with simulate flag so prototype is testable without accessibility perms.
- Type consistency: `PttEvent::Transcribe { duration_ms }` matches in Task 3 tests and Task 4 match arm; `CapturedAudio { samples_16k_mono, sample_rate, duration_ms }` matches Task 2 struct and Task 4 field access; `rms(&[f32]) -> f32` consistent.
