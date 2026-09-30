# Phase 4 Inject + History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Dictation output lands at the cursor (clipboard + Cmd+V) and in a local top-50 history, with STT follow-ups closed.

**Architecture:** Phase 3 follow-ups first (lazy `Stt` singleton, small STT hygiene fixes), then `inject.rs` (arboard clipboard save→set→Cmd+V via enigo→restore, graceful Err with leave-on-clipboard fallback), then `history.rs` (serde JSON top-50 with pure-path testable core), then wiring into the simulate branch (`--no-inject` escape, empty-transcript skip) + docs + user manual matrix.

**Tech Stack:** Rust 2021, `arboard 3.6` (`Clipboard::new/get_text/set_text` — verified against vendored 3.6.1), `enigo 0.6` (`Enigo::new(&Settings::default())`, `Keyboard::key`, `Key::Meta`/`Key::Unicode`, `Direction` — verified against vendored 0.6.1), `serde 1` + `serde_json 1`, existing `stt`/`vad`/`audio`/`hotkey` modules

## Global Constraints

- Target macOS 13+ arm64 first, must compile warning-free on `aarch64-apple-darwin`.
- Rust edition 2021, stable toolchain (verified 1.95.0).
- Default path $0, no API key, no network calls (model already on disk).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets -- -D warnings` clean.
- `cargo test` passes; new logic needs a test.
- Recording indicator mandatory whenever mic open (console log in prototype).
- Injection failure must still leave text in clipboard + notify (rules.md UX rule).
- Password-field caution: clipboard-only fallback, never raw keystroke dump (PRD edge case).

---

## File Structure

- Modify: `src/stt.rs` — Task 1 (`shared_stt` singleton).
- Modify: `src/main.rs` — Task 1 (dump dogfood, --model verify warn, kept/total log, kept-duration RTF), Task 4 (`mod inject/history`, `--no-inject` flag, inject+history wiring).
- Create: `src/inject.rs` — Task 2 (`inject_text`, `leave_on_clipboard`, `InjectReport`).
- Create: `src/history.rs` — Task 3 (`HistoryEntry`, path fns, load/push with pure-path core).
- Modify: `Cargo.toml` — Task 2 (add `arboard`, `enigo`), Task 3 (add `serde`, `serde_json`).
- Modify: `task.md`, `memory.md` — Task 4 (check Phase 4 boxes + numbers, dated entry, user manual matrix).

Interfaces:
- `stt::shared_stt(&Path) -> Result<&'static Mutex<Stt>, String>` (new Task 1; `OnceLock` + `get_or_try_init`, stable since 1.83; `Stt` auto Send+Sync via `Arc<WhisperInnerContext>` — verified in whisper-rs 0.16.0 source).
- `inject::InjectReport { pasted_via: &'static str, clipboard_restored: bool }` (new Task 2).
- `inject::inject_text(&str) -> Result<InjectReport, String>` (new Task 2; Err on empty; clipboard save→set→Cmd+V→200ms→restore).
- `inject::leave_on_clipboard(&str)` (new Task 2; best-effort, never Err).
- `history::HistoryEntry { text: String, at_ms: u64, duration_ms: u64, rtf: f64 }` (new Task 3, Serialize/Deserialize/PartialEq/Clone/Debug).
- `history::MAX_ENTRIES: usize = 50`, `history::history_path() -> PathBuf` (new Task 3).
- `history::load_history_from(&Path) -> Vec<HistoryEntry>`, `history::push_history_to(&Path, HistoryEntry) -> Result<(), String>` (new Task 3, pure-path core).
- `history::load_history() -> Vec<HistoryEntry>`, `history::push_history(HistoryEntry) -> Result<(), String>` (new Task 3, thin wrappers).

---

### Task 1: STT Follow-ups — Singleton + Hygiene

**Files:**
- Modify: `src/stt.rs` (append `shared_stt`)
- Modify: `src/main.rs:23-36,73,84-93,104-109` (dump dogfood, model verify warn, kept log, RTF denominator)
- Test: inline — singleton bad-path test (no model needed)

**Interfaces:**
- Consumes: `std::sync::{Mutex, OnceLock}` (std, no dep).
- Produces: `shared_stt` for Task 4 wiring; corrected logs for honest benching.

- [ ] **Step 1: Write the failing test (append to stt tests)**

```rust
#[test]
fn test_shared_stt_bad_path_is_err() {
    assert!(shared_stt(std::path::Path::new("/nonexistent/ggml.bin")).is_err());
    // Second call with same bad path: still Err, no panic, no hang.
    assert!(shared_stt(std::path::Path::new("/nonexistent/ggml.bin")).is_err());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test shared_stt 2>&1 | tail -4`
Expected: FAIL with `cannot find function shared_stt`.

- [ ] **Step 3: stt.rs addition (append after Stt impl, before model constants)**

```rust
use std::sync::{Mutex, OnceLock};

static STT: OnceLock<Mutex<Stt>> = OnceLock::new();

/// Load once per process: 5.7s Metal init must not repeat per hold.
/// First call wins — later calls with a different path return the cached instance.
pub fn shared_stt(model_path: &Path) -> Result<&'static Mutex<Stt>, String> {
    STT.get_or_try_init(|| Stt::load(model_path).map(Mutex::new))
}
```

- [ ] **Step 4: main.rs hygiene edits (exact replacements)**

dump_wav line 32: replace `w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;` with `w.write_sample(crate::audio::f32_to_i16(s))?;`

Line 73 kept-log: replace `info!("vad kept {}/{} samples", kept.len(), out.samples_mono.len());` with `info!("audio raw {} → 16k {} → kept {}", out.samples_mono.len(), s16.len(), kept.len());` — requires binding `s16`: replace lines 71–72 (`let mut vad...`/`let kept...`) with:

```rust
let mut vad = vad::Vad::new();
let s16 = vad::resample_to_16k(&out.samples_mono, out.sample_rate);
let kept = vad.trim_silence(&s16);
```

Wait — this inlines the pipeline instead of `transcribe_ready`, breaking the enforced contract from Phase 3. DO NOT do that. Instead keep `transcribe_ready` and log without the middle number:

```rust
info!("vad kept {}/{} raw @ {}Hz", kept.len(), out.samples_mono.len(), out.sample_rate);
```

(Transcribe this simpler form. The raw→16k→kept triple-log is deferred to a pipeline that returns counts — out of scope.)

--model arm (lines 84–85): replace `Some(p) => p.clone(),` with:

```rust
Some(p) => {
    if !stt::verify_model(p) {
        warn!("custom model fails size check, attempting load anyway: {}", p.display());
    }
    p.clone()
}
```

RTF denominator (lines 106–107): replace

```rust
let ms = t1.elapsed().as_millis();
let rtf = ms as f64 / duration_ms.max(1) as f64;
```

with

```rust
let ms = t1.elapsed().as_millis();
let kept_ms = kept.len() as f64 / vad::VAD_SAMPLE_RATE as f64 * 1000.0;
let rtf = ms as f64 / kept_ms.max(1.0);
```

- [ ] **Step 5: Run gates**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`
Expected: `FMT_OK`.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings. `shared_stt` is unused by main yet → `#[allow(dead_code)]` on the fn with `// Task 4 wires` comment ONLY if clippy demands; prefer no allow if it compiles clean (bin crate: unused pub fn in binary = dead_code warn — expect to need the scoped allow; add it preemptively with comment).

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: zero failures; report actual count (26 passed + 1 new + 1 ignored).

Run: `cargo run -- --simulate-hold-ms 1200 2>&1 | tail -2`
Expected: unchanged behavior (no-speech or would-transcribe path), no panic.

- [ ] **Step 6: Commit**

```bash
git add src/stt.rs src/main.rs
git commit -m "feat: add shared stt singleton and stt hygiene fixes"
```

---

### Task 2: Inject Module — Clipboard + Cmd+V

**Files:**
- Modify: `Cargo.toml` (add `arboard = "3.6"`, `enigo = "0.6"`)
- Create: `src/inject.rs`
- Modify: `src/main.rs:1-4` (add `mod inject;` only, alphabetical: audio, history later, hotkey, inject, stt, vad — insert `mod inject;` after `mod hotkey;`)
- Test: inline — empty-Err (deterministic) + live round-trip-or-honest-skip

**Interfaces:**
- Consumes: `arboard::Clipboard` (`new/get_text/set_text` — verified 3.6.1), `enigo::{Direction, Enigo, Key, Keyboard, Settings}` (`new(&Settings)`, `key` — verified 0.6.1).
- Produces: `inject_text`, `leave_on_clipboard`, `InjectReport` per File Structure.

- [ ] **Step 1: Add deps + write failing tests**

Add to `Cargo.toml` `[dependencies]` (after `webrtc-vad`, before `whisper-rs` — keep rough alpha order):

```toml
arboard = "3.6"
enigo = "0.6"
```

Create `src/inject.rs` with ONLY the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_empty_is_err() {
        assert!(inject_text("").is_err());
        assert!(inject_text("   ").is_err());
    }

    #[test]
    fn test_inject_roundtrip_or_skip() {
        // Needs GUI session for enigo; headless runners report honestly.
        let mut cb = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipped: no clipboard: {e:?}");
                return;
            }
        };
        let marker = "wiflow-probe-marker";
        cb.set_text(marker).unwrap();
        match inject_text("wiflow-probe") {
            Ok(r) => {
                assert!(r.clipboard_restored);
                assert_eq!(cb.get_text().unwrap(), marker);
            }
            Err(e) => eprintln!("skipped inject (no GUI session): {e:?}"),
        }
    }
}
```

Note: `inject_text("   ")` (whitespace-only) must Err — trim check in implementation. Add `mod inject;` to main.rs.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test inject:: 2>&1 | tail -5`
Expected: FAIL with `cannot find function inject_text` (deps build first — arboard/enigo compile, allow minutes on first build).

- [ ] **Step 3: Minimal implementation (prepend to src/inject.rs above tests)**

```rust
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectReport {
    pub pasted_via: &'static str,
    pub clipboard_restored: bool,
}

fn enigo_err(ctx: &str, e: enigo::InputError) -> String {
    format!("{ctx}: {e:?}")
}

/// Paste text at the focused cursor: save clipboard → set → Cmd+V → restore.
/// Fails cleanly (never panics); callers must leave text on clipboard + notify.
/// NOTE: Cmd+V uses the QWERTY V position via `Key::Unicode('v')` — Dvorak/Colemak
/// layouts need a layout-aware path (Phase 5 follow-up, manual matrix covers).
pub fn inject_text(text: &str) -> Result<InjectReport, String> {
    if text.trim().is_empty() {
        return Err("refusing to inject empty text".into());
    }
    let mut cb = Clipboard::new().map_err(|e| format!("clipboard open: {e:?}"))?;
    let saved = cb.get_text().unwrap_or_default();
    cb.set_text(text)
        .map_err(|e| format!("clipboard set: {e:?}"))?;
    let mut en =
        Enigo::new(&Settings::default()).map_err(|e| enigo_err("enigo init", e))?;
    en.key(Key::Meta, Direction::Press)
        .map_err(|e| enigo_err("meta press", e))?;
    let paste = en.key(Key::Unicode('v'), Direction::Click);
    let _ = en.key(Key::Meta, Direction::Release);
    paste.map_err(|e| enigo_err("paste key", e))?;
    // Let the target app consume the paste before restoring the clipboard.
    std::thread::sleep(std::time::Duration::from_millis(200));
    cb.set_text(saved)
        .map_err(|e| format!("clipboard restore: {e:?}"))?;
    Ok(InjectReport {
        pasted_via: "clipboard+Cmd+V",
        clipboard_restored: true,
    })
}

/// Best-effort fallback: leave text on the clipboard so the user can Cmd+V.
/// Never returns Err, never panics.
pub fn leave_on_clipboard(text: &str) {
    if let Ok(mut cb) = Clipboard::new() {
        let _ = cb.set_text(text);
    }
}
```

Check `enigo::InputError` is exported at crate root (verified `pub enum InputError` lib.rs:389 + `pub type InputResult` lib.rs:385; root re-export assumed — if compile fails on `enigo::InputError`, use `enigo::NewConError`-style path from compiler suggestion and report the deviation).

- [ ] **Step 4: Run gates**

Run: `cargo test inject:: 2>&1 | tail -6`
Expected: 2 passed (or 1 pass + honest `skipped` eprintln on headless — both acceptable; report which).

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings in our code.

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: zero failures; report actual count.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/inject.rs src/main.rs
git commit -m "feat: add clipboard Cmd+V text injection"
```

---

### Task 3: History Module — Top-50 JSON

**Files:**
- Modify: `Cargo.toml` (add `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`)
- Create: `src/history.rs`
- Modify: `src/main.rs` (add `mod history;` after `mod hotkey;` — order: audio, history, hotkey, inject, stt, vad)
- Test: inline — pure-path round-trip (hermetic, temp dir)

**Interfaces:**
- Consumes: `serde::{Deserialize, Serialize}`, `serde_json`, `std::fs`.
- Produces: all `history::` items per File Structure.

- [ ] **Step 1: Add deps + write failing tests**

Add to `Cargo.toml`:

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

Create `src/history.rs` with ONLY the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(text: &str) -> HistoryEntry {
        HistoryEntry { text: text.into(), at_ms: 1_700_000_000_000, duration_ms: 1500, rtf: 0.1 }
    }

    #[test]
    fn test_roundtrip_single() {
        let p = std::env::temp_dir().join("wiflow_hist_test_1.json");
        let _ = std::fs::remove_file(&p);
        push_history_to(&p, sample_entry("hello")).unwrap();
        let got = load_history_from(&p);
        assert_eq!(got, vec![sample_entry("hello")]);
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_missing_file_is_empty() {
        let p = std::env::temp_dir().join("wiflow_hist_missing_xyz.json");
        let _ = std::fs::remove_file(&p);
        assert!(load_history_from(&p).is_empty());
    }

    #[test]
    fn test_corrupt_file_is_empty() {
        let p = std::env::temp_dir().join("wiflow_hist_corrupt_xyz.json");
        std::fs::write(&p, "{not json").unwrap();
        assert!(load_history_from(&p).is_empty());
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_truncates_to_50() {
        let p = std::env::temp_dir().join("wiflow_hist_trunc_xyz.json");
        let _ = std::fs::remove_file(&p);
        for i in 0..55 {
            push_history_to(&p, sample_entry(&format!("e{i}"))).unwrap();
        }
        let got = load_history_from(&p);
        assert_eq!(got.len(), MAX_ENTRIES);
        assert_eq!(got.last().unwrap().text, "e54");
        assert_eq!(got.first().unwrap().text, "e5");
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_history_path_name() {
        assert_eq!(history_path().file_name().unwrap(), "history.json");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test history:: 2>&1 | tail -4`
Expected: FAIL with `cannot find HistoryEntry / MAX_ENTRIES / push_history_to / load_history_from / history_path`.

- [ ] **Step 3: Minimal implementation (prepend to src/history.rs above tests)**

```rust
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub text: String,
    pub at_ms: u64,
    pub duration_ms: u64,
    pub rtf: f64,
}

pub const MAX_ENTRIES: usize = 50;
const HISTORY_NAME: &str = "history.json";

pub fn history_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/wiflow/history.json")
}

/// Missing or corrupt file → empty vec (never Err — history must not break dictation).
pub fn load_history_from(path: &Path) -> Vec<HistoryEntry> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn push_history_to(path: &Path, entry: HistoryEntry) -> Result<(), String> {
    let mut all = load_history_from(path);
    all.push(entry);
    if all.len() > MAX_ENTRIES {
        all.drain(..all.len() - MAX_ENTRIES);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir history: {e:?}"))?;
    }
    let json = serde_json::to_string_pretty(&all).map_err(|e| format!("encode history: {e:?}"))?;
    std::fs::write(path, json).map_err(|e| format!("write history: {e:?}"))
}

pub fn load_history() -> Vec<HistoryEntry> {
    load_history_from(&history_path())
}

pub fn push_history(entry: HistoryEntry) -> Result<(), String> {
    push_history_to(&history_path(), entry)
}
```

- [ ] **Step 4: Run gates**

Run: `cargo test history:: 2>&1 | tail -4`
Expected: 5 passed.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings.

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: zero failures; report actual count.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/history.rs src/main.rs
git commit -m "feat: add top-50 json history with pure-path core"
```

---

### Task 4: Wire Inject + History + Docs + User Matrix

**Files:**
- Modify: `src/main.rs` (`--no-inject` flag, inject+history wiring, empty-transcript skip)
- Modify: `task.md` (check Phase 4 boxes + numbers), `memory.md` (dated entry + matrix for user)
- Test: `cargo test` + `--no-inject` end-to-end + clipboard-fallback run

**Interfaces:**
- Consumes: `inject::inject_text/leave_on_clipboard`, `history::push_history/HistoryEntry`, `stt::shared_stt` (Task 1), transcribe `text`/`duration_ms`/`rtf` in scope.
- Produces: injected cursor text (or clipboard fallback + warn), history entry, docs.

- [ ] **Step 1: Wire into Transcribe arm (exact replacements)**

Add flag to `Args` after `model`:

```rust
/// Skip cursor injection (headless/CI runs)
#[arg(long)]
no_inject: bool,
```

Replace the `Ok(text) => {...}` block (lines ~105–110: ms/rtf log + TRANSCRIPT println) with:

```rust
Ok(text) => {
    let ms = t1.elapsed().as_millis();
    let kept_ms = kept.len() as f64 / vad::VAD_SAMPLE_RATE as f64 * 1000.0;
    let rtf = ms as f64 / kept_ms.max(1.0);
    info!("model loaded in {load_ms}ms, transcribed in {ms}ms (RTF {rtf:.2})");
    println!("TRANSCRIPT: {text}");
    if text.trim().is_empty() {
        info!("empty transcript, nothing to inject");
    } else {
        let entry = history::HistoryEntry {
            text: text.clone(),
            at_ms: now_ms(),
            duration_ms,
            rtf,
        };
        if let Err(e) = history::push_history(entry) {
            warn!("history push failed: {e}");
        }
        if args.no_inject {
            info!("--no-inject: skipping cursor injection");
        } else {
            match inject::inject_text(&text) {
                Ok(r) => info!("injected via {} (clipboard restored: {})", r.pasted_via, r.clipboard_restored),
                Err(e) => {
                    warn!("inject failed ({e}) — text left on clipboard, press Cmd+V");
                    inject::leave_on_clipboard(&text);
                }
            }
        }
    }
}
```

Add helper above `main` (after `dump_wav`):

```rust
fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
```

(Wall-clock is correct here: history timestamp, not a duration — `Instant` would be wrong.)

Also switch the Stt load to the singleton: replace

```rust
let mut stt = match stt::Stt::load(&model_path) {
```

with

```rust
let stt_lock = match stt::shared_stt(&model_path) {
    Ok(s) => s,
    Err(e) => {
        warn!("stt load failed: {e}");
        return;
    }
};
let mut stt = stt_lock.lock().unwrap_or_else(|e| e.into_inner());
```

And `stt.transcribe(&kept)` → same call on the guard (transcribe takes `&mut self` — guard derefs mutably; if the compiler rejects, bind `let mut stt = ...lock()...` then call — report the exact form used). Remove the Task 1 `#[allow(dead_code)]` on `shared_stt` if now consumed (keep only if clippy still demands with comment).

- [ ] **Step 2: Run gates + live runs**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`
Expected: `FMT_OK`.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings.

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: zero failures; report actual count.

Run: `cargo run -- --simulate-hold-ms 2000 --no-inject 2>&1 | tail -4`
Expected: TRANSCRIPT (or no-speech path), history file created at `~/Library/Application Support/wiflow/history.json` — verify with `cat` (1 entry). Record.

Run: `cargo run -- --simulate-hold-ms 2000 2>&1 | tail -4` (WITH inject, live mic if available)
Expected: inject Ok (GUI session) or clean warn + clipboard fallback (headless). Record which. Never panic.

- [ ] **Step 3: Commit code**

```bash
git add src/main.rs
git commit -m "feat: wire inject and history into transcribe path"
```

- [ ] **Step 4: Update docs + commit (include USER manual matrix)**

In `task.md`: check Phase 4 boxes (6 pre-reqs + inject + history) with numbers (inject Ok-or-fallback outcome, history entry verified, test count). Mark the 5-app manual matrix (VS Code, Safari, Slack, Terminal, password-field) as `[ ] USER` items with exact commands:

```bash
# terminal check first (paste target):
cargo run -- --simulate-hold-ms 3000   # speak, then check text appeared in the FOCUSED app
```

Note Accessibility permission requirement for enigo (System Settings → Privacy → Accessibility → add Terminal/binary) and password-field expectation (clipboard-only + warn).

In `memory.md`: append dated entry with dep versions (arboard 3.6, enigo 0.6, serde 1, serde_json 1), inject outcome, gate status.

```bash
git add task.md memory.md
git commit -m "docs: mark phase4 inject-history complete with numbers"
```

---

## Self-Review

- Spec coverage: task.md Phase 4 (inject.rs → Task 2; history.rs → Task 3; manual matrix → Task 4 USER items; 6 pre-reqs → Task 1 + Task 4 wiring of shared_stt) + PRD inject/edge rules (empty skip, clipboard fallback + notify, password-field expectation documented) + rules.md (history local, clearable — file is plain JSON the user can delete; note in memory).
- Placeholder scan: no TBD/TODO; enigo root-export risk fenced with compiler-suggestion protocol; Mutex-guard-deref form fenced with report-exact-form protocol; sandbox inject-failure fenced as warn+fallback (never fail gate); manual matrix explicitly USER-owned.
- Type consistency: `shared_stt(&Path) -> Result<&'static Mutex<Stt>>` matches wiring `stt_lock.lock()` + `transcribe(&mut)`; `HistoryEntry` fields match construction in Task 4; `InjectReport` fields match both test assert and wiring log; `--no-inject`/`--model`/`--device` flags coexist; `now_ms()` u64 matches `at_ms: u64`, `duration_ms` u64 matches, `rtf` f64 matches.
