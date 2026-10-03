# Architecture Alignment — Wiflow ↔ Target Architecture Diagram

**Date:** 2026-10-03
**Status:** Design v1 approved section-by-section; hardening round (**H1–H32**,
user review 2026-10-03) incorporated. **Awaiting freeze approval before
`writing-plans`.**
**Scope owner:** user (approvals captured in session).
**Source of truth:** the architecture diagram presented 2026-10-03 ("Wiflow Complete
Architecture") + this spec. Reconciled into `architecture.md` during S5 (§10).

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
  diagram (with the explicitly-noted carve-outs in §10).
- `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
  green after every task; suite starts at 129 tests, never regresses.
- One full dictation cycle works live after every sub-project.
- No behavior regressions to the just-hardened PTT lifecycle (admission rules
  and their 12 tests survive; one intentional semantic change is documented in
  §7.9: fatal `Failed` now lands in ERROR instead of Idle).
- S5 exits only through the acceptance gate (§10, H32): diagram, spec, and
  `architecture.md` all describe the same system.

## 2. Decisions log (conversation approvals)

| # | Decision | Choice |
|---|---|---|
| D1 | Spec shape | One **master spec** for S0–S5; execution starts S0+S1 |
| D2 | UI stack (S4) | Full diagram UI, **pure Rust**: winit/egui, native macOS notifications (no Tauri) |
| D3 | Media duck (S3) | **Cherry-pick** `d9172df`→`a8e3fed`→`40a7233` as-is, then adapt hooks; trait isolates it |
| D4 | Groq STT model (S5) | Switch to **`whisper-large-v3-turbo`** per diagram |
| D5 | Config format | **Stays JSON** — diagram says "Persistent storage (TOML/JSON)", JSON conforms |
| D6 | Execution approach | **A: sequential strangler** — mechanical move, then traits, then features; every step compiles, tests green, frequent commits |
| D7 | Hardening round | 32 clarifications (H1–H32): single orchestrator ownership, universal restore, immutable snapshots, dependency rules, failure/race semantics, test matrix, acceptance gate |

## 3. Binding architecture rules (H1, H25, H26)

### 3.1 Single lifecycle owner (H1)

`AppOrchestrator` is the **single owner** of session lifecycle and event routing.
Responsibilities are exclusive — no second decision-maker:

| Area | Role — and role only |
|---|---|
| `main.rs` | **Bootstrap only**: parse args, init logging, construct `AppOrchestrator`, run the event loop. No session logic, no event interpretation (H26). |
| `app/` (`AppOrchestrator`) | **Owns the session lifecycle**: owns `Session`, routes every event to a state transition, runs `finalize_session()` on every terminal path, owns the supervision deadline (§7.7). |
| `ptt.rs` | **Validates state transitions only**: pure function of (event × phase) → phase. No I/O, no timers, no resource handling. |
| `daemon.rs` | **Worker / execution engine**: capture → VAD → STT → cleanup pipeline. Reports events (`CaptureStarted`, `Done`, `Failed`, `TapIssue`); owns **no** lifecycle decisions. Keeps its internal `PushToTalk` key bookkeeping (unchanged). |

### 3.2 Dependency direction (H25)

```text
UI ──observes──▶ App ──uses──▶ Core ──defines──▶ Traits
                            Platform ──implements──▶ Core traits
```

- **Core never imports UI or platform implementation modules.** Platform never
  imports App/UI.
- UI never manipulates daemon/PTT state directly — it renders App state and
  emits *intent commands* that App handlers execute (H20).
- Composition (which concrete impl backs each trait) happens in App/main only.

### 3.3 No business logic in `main.rs` (H26)

`main.rs` may: parse args, init logging, build the orchestrator, run the loop.
Anything else is a review-blocking violation. This prevents drift back into the
current flat orchestration style.

## 4. Sub-project map & order

```
S0 housekeeping → S1 structure+traits → S2 orchestrator states → S3 media duck
→ S4 UI layer → S5 minors+docs
```
Checkpoint after each: commit(s) green + live single-cycle smoke + report to user.

## 5. S0 — Housekeeping

Commit the pending PTT-lifecycle work before any refactor:
- New: `src/ptt.rs`, `src/logfile.rs`, `docs/ptt-lifecycle-debug.md`, `CONTEXT.md`
- Modified: `src/{app,daemon,tap,audio,main}.rs` (session logging, out-of-band
  tap re-enable + synthesized PttUp, `PttMachine` admission gate, 60 s
  watchdog `WIFLOW_MAX_RECORDING_MS`, teardown logs)
- Leave: `stash@{0}` (superseded reference), duck commits (S3 material).

## 6. S1 — Structure & traits (no behavior change)

### 6.1 Folder layout (via `git mv`, tests green after the move; roles per H1, impl split per H4)

```
src/
├── main.rs        # bootstrap only (H26)
├── app/           # AppOrchestrator: session lifecycle owner, event routing (H1)
├── ptt.rs         # transition validator only — pure, no I/O (H1)
├── daemon.rs      # worker/execution engine; reports events, no decisions (H1)
├── logfile.rs     # infra
├── core/          # audio, vad, stt, groq_stt, analyze, cleanup, history,
│                  #   config, hotkey (PushToTalk key-state only), traits.rs
│                  #   NEVER imports ui/ or platform impls (H25)
├── platform/
│   └── macos/     # tap, inject, permissions, context, duck (S3), panel (S4) (H4)
└── ui/            # created in S4: settings, pill, notify — observes App (H20)
```

### 6.2 Traits (`core/traits.rs`; TDD — failing test defines each trait first)

| Trait | Essence | Backed by |
|---|---|---|
| `SpeechRecognizer` | `transcribe(audio) -> Result<Transcript>` | core router (`RouterRecognizer`) over `stt`/`groq_stt` |
| `CleanupProvider` | `clean(text, ctx) -> String` | core chain (`ChainProvider`) |
| `TextInjector` | `inject(text) -> Result<InjectResult>` | `platform::macos::ClipboardInjector` (H4) |
| `ContextProvider` | `get() -> AppContext` | `platform::macos::OsascriptContext` (H4) |
| `MediaController` | `duck()` / `restore()` | `platform::macos::CoreAudioDuck` — **defined in S3**, not S1: no caller exists until the duck lands, and an impl-less unused trait trips `-D warnings` |

Call sites switch from direct functions to trait calls with the same concrete
impls — zero behavior change; fakes become available to S2/S4 tests.
Recoverable-vs-fatal failure semantics and fallback per trait: §12 (H17–H19).

## 7. S2 — Orchestrator: states, Session, centralized finalize

### 7.1 State mapping

| Diagram | Current | Change |
|---|---|---|
| IDLE | `Idle` | — |
| STARTING | folded into Recording | **new** — §7.3 (H14) |
| LISTENING | `Recording` | rename; mic hot only here; duck here (§8) |
| PROCESSING | `Processing` | unchanged (VAD→STT→cleanup); mic released |
| INJECTING | folded into Done | **new** — inject on main thread; success/failure diverge (§7.2, H8) |
| RESTORING | missing | **new** — the single finalize phase: `finalize_session()` runs here on **every** terminal path (§7.6, H7) |
| CANCELLED | `Cancelling` | rename; resolves via `Failed` → RESTORING → IDLE |
| ERROR | tray-only | promoted to machine phase; entered **only from RESTORING after finalize completes** (§11, H9); held until next PttDown → STARTING |

### 7.2 Transitions

```text
IDLE --PttDown--> STARTING --CaptureStarted(new DaemonEvent)--> LISTENING
LISTENING --PttUp--> PROCESSING --Done(text)--> INJECTING
INJECTING --inject ok--> RESTORING --> IDLE
INJECTING --inject fail (whole chain incl. clipboard fallback exhausted)--> RESTORING --> ERROR   (H8)
PROCESSING --Done(empty)--> RESTORING --> IDLE          # empty transcript still finalizes (H2)
LISTENING|STARTING --Esc--> CANCELLED --Failed--> RESTORING --> IDLE
any active --fatal--> RESTORING (finalize) --> ERROR    # ordering §11 (H9)
ERROR --PttDown--> STARTING                             # Down admitted from ERROR as if IDLE
LISTENING --watchdog (60 s, loud WARN)--> stop capture → finalize audio →
    PROCESSING (usable audio, as if PttUp) or RESTORING→IDLE (nothing usable)    (H16)
ANY --supervision deadline (watchdog + grace, no worker event)--> worker-failure
    path: finalize → ERROR + notification                                       (H2/H16)
```

Stale-event rules: `CaptureStarted` arriving when phase ≠ STARTING (e.g. after
Esc) is ignored; `PttUp` outside LISTENING ignored; duplicate Downs ignored.

Admission rules: Down only from IDLE or ERROR; Up only from LISTENING; Esc only
from LISTENING or STARTING; duplicates/stray events ignored.

### 7.3 STARTING ownership (H14)

```text
PTT accepted → Session created → immutable snapshot captured (§7.5)
→ capture requested → waiting for CaptureStarted
```

Only after `CaptureStarted` does the microphone become logically LISTENING.
Esc during STARTING cancels the pending capture request (CANCELLED path; the
late `CaptureStarted`, if it still arrives, is ignored as stale).

### 7.4 Microphone lifecycle (H15)

| Phase | Mic |
|---|---|
| IDLE | off |
| STARTING | initializing (not hot) |
| LISTENING | **active — the only hot phase** |
| PROCESSING / INJECTING / RESTORING / CANCELLED / ERROR | released |

Rule: the orchestrator drops its capture handle as step 1 of every LISTENING
exit; the worker stops capture before emitting `Done`/`Failed`. Entering ERROR
with the mic hot is a bug (§11).

### 7.5 Session ownership + immutable snapshot (H5, H6, H24)

- `Session` owns **lifecycle metadata/state only**. The audio subsystem owns
  actual audio buffers; `Session` holds an **audio-session reference**
  (id/handle), never raw samples (H5).
- Snapshot captured at STARTING, frozen for the session lifetime (H6):
  session ID · started-at timestamp · full settings snapshot · STT
  provider/model · cleanup provider/model · focused-app context (or None, H19)
  · media snapshot (output device, was-playing — feeds `MediaSessionState`,
  §8) · audio-session reference.
- **Configuration concurrency (H24):** mid-session settings changes never
  affect the active session — all session decisions read the snapshot. New
  settings apply to the **next session**. Exception mechanism only: a setting
  read live must be marked `/// RUNTIME-SAFE` with justification in code;
  default is next-session.

### 7.6 Centralized terminal cleanup (H7)

One function — `AppOrchestrator::finalize_session()` — **idempotent**, called on
**every** path out of an active session (all nine H2 cases + shutdown, §7.8).
RESTORING is the phase where it runs. No error path performs its own partial
cleanup; all funnel here:

```text
finalize_session()
 ├── 1. stop/release capture (no-op if already stopped)
 ├── 2. restore media (MediaController::restore — idempotent, §8/H3)
 ├── 3. hide/reset recording UI (pill hidden — H22)
 ├── 4. release audio (drop audio-session reference; subsystem frees buffers)
 ├── 5. finalize logging (session-end line: id, duration, outcome, provider/model)
 └── 6. release session resources (timers cancelled, worker signalled)
        → terminal phase (IDLE or ERROR)
```

### 7.7 Watchdog + supervision (H16, H2)

- Worker-side 60 s watchdog (kept, loud WARN): stop capture → finalize
  available audio → process (as PttUp) **or** safely abort → terminal state via
  RESTORING. Never leaves the session stuck in LISTENING.
- Orchestrator-side supervision deadline (new): armed on LISTENING entry at
  watchdog + small grace. If no terminal worker event arrives (unexpected
  worker failure/thread death), the orchestrator itself forces
  finalize → ERROR + notification. The worker watchdog is the mechanism; the
  supervision deadline is the backstop.

### 7.8 Shutdown as an explicit lifecycle (H30)

```text
any active state → shutdown request → cancel timers/work
→ finalize_session() (§7.6) → join worker → exit
```

Whisper/Metal teardown caution (project has crash history here): model-context
release keeps its current thread discipline; no new threads touch whisper
during teardown. Covered by finalize-from-every-phase unit tests (§13) plus a
manual quit-during-recording check per sub-project (§14).

### 7.9 Failed classification + touch points

`on_failed` while phase == CANCELLED → RESTORING → IDLE (the cancel
resolution); `on_failed` in any other phase → RESTORING → ERROR. The existing
`Failed(String)` event shape stays as-is; the **target after finalize** is
what differs.

**Intentional semantic change (recorded, not a regression):** fatal `Failed`
previously recovered straight to Idle; it now lands in ERROR (held until next
press) so failures stay visible. Cancel-`Failed` still resolves to IDLE. The
no-stuck guarantee is preserved via Down-from-ERROR admission. One existing
test (`failed_while_recording`-family) is updated for the new target; the other
11 admission tests stay green through renames.

**Touch points:** `ptt.rs` (phases + tests), `daemon.rs` (`CaptureStarted`
event, worker reports start, worker-side watchdog), `app/` (routing,
finalize, supervision deadline, ERROR handling, INJECTING/RESTORING steps move
from the Done handler into explicit transitions).

## 8. S3 — Media ducking (cherry-pick + trait fit)

- Cherry-pick `d9172df` (CoreAudio volume duck w/ exact per-channel restore, no
  OSD; delayed Music/Spotify pause 600 ms gate, resume-only-what-we-paused;
  idempotent state machine; stale-hold invalidation; menu toggle), then
  `a8e3fed` (timing-race fix, restore-after-injection), then `40a7233`
  (transition logs, output-device tracking). Run full suite after each —
  bisectable; revert exactly one if it fights the new structure.
- Implementation: `platform::macos::CoreAudioDuck` (`platform/macos/duck.rs`)
  implementing `core::traits::MediaController` (H4). The duck machine itself
  ships untouched; only call sites are adapted: **`duck()` is called on
  LISTENING entry; `restore()` is called only inside `finalize_session()` —
  never from ad-hoc call sites.**

### 8.1 MediaController contract (H11)

`duck()` owns the whole sequence — the orchestrator holds no Music/Spotify
timing logic:

```text
duck(): immediate volume duck → arm 600 ms gate → optional Music/Spotify pause
        (only if the session still holds duck when the gate elapses)
restore(): invalidate gate + pause timers → resume what we paused (only what
        we paused) → restore exact per-channel volumes → clear flags
```

### 8.2 Media state model (H10)

```text
MediaSessionState {
    was_playing_before:  bool,      // from STARTING snapshot
    output_device:       DeviceId,  // from STARTING snapshot; re-checked at restore
    ducked_by_wiflow:    bool,
    paused_by_wiflow:    bool,
    restoration_required: bool,     // == ducked_by_wiflow || paused_by_wiflow
    epoch:               u64,        // bumped by restore(); invalidates stale timers (H12/H13)
}
```

Owned by the controller, initialized from the STARTING snapshot; mirrored into
the session-end log line. This makes "restore only what Wiflow changed"
executable: restore touches exactly the flagged state, nothing else.

### 8.3 Universal restore (H2)

Any session that modified media is restored before reaching a terminal state —
via `finalize_session()`, which runs even when duck never completed (no-op) or
the failure happened outside LISTENING:

| Case | Path → restore |
|---|---|
| normal completion | INJECTING → RESTORING (finalize) → IDLE |
| empty transcript | PROCESSING → RESTORING (finalize) → IDLE |
| Esc cancellation | CANCELLED → RESTORING (finalize) → IDLE |
| STT failure | PROCESSING → RESTORING (finalize) → ERROR |
| cleanup failure | PROCESSING (downgraded, §12) or → RESTORING → ERROR |
| injection failure | INJECTING → RESTORING (finalize) → ERROR |
| watchdog | §7.7 → RESTORING (finalize) → terminal |
| capture failure | → RESTORING (finalize) → ERROR |
| unexpected worker failure | supervision deadline → RESTORING (finalize) → ERROR |

The old "no state outside LISTENING holds ducked audio" invariant is subsumed:
**no terminal state is reached with unrestored media modifications.**

### 8.4 Idempotent restore (H3)

`restore()` is safe to call zero, one, or multiple times: safe after partial
ducking, safe after pause failure, safe across output-device changes (restore
targets the device(s) it changed; a vanished device → loud log, session
continues — never traps finalize), and never restores state Wiflow did not
modify.

### 8.5 Race safety + stale-event rules (H12, H13)

- Esc during the 600 ms gate: the CANCELLED path runs finalize → `restore()`
  bumps `epoch` → the pending pause timer is **invalidated and must not fire
  after cancellation** (H12).
- Explicitly required (H13): duplicate `duck()` → no-op; duplicate `restore()`
  → no-op; stale pause timer (epoch mismatch) → ignored; stale restore (epoch
  mismatch / nothing outstanding) → ignored.
- S3 adds test seams (fake clock, fake audio/media backend) so the §13 media
  scenarios run headless in `cargo test`.

Live check: Music playing → hold Fn → duck/pause → release → inject →
restored; plus Esc-during-gate → no pause fires, volumes restored.

## 9. S4 — UI layer (pure Rust, on the existing winit loop)

winit = one event loop per process → settings window and pill are `winit::Window`s
handled in `app.rs`'s empty `window_event`, painted with egui (`egui-winit` +
`egui_glow`). New `src/ui/`.

**Ownership (H20):** UI observes App state; it does not control lifecycle.
`App State → UI rendering` — never `UI → daemon/PTT state`. UI emits intent
commands (open window, copy history entry, save setting); App handlers execute
them (which is also how the next-session rule, H24, is enforced for settings).

1. **`ui/settings.rs` — Settings Window** (tray menu opens it): hotkey recorder
   (combos via window key events; bare mods still via tap; single keys rejected —
   macOS can't register them), mic picker, model picker (tiny/base/small),
   media control (`duck_enabled`), provider settings (STT local/Groq, cleanup
   provider, keys status), launch-at-login, history browser (last 50,
   copy/clear). Reuses `config.rs`/`history.rs`/`audio::list_devices` — a new
   view, not new logic.
2. **`ui/pill.rs` — Recording Pill, strictly observational (H21)**: frameless,
   always-on-top, **non-activating** NSPanel (`platform/macos/panel.rs` via
   objc; must never steal focus from the dictation target). Displays only:
   LISTENING state (passed in, not sensed), RMS/amplitude, elapsed time,
   cancel instruction. No recording-state of its own beyond render cache —
   never a second source of truth.
3. **`ui/notify.rs` — Notifications**: Notification Center via
   `osascript display notification` — transcription complete (preview),
   errors, permission prompts. **Notification failure never fails a dictation
   session (H23):** fire-and-forget; on failure, tray warn-note + log, session
   outcome unaffected.
4. **Diagram extras**: tray Transcribing spinner (animated icon frames while
   PROCESSING); settings Permissions panel gains "Test record" (1 s capture →
   RMS result, no STT).

**UI cleanup is part of terminal cleanup (H22):** pill visibility is owned by
`finalize_session()` step 3 — the pill disappears on PttUp, Esc, error,
watchdog, capture failure, and shutdown, because all funnel through finalize.

**Risk control:** S4's first task is a 30-minute egui-into-winit integration
spike; fallback if stalled: settings panel without waveform (pill deferred),
flagged to user before proceeding.
**Live checks:** dictation into Terminal while pill visible → Terminal keeps
focus; settings write → reload → same values (next session takes effect).

## 10. S5 — Minor conformance + docs + acceptance gate

1. Groq STT model → `whisper-large-v3-turbo` + one live transcription check.
2. GPU/CPU fallback: Metal init failure → log warn + retry CPU whisper context
   instead of failing the cycle.
3. Conflict detection (pragmatic scope): surface registration failure with
   guidance; settings recorder rejects unregistrable single keys. (System-wide
   hotkey enumeration doesn't exist on macOS — nothing faked.)
4. **Docs reconciliation:** `architecture.md`, `task.md`, `memory.md` updated to
   final state (docs are source of truth). `architecture.md` gains two
   explicit sections (H31): **"Intentional deviations"** (history search =
   diagram-marked future, not built; config stays JSON per D5; 16 kHz
   conversion already happens at the VAD stage; any further deviations found
   during S0–S4) vs **"Alignment status"** (what now matches). Carve-outs must
   never be silently relabeled as done, and unfinished work must never hide
   among carve-outs.
5. **Architecture acceptance gate (H32):** S5 is not complete until every item
   is ✅ and verified:

```text
structure       ✅  (mtime layout §6.1 on disk; H25/H26 hold by inspection)
traits          ✅  (all five bound to impls; fakes exist)
state machine   ✅  (§7 transitions incl. ERROR-hold + supervision deadline)
media lifecycle ✅  (§8: universal restore demonstrated, §13 media tests green)
UI              ✅  (settings + pill + notifications; focus-steal check passed)
provider deltas ✅  (turbo model live-checked; CPU fallback exercised or fault-injected)
error cleanup   ✅  (finalize-from-every-phase tests; ERROR ordering §11)
tests           ✅  (full suite green: baseline 129 + all §13 additions)
live cycle      ✅  (full dictation cycle + Music duck/restore on hardware)
docs            ✅  (architecture.md deviations vs status sections written)
```

## 11. Error handling & ERROR ordering (H9)

Canonical ordering — `ERROR` is entered **only after** resources are released:

```text
fatal error → stop/release capture → restore media → hide/reset UI → ERROR
```

In machine terms: any fatal event → RESTORING (`finalize_session()`, §7.6) →
ERROR. **ERROR must never leave microphone, media, or UI resources active** —
entering ERROR with mic hot, duck held, or pill visible is a bug, covered by
finalize-from-every-phase tests (§13).

- Watchdog and TapIssue keep their loud WARNs.
- `tx.send` failures stay surfaced (PttWork's `send_control` Error path).
- Unexpected worker failure (supervision deadline, §7.7) → finalize → ERROR +
  notification (text preserved in history where available).

## 12. Failure semantics and provider fallback

Covers (H17) per-trait failure contracts, (H18) explicit provider fallback, and
(H19) non-blocking context acquisition.

Recoverable provider failure vs fatal session failure, per trait:

| Trait | Recoverable → | Fatal (→ ERROR path) when |
|---|---|---|
| `SpeechRecognizer` | provider error → **configured fallback behavior** (e.g. alternate provider per settings) | all configured providers exhausted → finalize → ERROR, text preserved in history |
| `CleanupProvider` | AI cleanup failure → **deterministic cleanup**; deterministic failure → **deliver raw transcript** — cleanup failure never discards dictated text | — (cleanup cannot fail a session) |
| `TextInjector` | inject failure → existing chain continues **including clipboard fallback** (preserved, H8) | whole chain exhausted → finalize → ERROR + notification carrying the text; history saved |
| `ContextProvider` | **any** failure/timeout → `None`, session continues without context (H19) | — (context cannot fail a session) |
| `MediaController` | duck failure → proceed unducked (logged); partial state → `restore()` still safe (H3) | — (media never fails a session; restore failure is loudly logged and finalize continues) |

**Non-blocking context (H19):** focused-app/context acquisition has a bounded
deadline and never gates `CaptureStarted` → LISTENING. Timeout/failure →
proceed with `None`, logged with session id. Basic dictation completes with
zero context available.

## 13. Test matrix (H27, H28, H29)

Baseline 129 tests never regress; the 12 PTT admission tests survive with the
one documented target change (§7.9). Additions:

### 13.1 Lifecycle tests — every transition (H27)

`IDLE→STARTING` · `STARTING→LISTENING` (incl. stale/late `CaptureStarted`
ignored) · `LISTENING→PROCESSING` · `PROCESSING→INJECTING` ·
`INJECTING→RESTORING→IDLE` (inject ok) · `INJECTING→RESTORING→ERROR` (inject
fail, H8) · cancellation from `STARTING` (pending capture cancelled) ·
cancellation from `LISTENING` · errors from every active state (fatal in
STARTING/LISTENING/PROCESSING/INJECTING → RESTORING → ERROR) · watchdog with
usable audio (→ PROCESSING) and without (→ safe abort) · supervision deadline
with silent worker (→ ERROR) · duplicate/stray events in every phase (no-ops) ·
Down-from-ERROR admitted · shutdown from every active phase (§7.8: finalize
runs, resources released).

### 13.2 Media lifecycle tests (H28) — headless via S3 seams

Already-paused media (no pause claimed, volumes still restored) · playing media
(duck → pause → resume) · duck-only path (pause gate never reached, e.g. short
hold) · delayed pause firing at gate · pause failure (proceed, restore still
safe) · cancellation during delay (pause never fires, H12) · duplicate duck →
no-op · duplicate restore → no-op · output-device change mid-session (restore
targets changed devices; vanished device logged, session continues) · STT
failure after media pause (restore runs, resume happens) · injection failure
after media pause (restore runs before ERROR).

### 13.3 Session isolation tests (H29)

Sequential sessions A→B: B's snapshot (settings/provider/model/context/media)
is freshly captured, never inherited from A · mid-session settings change does
not alter the active session (H24) · `restoration_required`/epoch reset per
session (no leak of media flags) · session-end log lines carry the correct
session id throughout.

## 14. Verification strategy

- **Every task:** `cargo fmt --check` · `cargo clippy --all-targets -- -D
  warnings` · `cargo test` — all green before commit; frequent commits.
- **Every sub-project:** live single-cycle smoke (`/tmp/poster_kb 1200` →
  Idle→…→Idle, mic released).
- **After S1 and S2:** full PTT synthetic matrix re-run (A/B/C/D/H conditions).
- **S3:** Music duck/restore live + Esc-during-gate live.
- **S4:** focus-steal + settings round-trip (change applies next session).
- **Shutdown:** manual quit-during-recording per sub-project (§7.8).
- **Docs** updated per sub-project (rules.md); S5 closes the acceptance gate (§10).

## 15. Risks

| Risk | Mitigation |
|---|---|
| egui-into-existing-winit (S4) | 30-min spike first; fallback = settings without waveform |
| Non-activating NSPanel | known objc technique; live focus test |
| Duck cherry-pick conflicts; unknown reset reason | per-commit suite; trait isolation; revert exactly one commit |
| State-machine renames break PTT hardening | 12 admission tests green through renames; 1 intentional target change documented (§7.9) |
| Shutdown teardown (Whisper/Metal history) | finalize-from-every-phase tests + manual quit-during-recording per S |
| ERROR-hold changes UX of failure path | Down-from-ERROR admitted; tray keeps error note until cycle end |

## 16. Out of scope

Windows/Linux, Tauri, wake-word, VAD-as-stop, cloud-by-default, history
SQLite, `small.en` default swap, notarization/signing changes.
