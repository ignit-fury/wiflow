# Tuning fixes and live text in the pill — design

Date: 2026-11-15
Status: approved in conversation, awaiting written-spec review

## Goal

Part 1 fixes four measured weak spots, landed as one commit. Part 2 then adds
live text in the recording pill: the words heard so far, shown while the key
is held. Nothing that works today may change behaviour except where stated.

## Part 1: four fixes (one commit)

### 1. Long dictations lose AI cleanup

Evidence: the cleanup chain has a 2 s budget and Ollama a 1.5 s request
timeout; a 57-word dictation timed out and pasted the raw text.

Change: both limits scale with the dictation length:
`limit = base + 0.4 s per 10 words` (integer tens, so under 10 words is
unchanged), capped at 6 s. `CleanupBudget::new(words)` computes both from the
word count of the transcript passed to `clean_chain`. Voice-edit's 8 s budget
(`for_edit`) is unchanged.

| Words | Budget (2 s base) | Ollama timeout (1.5 s base) |
|---|---|---|
| 0–9 | 2.0 s | 1.5 s |
| 50 | 4.0 s | 3.5 s |
| 100 | 6.0 s (cap) | 5.5 s |
| 150+ | 6.0 s (cap) | 6.0 s (cap) |

### 2. Rare slow transcriptions

Evidence: 16 of about 550 dictations took over 1 s, up to 4.2 s. These clips
contained real words (for example 1 word from 4.0 s of audio in 3.9 s). Pure
silence is already skipped by the existing energy check, so the original idea
(skip near-silent clips) would drop real words. The slowness is Whisper's
temperature fallback: when unsure it re-decodes at temperature 0.2, 0.4 … 1.0,
up to 6 passes.

Change: temperature step 0.2 → 0.4 (passes at 0, 0.4, 0.8: at most 3),
through a `Decode` value (`Decode::FULL`) passed to a new
`Stt::transcribe_with`; `Stt::transcribe` keeps its signature. Gate: an
ignored benchmark test transcribes clean and noisy synthesized clips at both
steps and requires the same words. If any clip differs, the step stays at 0.2
and only the refactor lands.

### 3. Pointless Ollama retry

Evidence: after an Ollama failure, the chain retries the same provider with
`cleanup_fallback_model`, whose default (`qwen/qwen3.8-27b`) is a cloud model
id never installed in Ollama ("model not installed" 11 times in the log).

Change: a pure `retry_model(provider, primary, fallback)` returns the model to
retry with: `None` for Ollama, and for Groq/OpenRouter the fallback when it
differs from the primary (today's behaviour). The now-dead special case that
masked the Ollama "model not installed" error is removed.

### 4. Permissions reset on every rebuild

Evidence: `build-app.sh` signs ad-hoc (`--sign -`); each build has a new code
hash, so macOS treats it as a new app and drops Microphone, Input Monitoring
and Accessibility grants.

Change: a one-time `packaging/make-signing-cert.sh` creates a self-signed
"Wiflow Dev" code-signing certificate in the login keychain (using
`/usr/bin/openssl`, which macOS `security import` accepts), and trusts it for
code signing; macOS asks for the user's password once. `build-app.sh` signs
with "Wiflow Dev" when that identity exists, otherwise ad-hoc as today with a
note. With a stable certificate the app's designated requirement stays the
same across builds, so grants persist after one final re-grant. The launch
agent should be re-toggled from the app so it points at the signed app.

## Part 2: live text in the pill

### Behaviour

While the key is held, about once a second the pill shows the most recent
words heard, on a new line under the waveform (older words drop off the left,
"…" in front). On release, the final text comes from today's full pass over
all the audio; the preview never changes what is pasted. A Settings toggle
"Show words in the pill while dictating" (on by default) turns it off.
Preview is local-only: off when the speech provider is Groq.

### Components

| Piece | Change |
|---|---|
| `core/audio.rs` | `AudioCapture::snapshot()` moves new samples from the realtime ring into an internal buffer and returns everything so far; `stop()` returns that buffer plus the rest. `sample_rate()` getter. |
| `core/stt.rs` | `Decode::PREVIEW` (greedy, one pass, no retries) and `transcribe_preview(model_path, samples, language)` on the shared model, with hallucination tokens stripped. |
| `core/config.rs`, `ui/settings.rs` | `live_preview: bool` (default true) and its checkbox. |
| `daemon.rs` | The worker, while the mic is open, wakes at the earlier of the watchdog deadline and the next preview time (every 1 s). A preview pass takes the last 15 s or less, skips silence, transcribes with `Decode::PREVIEW`, and sends `DaemonEvent::LivePreview(text)`. |
| `app/mod.rs`, `app/orchestrator.rs` | `LivePreview` goes straight to the pill (no orchestrator action, no state change). |
| `ui/pill.rs` | Live text state (cleared on every show), pure `live_tail(text, max_chars)`, a third row; pill height 84 → 108. |

### Trade-offs

- The preview runs on the worker thread, so a release that arrives during a
  preview pass waits for it: about 0.1–0.3 s with a 15 s tail. A parallel
  second Whisper instance was rejected (about 200 MB more memory, GPU
  contention).
- Battery: one short GPU pass per second while the key is held; the toggle
  covers users who mind.

## Not breaking anything

- Every change is test-first where the logic is pure; hardware paths get
  ignored tests run on the real Mac.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and the
  full `cargo test` pass before each commit; the stuck-mic test
  (`cargo test named_mic -- --ignored`) is re-run.
- Dictation without the preview, voice-edit, mic release, the 60 s watchdog
  and the password guard keep today's behaviour.
- Live checks that take keyboard focus run only when the user says the Mac
  is free.
- Work happens on branch `tune-and-live-pill`, stacked on `voice-edit`.

## Out of scope

- Typing text into the target app while speaking (rejected: flicker,
  fragmented undo, breaks cleanup and voice-edit).
- Preview for Groq speech-to-text.
- Changing the final transcription path's beam search.
