# Next phases plan (2026-10-30)

Process: each phase ends with a check list. A phase is done only when every
check passes; if one fails, fix and re-run all checks of that phase before
moving on. Every phase also runs `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings` and `cargo test`.

## Phase 1 — CI on GitHub

Add `.github/workflows/ci.yml`: on push and pull request, a macOS runner runs
fmt, clippy and tests.

Checks:
- [x] Workflow file is valid and runs on the PR
- [x] fmt, clippy and test steps pass on the runner (Metal build included, 4m25s)
- [x] Tests that need models or network are skipped (`#[ignore]`) on CI

## Phase 2 — Load the Ollama model when it is switched

Today only startup loads the model. After a switch in Settings the first
cleanup is cold (~3.4 s), times out, and falls back to the uncleaned text.

Checks:
- [x] Saving a new Ollama model (or switching provider to Ollama) calls `warm_ollama` in the background, and unloads the previous model
- [x] Unit test covers the "should warm" decision
- [x] Live through the tray menu (fresh app start, 1b selected): picking 3b freed 1b and loaded 3b in 3 s; the next cleanup call took 242 ms (model load 1 ms), under the 1.5 s timeout
- [x] Fixed in re-check: the first switch after launch did not free the startup model (the app did not record which model it loaded at startup)

## Phase 3 — Per-stage timing line

One log line per dictation: capture, VAD, STT, cleanup, paste and total ms.

Checks:
- [x] Line appears once per dictation with all stages: 7 of 7 real dictations since the change, plus a live run through AI cleanup (BlackHole input, pasted into TextEdit): `stt=259ms context=61ms cleanup=1218ms worker_total=1541ms`, paste 223 ms
- [x] Skipped stages (no cleanup, no speech) still log a line: seen live on an empty 1.2 s hold
- [x] No transcript text in the line (privacy)

## Phase 4 — Accuracy: long dictations and "no wait"

4a. Long dictations lose words. History shows 13–14 s holds that came out
as 2–4 words ("Question 1.", "First complete all 5."). The audio reached STT
in full (log: 13.6 s of 14 s kept), so Whisper stopped early. Synthetic
long clips with pauses do not reproduce it, so real failing clips are needed:
opt-in saving to `~/Library/Logs/wiflow/clips/` (exists only while the folder
exists).

4b. "Postgres no wait MySQL" is still wrong with llama3.2:3b.

Checks:
- [ ] At least one real failing clip reproduces the drop in a test
- [ ] Fix makes those clips transcribe fully; short-clip speed stays close to today
- [x] "A no wait B" gives the right text: three new prompt examples; llama3.2:3b now gets 7 of 7 correction sentences right (was 4 of 7) and 2 of 3 unseen ones ("docs folder I mean the src folder" still wrong; the guard rejects it). Guard tests added for the 3b hallucinations seen
- [x] Debug clip saving stays opt-in, documented

## Phase 5 — Bluetooth mic cold start

Keeping the mic open between holds conflicts with rules.md §5. Needs a
decision from the owner; options and trade-offs written up here.

Checks:
- [x] Decision recorded in memory.md (option B)
- [x] If a code change is chosen, it keeps the recording indicator rule: the pill stays visible, only its dot and hint change; unit test added. Built-in mic quiet room measured at RMS 0.004, so it never sticks on "connecting". Not tried with a Bluetooth mic

## Progress

- Phase 1 done (CI green on PR 3).
- Phase 2 done, verified live through the tray menu.
- Phase 3 done. What the timing line found on its first live run:
  1. The first cleanup after ~1 h idle took 1.21 s inside Ollama (its own
     log), near the 1.5 s timeout, though the model stays loaded. Back-to-back
     calls take 210–300 ms. Fixed: on key press, if the app has not used
     Ollama for over 5 min, it sends a one-token warm-up request in the
     background, which finishes while the user speaks.
     Live: right after startup, no wake-up (as intended). After 6 min idle,
     the key press woke Ollama in 1.1 s, still inside the 1.6 s hold, so
     the cleanup after it would hit a warm model. Ollama goes slow within
     about 6 minutes, so the 5-minute threshold may be a little long.
  2. Whisper wrote "Thursday. No, actually Wednesday" and the router called
     it already clean, so the correction was pasted as spoken. Punctuation
     between the marker words breaks detection. Goes into Phase 4.
  3. Paste always takes ~200 ms: a deliberate wait after Cmd+V, before the
     old clipboard is put back. The text is already in place; left as is.
- Phase 4b done. 4a waiting for real failing clips. The STT log line now
  shows word count and audio seconds, so drops are visible.
- Phase 5 done with option B (owner's choice). Options were:
  A. Leave as is (current mic is the built-in one; no Bluetooth in recent logs).
  B. Show "connecting" in the pill until the first non-silent audio arrives,
     so the user knows when to start speaking. Fits rules.md §5.
  C. Keep the mic open a few seconds after release, indicator visible, so a
     quick second hold starts warm. Conflicts with the spirit of §5.
