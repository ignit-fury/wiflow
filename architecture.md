# Architecture — wiflow Dictation (reconciled 2026-10-04, program S0–S5)

Supersedes the Phase-1 sketch below where they disagree (rules.md: code
that contradicts re-locked decisions gets fixed, not the docs — the
re-lock happened in `docs/superpowers/specs/2026-10-03-architecture-alignment-design.md`).

## 1. Pipeline (current)

```
[tap CGEventTap | global-hotkey] → DaemonEvent → AppOrchestrator (PttMachine gate)
  → worker Control → [cpal capture, native rate] → ringbuf → VAD (16 kHz stage)
  → STT [local whisper-rs Metal (CPU fallback) | Groq whisper-large-v3-turbo]
  → cleanup [deterministic | AI chain] → history → main-thread inject
  → finalize (media restore, UI reset, logging) → Idle / Error(held)
```

Push-to-talk only. No always-listening. Mic hot in LISTENING only.

## 2. Module layout (on disk)

```
src/
  main.rs            # bootstrap only (args, logging, dispatch) — H26
  app/               # AppOrchestrator: THE session lifecycle owner (H1)
    mod.rs           # winit shell: tray, menu, windows, intent executor
    orchestrator.rs  # routing + finalize_session (pure, headless-tested)
    session.rs       # immutable STARTING snapshot (H5/H6/H24)
    headless.rs      # --simulate-hold-ms harness
  ptt.rs             # transition VALIDATOR only (pure, no I/O) (H1)
  daemon.rs          # worker/execution engine: reports events, no decisions (H1)
  logfile.rs
  core/              # OS-agnostic logic; NEVER imports ui/ or platform impls (H25)
    audio, vad, stt, groq_stt, analyze, cleanup, history, config,
    hotkey (key-state only), traits.rs (trait + data-type definitions only)
  platform/macos/    # macOS implementations of core traits (H4)
    tap, inject (ClipboardInjector), context (OsascriptContext),
    duck + media (CoreAudioDuck/MediaController), panel (NSPanel), permissions
  ui/                # observes App state, emits intent commands (H20)
    settings (winit/egui window), pill (non-activating panel), notify, gl (shared host)
```

Dependency direction (H25): `UI → App → Core → Traits`; `Platform implements Core traits`. Composition (which impl) happens in App/main only.

## 3. State machine (7 phases + ERROR hold)

`IDLE → STARTING --CaptureStarted--> LISTENING --PttUp--> PROCESSING
--Done--> INJECTING --ok--> RESTORING(finalize) --> IDLE`
Failure/cancel/watchdog/supervision paths all funnel through RESTORING
(`finalize_session`: stop capture → restore media → hide UI → release audio
→ log → release resources). `ERROR` is HELD until the next press
(Down admitted from IDLE or ERROR). Up during STARTING is remembered and
applied at CaptureStarted (R18 — no orphaned captures on short holds).
Supervision deadline backstops a dead worker. 60 s watchdog preserved.

## 4. Media lifecycle (universal restore)

`MediaController { duck/restore/set_enabled }` over an epoch-guarded
idempotent CoreAudio machine (600 ms pause gate inside `duck()`, H11).
Worker ducks at capture-Ok on shared state (event-loss backstop);
orchestrator records state at LISTENING and restores EXACTLY ONCE inside
`finalize_session` on every terminal path (normal, empty, cancel, STT/
cleanup/inject failure, watchdog, capture failure, worker death — §8.3).
Restore is idempotent, never touches unmodified state, survives pause
failure and device changes. Delayed pause invalidated by epoch on cancel.

## 5. UI layer (pure Rust, one winit loop)

- Tray menu (all controls incl. Settings… item).
- Settings window (winit + egui + glutin/eglow): hotkey recorder
  (presets only; singles rejected; bare mods via tap), mic/model pickers,
  provider + media + login settings, history browser (copy/clear-deletes),
  permissions panel + 1 s mic probe. Writes go through App handlers;
  sessions snapshot at STARTING (next hold applies, H24).
- Recording pill: non-activating floating NSPanel (`orderFront:`, never
  `makeKeyAndOrderFront:` — winit forces key status), click-through, live
  RMS bars (amplitude-only atomic feed), elapsed timer. Observational only
  (H21); shown on LISTENING, hidden on every exit + finalize (H22).
- Notifications: Notification Center via osascript, fire-and-forget with
  tray-note fallback (never fails a session, H23).
- Tray Transcribing spinner (250 ms cadence while worker runs).

## 6. Providers + fallback (H17/H18)

STT: local Metal (CPU retry on GPU-init failure) → Groq turbo fallback
branch preserved; Groq failure alerts + falls back, never loses text.
Cleanup: AI failure → deterministic → raw transcript (failures downgrade,
never discard). Context: unavailable → proceed without. Media: unavailable
→ proceed unducked. Config stays JSON (diagram allows TOML/JSON).

## 7. Testing

`cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` ·
`cargo test` (250 green) before every commit. Headless suites: 8-phase
machine (admission + recovery + R18), orchestrator (routing, finalize from
every phase, idempotency, supervision, shutdown, universal-restore rows
with fake media backend), media lifecycle (11 scenarios: pause/race/
device/epoch/failure), session isolation, traits, UI view-models.
Live per phase: poster_kb cycles (incl. 300 ms short-hold R18 proof),
Music-less volume duck/restore exactness (0.6→0.12→0.6, same device),
focus-steal checks (Terminal keeps focus), Esc/watchdog cancels, shutdown.

## 8. Intentional deviations (H31 — deliberate, not unfinished)

1. **History search**: not built — the diagram marks it future. Browser
   (last 50, copy, clear-deletes) ships instead.
2. **Config stays JSON** (D5): diagram says "TOML/JSON" — JSON conforms.
3. **16 kHz conversion at the VAD stage** (not capture): native-rate capture
   + VAD resample; acoustically identical, avoids resampling twice.
4. **`small.en` not default**: default stays base.en (model picker exposes
   tiny/base/small; user chose no default swap).
5. **No system-wide hotkey conflict enumeration**: doesn't exist on macOS —
   registration failure surfaces with guidance instead (nothing faked).
6. **Machine transition logs lack session ids** (cosmetic): session-end
   lines + session-tagged event lines carry traceability; pure-machine
   tagging deferred (no behavior impact).
7. **Session ids increment 2×/press** (tap log pre-increment + session
   mint): cosmetic log wart; sessions are 1:1 with accepted presses.
8. **Stereo input captured interleaved-as-mono**: pipeline assumes mono
   mics (MacBook mic is mono); stereo sources (e.g. BlackHole 2ch loopback)
   defeat VAD. Known audio-layer limitation, predates S0–S5, untouched.

## 9. Alignment status (H31 — what matches the diagram now)

Structure ✅ · traits (5/5 bound, fakes) ✅ · 7-state machine + ERROR hold +
supervision ✅ · universal media restore ✅ · UI (settings/pill/notify/
spinner) ✅ · provider deltas (turbo id pinned; CPU fallback) with 2 user
acceptances pending (one Groq cycle with key; Music pause with library) ✅-conditional ·
error cleanup (finalize funnel, ERROR ordering) ✅ · tests (250 green) ✅ ·
live cycles (full incl. non-empty STT→inject; short-hold R18 proof) ✅ ·
docs (this file) ✅.

## 10. Carried-forward operational notes (still true)

- Distribution needs signing + notarization + hardened runtime (global
  hotkey + injection fail outside dev without them).
- `Info.plist`: `NSMicrophoneUsageDescription`, `LSUIElement=true`
  (menu-bar, no dock).
- Permissions: mic / Accessibility / Input Monitoring (tap) checks with
  System Settings deep-links (`config::permissions`); denial paths tested.
- Launch at login: implemented via config + system login-item path
  (`set_launch_at_login`), toggle in tray menu and settings window.
- Manual matrix (run per audio/inject change): 2 apps + permission-denied
  path (rules.md); password-field behavior unchanged (inject via clipboard
  + Cmd+V, same as before).
