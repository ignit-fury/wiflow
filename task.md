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

## Phase 2 — VAD
- [ ] `vad.rs`: `webrtc-vad` impl, 30ms frames, trim silence + 200ms padding
- [ ] Unit tests: silence-only → discard; speech+silence → trimmed
- [ ] Golden wavs in `tests/data/` (not committed if large — document source)
- [ ] Audio hardening tickets (from 2026-09-30 final review, all in `src/audio.rs` unless noted):
  - [ ] Pick sample rate clamped to 16kHz instead of range max (`audio.rs:99`) — 192kHz devices blow up ringbuf (~53MB) and break 16kHz contract
  - [ ] Rename `samples_16k_mono` → `samples_mono` (stores native rate until resample lands) + add `resample_to_16k()`; update `main.rs` field uses
  - [ ] Format negotiation: handle I16/U16-only devices (`i16→f32`/`u16→f32` conversion) instead of clean Err
  - [ ] Lock-free capture: `HeapRb::split()` producer/consumer + `try_lock`, remove `Mutex` from realtime callback
  - [ ] `Instant` instead of `SystemTime` for hold-duration clock (NTP skew)
  - [ ] `warn!` on lock-poison in `stop()` instead of silent empty default
  - [ ] Log `dump_wav` IO errors (`src/main.rs`) instead of `let _`
  - [ ] Fix simulate clock divergence: sleep bound vs `on_key_up(hold)` (`src/main.rs:57 vs 66`)
  - [ ] `assert!(!devs.is_empty())`, drop `#[allow(clippy::len_zero)]` (`src/audio.rs:160`)
  - [ ] Extra tests: stray `on_key_up` without down (`Ignored`), backwards time, bogus device name → Err
  - [ ] Decide `global-hotkey` stub: wire behind feature or drop dep until Phase 5; fix doc drift

## Phase 3 — Local STT (whisper-rs Metal)
- [ ] `stt.rs`: model manager (download base.en with progress + SHA), transcribe i16
- [ ] Feature `metal` on aarch64, CPU fallback documented
- [ ] WER check on golden files, RTF log, OOM → fallback to base
- [ ] Bench base.en vs small.en on target Mac, lock default

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
