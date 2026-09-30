# PRD — Voice Dictation App (Rust, macOS-first, $0)

## 1. Goal
Fast, offline-first, push-to-talk voice dictation. Rust for speed + portability. macOS first, core portable to Windows/Linux later. Default path costs $0 — no paid API required.

## 2. Users
- Developers, writers, anyone who dictates into any app (editor, browser, chat).
- MacBook M1+ users who want SuperWhisper-style speed without subscription.

## 3. Scope (v1)
IN:
- Menu-bar app, launches at login (optional).
- Global hotkey, push-to-talk: hold = record, release = transcribe + inject.
- Mic capture 16kHz mono via `cpal`.
- VAD (silence trim) via Silero / webrtc-vad.
- Local STT via `whisper-rs` (whisper.cpp, Metal). Default model `base.en`, upgrade to `small.en` in settings.
- Text injection at cursor: clipboard + Cmd+V (`arboard` + `enigo`).
- On-screen state: idle / recording (waveform + timer) / transcribing / done + error toast.
- History: last 50 transcriptions, click-to-copy, clear.
- Settings: hotkey remap, mic select, model select, launch-at-login.
- Permissions onboarding: Microphone + Accessibility walkthrough.

OUT (v1):
- No cloud STT by default. No wake-word / always-listening.
- No LLM rewrite by default (optional later, still free via Ollama).
- No Windows/Linux installer (core stays portable, but no packaging v1).
- No team sync, no cloud history.

## 4. Success Criteria
- End-to-end (release key → text appears) < 2s for 10s utterance on M1 with base model.
- Works fully offline after first model download.
- $0 marginal cost per user in default config.
- No crash on device plug/unplug, permission denied handled with guidance.
- Transcription usable without manual correction for clear English.

## 5. UX Flow
1. User holds hotkey (default: Fn or Right-Option, remappable).
2. Pill/menubar shows recording + live timer + waveform, Esc cancels.
3. User releases → "Transcribing…" (max 2s) → text injected at focused app cursor.
4. Toast + history entry. If injection fails (no focus / permission), copy to clipboard + notify.

## 6. Edge Cases
- No mic / permission denied → onboarding sheet, no panic.
- Very short press (<300ms) → discard, no transcribe call.
- Long press (>60s) → auto-stop, transcribe chunk.
- Silence only → VAD discards, show "No speech detected".
- Focused app is password field → inject blocked, clipboard only + warning.

## 7. Cost Strategy (almost-free requirement)
- Default: 100% local = $0 forever.
- Optional fallback (behind setting, off by default):
  - Groq `whisper-large-v3-turbo` (~$0.111/hr audio, generous free tier) for low-end Macs.
  - OpenRouter `:free` LLM models for punctuation cleanup only, never for STT.
  - Local Ollama (`llama3.2:1b`) as free offline cleanup alternative.
- No API key shipped. User brings own key if they enable cloud.

## 8. Open Questions (locked unless reopened)
- [x] Mode: push-to-talk (locked 2026-09-30).
- [ ] Default hotkey: Fn vs Right-Option (needs prototype test for conflicts).
- [ ] Default model: base.en vs small.en (benchmark on M1/M2 needed).
