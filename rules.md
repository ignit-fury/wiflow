# Rules — Working Agreements (v1)

## 1. Cost Rule ($0 default)
- Default build/run must work with $0 spend and no API key.
- Cloud (Groq / OpenRouter / Ollama-cloud) is opt-in only, behind a setting, never in default path.
- Never commit API keys, tokens, or model URLs with auth. Sample config uses placeholders.

## 2. Privacy Rule
- Audio never leaves device in default config.
- No telemetry in v1. If added later: opt-in only, no audio upload ever.
- History stays local (`~/Library/Application Support/Dictation/`). Clear button must actually delete.

## 3. Platform Rule
- Target: macOS 13+ arm64 first. Must compile warning-free on `aarch64-apple-darwin`.
- `core/` stays OS-agnostic. No UI toolkit imports in core. Platform code lives in `platform/`.
- No new dependency that breaks `cargo build` on stable without documented reason.

## 4. Quality Gates (before any PR/merge)
- `cargo fmt --check` clean.
- `cargo clippy -- -D warnings` clean.
- `cargo test` passes. New logic (VAD, inject, settings) needs a test.
- Manual checklist for audio/inject changes: test in 2 apps + permission-denied path.

## 5. UX Rules
- Push-to-talk only in v1. No always-listening, no background recording without visible indicator.
- Recording indicator mandatory whenever mic is open.
- Esc always cancels. Short tap (<300ms) never transcribes.
- Injection failure must still leave text in clipboard + notify.

## 6. Docs & Memory Rules
- Update `memory.md` on every locked decision (date + what + why).
- Update `task.md` checkboxes as work completes — no silent scope creep.
- `prd.md` / `architecture.md` are source of truth. Code that contradicts them gets fixed, not the docs (unless decision re-locked here).

## 7. Build Hygiene
- Rust edition 2021+, MSRV documented in `Cargo.toml`.
- Model binaries never committed to git. Download at first run with hash check.
- `cargo auditable` / `cargo deny` encouraged before release packaging.
- Sign + notarize macOS bundle before distributing outside dev machines.

## 8. No-Code-Yet Rule
- No implementation until user says "build". Docs/planning only until then.
