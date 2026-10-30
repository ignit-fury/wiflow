# Speed and accuracy fix plan (2026-10-30)

Findings come from a code review plus `~/Library/Logs/wiflow/wiflow.log`
(205 dictations, `small.en`, built-in mic at 44.1 kHz, Ollama cleanup).
Baseline before changes: 269 tests pass; transcription costs 470–770 ms
regardless of clip length, so most of it is fixed overhead.

## Phase 1 — Fixed latency

| # | Problem | Fix |
|---|---------|-----|
| 1.1 | SHA-256 of the whole model on every dictation (285 ms on `small.en`) | Verify once per process; later calls only check the file size |
| 1.2 | Model path resolved (and hashed) even when Groq does the STT | Resolve the local model only for local STT or the Groq fallback |
| 1.3 | First dictation pays model load + Metal setup | Warm the Whisper model in a background thread at startup |
| 1.4 | New HTTP client (new TLS handshake) for every request | One shared `ureq::Agent`, per-request timeouts |

## Phase 2 — Cleanup and context

| # | Problem | Fix |
|---|---------|-----|
| 2.1 | Ollama cleanup times out at 800 ms because the model is cold (3.4 s cold vs 0.12 s warm, measured) | Load the Ollama model at startup and again in the background after a timeout |
| 2.2 | Ollama retries with `qwen/qwen3.8-27b`, which is not installed, and warns every time | Retry on Ollama only when the fallback model is installed |
| 2.3 | `num_predict: 100` can cut long dictations short | Scale the token cap with transcript length |
| 2.4 | Context lookup: `osascript` (~200 ms) on the critical path, uncached Groq call with a 10 s timeout outside the cleanup budget | Read the focused app in parallel with STT; cache context per app; 1.5 s timeout |
| 2.5 | `gpt-oss-20b` cleanup reasons at default (medium) effort | Send `reasoning_effort: "low"` for gpt-oss models |

## Phase 3 — Audio input

| # | Problem | Fix |
|---|---------|-----|
| 3.1 | Stereo/multi-channel input is stored interleaved as if mono (half-speed audio) | Downmix frames to mono in the capture callback; prefer mono configs |
| 3.2 | 44.1/48 kHz to 16 kHz with no low-pass filter (aliasing) | Windowed-sinc low-pass before interpolation |
| 3.3 | Speech gate averages loudness over the whole clip; quiet speech with pauses is dropped | Pass if any 30 ms window is loud enough |

## Phase 4 — Whisper decoding

| # | Problem | Fix |
|---|---------|-----|
| 4.1 | Flash attention off | Enable `flash_attn` on model load |
| 4.2 | Greedy decoding (`best_of: 1`) | Beam search, beam size 5 |

## Phase 5 — Found in live testing

| # | Problem | Fix |
|---|---------|-----|
| 5.1 | Each key press ran `osascript` on the main thread (waits up to 500 ms) before the mic opened; median 184 ms, p90 460 ms over 238 presses. Clipped first words. Nothing read the result. | Removed the lookup from `begin_session`; the cleanup step already reads the app itself |
| 5.2 | `supervision_deadline_fires_without_worker_events` raced a session test over a shared env var | Set the watchdog on the session directly |

## Deferred — needs a decision

- **Bluetooth mic cold start clips first words.** The only fix is keeping the
  mic open between holds, which conflicts with rules.md §5 (no recording
  without a visible indicator) and the mic-teardown lifecycle. Not changed.

## Results

All phases implemented 2026-10-30. 272 tests pass (3 tests for the removed lookup deleted).

Whisper `small.en` on Metal, release build, warm runs (macOS `say` fixtures):

| Setting | 8 s clip | 60 s clip | Output |
|---------|----------|-----------|--------|
| Before (greedy) | ~465 ms | ~710 ms | "ping priya" |
| + flash attention | ~405 ms | ~620 ms | same |
| + beam 5 (shipped) | ~465 ms | ~670 ms | "ping Priya" |

On top of that, each dictation no longer hashes the model (~285 ms), and the
focused-app lookup (~200 ms) now runs during STT instead of after it.

Possible next step (not in this plan): set Whisper `audio_ctx` from clip
length. The encoder always processes a 30 s window, which is most of the
fixed cost on short clips, but shrinking it can cost accuracy.

Live app, 6–10 s dictations: 375–387 ms to transcribe (before: 570–700 ms).
Key press to recorder start: 21 ms (before: median 184 ms).
Ollama cleanup: answers in ~200 ms with no timeouts (before: every call timed
out). Fillers and repeated words are fixed locally without the model. For
self-corrections ("three no actually four"), `llama3.2:1b` still drops words;
the safety check rejects that output and keeps the local transcript.

Switched cleanup to `llama3.2:3b` (same 4 self-correction sentences): 3B got 3
right, 1B got 1. Warm 3B takes ~230 ms. Ollama's 64K default context reserved
9.9 GB for 3B; capped at 4,096 tokens, it uses 2.6 GB. The startup warm-up
now also processes the system prompt, so the first cleanup takes 0.47 s instead of 1.86 s.

Self-correction fixes (Phase 6):
- Detect "no wait", "wait no", "or rather", "actually make it/that" (whole
  words, so "no waiting list" is not flagged) in `analyze.rs`.
- Safety check (`candidate_preserves_input`): spelled numbers after a
  correction must survive, "no wait" is not a negation, and "60,000" reads as
  one number.
- Cleanup instructions list the new markers and show two more examples.

3B on six self-correction sentences: 1 right before the prompt change (the
check now rejected the wrong ones instead of pasting them), 5 right after.
The remaining miss ("Postgres no wait MySQL") is rejected and pasted as
dictated. About 400 ms per cleanup with the longer instructions.

## Validation

Each phase: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`. New logic gets a unit test. Live check after all phases:
compare `transcribed in` times in the log against the baseline above.
