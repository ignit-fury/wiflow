# Phase 2 VAD + Audio Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harden audio capture (clock, rate, lock-free, format negotiation) and add VAD silence-trimming wired into the prototype loop.

**Architecture:** Fix `audio.rs` flaws first (Tasks 1–3, each independently testable), then add `vad.rs` (linear resample to 16k + `webrtc-vad` 30ms trim with 200ms padding) on the stabilized API, then wire VAD into `--simulate-hold-ms` and drop the unused `global-hotkey` stub dep.

**Tech Stack:** Rust 2021, `cpal 0.15`, `ringbuf 0.4.8` (`Split`/`Producer`/`Consumer` traits — semantics verified against vendored source), `webrtc-vad 0.4` (`Vad::new_with_rate_and_mode`, `is_voice_segment(&mut [i16;480]) -> Result<bool,()>` — verified against vendored source), `hound 3.5`, `clap 4`

## Global Constraints

- Target macOS 13+ arm64 first, must compile warning-free on `aarch64-apple-darwin`.
- Rust edition 2021, stable toolchain (verified 1.95.0).
- Default path $0, no API key, no network calls in Phase 2.
- `cargo fmt --check` clean.
- `cargo clippy --all-targets -- -D warnings` clean.
- `cargo test` passes; new logic needs a test.
- `core/` stays OS-agnostic (no new modules yet — still single-crate prototype).
- Recording indicator mandatory whenever mic open (console log in prototype).
- Esc cancels; short tap <300ms discards (unchanged PTT rules).

---

## File Structure

- Modify: `src/audio.rs` — Tasks 1–3 (clock, rate clamp, rename, poison warn, lock-free split, format negotiation, conversion helpers).
- Modify: `src/hotkey.rs` — Task 1 only (one added edge-case test, no logic change).
- Create: `src/vad.rs` — Task 4 (`resample_to_16k`, `Vad::new`, `Vad::trim_silence`, constants, tests).
- Modify: `src/main.rs` — Task 1 (field renames, measured clock, dump error log), Task 5 (VAD wiring, stub text update).
- Modify: `Cargo.toml` — Task 4 (add `webrtc-vad = "0.4"`), Task 5 (remove `global-hotkey = "0.6"` stub dep).
- Modify: `task.md`, `memory.md` — Task 5 (check Phase 2 boxes, dated entry).

Interfaces:
- `audio::CapturedAudio { samples_mono: Vec<f32>, sample_rate: u32, duration_ms: u64 }` (renamed in Task 1; native device rate until resample).
- `audio::AudioCapture::start(Option<String>) -> Result<Self, AudioError>` / `stop(self) -> CapturedAudio` (signatures unchanged).
- `audio::i16_to_f32(i16) -> f32`, `audio::u16_to_f32(u16) -> f32` (new in Task 3, pure, unit-tested).
- `vad::VAD_SAMPLE_RATE: u32 = 16_000`, `vad::FRAME_SAMPLES: usize = 480`, `vad::PAD_FRAMES: usize = 7`.
- `vad::resample_to_16k(&[f32], u32) -> Vec<f32>` (Task 4, pure, unit-tested).
- `vad::Vad::new() -> Self`, `vad::Vad::trim_silence(&mut self, &[f32]) -> Vec<f32>` (Task 4; input must already be 16kHz mono).
- `hotkey::PushToTalk` / `PttEvent` unchanged (Task 1 adds one test only).

---

### Task 1: Audio Hardening — Clock, Rate, Rename, Small Fixes

**Files:**
- Modify: `src/audio.rs:10-25,59-72,74-141,143-169`
- Modify: `src/hotkey.rs` (append one test in existing `#[cfg(test)]` module)
- Modify: `src/main.rs:49,57-76` (field renames, measured clock, dump error log)
- Test: inline `#[cfg(test)]` in `src/audio.rs`, `src/hotkey.rs`

**Interfaces:**
- Consumes: `cpal`, `ringbuf` (unchanged usage this task).
- Produces: renamed `CapturedAudio::samples_mono`, `Instant`-based `duration_ms`, clamped sample rate, conversion-ready structure for Task 3.

- [ ] **Step 1: Write the failing tests (append to existing test modules)**

In `src/audio.rs` tests module, append:

```rust
#[test]
fn test_start_bogus_device_is_err() {
    assert!(AudioCapture::start(Some("no-such-device-xyz".into())).is_err());
}
```

In `src/hotkey.rs` tests module, append:

```rust
#[test]
fn test_stray_key_up_ignored() {
    let mut p = PushToTalk::new(300, 60_000);
    assert!(matches!(p.on_key_up(5000), PttEvent::Ignored));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test test_start_bogus_device_is_err test_stray_key_up_ignored 2>&1 | tail -6`
Expected: FAIL with `no function or associated item named start` / `can't find crate hotkey` — more precisely `AudioCapture::start` exists already (Task 2 built it), so the bogus-device test may PASS immediately (it asserts Err, and bogus name always errors). The stray-key-up test FAILS (no `hotkey` module wiring? `mod hotkey` exists since Task 3). Honest expectation: bogus-device test passes on first run (documents headless-safe Err path — keep it as regression test), stray-key-up FAILS only if `Ignored` variant handling is wrong — it exists too. Both may pass immediately; that is acceptable RED-evidence-substitute ONLY if the implementer first demonstrates a deliberate break: temporarily change `PttEvent::Ignored` arm to `Transcribe` and show the stray test failing, then revert. Report both runs.

- [ ] **Step 3: Apply audio.rs edits (exact replacements)**

Replace lines 6–7 (`use std::sync::{Arc, Mutex};`) — keep for this task (Mutex still used; removed in Task 2). No change.

Replace the `CapturedAudio` struct (lines 20–25) with:

```rust
#[derive(Debug)]
pub struct CapturedAudio {
    /// Mono samples at NATIVE device rate (see `sample_rate`).
    /// Resampling to 16kHz happens in `vad::resample_to_16k`, not here.
    pub samples_mono: Vec<f32>,
    pub sample_rate: u32,
    pub duration_ms: u64,
}
```

Replace the `AudioCapture` struct + `now_ms` (lines 59–72) with:

```rust
pub struct AudioCapture {
    stream: cpal::Stream,
    ring: Arc<Mutex<HeapRb<f32>>>,
    started: std::time::Instant,
    sample_rate: u32,
}
```

Delete the entire `fn now_ms()` (lines 66–72).

Replace rate selection (line 99):

```rust
let min_rate = cfg.min_sample_rate().0;
let max_rate = cfg.max_sample_rate().0;
// Clamp to 16kHz: 192kHz devices blew the 70s ringbuf to ~53MB (Phase 1 review).
let sample_rate = 16_000.clamp(min_rate, max_rate);
```

Replace `started_ms: now_ms(),` (line 122) with:

```rust
started: std::time::Instant::now(),
```

Replace `stop()` drain (lines 127–140) with:

```rust
pub fn stop(self) -> CapturedAudio {
    drop(self.stream);
    let samples: Vec<f32> = match self.ring.lock() {
        Ok(rb) => rb.iter().copied().collect(),
        Err(e) => {
            warn!("audio ring lock poisoned, returning empty capture: {e}");
            Vec::new()
        }
    };
    let duration_ms = self.started.elapsed().as_millis() as u64;
    CapturedAudio {
        samples_mono: samples,
        sample_rate: self.sample_rate,
        duration_ms,
    }
}
```

Replace the len_zero test (lines 159–168) with:

```rust
#[test]
fn test_list_devices_returns_vec() {
    let devs = list_devices();
    assert!(
        !devs.is_empty(),
        "expected at least default device, got {:?}",
        devs
    );
}
```

Delete the `#[allow(clippy::len_zero)]` line entirely.

- [ ] **Step 4: Apply main.rs edits (exact replacements)**

Line 49 stays unchanged (`assert!(matches!(ptt.on_key_down(0), PttEvent::Started));` — spec-mandated, flagged Minor for event-loop follow-up).

Replace lines 57–79 with:

```rust
std::thread::sleep(std::time::Duration::from_millis(hold.min(3000)));
let out = cap.stop();
info!(
    "captured {} samples @ {}Hz device-ms={} rms={:.3}",
    out.samples_mono.len(),
    out.sample_rate,
    out.duration_ms,
    audio::rms(&out.samples_mono)
);
// Use the MEASURED clock, not the requested hold: sleep bound (3000ms) and
// stream-setup latency diverge from `hold` (Phase 1 review).
match ptt.on_key_up(out.duration_ms) {
    PttEvent::Transcribe { duration_ms } => {
        info!("would transcribe {duration_ms}ms");
        if args.dump_wav {
            match dump_wav("/tmp/wiflow_hold.wav", &out.samples_mono, out.sample_rate) {
                Ok(()) => info!("dumped /tmp/wiflow_hold.wav"),
                Err(e) => warn!("wav dump failed: {e}"),
            }
        }
    }
    e => info!("discarded: {:?}", e),
}
```

- [ ] **Step 5: Run gates**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`
Expected: `FMT_OK`.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings.

Run: `cargo test 2>&1 | grep -E "test result|FAILED|passed"`
Expected: `ok. 10 passed` (8 existing + 2 new), 0 failed.

Run: `cargo run -- --simulate-hold-ms 1200 2>&1 | tail -3`
Expected: `captured N samples` + `would transcribe ~13xxms` (measured clock ≈ hold + setup), no panic. Headless CI without mic: clean `capture failed` warn, no panic — both acceptable.

- [ ] **Step 6: Commit**

```bash
git add src/audio.rs src/hotkey.rs src/main.rs
git commit -m "feat: harden audio clock, rate clamp, field rename, edge tests"
```

---

### Task 2: Lock-Free Capture Ring

**Files:**
- Modify: `src/audio.rs:1-7,59-64,101-125` (imports, struct, start/stop internals)
- Test: existing suite (no API change — `start`/`stop` signatures unchanged)

**Interfaces:**
- Consumes: `ringbuf::{traits::{Consumer, Producer, Split}, HeapCons, HeapProd, HeapRb}` — all verified present at these paths in vendored ringbuf 0.4.8 (`pub use alias::*` re-exports `HeapProd`/`HeapCons`; `Split::split(self) -> (Prod, Cons)`; `Producer::try_push` returns `Err(elem)` when full; `Consumer::pop_iter` drains).
- Produces: same `AudioCapture` public API; callback never blocks.

- [ ] **Step 1: State the invariant (no failing test — refactor with identical API)**

This task changes internals only; public signatures are byte-identical, so the existing 4 audio tests + full suite are the regression net. No new test required. Record in report: `cargo test` before (10 passed) and after (10 passed).

- [ ] **Step 2: Apply exact replacements**

Replace imports (lines 1–7) with:

```rust
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{
    traits::{Consumer, Producer, Split},
    HeapCons, HeapProd, HeapRb,
};
use tracing::{info, warn};
```

Replace struct (lines 59–64) with:

```rust
pub struct AudioCapture {
    stream: cpal::Stream,
    consumer: HeapCons<f32>,
    started: std::time::Instant,
    sample_rate: u32,
}
```

Replace ring creation + callback (lines 101–115) with:

```rust
// Split halves: producer owns the write side, so the realtime callback
// never locks. Capacity 70s exceeds the 60s PTT auto-stop, therefore
// drop-newest on full is unreachable in practice (and preferable to
// blocking the audio thread).
let (mut producer, consumer) = HeapRb::<f32>::new(sample_rate as usize * 70).split();
let stream = device
    .build_input_stream(
        &config,
        move |data: &[f32], _| {
            for &s in data {
                let _ = producer.try_push(s);
            }
        },
        |err| warn!("audio stream error: {err}"),
        None,
    )
    .map_err(|e| AudioError(e.to_string()))?;
```

Replace constructor return (lines 119–124) with:

```rust
Ok(Self {
    stream,
    consumer,
    started: std::time::Instant::now(),
    sample_rate,
})
```

Replace `stop()` (Task 1 version) with:

```rust
pub fn stop(mut self) -> CapturedAudio {
    drop(self.stream);
    let samples: Vec<f32> = self.consumer.pop_iter().collect();
    let duration_ms = self.started.elapsed().as_millis() as u64;
    CapturedAudio {
        samples_mono: samples,
        sample_rate: self.sample_rate,
        duration_ms,
    }
}
```

Note: `stop(mut self)` — `pop_iter` takes `&mut self`, and `self.consumer` is owned; `mut self` binding allows the call. If the compiler rejects field mutation through `mut self` after partial move of nothing — it will not; `drop(self.stream)` first is fine since other fields remain.

- [ ] **Step 3: Run gates**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`
Expected: `FMT_OK`.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings. If `unused_imports` fires for `Producer` (only `try_push` used — trait must be in scope, so it is used) — keep imports as written.

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: `ok. 10 passed`, 0 failed.

Run: `cargo run -- --simulate-hold-ms 1200 2>&1 | tail -2`
Expected: captured samples + would-transcribe (or clean no-mic warn), no panic.

- [ ] **Step 4: Commit**

```bash
git add src/audio.rs
git commit -m "feat: lock-free audio ring via split producer/consumer"
```

---

### Task 3: Sample-Format Negotiation (F32/I16/U16)

**Files:**
- Modify: `src/audio.rs:74-125` (start() config + stream build)
- Test: inline — pure conversion helpers + existing bogus-device test

**Interfaces:**
- Consumes: `cpal::SampleFormat::{F32, I16, U16}` via `cfg.sample_format()`; `SupportedStreamConfigRange::sample_format()` (cpal 0.15).
- Produces: `audio::i16_to_f32(i16) -> f32`, `audio::u16_to_f32(u16) -> f32` (pure, unit-tested); `start()` works on I16/U16-only devices.

- [ ] **Step 1: Write the failing tests (append to audio tests module)**

```rust
#[test]
fn test_i16_to_f32_endpoints() {
    assert_eq!(i16_to_f32(0), 0.0);
    assert!((i16_to_f32(i16::MAX) - 1.0).abs() < 0.001);
    assert!((i16_to_f32(i16::MIN) + 1.0).abs() < 0.001);
}

#[test]
fn test_u16_to_f32_endpoints() {
    assert!((u16_to_f32(32768) - 0.0).abs() < 0.001);
    assert!((u16_to_f32(u16::MAX) - 1.0).abs() < 0.01);
    assert!((u16_to_f32(u16::MIN) + 1.0).abs() < 0.01);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test conversion 2>&1 | tail -4; cargo test i16_to_f32 u16_to_f32 2>&1 | tail -4`
Expected: FAIL with `cannot find function i16_to_f32 / u16_to_f32 in this scope`.

- [ ] **Step 3: Add helpers + rework stream build (exact code)**

Add after the `rms` function:

```rust
pub fn i16_to_f32(s: i16) -> f32 {
    s as f32 / 32768.0
}

pub fn u16_to_f32(s: u16) -> f32 {
    (s as f32 - 32768.0) / 32768.0
}
```

Replace the stream-build block (Task 2 version: ring split + `build_input_stream` f32-only) with a format match. The full replacement for everything from `let (mut producer, consumer) = ...` through the `.map_err(|e| AudioError(e.to_string()))?;` after the build call:

```rust
let (mut producer, consumer) = HeapRb::<f32>::new(sample_rate as usize * 70).split();
// NOTE: `producer` is moved into exactly one match arm (only one arm runs).
let stream = match config.sample_format() {
    cpal::SampleFormat::F32 => device.build_input_stream(
        &config,
        move |data: &[f32], _| {
            for &s in data {
                let _ = producer.try_push(s);
            }
        },
        |err| warn!("audio stream error: {err}"),
        None,
    ),
    cpal::SampleFormat::I16 => device.build_input_stream(
        &config,
        move |data: &[i16], _| {
            for &s in data {
                let _ = producer.try_push(i16_to_f32(s));
            }
        },
        |err| warn!("audio stream error: {err}"),
        None,
    ),
    cpal::SampleFormat::U16 => device.build_input_stream(
        &config,
        move |data: &[u16], _| {
            for &s in data {
                let _ = producer.try_push(u16_to_f32(s));
            }
        },
        |err| warn!("audio stream error: {err}"),
        None,
    ),
    fmt => return Err(AudioError(format!("unsupported sample format: {fmt:?}"))),
}
.map_err(|e| AudioError(e.to_string()))?;
```

`config` here is `cpal::StreamConfig` (from `cfg.with_sample_rate(...).config()`); `StreamConfig::sample_format()` does not exist — the format lives on the `SupportedStreamConfigRange`. Therefore BEFORE this block, capture the format: right after `let cfg = supported.into_iter().next()...`, insert:

```rust
let sample_format = cfg.sample_format();
```

and match on `sample_format` instead of `config.sample_format()`. (This correction is intentional — `StreamConfig` has no `sample_format()` method in cpal 0.15.)

- [ ] **Step 4: Run gates**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`
Expected: `FMT_OK`.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings.

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: `ok. 12 passed` (10 + 2 new), 0 failed.

Run: `cargo run -- --list-devices 2>&1 | tail -4`
Expected: device list unchanged, exit 0.

- [ ] **Step 5: Commit**

```bash
git add src/audio.rs
git commit -m "feat: negotiate i16/u16 sample formats with f32 conversion"
```

---

### Task 4: VAD Module — Resample + Silence Trim

**Files:**
- Modify: `Cargo.toml:11-18` (add `webrtc-vad = "0.4"`)
- Create: `src/vad.rs`
- Modify: `src/main.rs:1-2` (add `mod vad;`)
- Test: inline `#[cfg(test)]` in `src/vad.rs`

**Interfaces:**
- Consumes: `webrtc_vad::{SampleRate, Vad as WebrtcVad, VadMode}` (verified: `Vad::new_with_rate_and_mode(SampleRate::Rate16kHz, VadMode::Aggressive)`, `is_voice_segment(&mut self, &[i16]) -> Result<bool, ()>`; invalid frame lengths return `Err(())`).
- Produces: constants + `resample_to_16k` + `Vad::new`/`trim_silence` per File Structure section.

- [ ] **Step 1: Add dependency + write failing tests**

Add to `Cargo.toml` `[dependencies]` (keep alphabetical-ish order as-is, append after `tracing-subscriber`):

```toml
webrtc-vad = "0.4"
```

Create `src/vad.rs` with ONLY the test module first (so RED is real):

```rust
pub const VAD_SAMPLE_RATE: u32 = 16_000;
pub const FRAME_SAMPLES: usize = 480;
pub const PAD_FRAMES: usize = 7;

#[cfg(test)]
mod tests {
    use super::*;

    fn complex_tone_16k(secs: u32) -> Vec<f32> {
        // Loud harmonic complex with tremolo — speechlike energy for the detector.
        let n = (16_000 * secs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / 16_000.0;
                let tremolo = 0.6 + 0.4 * (2.0 * std::f32::consts::PI * 5.0 * t).sin();
                0.8 * tremolo
                    * ((2.0 * std::f32::consts::PI * 300.0 * t).sin()
                        + 0.5 * (2.0 * std::f32::consts::PI * 600.0 * t).sin()
                        + 0.25 * (2.0 * std::f32::consts::PI * 900.0 * t).sin())
                    / 1.75
            })
            .collect()
    }

    #[test]
    fn test_resample_same_rate_identity() {
        let v = vec![0.5, -0.25, 0.0, 1.0];
        assert_eq!(resample_to_16k(&v, 16_000), v);
    }

    #[test]
    fn test_resample_constant_preserved() {
        let v = vec![0.5; 480];
        let out = resample_to_16k(&v, 48_000);
        assert_eq!(out.len(), 160);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 0.01));
    }

    #[test]
    fn test_resample_empty() {
        assert!(resample_to_16k(&[], 44_100).is_empty());
    }

    #[test]
    fn test_trim_silence_only_is_empty() {
        let mut v = Vad::new();
        assert!(v.trim_silence(&vec![0.0; 16_000]).is_empty());
    }

    #[test]
    fn test_trim_keeps_tone_core() {
        // 1s silence + 1s tone + 1s silence -> output keeps middle, drops edges.
        let mut sig = vec![0.0; 16_000];
        sig.extend(complex_tone_16k(1));
        sig.extend(vec![0.0; 16_000]);
        let mut v = Vad::new();
        let out = v.trim_silence(&sig);
        assert!(!out.is_empty(), "loud complex tone must survive trim");
        assert!(out.len() < sig.len(), "edge silence must be trimmed");
        assert!(out.len() >= 16_000 - PAD_FRAMES * FRAME_SAMPLES);
    }
}
```

Add `mod vad;` to `src/main.rs` (line 2, after `mod hotkey;`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test vad:: 2>&1 | tail -6`
Expected: FAIL — `cannot find function resample_to_16k`, `cannot find struct Vad` (only constants + tests exist).

- [ ] **Step 3: Minimal implementation (prepend to src/vad.rs above tests)**

```rust
use webrtc_vad::{SampleRate, Vad as WebrtcVad, VadMode};

pub const VAD_SAMPLE_RATE: u32 = 16_000;
/// 30ms frames at 16kHz — the only size `trim_silence` feeds the detector.
pub const FRAME_SAMPLES: usize = 480;
/// Keep ~210ms of context audio around speech (7 * 30ms).
pub const PAD_FRAMES: usize = 7;

/// Linear-interpolate any mono rate to 16kHz mono.
pub fn resample_to_16k(samples: &[f32], from_rate: u32) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    if from_rate == VAD_SAMPLE_RATE {
        return samples.to_vec();
    }
    let ratio = VAD_SAMPLE_RATE as f64 / from_rate as f64;
    let out_len = ((samples.len() as f64) * ratio).round() as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 / ratio;
            let lo = pos.floor() as usize;
            let frac = (pos - lo as f64) as f32;
            let hi = (lo + 1).min(samples.len() - 1);
            samples[lo] * (1.0 - frac) + samples[hi] * frac
        })
        .collect()
}

pub struct Vad {
    inner: WebrtcVad,
}

impl Vad {
    pub fn new() -> Self {
        Self {
            inner: WebrtcVad::new_with_rate_and_mode(
                SampleRate::Rate16kHz,
                VadMode::Aggressive,
            ),
        }
    }

    /// Return speech regions with ~210ms padding; empty vec when silence-only.
    /// Input MUST be 16kHz mono (use `resample_to_16k` first).
    pub fn trim_silence(&mut self, samples_16k: &[f32]) -> Vec<f32> {
        if samples_16k.is_empty() {
            return Vec::new();
        }
        let frames: Vec<&[f32]> = samples_16k.chunks(FRAME_SAMPLES).collect();
        let mut speech = vec![false; frames.len()];
        let mut buf = [0i16; FRAME_SAMPLES];
        for (i, f) in frames.iter().enumerate() {
            for (j, b) in buf.iter_mut().enumerate() {
                *b = if j < f.len() {
                    (f[j].clamp(-1.0, 1.0) * 32767.0) as i16
                } else {
                    0
                };
            }
            speech[i] = self.inner.is_voice_segment(&buf).unwrap_or(false);
        }
        if !speech.contains(&true) {
            return Vec::new();
        }
        let first = speech.iter().position(|&s| s).unwrap();
        let last = speech.iter().rposition(|&s| s).unwrap();
        let lo = first.saturating_sub(PAD_FRAMES) * FRAME_SAMPLES;
        let hi = ((last + PAD_FRAMES + 1) * FRAME_SAMPLES).min(samples_16k.len());
        samples_16k[lo..hi].to_vec()
    }
}

impl Default for Vad {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Run gates**

Run: `cargo test vad:: 2>&1 | tail -8`
Expected: 5 passed. If `test_trim_keeps_tone_core` fails (detector rejects synthetic tone): do NOT weaken production code — report DONE_WITH_CONCERNS with the failure output; controller decides (fallback: gate tone assertion to `!out.is_empty()` only if trim returned the padded middle, else escalate).

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: `ok. 17 passed` (12 + 5), 0 failed.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/vad.rs src/main.rs
git commit -m "feat: add vad module with resample and silence trim"
```

---

### Task 5: Wire VAD Into Prototype + Cleanup + Gates + Docs

**Files:**
- Modify: `src/main.rs:46-81` (VAD wiring in simulate branch, stub text update)
- Modify: `Cargo.toml:14` (remove `global-hotkey = "0.6"` stub dep)
- Modify: `task.md` (check Phase 2 boxes + bench note), `memory.md` (dated entry)
- Test: `cargo test` + live `--simulate-hold-ms` runs (speech + silence)

**Interfaces:**
- Consumes: `vad::resample_to_16k`, `vad::Vad::trim_silence`, `audio::CapturedAudio::samples_mono`.
- Produces: prototype that logs `vad kept X/Y samples` and `No speech detected`; verified gates; updated docs.

- [ ] **Step 1: Wire VAD into simulate branch (exact replacement)**

Replace the post-`stop()` block in `src/main.rs` (Task 1 version: info! captured + match on `on_key_up`) with:

```rust
let out = cap.stop();
info!(
    "captured {} samples @ {}Hz device-ms={} rms={:.3}",
    out.samples_mono.len(),
    out.sample_rate,
    out.duration_ms,
    audio::rms(&out.samples_mono)
);
let s16 = vad::resample_to_16k(&out.samples_mono, out.sample_rate);
let mut vad = vad::Vad::new();
let kept = vad.trim_silence(&s16);
info!("vad kept {}/{} samples", kept.len(), s16.len());
if kept.is_empty() {
    info!("no speech detected");
    return;
}
match ptt.on_key_up(out.duration_ms) {
    PttEvent::Transcribe { duration_ms } => {
        info!("would transcribe {duration_ms}ms ({} vad samples)", kept.len());
        if args.dump_wav {
            match dump_wav("/tmp/wiflow_hold.wav", &kept, vad::VAD_SAMPLE_RATE) {
                Ok(()) => info!("dumped /tmp/wiflow_hold.wav"),
                Err(e) => warn!("wav dump failed: {e}"),
            }
        }
    }
    e => info!("discarded: {:?}", e),
}
```

Replace the stub printlns (lines 82–85) with:

```rust
println!("Phase 5: tray + global-hotkey wiring lands here. Use --simulate-hold-ms 1500 for now.");
```

- [ ] **Step 2: Drop the stub global-hotkey dep**

Delete line 14 (`global-hotkey = "0.6"`) from `Cargo.toml`. Verify nothing references it:

Run: `grep -rn "global_hotkey\|global-hotkey" src/ Cargo.toml 2>&1`
Expected: only the new stub println mentions "global-hotkey" (hyphenated prose, not the crate). If any `use`/code reference remains, STOP and report NEEDS_CONTEXT.

- [ ] **Step 3: Run gates + live VAD verification**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`
Expected: `FMT_OK`.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings.

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: `ok. 17 passed`, 0 failed.

Run (speak during hold): `cargo run -- --simulate-hold-ms 2000 2>&1 | tail -4`
Expected: `vad kept X/Y` with X > 0 and X < Y (room tone trimmed), `would transcribe`. Record numbers.

Run (silent hold): `cargo run -- --simulate-hold-ms 1200 2>&1 | tail -2`
Expected: `no speech detected` (in a quiet room; if room is noisy the VAD may keep audio — report what happened honestly).

- [ ] **Step 4: Commit code**

```bash
git add src/main.rs Cargo.toml Cargo.lock
git commit -m "feat: wire vad into prototype loop, drop hotkey stub dep"
```

- [ ] **Step 5: Update docs + commit**

In `task.md`: check all 8 Phase-2 boxes (4 VAD + the hardening lines that are done — rate clamp, rename+resample, format negotiation, lock-free split, Instant, poison warn, dump log, clock fix, len_zero assert, extra tests, dep decision), append observed VAD numbers (`kept X/Y` speech run, silent-run outcome).

In `memory.md`: append dated entry `2026-09-30 — Phase 2 ...` with dep versions (`webrtc-vad 0.4`, ringbuf split), VAD numbers, gate status.

```bash
git add task.md memory.md
git commit -m "docs: mark phase2 vad complete with bench numbers"
```

---

## Self-Review

- Spec coverage: task.md Phase 2 section (vad.rs, unit tests, golden wavs note) → Tasks 4–5. Golden wavs: synthetic in-code fixtures instead of `tests/data/` files — justified (no binary blobs in repo, deterministic); noted as deviation in Task 4 report. All 11 hardening tickets → Tasks 1–3 + Task 5 dep decision. PRD `$0`/offline → no new network deps (`webrtc-vad` links C code, no network at runtime).
- Placeholder scan: no TBD/TODO; every step has exact code, exact commands, exact expected output. Two honest uncertainty points are fenced: Task 1 Step 2 (both tests may pass first run — break-then-revert protocol given), Task 4 Step 4 (synthetic-tone smoke — escalate, don't weaken).
- Type consistency: `samples_mono` rename applied in Tasks 1 (struct/stop), 1 (main 3 uses), 5 (wiring); `stop(mut self)` introduced Task 2, unchanged after; `Vad::trim_silence(&mut self, &[f32]) -> Vec<f32>` matches Task 5 call `vad.trim_silence(&s16)`; `VAD_SAMPLE_RATE` used in Task 5 dump call; `out.duration_ms` (u64) matches `on_key_up(u64)`.
