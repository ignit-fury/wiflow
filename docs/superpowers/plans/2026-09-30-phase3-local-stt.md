# Phase 3 Local STT (whisper-rs Metal) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** End-to-end push-to-talk → text: VAD-trimmed audio transcribed locally by Whisper base.en (Metal) and printed, with model auto-download.

**Architecture:** Phase 2 pre-reqs first (energy gate + pipeline helper enforcing resample→trim order, f32→i16 helper, ring drop-counter), then `stt.rs` (`Stt::load`/`transcribe` over whisper-rs 0.16 `metal`), then model manager (size-gated curl download to `~/Library/Application Support/wiflow/models`), then live wiring with load/transcribe timing + RTF + docs.

**Tech Stack:** Rust 2021, `whisper-rs 0.16` (`metal` feature — API verified against vendored 0.16.0 source: `WhisperContext::new_with_params`, `create_state`, `state.full`, `full_n_segments`/`get_segment`/`to_str_lossy`, `FullParams::new(Greedy)`, `WhisperContextParameters::use_gpu`), `webrtc-vad` (existing), `curl(1)` subprocess for download (zero new HTTP deps), `cmake` build prerequisite via Homebrew

## Global Constraints

- Target macOS 13+ arm64 first, must compile warning-free on `aarch64-apple-darwin`.
- Rust edition 2021, stable toolchain (verified 1.95.0).
- Default path $0, no API key, no network calls except one-time model download from huggingface (user-initiated via transcribe run).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets -- -D warnings` clean.
- `cargo test` passes; new logic needs a test.
- Recording indicator mandatory whenever mic open (console log in prototype).
- Esc cancels; short tap <300ms discards (unchanged PTT rules).

---

## File Structure

- Modify: `src/audio.rs` — Task 1 (`f32_to_i16` helper + tests, drop-counter on producer).
- Modify: `src/vad.rs` — Task 1 (`MIN_SPEECH_RMS` + `has_speech_energy` + `transcribe_ready` pipeline + tests).
- Create: `src/stt.rs` — Tasks 2–3 (`Stt`, model constants, `models_dir`/`model_path`/`verify_model`/`ensure_model`, `#[ignore]`d live-model test).
- Modify: `src/main.rs` — Task 1 (nothing), Task 4 (`mod stt`, `--model` flag, transcribe wiring with timing).
- Modify: `Cargo.toml` — Task 2 (add `whisper-rs = { version = "0.16", features = ["metal"] }`).
- Modify: `task.md`, `memory.md` — Task 4 (check Phase 3 boxes + bench numbers, dated entry).

Interfaces:
- `audio::f32_to_i16(f32) -> i16` (new Task 1, pure, unit-tested).
- `audio::AudioCapture` gains visible drop count: `stop()` warns `dropped {n} samples (ring full)` when counter > 0 (new Task 1).
- `vad::MIN_SPEECH_RMS: f32 = 0.01` (new Task 1; room tone measured 0.002, speech ≫ 0.01).
- `vad::has_speech_energy(&[f32]) -> bool` (new Task 1: `rms >= MIN_SPEECH_RMS`, reuses `crate::audio::rms`).
- `vad::transcribe_ready(&[f32], u32, &mut Vad) -> Vec<f32>` (new Task 1: gate → resample → trim; THE entry point for STT input).
- `stt::MODEL_URL/MODEL_SIZE/MODEL_NAME`, `stt::model_path() -> PathBuf`, `stt::verify_model(&Path) -> bool`, `stt::ensure_model() -> Result<PathBuf, String>` (new Task 3).
- `stt::Stt::load(&Path) -> Result<Self, String>`, `stt::Stt::transcribe(&mut self, &[f32]) -> Result<String, String>` (new Task 2; input must be 16kHz mono — callers pass `transcribe_ready` output).

---

### Task 1: Phase 2 Pre-reqs — Energy Gate, Pipeline, i16 Helper, Drop Counter

**Files:**
- Modify: `src/audio.rs` (append `f32_to_i16` + tests; producer/consumer drop-counter)
- Modify: `src/vad.rs` (append gate + pipeline + tests)
- Test: inline `#[cfg(test)]` in both files; main.rs untouched

**Interfaces:**
- Consumes: `crate::audio::rms` (vad gate), `std::sync::Arc<AtomicUsize>` (counter).
- Produces: all four `Interfaces` entries above except the `stt::` ones.

- [ ] **Step 1: Write the failing tests (append to existing test modules)**

In `src/audio.rs` tests, append:

```rust
#[test]
fn test_f32_to_i16_endpoints() {
    assert_eq!(f32_to_i16(0.0), 0);
    assert_eq!(f32_to_i16(1.0), 32767);
    assert_eq!(f32_to_i16(-1.0), -32767);
    assert_eq!(f32_to_i16(2.0), 32767);
    assert_eq!(f32_to_i16(-2.0), -32767);
}
```

In `src/vad.rs` tests, append:

```rust
#[test]
fn test_energy_gate_rejects_quiet() {
    assert!(!has_speech_energy(&vec![0.0; 1600]));
    assert!(!has_speech_energy(&vec![0.001; 1600]));
}

#[test]
fn test_energy_gate_accepts_loud() {
    let loud: Vec<f32> = (0..1600).map(|i| 0.5 * (i as f32 * 0.02).sin()).collect();
    assert!(has_speech_energy(&loud));
}

#[test]
fn test_pipeline_rejects_silence_at_any_rate() {
    let mut v = Vad::new();
    assert!(transcribe_ready(&vec![0.0; 4410], 44_100, &mut v).is_empty());
}

#[test]
fn test_pipeline_keeps_tone_core() {
    let mut sig = vec![0.0; 16_000];
    sig.extend(complex_tone_16k(1));
    sig.extend(vec![0.0; 16_000]);
    let mut v = Vad::new();
    let out = transcribe_ready(&sig, 16_000, &mut v);
    assert!(!out.is_empty());
    assert!(out.len() < sig.len());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test f32_to_i16 has_speech_energy transcribe_ready 2>&1 | tail -6`
Expected: FAIL with `cannot find function f32_to_i16 / has_speech_energy / transcribe_ready`.

- [ ] **Step 3: audio.rs implementation (exact additions)**

Append after `u16_to_f32`:

```rust
pub fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
}
```

Drop counter — current struct (Phase 2 Task 2 shape) is:

```rust
pub struct AudioCapture {
    stream: cpal::Stream,
    consumer: HeapCons<f32>,
    started: std::time::Instant,
    sample_rate: u32,
}
```

Change to add `dropped: Arc<AtomicUsize>` (reintroduce `use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};` — `Arc` was removed in Task 2, add it back with the atomic import). In `start()`, before the split:

```rust
let dropped = Arc::new(AtomicUsize::new(0));
let dropped_cb = dropped.clone();
```

In the F32 callback (and identically in the I16/U16 callbacks — all three arms from Task 3):

```rust
move |data: &[f32], _| {
    for &s in data {
        if producer.try_push(s).is_err() {
            dropped_cb.fetch_add(1, Ordering::Relaxed);
        }
    }
}
```

Constructor stores `dropped`. `stop()` reads it first:

```rust
pub fn stop(mut self) -> CapturedAudio {
    drop(self.stream);
    let dropped = self.dropped.load(Ordering::Relaxed);
    if dropped > 0 {
        warn!("dropped {dropped} samples (ring full)");
    }
    let samples: Vec<f32> = self.consumer.pop_iter().collect();
    ...
}
```

- [ ] **Step 4: vad.rs implementation (exact additions)**

Append after `resample_to_16k`:

```rust
use crate::audio::rms;

/// Below this RMS the input is room tone, not speech (measured room: 0.002).
pub const MIN_SPEECH_RMS: f32 = 0.01;

pub fn has_speech_energy(samples: &[f32]) -> bool {
    rms(samples) >= MIN_SPEECH_RMS
}

/// Single enforced entry point for STT input: energy gate → resample → trim.
/// Guarantees the 16kHz contract by construction (fixes Phase 2 review finding).
pub fn transcribe_ready(samples: &[f32], from_rate: u32, vad: &mut Vad) -> Vec<f32> {
    if !has_speech_energy(samples) {
        return Vec::new();
    }
    let s16 = resample_to_16k(samples, from_rate);
    vad.trim_silence(&s16)
}
```

Place `use crate::audio::rms;` at the top with the existing `use webrtc_vad::...` line.

- [ ] **Step 5: Run gates**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`
Expected: `FMT_OK`.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings.

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: `ok. 24 passed` (18 + 6 new: 1 audio + 5 vad... count: f32 test + 2 gate + 2 pipeline = 5 new → 23). Expected: `ok. 23 passed`, 0 failed. (If count differs, report actual — the gate is zero failures, not the exact number.)

Run: `cargo run -- --simulate-hold-ms 1200 2>&1 | tail -2`
Expected: unchanged behavior (VAD log lines), no panic. Public behavior identical — pre-reqs are additive.

- [ ] **Step 6: Commit**

```bash
git add src/audio.rs src/vad.rs
git commit -m "feat: add energy gate, stt pipeline helper, i16 conv, drop counter"
```

---

### Task 2: STT Core — whisper-rs Metal Transcribe

**Files:**
- Modify: `Cargo.toml:11-19` (add dep)
- Create: `src/stt.rs`
- Modify: `src/main.rs:1-3` (add `mod stt;` only)
- Test: inline missing-file test (no model needed)

**Interfaces:**
- Consumes: `whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters}` (0.16.0 API verified: `new_with_params(AsRef<Path>, params)`, `create_state`, `state.full(params, &[f32])`, `full_n_segments() -> c_int`, `get_segment(c_int)`, `to_str_lossy()`; `FullParams::new(Greedy{best_of})`, `set_n_threads/set_print_progress/set_single_segment`).
- Produces: `Stt::load`, `Stt::transcribe` per File Structure.

- [ ] **Step 0: Build prerequisite (do this FIRST — whisper.cpp needs cmake)**

Run: `cmake --version 2>&1 | head -1`
Expected on this machine: `command not found` (verified 2026-09-30). Then run: `brew install cmake 2>&1 | tail -2`. If Homebrew is missing, STOP and report BLOCKED ("no cmake, no brew — install Xcode CLT + cmake manually"). Verify: `cmake --version` prints a version.

- [ ] **Step 1: Add dependency + failing test**

Add to `Cargo.toml` `[dependencies]` after `webrtc-vad`:

```toml
whisper-rs = { version = "0.16", features = ["metal"] }
```

Create `src/stt.rs` containing ONLY the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_load_missing_model_is_err() {
        assert!(Stt::load(Path::new("/nonexistent/ggml-base.en.bin")).is_err());
    }
}
```

Add `mod stt;` to `src/main.rs` after `mod hotkey;` (line 2 → lines: `mod audio; mod hotkey; mod stt; mod vad;` — keep alphabetical: audio, hotkey, stt, vad).

- [ ] **Step 2: Run to verify failure (expect a LONG build)**

Run: `cargo test stt:: 2>&1 | tail -6`
Expected: FAIL with `cannot find struct Stt`. NOTE: first build compiles whisper.cpp + Metal backend — allow up to 10 minutes (run with `timeout` ≥ 600000ms in your harness). Build noise from `cc` is expected; only Rust warnings are findings.

- [ ] **Step 3: Minimal implementation (prepend to src/stt.rs above tests)**

```rust
use std::path::{Path, PathBuf};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct Stt {
    ctx: WhisperContext,
}

fn num_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n) => n.get() as i32)
        .unwrap_or(4)
        .min(8)
}

impl Stt {
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut ctx_params = WhisperContextParameters::new();
        // Explicit: Metal GPU with `metal` feature (default would also be true via _gpu).
        ctx_params.use_gpu(true);
        WhisperContext::new_with_params(path, ctx_params)
            .map(|ctx| Self { ctx })
            .map_err(|e| format!("load model {}: {e:?}", path.display()))
    }

    /// Input MUST be 16kHz mono — callers pass `vad::transcribe_ready` output.
    /// Empty input short-circuits to Ok("") without touching the model.
    pub fn transcribe(&mut self, samples_16k: &[f32]) -> Result<String, String> {
        if samples_16k.is_empty() {
            return Ok(String::new());
        }
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(num_threads());
        params.set_print_progress(false);
        params.set_single_segment(true);
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| format!("create state: {e:?}"))?;
        state
            .full(params, samples_16k)
            .map_err(|e| format!("transcribe: {e:?}"))?;
        let n = state.full_n_segments();
        let mut text = String::new();
        for i in 0..n {
            if let Some(seg) = state.get_segment(i) {
                text.push_str(&seg.to_str_lossy().map_err(|e| format!("segment text: {e:?}"))?);
            }
        }
        Ok(text.trim().to_string())
    }
}
```

CORRECTION before compiling: `.map(|n| => n.get() ...)` is a typo — write `.map(|n| n.get() as i32)`. (Transcribe this corrected form, not the typo.)

- [ ] **Step 4: Run gates (LONG build expected)**

Run: `cargo test stt:: 2>&1 | tail -4`
Expected: 1 passed. First `whisper-rs-sys` compile takes minutes — do not abort before 10 minutes.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -3`
Expected: `Finished`, no warnings in OUR code (`whisper-rs-sys` build warnings, if any, are third-party — note but do not fix).

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: `ok. 24 passed` (23 + 1), 0 failed. (Zero failures is the gate; report actual count.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/stt.rs src/main.rs
git commit -m "feat: add stt core with metal transcribe"
```

---

### Task 3: Model Manager — Size-Gated Download

**Files:**
- Modify: `src/stt.rs` (append constants + manager + tests)
- Test: hermetic temp-file tests (sparse file via `set_len` — no 141MB disk use, no network in tests)

**Interfaces:**
- Consumes: `curl(1)` at runtime (`/usr/bin/curl` verified present), `std::fs`, `std::process::Command`.
- Produces: `MODEL_URL/MODEL_SIZE/MODEL_NAME`, `models_dir`, `model_path`, `verify_model`, `ensure_model` per File Structure. Verified sizes 2026-09-30: base.en 147,964,211 bytes, small.en 487,614,201 bytes.

- [ ] **Step 1: Write the failing tests (append to stt tests module)**

```rust
#[test]
fn test_model_path_name() {
    assert_eq!(model_path().file_name().unwrap(), MODEL_NAME);
}

#[test]
fn test_verify_model_size_gate() {
    let p = std::env::temp_dir().join("wiflow_verify_test.bin");
    let f = std::fs::File::create(&p).unwrap();
    f.set_len(MODEL_SIZE).unwrap(); // sparse — instant, no disk use
    drop(f);
    assert!(verify_model(&p));
    std::fs::remove_file(&p).unwrap();
    assert!(!verify_model(&p));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test model_ verify_ 2>&1 | tail -4`
Expected: FAIL with `cannot find MODEL_PATH / MODEL_NAME / MODEL_SIZE / verify_model`.

- [ ] **Step 3: Append implementation to src/stt.rs (after the Stt impl, before tests)**

```rust
pub const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
/// Verified 2026-09-30 via HEAD (HTTP 200, content-length).
pub const MODEL_SIZE: u64 = 147_964_211;
pub const MODEL_NAME: &str = "ggml-base.en.bin";

pub fn models_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/wiflow/models")
}

pub fn model_path() -> PathBuf {
    models_dir().join(MODEL_NAME)
}

pub fn verify_model(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() == MODEL_SIZE)
        .unwrap_or(false)
}

/// Download base.en on first use (curl ships with macOS — no HTTP dep).
/// Skips download when a size-verified model already exists.
pub fn ensure_model() -> Result<PathBuf, String> {
    let path = model_path();
    if verify_model(&path) {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir models: {e:?}"))?;
    }
    let status = std::process::Command::new("curl")
        .args(["-fSL", "-C", "-", "-o"])
        .arg(&path)
        .arg(MODEL_URL)
        .status()
        .map_err(|e| format!("spawn curl: {e:?}"))?;
    if !status.success() || !verify_model(&path) {
        return Err(format!("download failed: {status}"));
    }
    Ok(path)
}
```

- [ ] **Step 4: Run gates + live download**

Run: `cargo test stt:: 2>&1 | tail -4`
Expected: 3 passed (load-missing + 2 new), 0 failed.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings.

Run (live, ~141MB download, allow 5+ minutes): `stt::ensure_model` has no CLI yet — verify via a one-off test harness instead: `cargo test -- --ignored` runs nothing yet (ignore-test lands Task 4). For THIS task, live-verify download manually: `mkdir -p ~/Library/Application\ Support/wiflow/models && curl -fSL -C - -o ~/Library/Application\ Support/wiflow/models/ggml-base.en.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin` then `ls -l` size == 147964211 and record wall time in report. (Task 4's `#[ignore]` test + wiring exercise `ensure_model` in code.)

- [ ] **Step 5: Commit**

```bash
git add src/stt.rs
git commit -m "feat: add size-gated model manager with curl download"
```

---

### Task 4: Wire Transcribe End-to-End + Bench + Docs

**Files:**
- Modify: `src/main.rs:9-21,47-91` (`--model` flag, transcribe wiring with timing)
- Modify: `src/stt.rs` (append `#[ignore]`d live-model test)
- Modify: `task.md` (check Phase 3 boxes + bench numbers), `memory.md` (dated entry)
- Test: `cargo test -- --ignored` live-model test + spoken end-to-end run

**Interfaces:**
- Consumes: `stt::ensure_model/Stt::load/transcribe`, `vad::transcribe_ready`, PTT `duration_ms`.
- Produces: `TRANSCRIPT: ...` stdout line; `model loaded in {n}ms`, `transcribed in {n}ms (RTF {x})` logs; updated docs.

- [ ] **Step 1: Wire transcribe into simulate branch (exact replacements)**

Add `--model` flag to `Args` after `device`:

```rust
/// Override model path (default: auto-download base.en to Application Support)
#[arg(long)]
model: Option<std::path::PathBuf>,
```

Replace the `match ptt.on_key_up(out.duration_ms)` block (lines 75–89) with:

```rust
match ptt.on_key_up(out.duration_ms) {
    PttEvent::Transcribe { duration_ms } => {
        info!("would transcribe {duration_ms}ms ({} vad samples)", kept.len());
        let model_path = match &args.model {
            Some(p) => p.clone(),
            None => match stt::ensure_model() {
                Ok(p) => p,
                Err(e) => {
                    warn!("model unavailable: {e}");
                    return;
                }
            },
        };
        let t0 = std::time::Instant::now();
        let mut stt = match stt::Stt::load(&model_path) {
            Ok(s) => s,
            Err(e) => {
                warn!("stt load failed: {e}");
                return;
            }
        };
        let load_ms = t0.elapsed().as_millis();
        let t1 = std::time::Instant::now();
        match stt.transcribe(&kept) {
            Ok(text) => {
                let ms = t1.elapsed().as_millis();
                let rtf = ms as f64 / duration_ms.max(1) as f64;
                info!("model loaded in {load_ms}ms, transcribed in {ms}ms (RTF {rtf:.2})");
                println!("TRANSCRIPT: {text}");
            }
            Err(e) => warn!("transcribe failed: {e}"),
        }
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

Also replace lines 67–70 (`let s16...`/`trim_silence`) — Task 1's `transcribe_ready` is now THE entry point:

```rust
let mut vad = vad::Vad::new();
let kept = vad::transcribe_ready(&out.samples_mono, out.sample_rate, &mut vad);
```

(Keep the existing `vad kept` log + empty→`no speech detected` return as-is.)

- [ ] **Step 2: Append live-model ignored test to stt.rs tests**

```rust
#[test]
#[ignore]
fn test_transcribe_tone_with_real_model() {
    let path = model_path();
    if !verify_model(&path) {
        eprintln!("skipped: model missing");
        return;
    }
    let mut stt = Stt::load(&path).expect("load");
    let tone: Vec<f32> = (0..16_000).map(|i| 0.5 * (i as f32 * 0.02).sin()).collect();
    let text = stt.transcribe(&tone).expect("transcribe must not error");
    eprintln!("tone transcript: {text:?}");
}
```

- [ ] **Step 3: Run gates + live verification**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`
Expected: `FMT_OK`.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings.

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: `ok. 27 passed` (24 + ignore-test compile + 2 manager... recount at runtime: Task1 23 + Task2 1 + Task3 2 = 26 non-ignored + 1 ignored = 27 total, 26 passed + 1 ignored printed separately). Gate: zero failures; report actual numbers.

Run: `cargo test -- --ignored 2>&1 | tail -6`
Expected: tone test passes (prints `tone transcript: ...`), needs model from Task 3 download.

Run (SPEAK a short sentence during hold): `cargo run -- --simulate-hold-ms 4000 2>&1 | tail -5`
Expected: `TRANSCRIPT: <your words, approximately>` + `RTF < 1.0` on M-class Metal. Record transcript accuracy + RTF honestly (even if wrong — it is data for the small-vs-base decision).

- [ ] **Step 4: Commit code**

```bash
git add src/main.rs src/stt.rs
git commit -m "feat: wire end-to-end transcribe with timing and transcript output"
```

- [ ] **Step 5: Update docs + commit**

In `task.md`: check all 5 Phase-3 boxes (4 pre-reqs + stt + metal + WER/bench-note + wire) with observed numbers (model size, load_ms, transcribe_ms, RTF, transcript sample, base-vs-small note: small.en NOT downloaded — 465MB deferred until accuracy data demands it).

In `memory.md`: append dated entry `2026-09-30 — Phase 3 ...` (whisper-rs 0.16 metal, base.en, bench numbers, gate status).

```bash
git add task.md memory.md
git commit -m "docs: mark phase3 stt complete with bench numbers"
```

---

## Self-Review

- Spec coverage: task.md Phase 3 (stt.rs+model manager → Task 2–3; metal feature → Task 2 dep; WER check → Task 4 live spoken run + RTF, true WER corpus deferred and noted as deviation; OOM→base fallback → deferred with note since only base ships v1 — record both deviations in Task 4 report; bench base-vs-small → Task 4 records base numbers, small deferred on size) + 4 pre-req bullets (energy gate → Task 1; VAD contract → Task 1 pipeline helper; drop counter → Task 1; f32_to_i16 → Task 1).
- Placeholder scan: no TBD/TODO; cmake prerequisite fenced with BLOCKED protocol; 10-minute build timeout stated; typo in plan (`|n| =>`) flagged with explicit correction; test-count expectations carry "report actual" escape on zero-failure gate.
- Type consistency: `transcribe_ready(&[f32], u32, &mut Vad) -> Vec<f32>` matches Task 4 call; `Stt::load(&Path)`, `transcribe(&mut self, &[f32]) -> Result<String,String>` match wiring; `kept` is 16k f32 in both dump and transcribe calls; `VAD_SAMPLE_RATE` reused for dump rate; `duration_ms` u64 vs `ms` u128 in RTF math — `ms as f64 / duration_ms.max(1) as f64` handles types.
