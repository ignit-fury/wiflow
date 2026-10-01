# Memory — Decisions & Context

## Crash Fixes — Exit SIGTRAP + SIGABRT (2026-10-01, commit 15e26fe)
- User-reported: `--app` runs crashed with `zsh: trace trap` after dictation cycles (enigo had permission, inject never completed).
- Root cause 1 (SIGTRAP): enigo `key()` ran on the WORKER thread; its HIToolbox keycode mapping (`TSMGetInputSourceProperty` → `islGetInputSourceListWithAdditions`) is main-queue-only — `dispatch_assert_queue_fail` → `EXC_BREAKPOINT`. Diagnosed via macOS crash report (`~/Library/Logs/DiagnosticReports/wiflow-dictation-*.ips`) + on-demand repro (app + posted Ctrl+Space + TTS → died after "has the permission"). Fix: inject moved from `daemon::pipeline_on_worker` into `app::user_event` Done handler (main thread). Verified live: "injected via clipboard+Cmd+V (restored: true)" + app stayed alive.
- Root cause 2 (SIGABRT): static `STT` leaks the WhisperContext; whisper.cpp's C++ static `unique_ptr<ggml_metal_device>` destructor runs at `exit()` → `ggml_metal_device_free` → `ggml_metal_rsets_free` asserts on the non-empty residency set (180s keep-alive buffers still referenced by the leaked backend). Test processes drop the ctx explicitly → clean; that's why tests never caught it. Fix: `stt::shutdown()` (takes Stt out of the static, poison-tolerant) called before every normal exit path (simulate branch + menu Quit). Verified: throwaway test leak + shutdown → exit 0.
- Lesson: crash reports in `~/Library/Logs/DiagnosticReports/` carry the exact faulting stack — check them FIRST before log archaeology.
- Zombie incident: a cancelled subagent later wrote unvetted changes (WhisperState reuse, post_process capitalization, Tiny model variant) interleaved with the fixes and got swept into a fix commit; reset + recommit cleaned it. Candidates later IMPLEMENTED properly on user request (commit a170b78, 2026-10-01): state reuse (84ms/cycle, no Metal re-init), set_no_timestamps, tiny.en (77,704,715B verified), post_process (capitalization + i→I, decimal-safe).
- macOS constraint remembered: single-key global hotkeys (AltRight/Fn) fail "Unknown scancode"; CtrlSpace works. PTT mode unchanged.
- 2026-10-01 — Hotkey options reality (user-reported error + investigation, commit 4d486ac): global-hotkey 0.7's macOS scancode table has NO entries for bare modifiers — RightOption/Fn CANNOT register via RegisterEventHotKey. AltSpace (Option+Space) preset added, then REMOVED on user request (commit 0dac01d) — bare modifiers ride the CGEventTap now, AltSpace unnecessary.
- 2026-10-01 — Transcript accuracy findings (user history, commits 0dac01d): (1) Whisper mishears proper nouns mid-sentence ("Rather Lie" → "rather lie", "Playboy Carti" → "play boy cartoon") — deterministic post-process can't fix those. (2) Spoken math "A plus B" → symbol via post_process rule (single alnum only, punctuation kept). (3) Whisper hallucination tokens ([BLANK_AUDIO]) on silent holds got pasted — now stripped; empty-after-strip = no-text, nothing injected. post_process sentence capitalization IS working ("The" cap ✓).
- 2026-10-01 — Initial-prompt vocabulary (commit 3190178): `set_initial_prompt` wired from `prompt.txt` (plain text — corrupt-proof vs JSON config; corrupt config falls back to defaults and LOSES settings). Menu "Edit Vocabulary…" opens the file; live-read per hold. Starter vocab: "Playboy Carti, Rather Lie". Long prompts slow decode slightly — keep one line.
- 2026-10-01 — LLM cleanup layer (commit b8ed491, user-provided system prompt verbatim in `src/cleanup_prompt.txt`): Ollama `/api/generate`, graceful skip on unreachable → deterministic output stands. EMPTY sentinel → no-text (nothing injected/history). Pipeline: transcribe → post_process (deterministic) → cleanup → inject/history.
- 2026-10-01 — Cleanup provider chain (commit 621db54, user-specified): Groq → OpenRouter → Ollama local → deterministic. Keys from env `GROQ_API_KEY` / `OPENROUTER_API_KEY` (never on disk); missing key = provider skipped silently. HTTP 429/402/401 → CleanupIssue → tray warn-note alert ("tokens exhausted — falling back to..."). Ollama background check at startup + fallback (reachable? model pulled? via /api/tags) → alerts "Ollama not running — install from ollama.com" / "model not installed — run ollama pull X". ureq 2.12 (json) for HTTPS. LIVE: Ollama installed + running on this machine (user pulled llama3.2:1b) — chain verified live.
- 2026-10-01 — Phase 6 (commits 9b612f7, 0e1c35a, a1173fa, 199fb72, c3f0ac8): provider switcher menu (Auto/Groq/OpenRouter/Ollama; explicit provider → Ollama fallback on quota only); keys.json in app dir (chmod 600, OUTSIDE repo, never committed) + env override; image models live-validated: Post-Processing `openai/gpt-oss-20b`, Fallback `qwen/qwen3.8-27b` (image said qwen3.6-27b — REAL id corrected; llama-3.3-70b NOT on this account), Context `qwen/qwen3.8-27b`; explicit fallback-model retry before provider switch; context synthesis (user's context prompt verbatim, focused-app via osascript, Groq→Ollama, never invents); Groq cloud STT `whisper-large-v3` (opt-in `stt_provider`, in-memory wav multipart, Err→alert+local fallback) + `stt_language` auto-detect. LIVE verified: Groq cleanup "The deploy is Wednesday. Can you make sure staging is green?" (self-correction via gpt-oss-20b — llama3.2:1b missed it earlier), cloud STT 515ms round-trip. User's real config state: Fn preset (CGEventTap), cleanup toggled off by user, provider=groq, model ids fixed to image defaults.
- 2026-10-01 — Process notes: Task 5 subagent died twice with EMPTY results (live-heavy task: LLM calls + app spawns + sleeps) — controller ran the full live pass inline (throwaway scripts deleted after). Pre-existing test was making LIVE Groq calls with the real key (Task 2 implementer caught + rewrote as localhost test — live calls in cargo test = quota waste + flaky; never do that).
- 2026-10-01 — USER CORRECTION + FIX (commit 7f388f7): bare-modifier PTT is NOT an OS limitation — CGEventTap intercepts raw flagsChanged system-wide (what Apple Dictation / SuperWhisper use). Built `src/tap.rs`: direct CoreGraphics/CoreFoundation FFI (zero deps), listen-only tap on kCGSessionEventTap, own runloop thread, keycode filter (RightOption=61, Fn=63) → PttDown/PttUp via the same proxy. Startup + switch_hotkey branch: bare presets → tap (fallback CtrlSpace + Input Monitoring guidance), combos → global-hotkey; rollback preserves PTT. Menu re-enabled. LIVE-VERIFIED: posted flagsChanged → PttDown → capture started → PttUp → clean cycle. Fn caveat: macOS globe-key behavior (emoji picker on tap) — set Keyboard → "Press 🌐 key to: Do Nothing" for clean Fn PTT. Input Monitoring permission (Privacy_ListenEvent) needed for the tap on other machines.
- 2026-10-01 — Crash-fix lessons: check `~/Library/Logs/DiagnosticReports/*.ips` FIRST (exact faulting stack); Carbon hotkey events need NSApplication pumping (bare CFRunLoop probes give false negatives).

## Phase 4 Inject + History — Wired (2026-09-30)
- Deps: arboard 3.6, enigo 0.6, serde 1, serde_json 1 (all $0, offline, no network).
- Wiring: `--no-inject` flag; Transcribe arm → empty-skip → `push_history` (warn-only on failure) → inject or skip; `Stt::load` → `shared_stt` singleton + `Mutex` guard; `now_ms()` wall-clock for `at_ms` (`Instant` would be wrong for a timestamp).
- History file: `~/Library/Application Support/wiflow/history.json`, plain JSON last-50, user-deletable (local-only, clearable per rules).
- Inject outcome: TTS-PROVEN live — `say "the quick brown fox jumps over the lazy dog"` → TRANSCRIPT exact, inject Ok `clipboard+Cmd+V (restored: true)`, history.json 1 entry rtf 0.055 (132096 samples rms 0.036, kept 47926/132096, RTF 0.05). Earlier no-speech observations: silent-room runs rms 0.003/0.005, kept 0, history ABSENT (correct), no panic. Teardown caveat: GGML_ASSERT Metal abort after work, results unaffected. USER spoken run + 5-app matrix (mic acoustics in real apps still user-owned) recorded in task.md.
- Gates: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean (narrow allow kept on `load_history` only), `cargo test` 34 passed + 1 ignored, 0 failed.
- Deferred: SHA256 model pin, `set_single_segment` revisit, `raw→16k→kept` log split, history copy/clear UI (Phase 5).

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

## Phase 5 Menu-Bar App + Packaging — Complete (2026-09-30)
- Stack: Rust 1.95.0, winit 0.30.13, tray-icon 0.24.2, muda 0.19.3, global-hotkey 0.7.0, open 5.4.
- CtrlSpace outcome: default preset registers immediately (id 524350); AltRight/Fn rejected by macOS ("Unknown scancode") — CtrlSpace locked as v1 default, fallback order CtrlSpace→Fn.
- Esc outcome: winit device_event proven dead for zero-window tray apps (Task 3). Fix: `register_cancel_hotkey` registers Escape (id 114) as a 2nd hotkey on the SAME GlobalHotKeyManager; bridge thread matches esc id → `DaemonEvent::Cancel` on Pressed (Released ignored). Live round-trip (Quartz-posted keys): `capture started @ 44100Hz` → `dictation cancelled (Esc)` → `dictation failed: cancelled (Esc)` → release → `ptt up ignored: Ignored` → `cycle done, no text` (tray back to Idle, no stuck Recording).
- Packaging: `target/Wiflow.app` 4.1 MB, ad-hoc signed (Signature=adhoc, Identifier=com.wiflow.dictation, TeamIdentifier=not set); bundle `--app` runs clean (tray built, both hotkeys registered, no panic). `target/Wiflow-0.1.0-arm64.dmg` 2,111,080 bytes (2.0 MB), `hdiutil verify` VALID. Notarization documented in docs/NOTARIZE.md (manual, paid Apple Developer $99/yr, no credentials in repo).
- Gates: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean, `cargo test` 51 passed + 1 ignored, 0 failed.
- Deferred to tracked v1.1 section in task.md: pill overlay, settings window, rich toasts, set_single_segment revisit, dirs crate, SHA256 pin, layout-aware paste, ggml-metal teardown abort, true WER corpus, small.en bench.

## Next Step
- Phase 5 done. v1 feature-complete: user approval → ship.

## Phase 2 VAD — Complete (2026-09-30)
- Deps: `webrtc-vad 0.4` added (links C code, no network at runtime); `global-hotkey 0.6` stub dropped until Phase 5; ringbuf split producer/consumer.
- Wiring: simulate branch resamples to 16k → `Vad::trim_silence` → logs `vad kept X/Y`; empty → "no speech detected"; wav dump now writes trimmed 16k audio.
- Carried fixes: removed crate-level `#![allow(dead_code)]` (clippy clean without it); `resample_to_16k` zero-rate guard + test → 18/18 tests pass.
- Live VAD numbers: 2000ms run `kept 31951/31951` rms=0.020; silent 1200ms run `kept 19133/19133` rms=0.002 — VAD keeps near-silent room tone, no "no speech detected" observed; threshold tuning needed before STT.
- Gates: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` clean, `cargo test` 18/18.
