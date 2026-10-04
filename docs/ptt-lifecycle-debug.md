# PTT lifecycle bug — diagnosis tracker (Fn / CGEventTap)

Symptom: hold Fn → mic on → release → mic stays on, tray stuck Recording, no
transcription, no useful logs.

Phases (user-specified):
- [x] P1 Instrument complete lifecycle (tap → daemon → state → audio → flush → STT → idle)
- [x] P2 Reproduce systematically (A quick, B 1s, C 5s, D 10s+, E silence, F speaking,
      G background media, H 20 cycles)
- [x] P3 Inspect CGEventTap (enabled? runloop alive? flags derivation? disabled-by-timeout?)
- [x] P4 Explicit idempotent PTT state machine (Idle/Recording/Processing/Cancelling)
- [x] P5 Audio teardown verification (stop → drop stream → thread exit → flush → process)
- [x] P6 Safety watchdog (max recording duration per PRD §6 "Long press >60s → auto-stop")
- [x] P7 macOS permission/tap failure handling (Input Monitoring, tap creation, tap disabled)
- [ ] P8 Regression tests (state machine unit tests DONE: 12 ptt + 4 tap + full suite 129
      pass; 20-cycle manual physical Fn test → AWAITING USER)

## Root cause
TBD — working hypothesis under test (see evidence). Established so far:
PttUp is lost **before** `user_event` (tray stays `Recording`, worker keeps
`capture` → mic stuck). Every automatable layer of the chain is proven
healthy (see evidence), so the loss is specific to real-world conditions
that only a physical Fn run with instrumentation can pin down.

## Evidence log (2026-10-03)
1. **Stash `stash@{0}`** ("pre-reset WIP: tap fix, file logging, show-logs",
   based on the reset-away duck commits d9172df/a8e3fed/40a7233): prior
   session already wrote tap-disable handling claiming a live observation —
   "a kill landing mid-hold previously ate the key-up and wedged the mic
   open (orange dot forever)". Never verified/committed; current HEAD
   bb416f0 has none of it.
2. **`/tmp/tapprobe` (standalone tap probe)**: Input Monitoring OK; synthetic
   flagsChanged down/up delivered (kc=63, fn=1 / fn=0); holds of 10s/30s
   deliver the UP reliably; a 8–15s stall *inside* the callback with the UP
   pending does NOT trigger `kCGEventTapDisabledByTimeout` on this macOS
   (listen-only session tap) — events deliver after resume.
3. **Plain keyboard posts of kc=63 to kCGHIDEventTap** produce
   system-derived flagsChanged (fn=1 down, fn=0 up) — hardware-faithful
   simulation path (`/tmp/poster_kb`).
4. **Full matrix on the real app** (HEAD, Fn preset, synthetic events):
   A(200ms discard), B(1s), C(5s), D(10s), H(20×1s) — **all cycles complete,
   mic released every time**. Tap→winit→worker→teardown chain healthy.
5. **Code inspection (certain defects regardless of trigger)**:
   - `tap.rs`: no handling of out-of-band `0xFFFFFFFE/0xFFFFFFFF`
     (Apple-recommended re-enable missing), no logging at all, no tap
     thread-death signal.
   - No max-recording watchdog: `PushToTalk::max_ms` only clamps duration
     at key-up; a lost PttUp leaves the mic open forever (PRD §6 unmet).
   - No explicit PTT state machine app-side: `PttDown` unconditionally sets
     `Recording` even while `Transcribing` (Phase 4 rules violated).
   - `worker_main` ignores `tx.send` failures silently.
6. **No live logs exist** anywhere (no file logging; unified log has no
   wiflow/tap entries) → "logs show nothing useful" is structural.
7. **Integration run 2026-10-03 09:14 (instrumented build, WIFLOW_MAX_RECORDING_MS=3000)**
   — log: `~/Library/Logs/wiflow/wiflow.log`:
   - Watchdog verified twice: DOWN with Up delayed → `WATCHDOG: PttUp LOST —
     force-stopping recording after 3000ms` WARN at exactly +3000ms, mic
     teardown → `mic released, flushed 132096 samples over 3023ms`, tray
     Recording→Processing (watchdog auto-stop)→Idle; the LATE Up (poster
     released at 8s/5s) → `PttUp IGNORED (phase Idle/Processing)` — dropped
     cleanly, no wedge.
   - Normal cycles (1s/5s/200ms discard/10×400ms rapid + a real speech
     cycle that transcribed "Hello" and injected): every cycle
     Idle→Recording→Processing→Idle with `mic released` each time.
   - Interleaved REAL physical Fn presses (flags `0x800100`, nonCoalesced
     bit, vs poster `0x20800000`) appeared mid-run (sessions 2/4/6/20) —
     all admitted/ignored exactly per the state machine; duplicate Down
     while Recording ignored; no wedge.
   - 17 `capture started` vs 16 `mic released`: the 1 missing pair is
     session 20, where the test script SIGKILLed the app mid-hold (process
     death releases the device — not a leak).
   - Default config (60s watchdog) sanity cycle: clean, no watchdog.
8. **`cargo test`: 129 passed, 0 failed, 1 ignored; `cargo build`: 0 warnings.**
 9. **S2 live regression (2026-10-04 04:55, HEAD a48c5bb)** — full suite **197 passed, 0 failed, 1 ignored**; matrix A(200ms)/B(1s)/C(5s)/D(10s)/H(20×1s) **all 24 cycles complete, mic released every time, tray Idle**; 8-phase sequence confirmed (IDLE→STARTING→LISTENING→PROCESSING→INJECTING/RESTORING→IDLE); 23 `capture started` ↔ 23 `mic released` ↔ 23 `session ended` (0 orphaned); config restored byte-identical (`diff` clean).

### Evidence clarifications (Task 9 / S2 live regression gate)
- **poster_kb invocations vs “sessions” accounting:** 24 synthetic key presses were posted total (**A=1, B=1, C=1, D=1, H=20**). In the retained `~/Library/Logs/wiflow/wiflow.log`, we observed **24** `PttDown` and **24** `PttUp` tap events, but only **23** occurrences of `capture started — mic open` / `mic released` / `session ended` (1 key press did not reach the mic-capture→session-ended stage in logs).
- **Session id ambiguity (why the report mentioned a range):** `session=...` ids in the log are internal worker/orchestrator ids and span a wider range during the whole matrix run (up to `session=47`). For a *single cycle*, the orchestrator/worker `session` is consistent; e.g. the cycle that showed **full 8-phase** transitions includes:
  - `state Idle -> Starting (PttDown)`
  - `state Starting -> Listening (CaptureStarted)`
  - `state Listening -> Processing (PttUp)`
  - `state Processing -> Injecting (transcript)`
  - `[session=5] worker Control::Down` / `[session=5] worker Control::Up` / `[session=5] capture started — mic open` / `[session=5] session ended`
  - `state Injecting -> Restoring (inject ok)`
  - `state Restoring -> Idle (finalized)`
- **Config restore evidence:** `/tmp/config_backup.json` md5=`7bf14202e6524b499dc6d2e97ba2e2f2` and the restored `~/Library/Application Support/wiflow/config.json` md5=`7bf14202e6524b499dc6d2e97ba2e2f2` match; running `diff /tmp/config_backup.json ~/Library/Application\ Support/wiflow/config.json` produced **no diff output**.

### Pristine single-cycle evidence (Task 9 / S2 live regression gate, Round 2)

#### 1) “24 vs 23 accounting” — what happened to the extra press
From the retained original matrix log (`/tmp/wiflow_matrix_before_pristine_round2.log`), the press whose `PttDown` never triggers a new `state Idle -> Starting (PttDown)` is this `PttDown` (session=3):

- `2026-10-04T04:55:04.778019Z  INFO wiflow_dictation::platform::macos::tap: [session=3] tap flagsChanged keycode=63 flags=0x0000000020800000 -> "PttDown"`

Immediately after, the next `PttUp` (still session=3) drives `state Listening -> Processing (PttUp)` and the capture stop, with the *session-ended* line belonging to the prior worker session (session=2):

```
2026-10-04T04:55:05.776656Z  INFO wiflow_dictation::platform::macos::tap: [session=3] tap flagsChanged keycode=63 flags=0x0000000020000000 -> "PttUp"
2026-10-04T04:55:05.776759Z  INFO wiflow_dictation::ptt: state Listening -> Processing (PttUp)
2026-10-04T04:55:05.777847Z  INFO wiflow_dictation::daemon: [session=3] worker Control::Up
2026-10-04T04:55:05.777902Z  INFO wiflow_dictation::daemon: [session=3] capture stop requested (2063ms hold)
2026-10-04T04:55:05.777919Z  INFO wiflow_dictation::core::audio: audio stop requested — dropping input stream (mic teardown)
2026-10-04T04:55:05.789508Z  INFO wiflow_dictation::core::audio: audio stream dropped — mic released, flushed 86016 samples over 1969ms
2026-10-04T04:55:05.789701Z  INFO wiflow_dictation::daemon: captured 0 vad-ready samples (86016 raw @ 44100Hz)
2026-10-04T04:55:05.789727Z  INFO wiflow_dictation::daemon: no speech detected, nothing to transcribe
2026-10-04T04:55:05.789770Z  INFO wiflow_dictation::ptt: state Processing -> Restoring (empty transcript)
2026-10-04T04:55:05.789801Z  INFO wiflow_dictation::app::orchestrator: [session=2] session ended
2026-10-04T04:55:05.789818Z  INFO wiflow_dictation::ptt: state Restoring -> Idle (finalized)
```

This is consistent with the extra posted press being ignored at the `PttDown` acceptance gate because the app was not Idle yet; its `PttUp` was then consumed by the currently-running capture.

#### 2) Invocation → session accounting rationale (why session ids span 1–47)
For a single physical press in the same retained matrix log, the tap-path `PttDown` and the worker-path `Control::Down` can have different session ids. Example: first matrix press shows:

- `2026-10-04T04:55:03.516968Z  INFO wiflow_dictation::platform::macos::tap: [session=1] tap flagsChanged keycode=63 flags=0x0000000020800000 -> "PttDown"`
- `2026-10-04T04:55:03.517018Z  INFO wiflow_dictation::ptt: state Idle -> Starting (PttDown)`
- `2026-10-04T04:55:03.714849Z  INFO wiflow_dictation::daemon: [session=2] worker Control::Down`

So, during the 24 invocations, session ids can advance across both the tap callback path and the worker/hotkey-bridge path, producing a larger continuous `session=...` range than “1..24”.

#### 3) Single-cycle evidence from ONE fresh run (counter restarts)
Log excerpt from `/tmp/wiflow_pristine_round2.log` for the single `/tmp/poster_kb 1200` cycle (includes the full `session=1` bracket lines and adjacent phase transitions):

```
2026-10-04T05:02:36.081431Z  INFO wiflow_dictation::platform::macos::tap: [session=1] tap flagsChanged keycode=63 flags=0x0000000020a00100 -> "PttDown"
2026-10-04T05:02:36.081493Z  INFO wiflow_dictation::ptt: state Idle -> Starting (PttDown)
2026-10-04T05:02:36.453625Z  INFO wiflow_dictation::daemon: [session=2] worker Control::Down
2026-10-04T05:02:36.565574Z  INFO wiflow_dictation::core::audio: capture started @ 44100Hz
2026-10-04T05:02:36.565629Z  INFO wiflow_dictation::daemon: [session=2] capture started — mic open
2026-10-04T05:02:36.565663Z  INFO wiflow_dictation::ptt: state Starting -> Listening (CaptureStarted)
2026-10-04T05:02:37.284624Z  INFO wiflow_dictation::platform::macos::tap: [session=2] tap flagsChanged keycode=63 flags=0x0000000020200100 -> "PttUp"
2026-10-04T05:02:37.284716Z  INFO wiflow_dictation::ptt: state Listening -> Processing (PttUp)
2026-10-04T05:02:37.285984Z  INFO wiflow_dictation::daemon: [session=2] worker Control::Up
2026-10-04T05:02:37.286029Z  INFO wiflow_dictation::daemon: [session=2] capture stop requested (833ms hold)
2026-10-04T05:02:37.286044Z  INFO wiflow_dictation::core::audio: audio stop requested — dropping input stream (mic teardown)
2026-10-04T05:02:37.294984Z  INFO wiflow_dictation::core::audio: audio stream dropped — mic released, flushed 31744 samples over 729ms
2026-10-04T05:02:37.296226Z  INFO wiflow_dictation::daemon: captured 0 vad-ready samples (31744 raw @ 44100Hz)
2026-10-04T05:02:37.296289Z  INFO wiflow_dictation::daemon: no speech detected, nothing to transcribe
2026-10-04T05:02:37.296320Z  INFO wiflow_dictation::ptt: state Processing -> Restoring (empty transcript)
2026-10-04T05:02:37.296348Z  INFO wiflow_dictation::app::orchestrator: [session=2] session ended
2026-10-04T05:02:37.296363Z  INFO wiflow_dictation::ptt: state Restoring -> Idle (finalized)
```

**Note:** this pristine run took the `empty transcript` path (so `INJECTING` did not appear), but the microphone was released and the machine returned to `Idle` (`Restoring -> Idle (finalized)`).

Tray: the log shows `tray built (idle, ...)` at startup and ends in `Restoring -> Idle (finalized)` for this single-cycle run.

### Pristine dual-path admission proof attempt (Round 3 / controller hypothesis)
**Goal:** demonstrate (from logs) that the *second* Down delivered while non-Idle is admitted as a duplicate *ignored* by the phase gate.

#### What the code logs (important constraint)
- The app’s log sink caps tracing at **INFO**: `logfile.rs` sets `.with_max_level(tracing::Level::INFO)`. 
- The “PttDown ignored” path is emitted as **debug**: `daemon.rs` has `e => tracing::debug!("ptt down ignored: {e:?}")`.
=> Therefore, **no `PttDown IGNORED` lines can appear in `~/Library/Logs/wiflow/wiflow.log` under the current logging configuration**.

#### Evidence from the pristine single-cycle log (Round 2)
The pristine log excerpt shows accepted Down and the subsequent worker/handoff, but contains **no ignored-duplicate line between them**:
```
2026-10-04T05:02:36.081431Z ... [session=1] ... -> "PttDown"
2026-10-04T05:02:36.081493Z ... state Idle -> Starting (PttDown)
2026-10-04T05:02:36.453625Z ... [session=2] worker Control::Down
...
2026-10-04T05:02:37.296320Z ... state Processing -> Restoring (empty transcript)
2026-10-04T05:02:37.296348Z ... [session=2] session ended
```
Because the IGNORED message is debug-only, we cannot prove the duplicate-admission suppression via quoted `PttDown IGNORED (phase ...)` log lines from this environment.

### Pristine rapid-matrix suppression proof (Task 9 / S2 live regression gate, Round 5)

#### Evidence: duplicate-Down is suppressed (one early press arrives while prior cycle still active)
Counts from `/tmp/wiflow_matrix_round5_full.log`:
- Tap `PttDown` events posted: **24**
- `capture started — mic open`: **23** (accepted presses)
- `mic released`: **23**
- `PttDown produced no actions ... — ignored`: **1**

#### Quoted end-to-end log lines (first accept cycle)
```
2026-10-04T05:15:12.532531Z  INFO wiflow_dictation::ptt: state Idle -> Starting (PttDown)
2026-10-04T05:15:13.400868Z  INFO wiflow_dictation::platform::macos::tap: [session=3] tap flagsChanged keycode=63 flags=0x0000000020800100 -> "PttDown"
2026-10-04T05:15:13.400959Z  INFO wiflow_dictation::app: [session=3] PttDown produced no actions (phase Listening) — ignored
2026-10-04T05:15:14.397152Z  INFO wiflow_dictation::ptt: state Listening -> Processing (PttUp)
2026-10-04T05:15:14.398296Z  INFO wiflow_dictation::daemon: [session=3] capture stop requested (1678ms hold)
2026-10-04T05:15:14.406034Z  INFO wiflow_dictation::ptt: state Restoring -> Idle (finalized)
2026-10-04T05:15:14.406034Z  INFO wiflow_dictation::app::orchestrator: [session=2] session ended
2026-10-04T05:15:14.405545Z  INFO wiflow_dictation::core::audio: audio stream dropped — mic released, flushed 69120 samples over 1577ms
```

#### Mechanism summary (controller hypothesis, with cosmetic session-id note)
- With Fn preset, each physical press is delivered **once via the tap path** (hotkey bridge does not see bare Fn).
- The app’s phase gate admits only the first Down while Idle; a second Down arriving while already **Listening** is ignored, producing the single `PttDown produced no actions ... — ignored` line above.
- Session ids appear to advance **2× per press** in log lines (tap odd ids vs worker even ids); this is cosmetic for accounting—the accepted press count matches `capture started — mic open` and `mic released`.
