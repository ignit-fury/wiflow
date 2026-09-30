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

## Next Step
- Phase 1 done. Next: user approves → start task.md Phase 2 (VAD).
