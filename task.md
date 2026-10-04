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
- [x] Pre-reqs (from Phase 3 final review 2026-09-30):
  - [x] Lazy `Stt` singleton (`OnceLock`) — Task 1 added `shared_stt`, Task 4 wired it into Transcribe arm via `Mutex` guard (`let mut stt = stt_lock.lock().unwrap_or_else(|e| e.into_inner())`, guard derefs mutably — compiles as-is)
  - [x] `dump_wav` dogfoods `audio::f32_to_i16`
  - [x] `--model` override `verify_model`-warns before load (SHA256 pin deferred)
  - [x] RTF denominator → kept-audio duration (`kept_ms` from 16k kept len)
  - [ ] Revisit `set_single_segment(true)` before 60s holds ship (may truncate long utterances) — still set, deferred to pre-ship
  - [ ] Log `raw N → 16k M → kept K` (current `kept/total` log mixes rates) — cosmetic, deferred
  - [ ] User-session spoken validation (sandbox mic silent; TTS substitute only so far)
- [x] `inject.rs`: clipboard save → set text → Cmd+V via `enigo` → restore (Task 2: real round-trip in GUI session; Task 4 wired into Transcribe arm with empty-skip + clipboard fallback + warn; `--no-inject` flag for headless/CI)
- [x] `history.rs`: last-50 JSON at `~/Library/Application Support/wiflow/history.json` (plain JSON, user-deletable), copy/clear deferred to UI phase (Task 3 core + Task 4 `push_history` wiring; live entry blocked — both Task 4 runs hit no-speech path, see below)
- [x] Wiring gates (2026-09-30): `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean (one narrow `#[allow(dead_code)]` kept on `load_history`, clippy-demanded read API for future UI), `cargo test` 34 passed + 1 ignored, 0 failed
- [x] Live runs (2026-09-30): TTS-PROVEN current — `say "the quick brown fox jumps over the lazy dog" & --simulate-hold-ms 6000` → 132096 samples @44100Hz rms=0.036, vad kept 47926/132096, load 147ms / transcribe 164ms RTF 0.05, TRANSCRIPT exact "The quick brown fox jumps over the lazy dog.", inject Ok `clipboard+Cmd+V (restored: true)`, history.json 1 entry rtf 0.055. Earlier no-speech observations: `--no-inject` 2000ms (rms 0.003, kept 0/88064, history ABSENT — correct); WITH-inject 2000ms (rms 0.005, inject not attempted, no panic). Teardown caveat: GGML_ASSERT Metal abort after work, results unaffected. Remaining USER-owned: mic acoustics in real apps (matrix below).
- [ ] Manual test matrix (USER — needs spoken audio + focused app; clipboard fallback expected in password fields):
  - [ ] USER Terminal check first (paste target):
    ```bash
    cargo run -- --simulate-hold-ms 3000   # speak, then check text appeared in the FOCUSED app
    ```
    Requires Accessibility permission for enigo (System Settings → Privacy & Security → Accessibility → add Terminal/binary), else inject warns + leaves text on clipboard (press Cmd+V).
  - [ ] USER VS Code: focus editor, run as above, check text appears at cursor
  - [ ] USER Safari: focus address bar / text field, check paste
  - [ ] USER Slack: focus message box, check paste
  - [ ] USER Password field: expect clipboard-only + warn (secure fields reject synthetic paste — by design, text stays on clipboard)

## Phase 5 — Menu-bar UI + Packaging (complete 2026-09-30)
- [x] Task 1: deps + reloadable STT + tray shell — winit 0.30.13, tray-icon 0.24.2, muda 0.19.3, global-hotkey 0.7.0; `transcribe_shared` reloadable holder; `--app` flag; AppState icons/tooltips (commit 520adcf)
- [x] Task 2: hotkey daemon + worker pipeline — `daemon.rs`: preset registration (CtrlSpace default wins; AltRight/Fn rejected by macOS "Unknown scancode"), worker thread (capture→VAD→STT→inject→history), DaemonEvent proxy protocol (commit f44bf0a)
- [x] Task 3: config + menu system + hardening — `config.rs` (JSON config, LaunchAgent plist, Settings deep links), full menu tree (mic/model/hotkey/history/permissions), atomic history write, small.en variant (commit d7261c2)
- [x] Task 4: Esc fix + packaging + docs — Esc registered as 2nd hotkey on same manager, forwarded via bridge as `DaemonEvent::Cancel`; live round-trip proven (see memory.md); `Info.plist` + `build-app.sh` + `make-dmg.sh` + `docs/NOTARIZE.md` (commits a3a3c61, cf6e90b)
- [x] Tray icon states (idle/recording/transcribing/error) + tooltip; recording pill → v1.1; toasts → tooltip+log, v1.1; settings window → menu submenus + config.json; onboarding → Permissions menu + mic usage plist key
- [x] `Info.plist` keys (LSUIElement, mic usage), launch-at-login (LaunchAgent plist), ad-hoc sign (`codesign -s -`), notarize documented (docs/NOTARIZE.md)
- [x] `cargo fmt --check` clean, `clippy --all-targets -- -D warnings` clean, `cargo test` 51 passed + 1 ignored, 0 failed
- [x] DMG: `target/Wiflow-0.1.0-arm64.dmg` 2,111,080 bytes (2.0 MB), `hdiutil verify` VALID; bundle `target/Wiflow.app` 4.1 MB ad-hoc signed (Signature=adhoc, Identifier=com.wiflow.dictation); bundle `--app` runs: tray built, both hotkeys registered, no panic; first-run model download via `ensure_model` (base.en 147,964,211 bytes, size-gated)

## v1.1 (tracked deferred — do not start without approval)
- [ ] Recording pill overlay window (tray icon + tooltip carry the indicator in v1)
- [ ] Settings window (v1 uses menu submenus + config.json)
- [ ] Rich toasts (v1 uses tooltip + tracing logs)
- [ ] Revisit `set_single_segment(true)` before 60s holds ship (may truncate long utterances)
- [ ] `dirs`-crate path centralization (HOME fallback accepted for v1)
- [ ] SHA256 model pin (size gate only in v1)
- [ ] Layout-aware paste (Dvorak/IME-aware injection)
- [x] Exit crashes fixed (2026-10-01, commit 15e26fe): two root causes from crash reports + on-demand repro — (1) enigo `key()` on worker thread tripped HIToolbox main-queue assert (`dispatch_assert_queue_fail` → SIGTRAP); inject now runs in the winit `user_event` Done handler (main thread), live-verified (inject Ok + app alive). (2) leaked WhisperContext (static) kept ggml residency-set entries; whisper.cpp C++ static device destructor aborted (SIGABRT) at exit; `stt::shutdown()` drops the ctx before every normal exit path, test-verified (leak + shutdown → exit 0). Rejected-TTS note: mic can't hear `say` output on this machine — the repro used synthetic tone via test.
- [x] Trace-trap crash user-reported in `--app` runs — same two root causes as above, both fixed
- [x] Zombie-incident candidates now IMPLEMENTED properly (2026-10-01, commit a170b78, user-requested): WhisperState reuse (created once in `load`, reused per transcribe — 84ms/cycle verified, no Metal re-init, ctx field load-bearing), `set_no_timestamps(true)`, tiny.en model variant (77,704,715 bytes verified via HEAD; menu + config + download arm), `post_process` (sentence capitalization + standalone i→I + contractions; decimal points preserved; 4 tests). Gates: fmt/clippy clean, 56 passed + 1 ignored.
- [ ] True WER corpus eval (TTS substitute in v1)
- [ ] small.en bench (465 MB deferred until accuracy data demands it)
- [ ] Streaming partials, Parakeet eval, Win/Linux packaging
- [x] Groq cloud fallback behind setting (2026-10-01, commit 199fb72): `stt_provider` local|groq (default local — privacy: audio leaves device only when opted in); whisper-large-v3 via Groq audio/transcriptions (in-memory wav + multipart, ureq); Err → alert + local fallback. Live-verified.
- [x] Ollama cleanup opt-in (2026-10-01, commits b8ed491 + 621db54): provider chain Groq → OpenRouter → Ollama; quota alerts (429/402/401 → tray warn-note); background model check at startup + fallback (reachable? pulled? → guidance alerts); keys via env/keys.json (outside repo).
- [x] Groq cloud STT verified live (2026-10-01): whisper-large-v3 HTTP 200, tone → "." in 515ms (transport + recognition path; synthetic tone = non-speech).

## Phase 6 — Cloud Providers + Context + Settings Models (complete 2026-10-01)
- [x] Provider switcher: menu submenu Auto (chain) / Groq / OpenRouter / Ollama; `cleanup_provider` config ("auto" default, "" normalized to auto); explicit provider → Ollama fallback on quota only
- [x] Keys: `keys.json` in app dir (OUTSIDE repo, chmod 600, never committed) + env override (`GROQ_API_KEY`/`OPENROUTER_API_KEY`); user's Groq key stored, validated live
- [x] Image models set (Groq live-validated): Post-Processing `openai/gpt-oss-20b`, Fallback `qwen/qwen3.8-27b` (explicit retry — image said qwen3.6-27b, real id corrected), Context `qwen/qwen3.8-27b`
- [x] Context synthesis: user's context-synthesis prompt verbatim (`src/context_prompt.txt`), focused-app via osascript, two-sentence context → `<context>` block → cleanup hint; LIVE: ctx(Terminal) → correct two sentences, chain → "The deploy is Wednesday. Can you make sure staging is green?" (self-correction via gpt-oss-20b!)
- [x] Transcription: `stt_provider` local|groq + `stt_language` auto-detect (config; whisper language param + Groq multipart field)
- [x] Live Groq cleanup with user's key: gpt-oss-20b cleaned filler/stutter/self-correction perfectly, 0 issues, ~1.2s
- [x] Gates: fmt + clippy clean, 76 passed + 1 ignored

## Done Definition (v1)
- Offline push-to-talk <2s on M1 base, $0 default, permissions handled, history works, docs updated, release signed.

## S4 egui spike (Task 13) — 2026-10-04: GO with one condition
- [x] egui 0.31 + egui-winit 0.31 + egui_glow 0.31 + glutin 0.32 resolve and compile on rustc 1.95 (`examples/egui_spike.rs` builds warning-free)
- [x] Runtime proven: spike window renders egui frames error-free for 9s (GL context + painter + event input all live)
- [x] (a)/(c) by API: `ActiveEventLoop::create_window` available in `user_event`; `Window::set_visible` covers show/hide
- [ ] CONDITION (user decision required): adopting egui needs MSRV 1.75 → 1.81 (egui 0.31 floor). Confirm before Tasks 14-16 move deps to [dependencies].

## Architecture Alignment S0–S5 (complete 2026-10-04, branch `arch-alignment`)
- [x] S0 housekeeping commit (PTT lifecycle work)
- [x] S1 structure + traits (core/platform/app/ui, 4 traits, bootstrap-only main)
- [x] S2 orchestrator (8-phase machine, Session snapshots, centralized finalize, supervision, shutdown) + live regression gate (matrix + suppression proof)
- [x] S3 media duck (cherry-picks d9172df/a8e3fed/40a7233, MediaController + epoch state, lifecycle wiring, universal restore; R16/R17/R18 fixes)
- [x] S4 UI (egui spike GO; settings window; non-activating pill; notifications + spinner) — MSRV CONDITION confirmed by user → 1.81
- [x] S5 minors (Groq turbo id pinned; CPU fallback; hotkey failure note; H25 impl move)
- [x] Docs reconciliation (architecture.md rewritten with deviations-vs-status; memory.md program entry)
- [x] Suite: 250 passed / 0 failed (baseline was 129)

## Acceptance gate (spec §10.5 H32) — run 2026-10-04
- [x] structure (layout on disk; H25 verified zero core→platform/ui refs; H26 main dispatch-only)
- [x] traits (5/5 bound; fakes; impls in platform)
- [x] state machine (admission + recovery + R18; ERROR-hold; supervision; 28 machine tests)
- [x] media lifecycle (11+13 headless scenarios green; duck 0.6→0.12→0.6 exact, same device, live ×7)
- [x] UI (settings renders all panels; pill renders + never steals focus; notifications fire)
- [ ] provider deltas — CONDITIONAL: turbo id pinned + CPU classifier pinned, BUT no live Groq cycle (no key in env) and no Music pause (no library). No waiver: see user items below.
- [x] error cleanup (finalize funnel; ERROR ordering; finalize-from-every-phase tests)
- [x] tests (250/0 green, gates clean)
- [ ] live cycle — CONDITIONAL: full cycles incl. non-empty STT→inject live; Music duck/restore partial (volume exact, pause unit-only)
- [x] docs (this reconciliation)

## User acceptance batch (explicit, not waived — do in daily use)
- [ ] One real dictation with provider=groq + key: confirm `cloud stt (whisper-large-v3-turbo)` line in wiflow.log
- [ ] Play music, dictate, confirm pause + resume + volume restore
- [ ] Transcription banner appears with Focus/DND off
- [ ] Settings click-through: record hotkey, change mic, Save, reopen, Test-record
- [ ] Spinner animation eyeballed during a long transcription
