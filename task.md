# Tasks — Dictation App (phase 1 complete: audio + hotkey prototype working)

> Phase 1 built 2026-09-30 after user approval. Phase 2+ needs approval per phase.

## Phase 0 — Docs (current)
- [x] `prd.md` — scope, push-to-talk, $0 rule
- [x] `architecture.md` — pipeline + crates + layout
- [x] `rules.md` — cost/privacy/quality gates
- [x] `design.md` — A locked, B/C deferred
- [x] `task.md` — this file
- [x] `memory.md` — decisions log
- [ ] User reviews 6 docs, locks default hotkey + default model choice
- [x] Commit docs to git

## Phase 1 — Audio + Hotkey prototype (complete 2026-09-30)
- [x] `cargo init`, `Cargo.toml` (edition 2021, `cpal`, `ringbuf`, `global-hotkey`, `hound`, `clap`, `tracing`)
- [x] `audio.rs`: list devices, capture 16kHz mono, RMS meter, wav dump flag — 3 input devices observed (MacBook Air Microphone, BlackHole 16ch, BlackHole 2ch)
- [x] `hotkey.rs`: push-to-talk state machine (keydown/up, Esc cancel, <300ms discard, 60s auto-stop) — 8/8 tests pass
- [x] Manual test: hold/release logs durations, no transcribe yet — simulate-hold wall 1.95s, 66048 samples @44100Hz
- [x] Bench: CPU % while recording on M1 — 5% CPU observed
- [x] Gates green: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean, `cargo test` 8/8

## Phase 2 — VAD (complete 2026-09-30)
- [x] `vad.rs`: `webrtc-vad` impl, 30ms frames, trim silence + 200ms padding
- [x] Unit tests: silence-only → discard; speech+silence → trimmed (6 vad tests, 18 total)
- [x] Golden wavs in `tests/data/` (not committed if large — document source) — deviation: synthetic in-code fixtures instead of binary blobs (deterministic, no blobs in repo)
- [x] Audio hardening tickets (from 2026-09-30 final review, all in `src/audio.rs` unless noted):
  - [x] Pick sample rate clamped to 16kHz instead of range max (`audio.rs:99`) — 192kHz devices blow up ringbuf (~53MB) and break 16kHz contract
  - [x] Rename `samples_16k_mono` → `samples_mono` (stores native rate until resample lands) + add `resample_to_16k()`; update `main.rs` field uses
  - [x] Format negotiation: handle I16/U16-only devices (`i16→f32`/`u16→f32` conversion) instead of clean Err
  - [x] Lock-free capture: `HeapRb::split()` producer/consumer + `try_push` (no `Mutex` anywhere — stronger than try_lock), remove `Mutex` from realtime callback
  - [x] `Instant` instead of `SystemTime` for hold-duration clock (NTP skew; backwards-time test N/A under `Instant`)
  - [x] `warn!` on lock-poison in `stop()` instead of silent empty default — then Mutex eliminated by split, so no poison path remains in final code (outcome correct)
  - [x] Log `dump_wav` IO errors (`src/main.rs`) instead of `let _`
  - [x] Fix simulate clock divergence: sleep bound vs `on_key_up(hold)` (`src/main.rs:57 vs 66`)
  - [x] `assert!(!devs.is_empty())`, drop `#[allow(clippy::len_zero)]` (`src/audio.rs:160`)
  - [x] Extra tests: stray `on_key_up` without down (`Ignored`), bogus device name → Err (backwards-time N/A under `Instant`)
  - [x] Decide `global-hotkey` stub: wire behind feature or drop dep until Phase 5; fix doc drift — dropped stub dep, rewired stub text to Phase 5
- VAD live numbers 2026-09-30: speech-intent run (2000ms, quiet room, no speaker) `vad kept 31951/31951` rms=0.020 → would transcribe; silent run (1200ms) `vad kept 19133/19133` rms=0.002 → would transcribe, NO "no speech detected" — VAD keeps near-silence room tone, honest finding, needs threshold tuning before STT lands

## Phase 3 — Local STT (whisper-rs Metal, complete 2026-09-30)
- [x] Pre-req (from Phase 2 review 2026-09-30): energy/RMS gate before STT — `vad::transcribe_ready` energy-gates (`MIN_SPEECH_RMS = 0.01`) before resample+trim; proven live: 2 silent holds (rms 0.002/0.004) → `vad kept 0/N` → "no speech detected", STT never touched
- [x] Pre-req (from Phase 2 final review): harden VAD 16kHz contract — `transcribe_ready(&[f32], u32, &mut Vad) -> Vec<f32>` is THE entry point in simulate branch; `Stt::transcribe` takes 16k f32 by contract
- [x] Pre-req (from Phase 2 final review): ring-full observability — `AtomicUsize` dropped-counter, `warn!` in `stop()` if >0
- [x] Pre-req (from Phase 2 final review): `audio::f32_to_i16` helper (clamp + round) with endpoint test — kept narrow `#[allow(dead_code)]` (test-only use; STT takes f32 so still unconsumed, clippy `-D warnings` demands it)
- [x] `stt.rs`: model manager (`ensure_model` curl download w/ size gate 147964211 bytes, skips when verified) + `Stt::load/transcribe` — all temp `#[allow(dead_code)]` removed (wired)
- [x] Feature `metal` on aarch64, CPU fallback documented
- [x] WER check on golden files, RTF log, OOM → fallback to base — deviation: no golden WER corpus; substituted live TTS end-to-end (ground truth "the quick brown fox" → `TRANSCRIPT: the QuickBrown Fox.`, ~correct modulo casing) + RTF 0.10 log; OOM→base fallback deferred (only base ships v1)
- [x] Bench base.en vs small.en on target Mac, lock default — base.en locked v1: load 5744ms (first load incl. Metal init), transcribe 139ms over 1.44s audio, RTF 0.10 on M-class Metal; small.en NOT downloaded (465MB deferred until accuracy data demands it); default model = base.en
- [x] Wire transcribe end-to-end (Task 4): `--model` flag, `ensure_model → Stt::load → transcribe` with `model loaded in {n}ms, transcribed in {n}ms (RTF {x})` + `TRANSCRIPT:` stdout; ignored live-model test (`tone transcript: "(dramatic music)"` — sine hallucinates, honest data); live-mic spoken run BLOCKED by hardware (default I/O = AirPods in case → silence; see memory.md); gates green: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean, `cargo test` 26 passed + 1 ignored (27 total)

## Phase 4 — Inject + History
- [ ] `inject.rs`: clipboard save → set text → Cmd+V via `enigo` → restore
- [ ] `history.rs`: last-50 JSON/SQLite, copy/clear
- [ ] Manual test matrix: VS Code, Safari, Slack, Terminal, password field (clipboard-only)

## Phase 5 — Menu-bar UI + Packaging
- [ ] Tray icon states, recording pill, toasts, settings window, onboarding (mic + accessibility)
- [ ] `Info.plist` keys, launch-at-login, sign + notarize dry run
- [ ] `cargo fmt`, `clippy -D warnings`, `cargo test` green
- [ ] DMG/zip + first-run model download UX

## Phase 6 — v1.1 / v1.2 (deferred, do not start)
- [ ] v1.1: Groq cloud fallback behind setting (`cloud` feature, user key)
- [ ] v1.2: Ollama cleanup opt-in (`llm` feature)
- [ ] Streaming partials, Parakeet eval, Win/Linux packaging

## Done Definition (v1)
- Offline push-to-talk <2s on M1 base, $0 default, permissions handled, history works, docs updated, release signed.
