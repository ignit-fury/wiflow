# Design — Options & Locked Choice

## Context
Rust dictation, macOS-first, portable core. Requirement: almost-free / completely free AI. Push-to-talk locked 2026-09-30.

## Option A — 100% Local (LOCKED for v1)
- Stack: `cpal` → `webrtc-vad` (later Silero) → `whisper-rs` Metal (`base.en` default, `small.en` optional) → clipboard+`enigo` inject.
- Pros: $0 forever, offline, private, <2s on M1, no keys, no backend.
- Cons: ~5-8% worse on heavy accents vs large cloud models; 142-466MB first download; 8GB RAM must stick to base.
- Cost: $0. Privacy: best. Latency: best (no network).
- Verdict: **chosen for v1**. Matches "completely free" requirement exactly.

## Option B — Hybrid (local VAD + Groq Cloud STT)
- Stack: same capture/VAD, send trimmed segment to Groq `whisper-large-v3-turbo`.
- Pros: best accuracy, tiny binary, works on Intel/old Macs, generous free tier.
- Cons: needs internet + key, privacy leak, 0.5-1.5s network tail, free tier can change/rate-limit, paid after tier ($0.111/hr).
- Cost: ~free at low volume, not $0 guaranteed.
- Verdict: **deferred to v1.1 as opt-in fallback** behind `cloud` feature + settings toggle. Useful for weak machines.

## Option C — Local + Local LLM Polish
- Stack: Option A + Ollama `llama3.2:1b` / `qwen2.5:1.5b` over localhost for punctuation, casing, homophone fix.
- Pros: still $0, smarter output, offline.
- Cons: +1-2GB RAM, +300-800ms latency, extra sidecar to install/manage, can over-correct code/jargon.
- Verdict: **deferred to v1.2 as opt-in**. v1 ships raw STT + light regex cleanup only.

## OpenRouter Role (clarification)
- OpenRouter is a router, not a free STT provider. No real free Whisper endpoint; adds markup/latency over Groq. Useful only for `:free` LLM cleanup models (e.g. `llama-3.3-70b:free`, `gemini-flash:free`) — rate-limited, break often. Not in v1 path. Documented here so we don't chase it for STT.

## UI Design (v1)
- Menu-bar icon: idle (mic), recording (red dot + timer), transcribing (spinner).
- Recording pill near cursor or top-center: waveform bars (RMS from capture thread), `Release to transcribe · Esc to cancel`.
- Toast on done/error (native notification, no custom window).
- Settings window: Hotkey recorder, Mic picker, Model picker (base/small + download progress), Launch-at-login, History (50, copy/clear).
- Onboarding: 2-step permissions (Mic → Accessibility) with "Open System Settings" buttons + test-record button.

## Model Choice
- Default `base.en` (142MB, fast, good EN). Optional `small.en` (466MB, better accents, slower on 8GB). `tiny.en` as emergency fallback for Intel test only. Full multilingual models deferred (bigger, slower).

## Risks & Mitigations
- Metal build complexity → pin `whisper-rs` version, cacheStatic build in CI, document `cmake` prereq.
- Global hotkey conflicts (Fn) → default Right-Option, remappable, conflict warning.
- Clipboard clobber → save/restore clipboard around inject, document 200ms restore delay.
- Wayland later → `TextInjector` trait isolates this; macOS `enigo` path ships first.

## Approval
- [x] Push-to-talk confirmed.
- [ ] Confirm default hotkey + default model after prototype bench (task.md Phase 1).
- No code until user says build.
