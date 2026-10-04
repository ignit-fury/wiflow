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
