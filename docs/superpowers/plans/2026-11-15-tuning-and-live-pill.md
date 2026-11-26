# Tuning fixes and live text in the pill Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix four measured weak spots (landed as one commit), then show the words heard so far in the recording pill while the key is held.

**Architecture:** Part 1 changes limits in place: cleanup time scales with word count (`CleanupBudget::new(words)`), Whisper decodes through a `Decode` value with capped retries, a pure `retry_model` stops pointless Ollama retries, and packaging signs with a stable self-signed certificate. Part 2 lets the worker, while the mic is open, wake once a second, transcribe the latest audio with a fast `Decode::PREVIEW` pass on the shared model, and send `DaemonEvent::LivePreview(text)`, which the app hands straight to the pill. The final transcript path is unchanged.

**Tech Stack:** Rust 2021 (MSRV 1.81), existing crates only (`whisper-rs`, `ringbuf`, `cpal`, `egui`, `hound` for tests). macOS `security`, `/usr/bin/openssl`, `codesign` for packaging.

**Spec:** `docs/superpowers/specs/2026-11-15-tuning-and-live-pill-design.md`

## Global Constraints

- Gates before every commit: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (baseline 301 passed, 6 ignored; never regresses).
- Nothing that works today may change behaviour except where the spec says. Plain dictation, voice-edit (PTT + Shift), mic release, the 60 s watchdog and the password-field guard must keep working.
- Cleanup limits: `limit = base + 0.4 s per 10 words` (integer tens), capped at 6 s; budget base 2 s, Ollama timeout base 1.5 s. Voice-edit keeps its 8 s budget.
- Whisper final pass: beam search 5 (unchanged), temperature step 0.4 (at most 3 passes), kept only if the benchmark shows the same words on every clip; otherwise 0.2.
- Ollama never retries with `cleanup_fallback_model`; Groq and OpenRouter keep today's retry.
- Signing identity name: exactly `Wiflow Dev`. `build-app.sh` falls back to ad-hoc signing when it is absent.
- Live preview: every 1 s, last 15 s of audio at most, greedy single pass, no retries, local model only (off when `stt_provider == "groq"`), toggle `live_preview` default `true`. It never changes the pasted text.
- Pill: height 84 → 108, one line of live text under the waveform, `LIVE_CHARS = 44`.
- `core/` imports nothing from `ui/` or `platform/`. No audio-sample logging.
- Never make real network/LLM calls inside `cargo test`. Hardware tests are `#[ignore]`.
- Do not run the app, post synthetic key events, run `packaging/build-app.sh` or `packaging/make-signing-cert.sh` unless the task says the controller does it with the user's OK (the user works on this Mac; build-app replaces the running app).
- Part 1 = exactly one commit on the branch in the end (Tasks 1–4 make `wip:` commits; Task 5 squashes them). Part 2 = its own commit(s) after that.
- Every commit ends with a `Co-Authored-By` trailer naming the model that wrote it.
- Branch: `tune-and-live-pill` (stacked on `voice-edit`). Leave the untracked `graphify-out/` alone.

## File Structure

| File | Responsibility |
|---|---|
| `src/core/cleanup.rs` | Task 1: `scaled_for`, `CleanupBudget::new(words)`. Task 3: `retry_model`. |
| `src/core/config.rs` | Task 3: fallback doc comment. Task 6: `live_preview` field. |
| `src/core/stt.rs` | Task 2: `Decode`, `Stt::transcribe_with`, benchmark test. Task 6: `Decode::PREVIEW`, `transcribe_preview`. |
| `packaging/make-signing-cert.sh` (new), `packaging/build-app.sh`, `README.md` | Task 4: stable signing. |
| `src/core/audio.rs` | Task 6: `AudioCapture::snapshot`, `sample_rate`. |
| `src/ui/settings.rs` | Task 6: live-preview checkbox. |
| `src/ui/pill.rs` | Task 7: live text state, `live_tail`, third row, height. |
| `src/daemon.rs`, `src/app/orchestrator.rs`, `src/app/mod.rs` | Task 7: `DaemonEvent::LivePreview`. Task 8: worker preview tick. |
| `docs/corrections.md`, `memory.md` | Task 5 / Task 8 docs. |

---

## Part 1: four fixes (one commit)

### Task 1: Cleanup time scales with dictation length

**Files:**
- Modify: `src/core/cleanup.rs` (`CleanupBudget` near line 28; `clean_chain` near line 485; tests near lines 1160–1190)

**Interfaces:**
- Produces: `CleanupBudget::new(words: usize) -> CleanupBudget` (private to `cleanup.rs`); `fn scaled_for(base: Duration, words: usize) -> Duration`.

- [ ] **Step 1: Write the failing test**

In `src/core/cleanup.rs` `mod tests`, add:

```rust
    #[test]
    fn dictation_time_grows_with_length_and_caps() {
        assert_eq!(scaled_for(OLLAMA_TIMEOUT, 0), OLLAMA_TIMEOUT);
        assert_eq!(scaled_for(OLLAMA_TIMEOUT, 9), OLLAMA_TIMEOUT, "under 10 words: unchanged");
        assert_eq!(scaled_for(OLLAMA_TIMEOUT, 50), Duration::from_millis(3500));
        assert_eq!(scaled_for(CLEANUP_BUDGET, 50), Duration::from_millis(4000));
        assert_eq!(scaled_for(OLLAMA_TIMEOUT, 500), DICTATION_CAP);
        let long = CleanupBudget::new(150);
        assert_eq!(long.ollama_timeout, DICTATION_CAP);
        assert!(long.remaining().unwrap() > Duration::from_secs(5));
        assert_eq!(CleanupBudget::new(3).ollama_timeout, OLLAMA_TIMEOUT);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test dictation_time_grows 2>&1 | tail -8`
Expected: compile error "cannot find function `scaled_for`" / "cannot find value `DICTATION_CAP`".

- [ ] **Step 3: Implement**

Next to `const CLEANUP_BUDGET` add:

```rust
/// Extra cleanup time per 10 dictated words: long dictations take longer to
/// tidy, and a fixed 1.5 s timed out on 50+ words and pasted raw text.
const PER_10_WORDS: Duration = Duration::from_millis(400);
/// Most a dictation ever waits for cleanup.
const DICTATION_CAP: Duration = Duration::from_secs(6);

/// `base` plus 0.4 s per whole 10 words, capped at 6 s.
fn scaled_for(base: Duration, words: usize) -> Duration {
    (base + PER_10_WORDS * (words / 10) as u32).min(DICTATION_CAP)
}
```

Replace `fn new() -> Self` in `impl CleanupBudget` with:

```rust
    /// Dictation cleanup; limits grow with the transcript's word count.
    fn new(words: usize) -> Self {
        Self {
            deadline: std::time::Instant::now() + scaled_for(CLEANUP_BUDGET, words),
            ollama_timeout: scaled_for(OLLAMA_TIMEOUT, words),
            ollama_stop: DICTATION_STOP,
            ollama_token_cap: None,
        }
    }
```

In `clean_chain`, replace `let budget = CleanupBudget::new();` with:

```rust
    let budget = CleanupBudget::new(transcript.split_whitespace().count());
```

Update the existing tests that call `CleanupBudget::new()` (they are in `mod tests`; `grep -n "CleanupBudget::new()" src/core/cleanup.rs` finds them) to `CleanupBudget::new(0)`. Their assertions stay as they are.

- [ ] **Step 4: Run all gates**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)"; cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no clippy output; `302 passed; 0 failed; 6 ignored`.

- [ ] **Step 5: WIP commit**

```bash
git add src/core/cleanup.rs
git commit -m "wip: scale cleanup time with dictation length"
```

---

### Task 2: Cap Whisper retries (benchmark-gated)

**Files:**
- Modify: `src/core/stt.rs` (`Stt::transcribe` near line 100; tests module near line 615)

**Interfaces:**
- Produces: `pub(crate) struct Decode { beam: bool, temperature_inc: f32 }` with `Decode::FULL`; `Stt::transcribe_with(&mut self, samples_16k: &[f32], initial_prompt: &str, language: &str, decode: Decode) -> Result<String, String>`. `Stt::transcribe(&mut self, samples_16k, initial_prompt, language)` keeps its signature and calls `transcribe_with(…, Decode::FULL)`.

- [ ] **Step 1: Add `Decode` and `transcribe_with`**

Above `impl Stt` (or near the top of the file after the `use` lines) add:

```rust
/// How Whisper decodes a clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Decode {
    /// Beam search (5 beams) when true, one greedy pass when false.
    beam: bool,
    /// Temperature added on each retry when Whisper is unsure; 0 = no retries.
    temperature_inc: f32,
}

impl Decode {
    /// Final transcript: beam search, retries at 0, 0.4, 0.8 (at most 3
    /// passes) instead of whisper.cpp's 0.2 steps (up to 6 passes). Unsure
    /// clips took up to 4 s; `temperature_step_keeps_words` gates the change.
    pub(crate) const FULL: Decode = Decode {
        beam: true,
        temperature_inc: 0.4,
    };
}
```

Rename the body of `pub fn transcribe(&mut self, samples_16k: &[f32], initial_prompt: &str, language: &str)` into a new method and make `transcribe` delegate:

```rust
    pub fn transcribe(
        &mut self,
        samples_16k: &[f32],
        initial_prompt: &str,
        language: &str,
    ) -> Result<String, String> {
        self.transcribe_with(samples_16k, initial_prompt, language, Decode::FULL)
    }

    pub(crate) fn transcribe_with(
        &mut self,
        samples_16k: &[f32],
        initial_prompt: &str,
        language: &str,
        decode: Decode,
    ) -> Result<String, String> {
        if samples_16k.is_empty() {
            return Ok(String::new());
        }
        // Beam search (whisper.cpp CLI default) over greedy: fewer dropped or
        // misheard words; cost measured in docs/perf-accuracy-plan.md.
        let mut params = if decode.beam {
            FullParams::new(SamplingStrategy::BeamSearch {
                beam_size: 5,
                patience: -1.0,
            })
        } else {
            FullParams::new(SamplingStrategy::Greedy { best_of: 1 })
        };
        params.set_temperature_inc(decode.temperature_inc);
        // … every remaining line of the old `transcribe` body, unchanged,
        // from `params.set_n_threads(num_threads());` to
        // `Ok(text.trim().to_string())`.
    }
```

Keep all existing comments and calls of the old body (n_threads, print_progress, single_segment, no_timestamps, audio_ctx, initial_prompt, language, `full`, segment loop) exactly as they are; only the `FullParams::new(...)` line and the new `set_temperature_inc` line change. The doc comment that was on `transcribe` stays on `transcribe`.

- [ ] **Step 2: Write the benchmark test (ignored: needs the model and `say`)**

In `src/core/stt.rs` `mod tests`, add:

```rust
    /// Gate for `Decode::FULL`: the 0.4 step must give the same words as
    /// whisper.cpp's 0.2 on clean and noisy clips. Needs the small model and
    /// macOS `say`. Run: cargo test --release temperature_step -- --ignored --nocapture
    #[test]
    #[ignore]
    fn temperature_step_keeps_words() {
        let path = ensure_model_variant("small").expect("small model");
        let mut stt = Stt::load(&path).expect("load");
        let dir = std::env::temp_dir().join("wiflow_temperature_step");
        std::fs::create_dir_all(&dir).unwrap();
        let phrases = [
            "OK.",
            "Ship it.",
            "Book the flight for Monday, no wait, Tuesday morning.",
            "We should hire three designers this quarter. The budget covers two senior roles and one junior role, and I want the job posts live by Friday.",
        ];
        let words = |s: &str| {
            s.to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric() || c.is_whitespace())
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let old_step = Decode {
            beam: true,
            temperature_inc: 0.2,
        };
        let (mut slowest_old, mut slowest_new) = (0u128, 0u128);
        for (i, phrase) in phrases.iter().enumerate() {
            let wav = dir.join(format!("{i}.wav"));
            let ok = std::process::Command::new("say")
                .args(["-o", wav.to_str().unwrap(), "--data-format=LEI16@16000", phrase])
                .status()
                .unwrap()
                .success();
            assert!(ok, "say failed");
            let clean: Vec<f32> = hound::WavReader::open(&wav)
                .unwrap()
                .into_samples::<i16>()
                .map(|s| s.unwrap() as f32 / 32768.0)
                .collect();
            // A quiet, noisy copy pushes the decoder into retries: the case
            // this change targets.
            let noisy: Vec<f32> = clean
                .iter()
                .enumerate()
                .map(|(j, s)| s * 0.3 + 0.08 * (((j * 7919) % 1000) as f32 / 500.0 - 1.0))
                .collect();
            for (kind, audio) in [("clean", &clean), ("noisy", &noisy)] {
                let t = std::time::Instant::now();
                let old = stt.transcribe_with(audio, "", "", old_step).unwrap();
                let ms_old = t.elapsed().as_millis();
                let t = std::time::Instant::now();
                let new = stt.transcribe_with(audio, "", "", Decode::FULL).unwrap();
                let ms_new = t.elapsed().as_millis();
                eprintln!("STEP {kind} {i}: 0.2 {ms_old}ms {old:?} | FULL {ms_new}ms {new:?}");
                assert_eq!(words(&old), words(&new), "{kind} clip {i}: words changed");
                slowest_old = slowest_old.max(ms_old);
                slowest_new = slowest_new.max(ms_new);
            }
        }
        eprintln!("STEP slowest: 0.2 {slowest_old}ms, FULL {slowest_new}ms");
    }
```

- [ ] **Step 3: Run the gates and the benchmark**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)"; cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no clippy output; `302 passed; 0 failed; 7 ignored`.

Run: `cargo test --release temperature_step -- --ignored --nocapture 2>&1 | grep -E "STEP|test result|panicked"`
(This uses the GPU but no keyboard or mic; safe while the user works.)

- If it PASSES: keep `temperature_inc: 0.4`. Copy the `STEP` lines into the commit message body.
- If it FAILS (words changed on any clip): set `Decode::FULL.temperature_inc` to `0.2`, change its doc comment to say the 0.4 step changed words on the benchmark so whisper.cpp's default stays, re-run the gates, and say so in the commit message.

- [ ] **Step 4: WIP commit**

```bash
git add src/core/stt.rs
git commit -m "wip: decode via Decode::FULL with capped Whisper retries"
```

---

### Task 3: Ollama skips the fallback-model retry

**Files:**
- Modify: `src/core/cleanup.rs` (`try_provider_with_retry` near line 442), `src/core/config.rs` (doc comment on `cleanup_fallback_model`)

**Interfaces:**
- Produces: `fn retry_model<'a>(p: Provider, primary: &str, fallback: &'a str) -> Option<&'a str>` (private).

- [ ] **Step 1: Write the failing test**

In `src/core/cleanup.rs` `mod tests`, add:

```rust
    #[test]
    fn only_cloud_providers_retry_with_the_fallback_model() {
        assert_eq!(retry_model(Provider::Groq, "a", "b"), Some("b"));
        assert_eq!(retry_model(Provider::OpenRouter, "a", "b"), Some("b"));
        assert_eq!(retry_model(Provider::Groq, "a", "a"), None, "same model: no retry");
        assert_eq!(
            retry_model(Provider::Ollama, "llama3.2:3b", "qwen/qwen3.8-27b"),
            None,
            "the shared fallback is a cloud model id, never installed in Ollama"
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test only_cloud_providers 2>&1 | tail -6`
Expected: compile error "cannot find function `retry_model`".

- [ ] **Step 3: Implement**

Above `fn try_provider_with_retry` add:

```rust
/// Model to retry with after the primary fails, if any. Ollama never retries:
/// the shared fallback setting holds a cloud model id (default
/// `qwen/qwen3.8-27b`) that is not installed locally.
fn retry_model<'a>(p: Provider, primary: &str, fallback: &'a str) -> Option<&'a str> {
    (p != Provider::Ollama && fallback != primary).then_some(fallback)
}
```

Replace the whole `Err(e) => { … }` arm of `try_provider_with_retry` with:

```rust
        Err(e) => match retry_model(p, primary, &cfg.cleanup_fallback_model) {
            Some(fallback) => {
                tracing::warn!("retry with fallback model {fallback}: {e}");
                try_provider_model(p, fallback, transcript, None, budget)
                    .map(|t| sanitize_llm_response(&t, transcript))
            }
            None => Err(e),
        },
```

(The removed `map_err` that masked the Ollama "model not installed" fallback error is now unreachable: Ollama no longer retries.)

In `src/core/config.rs`, update the doc comment above `pub cleanup_fallback_model` to:

```rust
    /// Post-Processing Fallback: when the primary model fails on a cloud
    /// provider (Groq, OpenRouter), the SAME provider is retried with this
    /// model before moving down the chain. Ollama never retries with it.
```

Check no existing test relied on the old Ollama retry: `grep -n "fallback" src/core/cleanup.rs | grep -n test` and read any hit; none may call the network.

- [ ] **Step 4: Run all gates**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)"; cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no clippy output; `303 passed; 0 failed; 7 ignored`.

- [ ] **Step 5: WIP commit**

```bash
git add src/core/cleanup.rs src/core/config.rs
git commit -m "wip: no fallback-model retry for Ollama"
```

---

### Task 4: Stable signing certificate

**Files:**
- Create: `packaging/make-signing-cert.sh`
- Modify: `packaging/build-app.sh` (last `codesign` line), `README.md` (Package section)

**Interfaces:**
- Produces: keychain identity named exactly `Wiflow Dev`; `build-app.sh` uses it when present.

- [ ] **Step 1: Create `packaging/make-signing-cert.sh`**

```sh
#!/bin/sh
# One-time setup: a self-signed "Wiflow Dev" code-signing certificate in the
# login keychain. Signing every build with the same certificate keeps macOS
# permissions (Microphone, Input Monitoring, Accessibility) across rebuilds;
# an ad-hoc signature changes on every build, so macOS treats each build as
# a new app. macOS asks for your password once (to trust the certificate).
set -eu
NAME="Wiflow Dev"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
if security find-identity -v -p codesigning | grep -q "\"$NAME\""; then
  echo "$NAME already exists"
  exit 0
fi
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF
# macOS's own openssl (LibreSSL): its .p12 files import cleanly with
# `security`; Homebrew OpenSSL 3 defaults do not.
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -config "$TMP/cert.cnf" -keyout "$TMP/key.pem" -out "$TMP/cert.pem"
PASS="wiflow-$$"
/usr/bin/openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
  -name "$NAME" -passout "pass:$PASS" -out "$TMP/id.p12"
security import "$TMP/id.p12" -k "$KEYCHAIN" -P "$PASS" -T /usr/bin/codesign
security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$TMP/cert.pem"
security find-identity -v -p codesigning | grep "\"$NAME\""
echo "Created $NAME. Rebuild with: sh packaging/build-app.sh"
echo "If codesign asks to use the key, click Always Allow."
```

- [ ] **Step 2: Sign with it in `packaging/build-app.sh`**

Replace the line `codesign --force --deep --sign - "$APP"` with:

```sh
# A stable identity keeps macOS permissions across rebuilds
# (create it once with: sh packaging/make-signing-cert.sh).
if security find-identity -v -p codesigning | grep -q '"Wiflow Dev"'; then
  codesign --force --deep --sign "Wiflow Dev" "$APP"
else
  echo "note: no 'Wiflow Dev' certificate; signing ad-hoc (permissions reset on every rebuild)"
  codesign --force --deep --sign - "$APP"
fi
```

- [ ] **Step 3: README**

In `README.md` `## Package`, after the `make-dmg.sh` code block, add:

```markdown
To keep macOS permissions across rebuilds, run this once first (macOS asks
for your password to trust the certificate):

```sh
sh packaging/make-signing-cert.sh
```

`build-app.sh` then signs with this "Wiflow Dev" certificate instead of
ad-hoc. Grant the permissions one last time after the first signed build. If
launch at login was turned on from a terminal build, turn it off and on again
in Wiflow so the login item points at the signed app.
```

- [ ] **Step 4: Check the scripts without running them**

Run: `sh -n packaging/make-signing-cert.sh && sh -n packaging/build-app.sh && echo SYNTAX_OK`
Expected: `SYNTAX_OK`. Do NOT run either script (one needs the user's password; the other replaces the running app). The controller runs them in Task 5 with the user's OK.

Run the gates (no code changed, counts unchanged): `cargo test 2>&1 | grep -E "test result|FAILED"` → `303 passed; 0 failed; 7 ignored`.

- [ ] **Step 5: WIP commit**

```bash
chmod +x packaging/make-signing-cert.sh
git add packaging/make-signing-cert.sh packaging/build-app.sh README.md
git commit -m "wip: stable self-signed signing for app builds"
```

---

### Task 5: Land Part 1 as one commit, then verify on the Mac

Controller task (not a subagent): it needs the user's password and quiet time on the Mac.

- [ ] **Step 1: Docs**

In `docs/corrections.md`, replace "Cleanup provider requests share a two-second budget; Ollama generation also retains its 800 ms timeout." with:

```markdown
Cleanup provider requests share a budget of two seconds plus 0.4 s per ten
words (at most six seconds); the Ollama request timeout grows the same way
from 1.5 s.
```

In `memory.md`, append in the file's existing decision-list style:

```markdown
- 2026-11-15 — Tuning: cleanup time scales with length (2 s / 1.5 s + 0.4 s per 10 words, cap 6 s); Whisper temperature step 0.4 (≤3 passes, benchmark-gated); no Ollama fallback-model retry; stable "Wiflow Dev" signing so permissions survive rebuilds. Why: measured timeouts, 1–4 s outliers, wasted retries, re-granting after every build.
```

- [ ] **Step 2: Squash Tasks 1–4 into one commit**

```bash
BASE=$(git log --format=%H --grep='^docs: implementation plan for tuning' -1)
git add docs/corrections.md memory.md
git reset --soft "$BASE"
git commit -F - <<'EOF'
fix: longer cleanup for long dictations, fewer Whisper retries, no wasted Ollama retry, stable signing

- Cleanup budget and Ollama timeout grow 0.4 s per 10 words (cap 6 s);
  50+ word dictations no longer time out and paste raw text.
- Whisper retries step 0.4 instead of 0.2 (at most 3 passes), gated by a
  benchmark that requires the same words on clean and noisy clips.
- Ollama no longer retries with the shared fallback model (a cloud id that
  is never installed locally).
- packaging/make-signing-cert.sh creates a "Wiflow Dev" certificate;
  build-app.sh signs with it so macOS permissions survive rebuilds.

<paste the STEP benchmark lines from Task 2 here>

Co-Authored-By: <model> <noreply@anthropic.com>
EOF
git log --oneline -3
```

Expected: one new `fix:` commit directly after the plan commit.

- [ ] **Step 3: Full gates on the squashed tree**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: clean; `303 passed; 0 failed; 7 ignored`.

- [ ] **Step 4: Hardware checks (ask the user first: about 2 minutes, Mac idle)**

1. Stuck-mic regression: `cargo test named_mic -- --ignored` → passes.
2. User runs `sh packaging/make-signing-cert.sh` (enters their password once).
3. Quit Wiflow, run `sh packaging/build-app.sh`; expect no "signing ad-hoc" note. Check `codesign -dv target/Wiflow.app 2>&1 | grep Authority` → `Authority=Wiflow Dev`. Save `codesign -d -r- target/Wiflow.app 2>&1 | tail -1`.
4. Run `sh packaging/build-app.sh` again; `codesign -d -r- target/Wiflow.app 2>&1 | tail -1` must equal the saved line (same designated requirement = macOS keeps the grants).
5. User grants Input Monitoring + Accessibility once, opens Wiflow, dictates one sentence (plain), and does one voice-edit; both work.
6. Rebuild once more and confirm dictation still works without re-granting.

---

## Part 2: live text in the pill

### Task 6: Preview building blocks (capture snapshot, fast decode, setting)

**Files:**
- Modify: `src/core/audio.rs`, `src/core/stt.rs`, `src/core/config.rs`, `src/ui/settings.rs`

**Interfaces:**
- Consumes: `Decode` and `Stt::transcribe_with` (Task 2).
- Produces:
  - `AudioCapture::snapshot(&mut self) -> &[f32]` — everything captured so far (mono, at `sample_rate()`), without stopping; `stop()` still returns everything.
  - `AudioCapture::sample_rate(&self) -> u32`.
  - `Decode::PREVIEW` and `pub fn transcribe_preview(model_path: &Path, samples: &[f32], language: &str) -> Result<String, String>` in `crate::core::stt`.
  - `Config.live_preview: bool` (default `true`).

- [ ] **Step 1: Write the failing tests**

`src/core/config.rs` `mod tests`:

```rust
    #[test]
    fn live_preview_defaults_on() {
        assert!(Config::default().live_preview);
    }
```

`src/core/stt.rs` `mod tests`:

```rust
    #[test]
    fn preview_with_missing_model_is_err() {
        assert!(transcribe_preview(Path::new("/nonexistent/ggml.bin"), &[0.1; 160], "").is_err());
    }

    /// Needs the small model and `say`. Run: cargo test --release preview_hears -- --ignored
    #[test]
    #[ignore]
    fn preview_hears_the_words() {
        let path = ensure_model_variant("small").expect("small model");
        let wav = std::env::temp_dir().join("wiflow_preview.wav");
        let ok = std::process::Command::new("say")
            .args(["-o", wav.to_str().unwrap(), "--data-format=LEI16@16000", "hello world"])
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let audio: Vec<f32> = hound::WavReader::open(&wav)
            .unwrap()
            .into_samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect();
        let text = transcribe_preview(&path, &audio, "").unwrap();
        assert!(text.to_lowercase().contains("hello"), "got {text:?}");
    }
```

`src/core/audio.rs` `mod tests` (hardware, ignored):

```rust
    /// Needs a mic. Run: cargo test snapshot_keeps -- --ignored
    #[test]
    #[ignore]
    fn snapshot_keeps_audio_for_stop() {
        let mut cap = AudioCapture::start(None).expect("a mic");
        std::thread::sleep(std::time::Duration::from_millis(400));
        let early = cap.snapshot().to_vec();
        std::thread::sleep(std::time::Duration::from_millis(400));
        let out = cap.stop();
        assert!(!early.is_empty());
        assert!(out.samples_mono.len() > early.len());
        assert_eq!(&out.samples_mono[..early.len()], &early[..], "stop keeps the snapshot");
    }
```

Run each (cargo takes one name filter per run):
`cargo test live_preview_defaults 2>&1 | tail -4` and `cargo test preview_with_missing 2>&1 | tail -4`
Expected: compile errors (no field `live_preview`, no function `transcribe_preview`).

- [ ] **Step 2: Config field and Settings checkbox**

`src/core/config.rs`, in `pub struct Config` after `duck_audio`:

```rust
    /// Show the words heard so far in the recording pill while the key is held.
    #[serde(default = "default_true")]
    pub live_preview: bool,
```

Add `live_preview: true,` to `impl Default for Config` (after `duck_audio: true,`) and to the `sample()` literal in `mod tests` (after `duck_audio: true,`).

`src/ui/settings.rs`, in `media_panel`, right after the `duck_audio` checkbox:

```rust
        ui.checkbox(
            &mut vm.config.live_preview,
            "Show words in the pill while dictating",
        );
```

- [ ] **Step 3: Fast preview decode in `src/core/stt.rs`**

In `impl Decode`, add:

```rust
    /// Live preview while the key is held: one greedy pass, no retries. Fast;
    /// the final transcript never uses it.
    pub(crate) const PREVIEW: Decode = Decode {
        beam: false,
        temperature_inc: 0.0,
    };
```

Refactor `transcribe_shared` so its model-loading body is shared:

```rust
/// Run `job` on the shared model: load once, reload on model switch, retry
/// after failure (Err never sticks).
fn with_shared_model(
    model_path: &Path,
    job: impl FnOnce(&mut Stt) -> Result<String, String>,
) -> Result<String, String> {
    let slot = STT.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    let hit = matches!(&*guard, Some((p, _)) if p == model_path);
    if !hit {
        *guard = Some((model_path.to_path_buf(), Stt::load(model_path)?));
    }
    STT_USED_MS.store(
        crate::core::cleanup::now_ms(),
        std::sync::atomic::Ordering::Relaxed,
    );
    job(&mut guard.as_mut().expect("slot just filled").1)
}

/// Load once, reload on model switch, retry after failure (Err never sticks).
/// First-implemented fix for the Phase 4 `OnceLock<Result>` Err-sticks finding.
pub fn transcribe_shared(
    model_path: &Path,
    samples: &[f32],
    initial_prompt: &str,
    language: &str,
) -> Result<String, String> {
    with_shared_model(model_path, |stt| stt.transcribe(samples, initial_prompt, language))
}

/// Quick pass for the live pill (16 kHz mono in). Never used for the pasted
/// text; hallucination tokens such as "[BLANK_AUDIO]" are stripped.
#[allow(dead_code)] // called by the worker from Task 8
pub fn transcribe_preview(model_path: &Path, samples: &[f32], language: &str) -> Result<String, String> {
    with_shared_model(model_path, |stt| {
        stt.transcribe_with(samples, "", language, Decode::PREVIEW)
    })
    .map(|text| strip_hallucination_tokens(&text).trim().to_string())
}
```

(`transcribe_shared`'s existing callers and tests stay unchanged.)

- [ ] **Step 4: Capture snapshot in `src/core/audio.rs`**

Add a field to `pub struct AudioCapture`:

```rust
    /// Samples already moved out of the ring by `snapshot` (live preview).
    taken: Vec<f32>,
```

Initialise it in `AudioCapture::start`'s `Ok(Self { … })` with `taken: Vec::new(),`.

Add to `impl AudioCapture`:

```rust
    /// Everything captured so far, without stopping (live preview). Moves new
    /// samples out of the realtime ring; `stop` returns them too.
    #[allow(dead_code)] // called by the worker from Task 8
    pub fn snapshot(&mut self) -> &[f32] {
        self.taken.extend(self.consumer.pop_iter());
        &self.taken
    }

    #[allow(dead_code)] // called by the worker from Task 8
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
```

In `stop`, replace `let samples: Vec<f32> = self.consumer.pop_iter().collect();` with:

```rust
        let mut samples = std::mem::take(&mut self.taken);
        samples.extend(self.consumer.pop_iter());
```

- [ ] **Step 5: Run all gates**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)"; cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no clippy output; `305 passed; 0 failed; 9 ignored`.
Then (GPU only, safe while the user works): `cargo test --release preview_hears -- --ignored 2>&1 | grep -E "test result|panicked"` → passes.
Do NOT run `snapshot_keeps_audio_for_stop` (opens the mic); the controller runs it later.

- [ ] **Step 6: Commit**

```bash
git add src/core/audio.rs src/core/stt.rs src/core/config.rs src/ui/settings.rs
git commit -m "feat: live-preview building blocks (capture snapshot, fast decode, setting)"
```

---

### Task 7: Live text in the pill

**Files:**
- Modify: `src/ui/pill.rs`, `src/daemon.rs` (`enum DaemonEvent`), `src/app/orchestrator.rs` (`handle`), `src/app/mod.rs` (`daemon_event_variant_name`, `user_event`)

**Interfaces:**
- Produces:
  - `DaemonEvent::LivePreview(String)`.
  - `Pill::set_live_text(&mut self, text: String)`.
  - `pub fn live_tail(text: &str, max_chars: usize) -> String` in `crate::ui::pill`.

- [ ] **Step 1: Write the failing tests**

`src/ui/pill.rs` `mod tests`:

```rust
    #[test]
    fn live_text_resets_on_every_hold() {
        let mut p = PillState::new();
        p.show_now();
        p.set_live("hello there".into());
        assert_eq!(p.live(), "hello there");
        p.show_now();
        assert_eq!(p.live(), "", "a new hold starts empty");
    }

    #[test]
    fn live_tail_keeps_the_newest_whole_words() {
        assert_eq!(live_tail("  short text ", 20), "short text");
        let tail = live_tail("the quick brown fox jumps over the lazy dog", 20);
        assert_eq!(tail, "…over the lazy dog");
        assert!(tail.chars().count() <= 20);
        assert_eq!(live_tail("supercalifragilistic", 8), "…ilistic", "one long word keeps its end");
    }
```

`src/app/orchestrator.rs` `mod tests`, next to `notice_changes_nothing`:

```rust
    #[test]
    fn live_preview_changes_nothing() {
        let mut o = new_orchestrator();
        assert!(o.handle(&DaemonEvent::LivePreview("hello".into())).is_empty());
        assert_eq!(o.phase(), Phase::Idle);
    }
```

Run each: `cargo test live_text_resets 2>&1 | tail -4`, `cargo test live_tail_keeps 2>&1 | tail -4`, `cargo test live_preview_changes 2>&1 | tail -4`
Expected: compile errors (no `set_live`, `live_tail`, `LivePreview`).

- [ ] **Step 2: Pill state and rendering (`src/ui/pill.rs`)**

Constants near `BAR_COUNT`:

```rust
/// Pill window height: timer row, waveform, live-text row.
const PILL_HEIGHT: f32 = 108.0;
/// Characters of live text that fit the pill's one line.
const LIVE_CHARS: usize = 44;
```

Pure helper (next to `smooth_rms`):

```rust
/// The end of `text` that fits one pill line: whole words, with "…" in front
/// when words were cut. Pure; unit-tested.
pub fn live_tail(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut len = 1; // the leading "…"
    for word in text.split_whitespace().rev() {
        let add = word.chars().count() + usize::from(!kept.is_empty());
        if len + add > max_chars {
            break;
        }
        len += add;
        kept.push(word);
    }
    if kept.is_empty() {
        // One word longer than the line: keep its end.
        let end: Vec<char> = text.chars().rev().take(max_chars - 1).collect();
        return format!("…{}", end.into_iter().rev().collect::<String>());
    }
    kept.reverse();
    format!("…{}", kept.join(" "))
}
```

`PillState`: add the field

```rust
    /// Words heard so far (live preview); cleared on every show.
    live: String,
```

initialise `live: String::new(),` in `PillState::new`, add `self.live.clear();` in `show_now`, and add methods:

```rust
    /// Latest live-preview text (render cache only).
    pub fn set_live(&mut self, text: String) {
        self.live = text;
    }

    pub fn live(&self) -> &str {
        &self.live
    }
```

`Pill`: add

```rust
    /// Show the words heard so far; drawn on the next animation frame.
    pub fn set_live_text(&mut self, text: String) {
        self.state.set_live(text);
    }
```

In `Pill::show`, change `height: 84.0,` to `height: PILL_HEIGHT,`.

In `Pill::frame`, before `g.paint(`, add `let live = self.state.live().to_string();`, and after the "Row 2: waveform" `ui.horizontal(...)` block (still inside `ui.vertical`), add:

```rust
                        // Row 3: the words heard so far (live preview).
                        if !live.is_empty() {
                            ui.label(
                                egui::RichText::new(live_tail(&live, LIVE_CHARS))
                                    .color(egui::Color32::from_rgb(230, 230, 235)),
                            );
                        }
```

- [ ] **Step 3: The event**

`src/daemon.rs`, in `enum DaemonEvent` after `Notice(String),`:

```rust
    /// Words heard so far while the key is held (live preview for the pill).
    #[allow(dead_code)] // sent by the worker from Task 8
    LivePreview(String),
```

`src/app/orchestrator.rs`, in `handle` after the `Notice` arm:

```rust
            // The app shell hands it straight to the pill.
            DaemonEvent::LivePreview(_) => Vec::new(),
```

`src/app/mod.rs`:
- in `daemon_event_variant_name` after the `Notice` arm: `DaemonEvent::LivePreview(_) => "LivePreview",`
- in `user_event`, right after the existing `OpenWindow` early return:

```rust
        // Live preview is display-only: straight to the pill, no lifecycle.
        if let DaemonEvent::LivePreview(text) = &event {
            self.pill.set_live_text(text.clone());
            return;
        }
```

- [ ] **Step 4: Run all gates**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)"; cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no clippy output; `308 passed; 0 failed; 9 ignored`.

- [ ] **Step 5: Commit**

```bash
git add src/ui/pill.rs src/daemon.rs src/app/orchestrator.rs src/app/mod.rs
git commit -m "feat: show live text in the recording pill"
```

---

### Task 8: Worker preview tick, docs, live check

**Files:**
- Modify: `src/daemon.rs` (`worker_main` loop near line 644; new fn `live_preview`), `src/core/audio.rs`, `src/core/stt.rs` (remove Task 6 allows), `README.md`, `memory.md`

**Interfaces:**
- Consumes: `AudioCapture::snapshot`, `AudioCapture::sample_rate`, `crate::core::stt::transcribe_preview` (Task 6); `DaemonEvent::LivePreview` (Task 7); existing `crate::core::vad::has_speech_energy(samples, rate) -> bool`, `crate::core::vad::resample_to_16k(samples, rate) -> Vec<f32>`, `crate::core::traits::local_model_path(&cfg)`.

- [ ] **Step 1: Add the preview pass to `src/daemon.rs`**

Near the other worker constants (or above `worker_main`):

```rust
/// How often the pill's live text refreshes while the key is held.
const PREVIEW_EVERY: Duration = Duration::from_millis(1000);
/// Most recent audio one preview pass transcribes: keeps passes short on
/// long holds (about 0.1–0.3 s).
const PREVIEW_TAIL_SECS: usize = 15;

/// One live-preview pass while the key is held: transcribe the latest audio
/// and show it in the pill. Never affects the final transcript.
fn live_preview(
    capture: &mut Option<crate::core::audio::AudioCapture>,
    proxy: &EventLoopProxy<DaemonEvent>,
) {
    let Some(cap) = capture.as_mut() else { return };
    // Re-read each pass so the Settings toggle applies mid-session too.
    let cfg = crate::core::config::load_config();
    // Preview is local-only: Groq users may have no local model loaded.
    if !cfg.live_preview || cfg.stt_provider == "groq" {
        return;
    }
    let rate = cap.sample_rate();
    let all = cap.snapshot();
    let tail = &all[all.len().saturating_sub(rate as usize * PREVIEW_TAIL_SECS)..];
    if !crate::core::vad::has_speech_energy(tail, rate) {
        return;
    }
    let audio = crate::core::vad::resample_to_16k(tail, rate);
    let t0 = Instant::now();
    match crate::core::traits::local_model_path(&cfg)
        .and_then(|path| crate::core::stt::transcribe_preview(&path, &audio, &cfg.stt_language))
    {
        Ok(text) if !text.is_empty() => {
            tracing::info!(
                "live preview: {} words in {}ms",
                text.split_whitespace().count(),
                t0.elapsed().as_millis()
            );
            let _ = proxy.send_event(DaemonEvent::LivePreview(text));
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("live preview failed: {e}"),
    }
}
```

- [ ] **Step 2: Wake for previews in `worker_main`**

Before `loop {` add:

```rust
    // Next live-preview time while the mic is open (None between holds).
    let mut preview_at: Option<Instant> = None;
```

Replace the `Some(start) => { … }` arm of the `let ctl = match capture_start {` with:

```rust
            Some(start) => {
                let deadline = start + Duration::from_millis(max_ms);
                let now = Instant::now();
                if now >= deadline {
                    watchdog(
                        &mut ptt,
                        &mut capture,
                        &mut capture_start,
                        &proxy,
                        max_ms,
                        &duck,
                    );
                    continue;
                }
                // Wake at the earlier of the watchdog and the next preview.
                let preview_due = *preview_at.get_or_insert(start + PREVIEW_EVERY);
                let wake = deadline.min(preview_due);
                match rx.recv_timeout(wake.saturating_duration_since(now)) {
                    Ok(c) => c,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if Instant::now() >= deadline {
                            watchdog(
                                &mut ptt,
                                &mut capture,
                                &mut capture_start,
                                &proxy,
                                max_ms,
                                &duck,
                            );
                        } else {
                            live_preview(&mut capture, &proxy);
                            preview_at = Some(Instant::now() + PREVIEW_EVERY);
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
```

and replace the `None => match rx.recv() {` arm with:

```rust
            None => {
                preview_at = None;
                match rx.recv() {
                    Ok(c) => c,
                    Err(_) => break,
                }
            }
```

Remove the four `#[allow(dead_code)]` lines added in Tasks 6–7 (on `AudioCapture::snapshot`, `AudioCapture::sample_rate`, `transcribe_preview`, `DaemonEvent::LivePreview`). Clippy must stay clean without them.

- [ ] **Step 3: Run all gates**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)"; cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no clippy output; `308 passed; 0 failed; 9 ignored`.

- [ ] **Step 4: Docs**

`README.md`, after the "Edit selected text by voice" paragraph:

```markdown
**Live text:** while you hold the key, the recording pill shows the words
heard so far. The pasted text still comes from the full pass when you
release. Turn it off in Settings ("Show words in the pill while dictating").
It uses the local model only.
```

`memory.md`, append:

```markdown
- 2026-11-15 — Live text in the pill: worker re-transcribes the last ≤15 s every 1 s (greedy, no retries) and sends LivePreview to the pill; final paste unchanged; toggle live_preview (default on); local STT only. Why: user wants to see what has been heard while dictating; typing into the app live was rejected (flicker, fragmented undo, breaks cleanup).
```

- [ ] **Step 5: Commit**

```bash
git add src/daemon.rs src/core/audio.rs src/core/stt.rs README.md memory.md
git commit -m "feat: refresh the pill's live text once a second while recording"
```

- [ ] **Step 6: Live check (controller, with the user's OK; Mac idle about 2 minutes)**

Use the BlackHole procedure from the voice-edit plan (`docs/superpowers/plans/2026-11-14-voice-edit-selection.md`, Task 5 Steps 1 and 5): back up the config, set `mic_name` to "BlackHole 2ch", quit the running Wiflow, start `target/release/wiflow-dictation --app`, restore everything afterwards.

1. `cargo test snapshot_keeps -- --ignored` → passes (mic free).
2. Hold Right Option for 6 s while `say -a <BlackHole id>` reads a long sentence. Log shows several `live preview: N words in Xms` lines with growing N and X under about 300; the final `transcribed in` line and the pasted text match the full sentence.
3. Turn the toggle off in `config.json` (`"live_preview": false`), repeat: no `live preview` lines; dictation still pastes.
4. Voice-edit (Right Option + Shift on selected TextEdit text, "make it shorter") still replaces the selection; Cmd+Z restores it.
5. A 2 s hold with silence: no preview lines, "no speech detected", mic released (`micbusy`-style check or `cargo test named_mic -- --ignored`).

---

## Self-Review

**Spec coverage**

| Spec item | Task |
|---|---|
| 1. Cleanup time scales, cap 6 s, edit unchanged | 1 |
| 2. Temperature step 0.4, benchmark gate, fallback to 0.2 | 2 |
| 3. Ollama no fallback retry, cloud unchanged | 3 |
| 4. Signing script, build-app identity, README, launch-agent note | 4, 5 |
| Part 1 as one commit + gates + hardware checks | 5 |
| Capture snapshot, fast decode, setting toggle | 6 |
| Pill line, height, `live_tail`, reset per hold, `LivePreview` | 7 |
| Worker 1 s tick, 15 s tail, silence skip, Groq off, final paste unchanged | 8 |
| Not breaking anything (gates, named_mic, live checks with consent) | Global Constraints, 5, 8 |

**Placeholder scan:** the only fill-ins are runtime values the executor pastes (benchmark output, the model name in the trailer) — no missing code.

**Type consistency:** `Decode { beam, temperature_inc }`, `Decode::FULL` (Task 2) and `Decode::PREVIEW` (Task 6) match their use in `transcribe_with` and `transcribe_preview`; `transcribe_preview(&Path, &[f32], &str)` matches its call in Task 8; `snapshot() -> &[f32]` and `sample_rate() -> u32` match Task 8; `DaemonEvent::LivePreview(String)` matches Tasks 7 and 8; `Pill::set_live_text(String)` matches the app hook. Test counts: 301 → 302 (T1) → 302, +1 ignored (T2) → 303 (T3) → 303 (T4) → 305, +2 ignored (T6) → 308 (T7) → 308 (T8).
