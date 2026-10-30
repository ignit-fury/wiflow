# Next phases plan (2026-10-30)

Process: each phase ends with a check list. A phase is done only when every
check passes; if one fails, fix and re-run all checks of that phase before
moving on. Every phase also runs `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings` and `cargo test`.

## Phase 1 — CI on GitHub

Add `.github/workflows/ci.yml`: on push and pull request, a macOS runner runs
fmt, clippy and tests.

Checks:
- [ ] Workflow file is valid and runs on the branch push
- [ ] fmt, clippy and test steps pass on the runner (Metal build included)
- [ ] Tests that need models or network are skipped (`#[ignore]`) on CI

## Phase 2 — Load the Ollama model when it is switched

Today only startup loads the model. After a switch in Settings the first
cleanup is cold (~3.4 s), times out, and falls back to the uncleaned text.

Checks:
- [ ] Saving a new Ollama model (or switching provider to Ollama) calls `warm_ollama` in the background
- [ ] Unit test covers the "should warm" decision
- [ ] Live: switch model, dictate at once, log shows no timeout

## Phase 3 — Per-stage timing line

One log line per dictation: capture, VAD, STT, cleanup, paste and total ms.

Checks:
- [ ] Line appears once per dictation with all stages
- [ ] Skipped stages (no cleanup, no speech) still log a line
- [ ] No transcript text in the line (privacy)

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
- [ ] "A no wait B" at sentence end gives the right text, with a unit test; the guard still rejects bad output
- [ ] Debug clip saving stays opt-in, documented

## Phase 5 — Bluetooth mic cold start

Keeping the mic open between holds conflicts with rules.md §5. Needs a
decision from the owner; options and trade-offs written up here.

Checks:
- [ ] Decision recorded in memory.md
- [ ] If a code change is chosen, it keeps the recording indicator rule

## Progress

(filled in as phases complete)
