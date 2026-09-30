# Memory — Decisions & Context

## Locked Decisions
- 2026-09-30 — Language: Rust. Why: speed (realtime audio), single binary, portable core to Win/Linux later.
- 2026-09-30 — Platform order: macOS arm64 first. Why: user target, Metal speeds Whisper 3-5x, menu-bar pattern proven.
- 2026-09-30 — Mode: push-to-talk (hold = record, release = transcribe). Why: simplest, no wake-word daemon, less hallucination, clear privacy story.
- 2026-09-30 — Cost: 100% local default = $0. Why: user wants completely free. Cloud only opt-in later.
- 2026-09-30 — STT v1: `whisper-rs` (whisper.cpp Metal), `base.en` default. Why: best free accuracy/speed tradeoff on M1+, offline, no key.
- 2026-09-30 — VAD v1: `webrtc-vad`, Silero later if needed. Why: tiny, fast, kills silence hallucinations.
- 2026-09-30 — Inject: clipboard + Cmd+V (`arboard` + `enigo`) with clipboard restore. Why: most reliable on macOS vs raw keystroke synthesis.
- 2026-09-30 — OpenRouter: NOT for STT (no free endpoint, adds markup). Only `:free` LLMs for optional cleanup later. Groq preferred if cloud fallback ever enabled.
- 2026-09-30 — Docs-first: 6 files before any code, per user request. No build until "build" command.

## Project Context
- Repo: `/Users/prempatel/Documents/wiflow`, `main` branch; Phase 1 prototype (src/main.rs, src/audio.rs, src/hotkey.rs) + docs (as of 2026-09-30).
- Docs: `prd.md`, `architecture.md`, `rules.md`, `design.md`, `task.md`, `memory.md` (this file).
- Brainstorming skill used; visual companion never needed (no mockup question arose).

## Open Questions
- Default hotkey: Right-Option vs Fn (test conflicts in prototype).
- Default model final: base.en vs small.en (bench on target Mac in Phase 3).
- UI stack: `tray-icon`+`winit` minimal vs `Tauri` full settings window (decide Phase 5).
- History store: JSON vs SQLite (decide Phase 4, lean JSON unless search needed).

## Constraints Remembered
- Terse caveman chat style for conversation; files/commits stay normal prose.
- Phase 1 built and verified (8/8 tests, live mic capture). No Phase 2 work until approved.
- $0 default, audio stays on device, recording indicator mandatory, Esc cancels.

## Phase 1 Prototype — Complete (2026-09-30)
- Input devices observed (3): MacBook Air Microphone (host default), BlackHole 16ch, BlackHole 2ch; live capture 66048 samples @44100Hz.
- Simulate-hold wall time 1.95s; 5% CPU while recording.
- Gates: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean (exit 0), `cargo test` 8/8 pass.

## Phase 3 STT — Complete (2026-09-30)
- Stack: whisper-rs 0.16 + Metal (`metal` feature, `use_gpu(true)`), base.en default (147964211 bytes, size-gated `ensure_model`, download skipped — already on disk).
- Wiring: simulate branch → `transcribe_ready` (energy gate → resample → trim) → `ensure_model → Stt::load → transcribe` with load/transcribe timing + RTF log and `TRANSCRIPT:` stdout; `--model` override flag added.
- Bench (TTS "the quick brown fox" → 16k wav through REAL pipeline): vad kept 22560/23042; load 5744ms; transcribe 139ms over 1.44s audio (RTF 0.10); transcript `the QuickBrown Fox.` — locks base.en for v1, small.en deferred (465MB).
- Live-mic spoken run blocked: default I/O routes to AirPods (in case → digital silence); explicit MacBook-mic run + audible TTS still rms 0.004 → energy gate correctly rejected both. No transcript-from-mic yet — needs user-held session with working input.
- Ignored live test green: sine tone → `"(dramatic music)"` hallucination (expected — pure tones are not speech).
- Gates: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean, `cargo test` 26 passed + 1 ignored. Deviations: true WER corpus deferred; OOM→base fallback deferred (base-only v1).

## Next Step
- Phase 3 done. Next: user approves → start task.md Phase 4 (Inject + History).

## Phase 2 VAD — Complete (2026-09-30)
- Deps: `webrtc-vad 0.4` added (links C code, no network at runtime); `global-hotkey 0.6` stub dropped until Phase 5; ringbuf split producer/consumer.
- Wiring: simulate branch resamples to 16k → `Vad::trim_silence` → logs `vad kept X/Y`; empty → "no speech detected"; wav dump now writes trimmed 16k audio.
- Carried fixes: removed crate-level `#![allow(dead_code)]` (clippy clean without it); `resample_to_16k` zero-rate guard + test → 18/18 tests pass.
- Live VAD numbers: 2000ms run `kept 31951/31951` rms=0.020; silent 1200ms run `kept 19133/19133` rms=0.002 — VAD keeps near-silent room tone, no "no speech detected" observed; threshold tuning needed before STT.
- Gates: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean, `cargo test` 18/18.
