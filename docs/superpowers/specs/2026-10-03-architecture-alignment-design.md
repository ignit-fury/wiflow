# Architecture Alignment — Wiflow ↔ Target Architecture Diagram

**Date:** 2026-10-03
**Status:** Design approved section-by-section in conversation (Sections 1–5, all "yes/ok").
**Scope owner:** user (approvals captured in session).
**Source of truth:** the architecture diagram presented 2026-10-03 ("Wiflow Complete
Architecture") + this spec. Reconciled into `architecture.md` during S5 (§9).

---

## 1. Intent

Wiflow currently conforms to the target architecture at ~70%: the core dictation
pipeline matches, but six areas diverge (media layer absent, state machine too
coarse, UI layer missing, no trait abstractions, flat module layout, minor
provider/feature deltas). This spec defines full conformance work, decomposed
into six sub-projects **S0–S5**, executed sequentially with a green checkpoint
after each.

**Success criteria:**
- Code structure, interfaces, state machine, media behavior, and UI match the
  diagram (with the four explicitly-noted carve-outs in §8).
- `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
  green after every task; suite starts at 129 tests, never regresses.
- One full dictation cycle works live after every sub-project.
- No behavior regressions to the just-hardened PTT lifecycle (admission rules
  and their 12 tests survive, renamed at most).

## 2. Decisions log (conversation approvals)

| # | Decision | Choice |
|---|---|---|
| D1 | Spec shape | One **master spec** for S0–S5; execution starts S0+S1 |
| D2 | UI stack (S4) | Full diagram UI, **pure Rust**: winit/egui, native macOS notifications (no Tauri) |
| D3 | Media duck (S3) | **Cherry-pick** `d9172df`→`a8e3fed`→`40a7233` as-is, then adapt hooks; trait isolates it |
| D4 | Groq STT model (S5) | Switch to **`whisper-large-v3-turbo`** per diagram |
| D5 | Config format | **Stays JSON** — diagram says "Persistent storage (TOML/JSON)", JSON conforms |
| D6 | Execution approach | **A: sequential strangler** — mechanical move, then traits, then features; every step compiles, tests green, frequent commits |

## 3. Sub-project map & order

```
S0 housekeeping → S1 structure+traits → S2 orchestrator states → S3 media duck
→ S4 UI layer → S5 minors+docs
```
Checkpoint after each: commit(s) green + live single-cycle smoke + report to user.

## 4. S0 — Housekeeping

Commit the pending PTT-lifecycle work before any refactor:
- New: `src/ptt.rs`, `src/logfile.rs`, `docs/ptt-lifecycle-debug.md`, `CONTEXT.md`
- Modified: `src/{app,daemon,tap,audio,main}.rs` (session logging, out-of-band
  tap re-enable + synthesized PttUp, `PttMachine` admission gate, 60 s
  watchdog `WIFLOW_MAX_RECORDING_MS`, teardown logs)
- Leave: `stash@{0}` (superseded reference), duck commits (S3 material).

## 5. S1 — Structure & traits (no behavior change)

### 5.1 Folder layout (via `git mv`, tests green after the move)

```
src/
├── main.rs        # CLI entry + orchestrator session glue
├── daemon.rs      # orchestrator: DaemonEvent/Control, worker, session
├── ptt.rs         # state machine (orchestrator/state)
├── logfile.rs     # infra
├── app/           # app.rs → app/mod.rs (tray, menu, tooltips, icons)
├── core/          # audio, vad, stt, groq_stt, analyze, cleanup, history,
│                  #   config, hotkey (PushToTalk), traits.rs
├── platform/      # tap (global hotkey), inject, permissions,
│                  #   focused-app context, media/duck (S3), objc panel (S4)
└── ui/            # created in S4: settings, pill, notify
```

### 5.2 Traits (`core/traits.rs`; TDD — failing test defines each trait first)

| Trait | Signature (essence) | Default impl | Wraps |
|---|---|---|---|
| `SpeechRecognizer` | `transcribe(audio) -> Result<Transcript>` | `RouterRecognizer` | `stt`/`groq_stt` provider branch |
| `CleanupProvider` | `clean(text, ctx) -> String` | `ChainProvider` | `cleanup::clean_chain` + `analyze::decide_route` |
| `TextInjector` | `inject(text) -> Result<InjectResult>` | `ClipboardInjector` | `inject::inject_text` |
| `ContextProvider` | `get() -> AppContext` | `OsascriptContext` | `focused_app_name` + context gating |
| `MediaController` | `duck()` / `restore()` | `CoreAudioDuck` | `duck.rs` — **defined in S3**, not S1: no caller exists until the duck lands, and an impl-less unused trait trips `-D warnings` |

Call sites switch from direct functions to trait calls with the same concrete
impls — zero behavior change; fakes become available to S2/S4 tests.

## 6. S2 — Orchestrator: 7-state machine + Session Context

### 6.1 State mapping

| Diagram | Current | Change |
|---|---|---|
| IDLE | `Idle` | — |
| STARTING | folded into Recording | **new** — awaiting `CaptureStarted` |
| LISTENING | `Recording` | rename; mic hot only here |
| PROCESSING | `Processing` | unchanged (VAD→STT→cleanup) |
| INJECTING | folded into Done | **new** — inject + history (main thread) |
| RESTORING | missing | **new** — media restore hook, tray reset |
| CANCELLED | `Cancelling` | rename; resolves via `Failed(cancelled)` → IDLE |
| ERROR | tray-only | promoted to machine phase; held until next PttDown → STARTING |

### 6.2 Transitions

```
IDLE --PttDown--> STARTING --CaptureStarted(new DaemonEvent)--> LISTENING
LISTENING --PttUp--> PROCESSING --Done(text)--> INJECTING --> RESTORING --> IDLE
                    \--Done(empty)---------------------------> RESTORING --> IDLE
LISTENING|STARTING --Esc--> CANCELLED --Failed--> IDLE
any --fatal capture/transcribe error (+notification)--> ERROR
ERROR --PttDown--> STARTING          # ERROR is held until the next press;
                                     # press is admitted from ERROR as if IDLE
                                     # (new admission rule + new tests; tray
                                     # keeps the error note until cycle end)
LISTENING --watchdog (60 s, loud WARN)--> PROCESSING        # PRD §6 safety net
```

Admission rules unchanged (existing 12 tests renamed at most, never weakened):
Down only from IDLE; Up only from LISTENING; Esc only from LISTENING;
duplicate/stray events ignored; Recording (=LISTENING) left only via
PttUp/Esc/watchdog/fatal-error.

**Failed classification rule** (no event-shape change needed): `on_failed`
while phase == CANCELLED → IDLE (that's the cancel resolution); `on_failed`
in any other phase → ERROR (fatal capture/transcribe failure). The existing
`Failed(String)` event stays as-is.

### 6.3 Session Context

`struct Session { id, started_at, audio, media_state, app_context, provider,
settings_snapshot }` — built at STARTING, threaded through states, logged at
cycle end (extends existing session-id logging; every lifecycle log line keeps
timestamp + session id + event type; no sample content).

**Touch points:** `ptt.rs` (phases + tests), `daemon.rs` (CaptureStarted event,
worker reports start), `app.rs` (event routing, ERROR handling, INJECTING/
RESTORING steps move from the Done handler into explicit transitions).

## 7. S3 — Media ducking (cherry-pick + trait fit)

- Cherry-pick `d9172df` ("duck competing audio while mic is hot": CoreAudio
  volume duck w/ exact per-channel restore, no OSD; delayed Music/Spotify
  pause 600 ms gate, resume-only-what-we-paused; idempotent state machine;
  stale-hold invalidation; menu toggle), then `a8e3fed` (timing-race fix,
  restore-after-injection), then `40a7233` (transition logs, output-device
  tracking). Run full suite after each — bisectable; revert exactly one if it
  fights the new structure.
- Move `duck.rs` under `platform/`; expose `MediaController` (`CoreAudioDuck`).
- **Lifecycle hooks** (adapt call sites only; duck machine itself untouched):
  - entry LISTENING → `duck()` (600 ms gate may then pause)
  - INJECTING → RESTORING → `restore()` after injection
  - **invariant:** any exit from LISTENING (cancel, error, watchdog, discard)
    → `restore()`. No state outside LISTENING holds ducked audio
    ("Wiflow only restores what it changes").
  - duplicate Down → no-op (duck machine idempotent).
- Live check: Music playing → hold Fn → duck/pause → release → inject → restored.

## 8. S4 — UI layer (pure Rust, on the existing winit loop)

winit = one event loop per process → settings window and pill are `winit::Window`s
handled in `app.rs`'s empty `window_event`, painted with egui (`egui-winit` +
`egui_glow`). New `src/ui/`:

1. **`ui/settings.rs` — Settings Window** (tray menu opens it): hotkey recorder
   (combos via window key events; bare mods still via tap; single keys rejected —
   macOS can't register them), mic picker, model picker (tiny/base/small),
   media control (`duck_enabled`), provider settings (STT local/Groq, cleanup
   provider, keys status), launch-at-login, history browser (last 50,
   copy/clear). Reuses `config.rs`/`history.rs`/`audio::list_devices` — a new
   view, not new logic.
2. **`ui/pill.rs` — Recording Pill**: frameless, always-on-top,
   **non-activating** NSPanel (`platform/` objc; must never steal focus from
   the dictation target). Shown on LISTENING, hidden on any exit. Live RMS
   waveform bars (new `AtomicF32` feed from the capture callback — amplitude
   only), elapsed timer, "Release to transcribe · Esc cancels".
3. **`ui/notify.rs` — Notifications**: Notification Center via
   `osascript display notification` — transcription complete (preview),
   errors, permission prompts; falls back to tray warn-note.
4. **Diagram extras**: tray Transcribing spinner (animated icon frames while
   PROCESSING); settings Permissions panel gains "Test record" (1 s capture →
   RMS result, no STT).

**Risk control:** S4's first task is a 30-minute egui-into-winit integration
spike; fallback if stalled: settings panel without waveform (pill deferred),
flagged to user before proceeding.
**Live checks:** dictation into Terminal while pill visible → Terminal keeps
focus; settings write → reload → same values.

## 9. S5 — Minor conformance + docs

1. Groq STT model → `whisper-large-v3-turbo` + one live transcription check.
2. GPU/CPU fallback: Metal init failure → log warn + retry CPU whisper context
   instead of failing the cycle.
3. Conflict detection (pragmatic scope): surface registration failure with
   guidance; settings recorder rejects unregistrable single keys. (System-wide
   hotkey enumeration doesn't exist on macOS — nothing faked.)
4. **Not built (documented carve-outs):** history search (diagram marks
   future), config JSON (D5), 16 kHz conversion (already at VAD stage).
5. Docs reconciliation: `architecture.md`, `task.md`, `memory.md` updated to
   final state; diagram deltas recorded (docs are source of truth).

## 10. Error handling

ERROR is a real phase: fatal capture/transcribe error → ERROR + notification;
watchdog and TapIssue keep their loud WARNs; every ERROR exit runs `restore()`.
Worker keeps its own `PushToTalk` bookkeeping (unchanged). `tx.send` failures
stay surfaced (PttWork's `send_control` Error path).

## 11. Verification strategy

- **Every task:** `cargo fmt --check` · `cargo clippy --all-targets -- -D
  warnings` · `cargo test` (baseline 129) — all green before commit; frequent
  commits.
- **Every sub-project:** live single-cycle smoke (`/tmp/poster_kb 1200` →
  Idle→Recording→Processing→Idle, mic released).
- **After S1 and S2:** full PTT synthetic matrix re-run (A/B/C/D/H conditions)
  — the just-hardened lifecycle must not weaken.
- **S3:** Music duck/restore live. **S4:** focus-steal + settings round-trip.
- **Docs** updated per sub-project (rules.md).

## 12. Risks

| Risk | Mitigation |
|---|---|
| egui-into-existing-winit (S4) | 30-min spike first; fallback = settings without waveform |
| Non-activating NSPanel | known objc technique; live focus test |
| Duck cherry-pick conflicts; unknown reset reason | per-commit suite; trait isolation; revert exactly one commit |
| State-machine renames break PTT hardening | 12 admission tests must stay green through renames |

## 13. Out of scope

Windows/Linux, Tauri, wake-word, VAD-as-stop, cloud-by-default, history
SQLite, `small.en` default swap, notarization/signing changes.
