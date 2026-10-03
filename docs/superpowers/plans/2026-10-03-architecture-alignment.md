# Architecture Alignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring wiflow to full conformance with the target architecture diagram via six gated phases (S0–S5): housekeeping commit, module layout + traits, 8-phase orchestrator with centralized finalize, media-duck integration, pure-Rust UI layer, provider minors + docs + acceptance gate.

**Architecture:** Sequential strangler (spec D6): mechanical `git mv` first, then traits behind existing call sites, then state-machine, media, and UI — every task ends with `fmt + clippy -D warnings + cargo test` green and a commit. A new `app/orchestrator.rs` (`Orchestrator` + `Action` effect seam) separates testable lifecycle logic from the winit shell so all §13 spec tests run headless.

**Tech Stack:** Rust 2021, MSRV 1.75 (single binary `wiflow-dictation`); winit 0.30 + tray-icon/muda/global-hotkey (existing); egui 0.31-line + egui-winit + egui_glow only if the S4 spike proves toolchain-compatible (Task 13 decides); no new paid/cloud deps ($0 rule).

**Spec:** `docs/superpowers/specs/2026-10-03-architecture-alignment-design.md` — the plan argues from the spec; executors read both. `(Hn)` tags below trace to spec hardening items.

## Global Constraints

- Gates before every commit: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (baseline 129 tests, never regresses).
- TDD: every new-logic task writes the failing test first, runs it red, implements, runs it green.
- `core/` imports nothing from `ui/` or `platform/` impl modules (H25, rules.md §3). `main.rs` stays bootstrap-only: parse args, init logging, build orchestrator, run loop (H26).
- Privacy: no audio-sample logging anywhere; pill RMS feed is amplitude-only (`f32` bits in an `AtomicU32`, never samples).
- $0 default: no new dependency that costs money or requires keys; never commit keys/URLs-with-auth.
- Conventional commits (`feat:`/`fix:`/`docs:`/`test:`); one task = one commit (cherry-picks keep original hashes/messages).
- Live smoke per phase with `/tmp/poster_kb` (e.g. `/tmp/poster_kb 1200` = one full Fn hold cycle); manual checks are steps with explicit expected observations.

## Review Focus

Spec-implied failure modes no happy-path test exercises; each is pinned to its owning task's tests:

1. Late `CaptureStarted` arriving after Esc-cancelled STARTING must not open the mic or show the pill → Task 8 test `stale_capture_started_after_cancel_ignored`.
2. Output device unplugged between duck and restore → loud log, session still completes, no trap in finalize → Task 11 test `restore_with_vanished_device_completes`.
3. Session A's delayed pause timer firing during session B must not pause B's audio (epoch isolation) → Task 11 test `stale_pause_timer_from_previous_session_ignored`.
4. `osascript` notification failing during the ERROR path must not wedge finalize → Task 16 test `notify_failure_never_fails_session`.
5. Duplicate `Done` (worker race) must not double-push history or double-inject → Task 8 test `duplicate_done_ignored_by_phase_gate`.

---

## File structure (locked by this plan)

**S1 moves (`git mv`, no behavior change):**
- `src/audio.rs, vad.rs, stt.rs, groq_stt.rs, analyze.rs, cleanup.rs, history.rs, config.rs, hotkey.rs, baseline.rs` → `src/core/` (fix `crate::x` → `crate::core::x`; `baseline.rs` keeps `#[cfg(test)]`, re-exported for existing test imports)
- `src/tap.rs, src/inject.rs` → `src/platform/macos/`; `focused_app_name()` moves `daemon.rs` → new `src/platform/macos/context.rs` (pure osascript call; daemon calls it as a leaf)
- `src/app.rs` → `src/app/mod.rs`
- `cleanup_prompt.txt, context_prompt.txt` stay or move with their loader — Task 2 verifies `prompt_path()` is runtime-relative first
- `ptt.rs, daemon.rs, logfile.rs, main.rs` stay top-level

**Created later:** `src/core/traits.rs` (T4) · `src/app/orchestrator.rs` + `src/app/headless.rs` (T3/T8) · `src/platform/macos/duck.rs` (T10) · `src/platform/macos/panel.rs` (T15) · `src/ui/{settings,pill,notify}.rs` (T14–T16)

---

### Task 1 (S0): Commit pending PTT-lifecycle work

**Files:** stage exactly: `src/app.rs src/audio.rs src/daemon.rs src/main.rs src/tap.rs` (modified) + `src/ptt.rs src/logfile.rs docs/ptt-lifecycle-debug.md CONTEXT.md` (new). Nothing else.

**Interfaces:** Consumes: working tree at `3d29d43`. Produces: clean tree, PTT work on `main`.

- [ ] **Step 1: Verify the staging list matches the spec §4 inventory and nothing else is staged**
- [ ] **Step 2: Run the gates** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` — Expected: all green (129 tests)
- [ ] **Step 3: Commit** `git add <files above>` + `git commit -m "feat: PTT lifecycle hardening (admission gate, watchdog, session logging)"`
- [ ] **Step 4: Verify** `git status --short` shows only the S3/S4 leftovers (`stash@{0}` untouched) and `git log --oneline -1` is the new commit

### Task 2 (S1): Mechanical layout move — zero behavior change

**Files:** `git mv` per the File-structure map above; edit only `mod` declarations (`main.rs:1-18` → `mod app; mod core; …` + `path` attributes or `mod.rs` files) and `crate::` paths; move `focused_app_name` to `platform/macos/context.rs`.

**Interfaces:** Consumes: Task 1 tree. Produces: `crate::core::*`, `crate::platform::macos::{tap,inject,context}`, `crate::app::` paths; all 129 tests passing unmodified.

- [ ] **Step 1: Confirm prompt loader is move-safe** — read `config::prompt_path()`; assert it resolves at runtime (Application Support / exe-relative), not `src/`-relative. If src-relative, move the two `.txt` files with their loader and update the path (still no behavior change).
- [ ] **Step 2: Perform the `git mv` moves + path fixes** (no logic edits; `config.rs` keeps its `pub use crate::daemon::HotkeyPreset` re-export working via updated path)
- [ ] **Step 3: Run the gates** — Expected: `cargo test` 129 pass, fmt + clippy clean
- [ ] **Step 4: Live smoke** — launch `--app`, one `/tmp/poster_kb 1200` cycle → tray returns to Idle, mic released (same as pre-move)
- [ ] **Step 5: Commit** `git commit -m "refactor: core/platform/app module layout (no behavior change)"`

### Task 3 (S1): `main.rs` becomes bootstrap-only (H26)

**Files:** Create `src/app/headless.rs`; modify `src/main.rs` (delete `main.rs:91-277` simulate pipeline + `dump_wav`/`maybe_dump_wav` helpers, keep `Args` + dispatch).

**Interfaces:** Consumes: `Args` (unchanged flags). Produces: `pub fn app::headless::run_simulate_hold(args: &SimulateArgs)` — moved verbatim (same core calls, same STT branch, same clipboard-fallback behavior); `main()` only parses args, inits logging, dispatches to `app::run` / `run_simulate_hold` / `list_devices`.

- [ ] **Step 1: Move the code verbatim** (no cleanup, no dedup with the daemon pipeline — that merge is out of scope; move only, per H26)
- [ ] **Step 2: Run the gates** — Expected: green
- [ ] **Step 3: Headless smoke** — `cargo run -- --simulate-hold-ms 500` on a mic-less machine → exits 0 via the existing "capture failed (expected in CI without mic)" path; `--help` lists all flags
- [ ] **Step 4: Commit** `git commit -m "refactor: main.rs bootstrap-only, simulate-hold moved to app::headless"`

### Task 4 (S1): Four core traits, wired with identical behavior

**Files:** Create `src/core/traits.rs`; modify call sites: `daemon.rs` pipeline (`pipeline_on_worker:207-368` STT branch → `SpeechRecognizer`, cleanup chain → `CleanupProvider`, `focused_app_name()` → `ContextProvider`), `app/` Done handler (`inject::inject_text` → `TextInjector`).

**Interfaces:**
- Consumes: existing fns (signatures unchanged).
- Produces (exact):
  - `pub trait SpeechRecognizer { fn transcribe(&self, audio: &[f32], sample_rate: u32, cfg: &Config) -> Result<String, String>; }` + `pub struct RouterRecognizer;` (impl = current groq→local-fallback branch, daemon.rs:245-274 moved verbatim)
  - `pub trait CleanupProvider { fn clean(&self, text: &str, ctx: Option<&str>, cfg: &Config) -> CleanupOutcome; }` + `pub struct ChainProvider;` (impl = current route + `clean_chain` block, daemon.rs:283-328; `CleanupOutcome` = existing `(String, Vec<String>)` shaped as struct `{ text, issues }`)
  - `pub trait TextInjector { fn inject(&self, text: &str) -> Result<InjectReport, String>; fn leave_on_clipboard(&self, text: &str); }` + `pub struct SystemInjector;` (impl = `platform::macos::inject::{inject_text, leave_on_clipboard}`)
  - `pub trait ContextProvider { fn focused_app(&self) -> Option<String>; }` + `pub struct OsascriptContext;` (impl = moved `focused_app_name`)

- [ ] **Step 1: Failing tests first** — `core/traits.rs` tests with hand fakes (`FakeRecognizerfreq`, `FakeInjectorFail`): fake STT text flows through; failing injector triggers `leave_on_clipboard`. Run `cargo test core::traits` — Expected: FAIL (traits don't exist)
- [ ] **Step 2: Define traits + real impls** (bodies = moved code, zero logic change)
- [ ] **Step 3: Switch call sites to trait calls** with the same concrete structs
- [ ] **Step 4: Run the gates** — Expected: green, suite still 129 + new trait tests
- [ ] **Step 5: Live smoke** — one full cycle via poster_kb (STT + cleanup + inject path exercised through traits)
- [ ] **Step 6: Commit** `git commit -m "feat: core traits (recognizer, cleanup, injector, context) with identical behavior"`

### Task 5 (S2): 8-phase `PttMachine` — pure transition validator (H1)

**Files:** Modify `src/ptt.rs` (phases + methods + tests).

**Interfaces:** Consumes: `Admission::{Accept, Ignore}` (unchanged).
Produces (exact):
- `pub enum Phase { Idle, Starting, Listening, Processing, Injecting, Restoring, Cancelled, Error }`
- Kept: `on_down()` (Idle→Starting), `on_up()` (Listening→Processing), `on_cancel()` (Listening|Starting→Cancelled), `on_watchdog()` (Listening→Processing)
- New: `on_capture_started()` (Starting→Listening; all other phases → Ignore — the stale-`CaptureStarted` rule) · `on_transcript()` (Processing→Injecting) · `on_empty()` (Processing→Restoring) · `on_inject_ok()` (Injecting→Restoring, error flag false) · `on_inject_failed()` (Injecting→Restoring, error flag true) · `on_failed()` (any active→Restoring; records error target = phase != Cancelled) · `on_finalized()` (Restoring→Error if any fail-flag was recorded since entering Restoring, else Idle; clears flags)
- [ ] **Step 1: Failing tests** — port the 12 existing tests to renamed phases (Down→Up→on_transcript→on_inject_ok→on_finalized→Idle, etc.); add spec §13.1 machine-level tests: `starting_requires_capture_started` (Up/Esc rules in Starting; stray CaptureStarted ignored unless Starting), `inject_failure_records_error_target`, `cancel_failed_resolves_idle`, `fatal_failed_resolves_error` (the one intentional target change, spec §7.9), `down_from_error_admitted`. Run `cargo test ptt` — Expected: FAIL (methods don't exist)
- [ ] **Step 2: Implement the machine** (pure; only new `tracing` lines are transition logs like `set()` today)
- [ ] **Step 3: Run `cargo test ptt`** — Expected: all green (12 ported + new)
- [ ] **Step 4: Run the gates** (full suite)
- [ ] **Step 5: Commit** `git commit -m "feat: 8-phase PTT machine (starting, injecting, restoring, error)"`

### Task 6 (S2): `CaptureStarted` event — worker reports, orchestrator gates

**Files:** Modify `src/daemon.rs` (`DaemonEvent` + `Control::Down` arm ~470-512), `src/app/` routing (Task 8 wires it; here: event + emission + worker test).

**Interfaces:** Consumes: `AudioCapture::start` result. Produces: `DaemonEvent::CaptureStarted` (fieldless variant); worker sends it immediately after `AudioCapture::start` Ok (daemon.rs:477-484), before any transcription; capture-failure `Failed` path unchanged.

- [ ] **Step 1: Failing test** — worker-level: simulate `Control::Down` with no mic → expect `Failed("capture failed…")`, never `CaptureStarted`. (Uses existing no-mic graceful path; asserts event ordering contract.) Run — Expected: FAIL (variant missing)
- [ ] **Step 2: Add variant + emission** (one `send_event` after capture-ok log line; on send failure, log + drop capture — never wedge)
- [ ] **Step 3: Gates green + commit** `git commit -m "feat: CaptureStarted worker event"`

### Task 7 (S2): `Session` + immutable snapshot (H5, H6, H24, H29)

**Files:** Create `src/app/session.rs` (`Session`, `AudioRef`, `MediaSnapshot`); modify daemon `SESSION` counter usage (counter stays as id source; ownership moves to orchestrator-held `Session`).

**Interfaces:** Produces (exact):
```rust
pub struct AudioRef { pub session_id: u64 }        // correlation only — never samples (H5)
pub struct MediaSnapshot { pub output_device: Option<String>, pub was_playing: bool }  // S3 fills was_playing for real; S2 defaults false/None
pub struct Session {
    pub id: u64, pub started_at_ms: u64,
    pub settings: Config,                          // full clone at STARTING (H6/H24)
    pub watchdog_ms: u64,                          // env WIFLOW_MAX_RECORDING_MS read at STARTING (same source as worker)
    pub app_context: Option<String>,               // None on timeout/failure (H19; bounded, never gates capture)
    pub audio_ref: AudioRef, pub media: MediaSnapshot,
}
pub fn begin_session(settings: &Config, ctx: impl ContextProvider) -> Session  // reads NOTHING live after construction
```

- [ ] **Step 1: Failing tests** — `snapshot_freezes_settings` (mutate global config after `begin_session` → session.settings unchanged), `context_timeout_yields_none_and_completes` (fake provider that sleeps/errors → `app_context == None`, no panic), `session_ids_monotonic`. Run — Expected: FAIL
- [ ] **Step 2: Implement** (context acquired inside `begin_session` with bounded wait; on failure/timeout → None + warn log with session id)
- [ ] **Step 3: Gates green + commit** `git commit -m "feat: Session with immutable STARTING snapshot"`

### Task 8 (S2): Orchestrator + centralized finalize + supervision + shutdown

**Files:** Create `src/app/orchestrator.rs`; modify `src/app/mod.rs` (`user_event` arms delegate to it; Done-handler inject + Idle-reset moves into INJECTING/RESTORING handling; `window_event` untouched).

**Interfaces:** Produces (exact):
```rust
pub enum Action { SendControl(Control), SetTray(AppState, Option<String>), Notify(String), Inject(String) }
pub struct Orchestrator { /* machine: PttMachine, session: Option<Session>, supervision_at: Option<Instant>, media: NoopMedia (S3 seam) */ }
impl Orchestrator {
    pub fn new(recognizer: RouterRecognizer, cleanup: ChainProvider, injector: SystemInjector, context: OsascriptContext) -> Self
    pub fn handle(&mut self, ev: &DaemonEvent) -> Vec<Action>   // full §7.2 routing incl. Failed→fatal-flag rule (§7.9), stale rules, Down-from-Error
    pub fn tick(&mut self) -> Vec<Action>                      // supervision deadline check (LISTENING expiry → finalize → ERROR + Notify)
    pub fn handle_shutdown(&mut self) -> Vec<Action>           // any phase → finalize → worker-stop actions (H30)
}
pub(crate) fn finalize_session(&mut self, fatal: bool) -> Vec<Action>  // THE funnel (H7): drop capture handle (orchestrator side) → restore_media() [S2 = safe no-op seam, tested] → hide pill (SetTray Idle/Error) → release audio_ref → session-end log line → release resources → on_finalized machine step
```
`App` executes `Action`s (real `send_control`, tray, `osascript`, main-thread `inject` — the enigo main-thread constraint from daemon.rs:360 stays: `Inject` executes only in `user_event`).

- [ ] **Step 1: Failing orchestrator tests** (all headless, fake clock where time matters): happy path actions sequence Down→[SetTray Recording, SendControl] … Done(text)→[Inject] …; `esc_in_starting_cancels_pending_capture`; `stale_capture_started_after_cancel_ignored` (Review #1); `duplicate_done_ignored_by_phase_gate` — assert single Inject + single history-effect (Review #5); `finalize_is_idempotent` (second call → no actions); `supervision_deadline_fires_without_worker_events`; `shutdown_from_listening_releases_everything`. Run — Expected: FAIL
- [ ] **Step 2: Implement routing + finalize** (move Done-handler inject/history logic from `app/mod.rs` into the INJECTING step; history push stays exactly-once: worker pushes pre-Done as today, orchestrator never pushes)
- [ ] **Step 3: Delegate `user_event` arms** (PttDown/PttUp/Cancel/Watchdog/TapIssue/Done/Failed/CleanupIssue) to `handle()` + execute returned `Action`s; wire `tick()` into `about_to_wait`; wire `handle_shutdown()` into the quit path before existing `stt::shutdown()` (app.rs:840)
- [ ] **Step 4: Gates green + commit** `git commit -m "feat: AppOrchestrator with centralized finalize_session"`

### Task 9 (S2): Live regression gate

**Files:** none (verification task). Requires Tasks 5–8 committed.

- [ ] **Step 1: Full suite green** `cargo test` — record count (129 + S2 additions)
- [ ] **Step 2: PTT synthetic matrix re-run** (conditions A/B/C/D/H from `docs/ptt-lifecycle-debug.md`) — Expected: same pass profile as the hardened baseline (50/50-class behavior; any deviation investigated, not shrugged)
- [ ] **Step 3: Single-cycle smoke** `/tmp/poster_kb 1200` → log shows `STARTING → CaptureStarted → LISTENING → PROCESSING → INJECTING → RESTORING → IDLE` with one session id throughout; mic released; tray Idle
- [ ] **Step 4: Record results** — append outcome line to `docs/ptt-lifecycle-debug.md`; commit `git commit -m "docs: S2 live regression results"`

### Task 10 (S3): Cherry-pick the duck trilogy + relocate

**Files:** `git cherry-pick d9172df`, then `a8e3fed`, then `40a7233`; move result `src/duck.rs` → `src/platform/macos/duck.rs`; resolve hunks into new paths.

**Interfaces:** Consumes: post-S1 tree + upstream commits. Produces: identical duck behavior at new paths; `duck_enabled` config field alive at `core/config.rs`.

- [ ] **Step 1: Cherry-pick `d9172df`** — expect conflicts in `src/app.rs` (→`src/app/mod.rs` + orchestrator), `src/config.rs` (→`src/core/config.rs`), `src/daemon.rs`, `src/main.rs` (→`main.rs` + `app/headless.rs`). Resolve keeping new structure; new `src/duck.rs` lands
- [ ] **Step 2: Gates green** (suite must pass before next pick — bisectability)
- [ ] **Step 3: Cherry-pick `a8e3fed`, gates green; cherry-pick `40a7233`, gates green**
- [ ] **Step 4: `git mv src/duck.rs src/platform/macos/duck.rs`** + fix `crate::` paths; gates green
- [ ] **Step 5: Live check** — pre-existing duck behavior works from new location (Music ducks on hold, restores after inject). Commit each pick resolution separately (3 commits, original messages kept via `-x`).

### Task 11 (S3): `MediaController` contract + state model + headless media tests

**Files:** Create `src/platform/macos/duck.rs` additions (or `media.rs` wrapper if `duck.rs` is left byte-identical — prefer wrapper `src/platform/macos/media.rs` so cherry-picked code stays pristine); modify `src/core/traits.rs` (+`MediaController`), `src/app/session.rs` (`MediaSnapshot` fill).

**Interfaces:** Produces (exact):
```rust
// core/traits.rs
pub trait MediaController { fn duck(&mut self); fn restore(&mut self); }
// platform/macos/media.rs
pub struct MediaSessionState { pub was_playing_before: bool, pub output_device: Option<String>, pub ducked_by_wiflow: bool, pub paused_by_wiflow: bool, pub restoration_required: bool, pub epoch: u64 }
pub struct CoreAudioDuck { state: MediaSessionState, clock: Box<dyn Clock>, backend: Box<dyn AudioBackend> }  // seams for headless tests
impl MediaController for CoreAudioDuck  // duck() = immediate duck → arm 600ms gate → conditional pause (H11); restore() = bump epoch → resume-only-what-we-paused → exact per-channel restore → clear flags (H3/H12); all four H13 rules hold
```

- [ ] **Step 1: Failing tests** — all §13.2 scenarios headless via fake clock/backend: already-paused (no pause claimed) · playing (duck→pause→resume) · duck-only short hold · gate firing · pause failure (restore safe) · cancel-during-delay (pause never fires — H12) · duplicate duck/restore no-ops · device change mid-session · vanished device at restore (Review #2) · stale timer from previous epoch ignored (Review #3, H13) · STT-failure-after-pause restores · injection-failure-after-pause restores. Run — Expected: FAIL
- [ ] **Step 2: Implement** (adapt cherry-picked machine behind the trait; orchestrator timing logic, if any arrived via picks, moves inside `duck()`)
- [ ] **Step 3: Gates green + commit** `git commit -m "feat: MediaController with idempotent restore and epoch-guarded pause"`

### Task 12 (S3): Wire media into lifecycle + live verification

**Files:** Modify `src/app/orchestrator.rs` (LISTENING entry → `duck()`; fill the S2 `restore_media()` seam with controller delegation; `NoopMedia` removed), `src/app/session.rs` (real `was_playing_before`/`output_device` into snapshot).

**Interfaces:** Consumes: Task 11 controller. Produces: universal restore — every §8.3 row routes through `finalize_session()`; no ad-hoc `restore()` call sites.

- [ ] **Step 1: Wire + assert the seam swap** — existing finalize tests still pass unchanged (seam contract held); add `cancel_in_starting_needs_no_restore` (restore on clean session = no-op actions)
- [ ] **Step 2: Gates green**
- [ ] **Step 3: Live checks** — Music playing → hold Fn → duck+pause → release → inject → resumed+volumes exact; Esc-during-gate → pause never fires, volumes restored; empty-transcript cycle restores
- [ ] **Step 4: Commit** `git commit -m "feat: wire media duck/restore into session lifecycle"`

### Task 13 (S4): egui integration spike — time-boxed, decision-forcing

**Files:** Scratch only (uncommitted or a `spike/` branch); no `src/` changes except possibly `Cargo.toml` (reverted if no-go).

- [ ] **Step 1: Verify toolchain fit** — `cargo add egui@0.31 egui-winit@0.31 egui_glow@0.31`; `cargo check` against winit 0.30 + MSRV 1.75. If MSRV conflicts → STOP, report to user (toolchain bump is a user decision, not an executor decision)
- [ ] **Step 2: Prove the critical unknowns** (≤30 min each): (a) second `winit::Window` on the existing loop receives events in `window_event`; (b) egui frame renders into it via `egui_glow`; (c) window can be shown/hidden without touching the tray loop
- [ ] **Step 3: Record go/no-go** in `task.md`: go → Tasks 14–16 proceed with deps pinned; no-go on waveform only → settings proceeds, pill deferred per spec fallback (flagged to user before proceeding)

### Task 14 (S4): Settings window

**Files:** Create `src/ui/settings.rs` (+ `src/ui/mod.rs`); modify `src/app/mod.rs` (tray menu opens window; settings writes go through App handlers → next-session rule H24).

**Interfaces:** Consumes: `Config` load/save, `audio::list_devices`, history APIs. Produces: `pub struct SettingsWindow` rendering all spec §9 panels (hotkey recorder rejecting single keys, mic/model pickers, media toggle = `duck_enabled`, provider settings, launch-at-login, history browser copy/clear, permissions panel + Test-record button).

- [ ] **Step 1: Failing tests** — settings view-model tests (no window needed): `recorder_rejects_single_keys` (macOS can't register them), `save_then_reload_roundtrip` (write → load → equal), `duck_toggle_persists`, `test_record_formats_rms_result`. Run — Expected: FAIL
- [ ] **Step 2: Implement window** (Task 13 pattern; opens from tray menu; never touches daemon/PTT directly — intent commands only, H20)
- [ ] **Step 3: Gates green; live round-trip** — change mic + toggle duck → reload shows values → next session uses them, active session unaffected (H24)
- [ ] **Step 4: Commit** `git commit -m "feat: settings window (winit/egui)"`

### Task 15 (S4): Recording pill — strictly observational (H21)

**Files:** Create `src/ui/pill.rs`, `src/platform/macos/panel.rs` (non-activating NSPanel level via objc — new `objc` dep only if spike shows no std route; prefer `winit` window attributes + minimal objc); modify `src/core/audio.rs` (per-callback RMS `AtomicU32` bits feed — amplitude only).

**Interfaces:** Produces: `pub struct Pill { visible: bool }` with `show()`/`hide()` called ONLY from `finalize_session()` step 3 and LISTENING entry; renders phase (passed in), RMS bars (smoothed read of the atomic), elapsed timer, "Release to transcribe · Esc cancels". Holds no recording state.

- [ ] **Step 1: Failing tests** — `rms_bits_roundtrip`, `smoothing_converges`, `pill_has_no_lifecycle_state` (construct + render with fake inputs; assert no Close/Control effects exist in its API)
- [ ] **Step 2: Implement feed + pill + panel** (focus-steal safety = non-activating panel; verified live)
- [ ] **Step 3: Gates green; live focus check** — dictate into Terminal with pill visible → Terminal keeps focus throughout; pill hides on PttUp, Esc, error, watchdog, capture failure, quit (H22 — one check each)
- [ ] **Step 4: Commit** `git commit -m "feat: observational recording pill"`

### Task 16 (S4): Notifications + spinner + failure safety

**Files:** Create `src/ui/notify.rs`; modify `src/app/mod.rs` (Transcribing spinner frames via existing `icon_rgba`/`make_icon` + tick; notify calls at Done/Failed/permission paths).

**Interfaces:** Produces: `pub fn notify(title: &str, body: &str) -> Result<(), String>` (osascript `display notification`; binary path injectable for tests); callers ALWAYS ignore Err after tray-note fallback + log (H23). `pub fn spinner_frame(tick: u64) -> Icon` (frames while PROCESSING).

- [ ] **Step 1: Failing tests** — `notify_failure_never_fails_session` (bogus binary path → Err; orchestrator finalize with notifier failing still reaches terminal phase — Review #4); `spinner_frames_cycle`; `notification_body_truncates_long_transcripts` (cap length for Notification Center)
- [ ] **Step 2: Implement + wire** (transcription-complete preview, errors, permission prompts; fallback = existing `warn_note`)
- [ ] **Step 3: Gates green; live check** — complete a cycle → notification appears; break osascript (rename binary temporarily) → cycle still completes, tray note instead
- [ ] **Step 4: Commit** `git commit -m "feat: native notifications and transcribing spinner"`

### Task 17 (S5): Groq STT → `whisper-large-v3-turbo` (D4)

**Files:** Modify `src/core/groq_stt.rs:64` (+ doc comment `:51`), `src/main.rs` log line, `src/daemon.rs` log line, tests at `groq_stt.rs:111-127`.

**Interfaces:** Consumes: `"whisper-large-v3"` literals. Produces: `"whisper-large-v3-turbo"` everywhere user-visible and wire-visible.

- [ ] **Step 1: Failing test first** — update multipart test to expect `turbo`; run — Expected: FAIL on old id
- [ ] **Step 2: Swap all four sites** (find via `grep -rn 'whisper-large-v3"' src/` — quoted form excludes already-turbo)
- [ ] **Step 3: Gates green; live transcription check** (one Groq cycle, `--app` with provider groq — needs key; if no key available, record as blocked-with-evidence and keep local path green)
- [ ] **Step 4: Commit** `git commit -m "feat: groq STT whisper-large-v3-turbo"`

### Task 18 (S5): GPU/CPU fallback (Metal failure → CPU retry)

**Files:** Modify `src/stt.rs` model-load path.

**Interfaces:** Produces: `pub(crate) fn backend_for(metal_ok: bool) -> Backend` (pure decision fn; `Backend::{Metal, Cpu}`) + load path: on Metal-context error → warn log → retry same model on CPU instead of failing the cycle.

- [ ] **Step 1: Read `stt.rs` load path** — locate whisper context creation + `use_gpu` flag; pin exact insertion point in the task log (comment on the commit if shape differs from this plan)
- [ ] **Step 2: Failing test** — `backend_for(false) == Cpu`, `backend_for(true) == Metal`; fault-injection test forces Metal error → returns CPU-loaded context (or documented simulator if hardware can't fault it)
- [ ] **Step 3: Implement fallback** (no behavior change on the success path; local-only, $0)
- [ ] **Step 4: Gates green + commit** `git commit -m "feat: CPU fallback on Metal init failure"`

### Task 19 (S5): Hotkey conflict detection (pragmatic scope, spec §10.3)

**Files:** Modify `src/app/mod.rs` startup path (near `app_main` registration).

**Interfaces:** Produces: `fn hotkey_failure_note(prefer: HotkeyPreset) -> String` (names the failed preset + actionable guidance: choose another preset in menu / check System Settings → Keyboard for conflicts); startup `Err` from `register_ptt_hotkey` → `AppState::Error` + note (today it must not fail silently — verify current behavior first).

- [ ] **Step 1: Read startup path** — confirm what happens today on registration `Err`; failing test on `hotkey_failure_note` content (mentions preset + next action)
- [ ] **Step 2: Implement surfacing** (no fake system-wide enumeration — spec-explicit)
- [ ] **Step 3: Gates green + commit** `git commit -m "feat: actionable hotkey registration-failure note"`

### Task 20 (S5): Docs reconciliation + acceptance gate run (H31, H32)

**Files:** Modify `architecture.md`, `task.md`, `memory.md`.

- [ ] **Step 1: Write `architecture.md` "Intentional deviations"** (history search = future/not built; JSON config per D5; 16 kHz at VAD stage; any S0–S4 deviations found) **vs "Alignment status"** (what now matches the diagram) — the two must never blur (H31)
- [ ] **Step 2: Update `task.md` checkboxes + `memory.md`** (dated decisions: orchestrator ownership, universal restore, turbo model, egui spike outcome)
- [ ] **Step 3: Run the acceptance gate** (spec §10.5) item by item, each with evidence (test counts, live-check logs); any ❌ becomes a new task, not a waiver
- [ ] **Step 4: Gates green + commit** `git commit -m "docs: architecture reconciliation and acceptance gate"`

---

## Self-review (run against the spec before handoff)

**1. Spec coverage:** §4 S0→T1 · §6 layout→T2, H26→T3, traits→T4 (MediaController→T11) · §7 machine→T5, CaptureStarted→T6, Session/H6→T7, routing+finalize+H7/H24→T8, live→T9 · §8 picks→T10, contract/H10–H13→T11, wiring+H2→T12 · §9 spike→T13, settings→T14, pill→T15, notify→T16 · §10 turbo→T17, CPU→T18, conflicts→T19, docs+gate→T20 · §11 ERROR ordering→T8 finalize + T5 `on_finalized` · §12 semantics→T4/T8/T11/T12 · §13 matrix→T5/T7/T8 (lifecycle+isolation) + T11 (media) · H30→T8 · H31/H32→T20. No gaps.
**2. Step scan:** each step names one action + checkable result; bodies appear only where signatures/tests underdetermine (epoch rules T11, Action seam T8, finalize order T8) — the rest is signatures + assertions + commands.
**3. Type consistency:** `Phase`/`Admission`/`DaemonEvent::CaptureStarted`/`Session`/`MediaSessionState`/`Action`/`Orchestrator` named once in Interfaces and reused verbatim downstream (T5→T8→T12 chain checked).
**4. Review Focus:** five items each pinned to an owning task test above.
**5. Proportion:** 20 tasks for a 6-phase migration; code blocks limited to new-type definitions the implementer cannot derive; everything else is signatures, assertions, commands.
