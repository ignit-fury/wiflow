# Voice-edit of selected text Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Select text in any app, hold the push-to-talk key with Shift, say an instruction, and the selection is replaced with the edited text.

**Architecture:** The event tap flags a hold as an "edit hold" when Shift is down with the PTT key. On release the worker transcribes the instruction while a second thread reads the selection (Accessibility first, Cmd+C fallback). A new `core::edit` module sends instruction + selection to the configured cleanup provider through a new `cleanup::edit_chat`, checks the result, and sends it as `DaemonEvent::Done`, so the existing paste step replaces the selection. Failures send a new `DaemonEvent::Notice` (native notification, no state change) and end the session with an empty `Done`.

**Tech Stack:** Rust 2021 (MSRV 1.81), existing crates only (`arboard`, `ureq`, `serde_json`, winit). Raw macOS FFI for Accessibility and CGEvent, as in `secure.rs` and `tap.rs`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-14-voice-edit-selection-design.md`. Where this plan differs from the spec (no provider fallback chain, no fake-model test, edits not saved to history, a `Notice` event), Task 5 updates the spec to match.

## Global Constraints

- Gates before every commit: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (baseline 281 passed, 5 ignored; never regresses).
- `core/` imports nothing from `ui/` or `platform/` (rules.md §3). `core::edit` must not call `platform::macos::*`; the worker (`daemon.rs`) passes the selection in.
- Push-to-talk only; the mic indicator and pill behave exactly as today. No new hotkey is registered. Plain dictation (no Shift) must behave exactly as before.
- Edit mode works for the bare-modifier presets (Right Option, Fn) only. Ctrl+Space dictates as today.
- Never make real LLM or network calls inside `cargo test`.
- Any failure leaves the user's text untouched and shows a native notification.
- Selection limit: `MAX_SELECTION_CHARS = 1500` (about 250 words; the Ollama output cap is 400 tokens).
- Edit time budget: 8 s. Normal dictation keeps its 2 s budget and 1.5 s Ollama timeout.
- One task = one conventional commit (`feat:` / `test:` / `docs:`), with the session's `Co-Authored-By` trailer.
- Work on branch `voice-edit`.

## File Structure

| File | Responsibility |
|---|---|
| `src/edit_prompt.txt` (new) | System prompt for edits. |
| `src/core/edit.rs` (new) | Pure edit logic: `build_user_message`, `check_request`, `check_result`, `run`. |
| `src/core/cleanup.rs` | Budget gains an Ollama timeout field and `for_edit()`; `try_provider_model` takes an optional system prompt; new `edit_provider` + `edit_chat`; `strip_surrounding_quotes` becomes `pub(crate)`. |
| `src/core/mod.rs` | Register `pub mod edit;`. |
| `src/platform/macos/tap.rs` | Detect Shift during a hold: `shift_with_ptt`, `edit_flag_after`, `take_edit_held`. |
| `src/platform/macos/secure.rs` | Read strings of any length; new `focused_string_attribute`. |
| `src/platform/macos/selection.rs` (new) | `selected_text()` with the Cmd+C fallback. |
| `src/platform/macos/mod.rs` | Register `pub mod selection;`. |
| `src/daemon.rs` | `DaemonEvent::Notice`; edit branch in `pipeline_on_worker`; `finish_edit`. |
| `src/app/orchestrator.rs`, `src/app/mod.rs` | Handle `Notice` (no actions; native notification). |

---

### Task 1: Edit core and provider hook

**Files:**
- Create: `src/edit_prompt.txt`, `src/core/edit.rs`
- Modify: `src/core/cleanup.rs`, `src/core/mod.rs`

**Interfaces:**
- Produces (used by Task 4):
  - `crate::core::edit::run(instruction: &str, selection: Option<&str>, cfg: &Config) -> Result<String, String>`; `Err` is a user-facing notice.
  - `crate::core::edit::MAX_SELECTION_CHARS: usize`.
  - `crate::core::cleanup::edit_chat(system: &str, user: &str, cfg: &Config) -> Result<String, String>`.

- [ ] **Step 1: Write the failing provider tests**

In `src/core/cleanup.rs`, inside the existing `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn edit_provider_follows_config_then_keys() {
        assert_eq!(edit_provider("groq", false, false), Provider::Groq);
        assert_eq!(edit_provider("openrouter", false, false), Provider::OpenRouter);
        assert_eq!(edit_provider("ollama", true, true), Provider::Ollama);
        assert_eq!(edit_provider("auto", true, true), Provider::Groq);
        assert_eq!(edit_provider("", false, true), Provider::OpenRouter);
        assert_eq!(edit_provider("auto", false, false), Provider::Ollama);
    }

    #[test]
    fn edit_budget_allows_long_rewrites_and_dictation_keeps_its_limits() {
        let edit = CleanupBudget::for_edit();
        assert!(edit.remaining().unwrap() > Duration::from_secs(5));
        assert_eq!(edit.ollama_timeout, EDIT_BUDGET);
        assert_eq!(CleanupBudget::new().ollama_timeout, OLLAMA_TIMEOUT);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test edit_ 2>&1 | tail -15`
Expected: compile errors "cannot find function `edit_provider`" / "no function `for_edit`".

- [ ] **Step 3: Implement the provider changes in `src/core/cleanup.rs`**

(a) Replace the `CleanupBudget` struct and its `new` with (keep `remaining` unchanged):

```rust
struct CleanupBudget {
    deadline: std::time::Instant,
    /// Per-request cap for the local Ollama call.
    ollama_timeout: Duration,
}

impl CleanupBudget {
    fn new() -> Self {
        Self {
            deadline: std::time::Instant::now() + CLEANUP_BUDGET,
            ollama_timeout: OLLAMA_TIMEOUT,
        }
    }
    /// Edits rewrite a whole selection: far longer than tidying a sentence.
    fn for_edit() -> Self {
        Self {
            deadline: std::time::Instant::now() + EDIT_BUDGET,
            ollama_timeout: EDIT_BUDGET,
        }
    }
```

Add next to `CLEANUP_BUDGET`:

```rust
/// Time limit for one voice-edit request (rewrites take longer than cleanup).
const EDIT_BUDGET: Duration = Duration::from_secs(8);
```

(b) In `ollama_chat`, change `.timeout(budget.remaining()?.min(OLLAMA_TIMEOUT))` to:

```rust
        .timeout(budget.remaining()?.min(budget.ollama_timeout))
```

(c) Give `try_provider_model` an optional system prompt. New signature and the two provider calls:

```rust
fn try_provider_model(
    p: Provider,
    model: &str,
    transcript: &str,
    system: Option<&str>,
    budget: &CleanupBudget,
) -> Result<String, String> {
```

Inside, in the Groq and OpenRouter arms replace the fifth argument `None,` of `chat_completion(...)` with `system,`, and replace the Ollama arm with:

```rust
        Provider::Ollama => ollama_chat(transcript, system, model, budget),
```

In `try_provider_with_retry`, update both calls to pass `None` for the new parameter:

```rust
    match try_provider_model(p, primary, transcript, None, budget)
```
```rust
                try_provider_model(p, fallback, transcript, None, budget)
```

(d) Make the quote helper reachable from `core::edit`: change `fn strip_surrounding_quotes` to `pub(crate) fn strip_surrounding_quotes`.

(e) Add after `single_provider_chain` (before `is_quota_error`):

```rust
/// Provider for one edit: the configured one, or for "auto" the first with a
/// key (Groq, then OpenRouter), else local Ollama.
fn edit_provider(provider: &str, has_groq: bool, has_openrouter: bool) -> Provider {
    match provider {
        "groq" => Provider::Groq,
        "openrouter" => Provider::OpenRouter,
        "ollama" => Provider::Ollama,
        _ if has_groq => Provider::Groq,
        _ if has_openrouter => Provider::OpenRouter,
        _ => Provider::Ollama,
    }
}

/// One edit request to the configured provider. No fallback chain: a failed
/// edit leaves the user's text alone and says why.
pub fn edit_chat(system: &str, user: &str, cfg: &crate::core::config::Config) -> Result<String, String> {
    let p = edit_provider(
        &cfg.cleanup_provider,
        groq_key().is_some(),
        openrouter_key().is_some(),
    );
    try_provider_model(p, provider_model(p, cfg), user, Some(system), &CleanupBudget::for_edit())
        .map_err(|e| format!("{} edit failed: {e}", p.name()))
}
```

- [ ] **Step 4: Run the provider tests**

Run: `cargo fmt && cargo test edit_ 2>&1 | tail -8`
Expected: `edit_provider_follows_config_then_keys` and `edit_budget_allows_long_rewrites_and_dictation_keeps_its_limits` pass; no compile errors elsewhere (`cargo build` is clean).

- [ ] **Step 5: Create the edit prompt `src/edit_prompt.txt`**

```text
You edit text. You receive an INSTRUCTION and a TEXT.
Apply the instruction to the text and reply with only the edited text.
Rules:
1. No quotes around the answer, no explanation, no preamble, no notes.
2. Keep the original language, meaning and formatting unless the instruction asks to change them.
3. If the instruction asks for a translation, reply with only the translation.
4. If the instruction is unclear or does not apply, reply with the text unchanged.
```

- [ ] **Step 6: Write the failing edit tests with stubs**

Create `src/core/edit.rs`:

```rust
//! Voice-edit: rewrite the selected text by a spoken instruction.
//!
//! Pure logic plus one provider call. The worker reads the selection and
//! passes it in; nothing here touches the OS (rules.md §3).

use crate::core::config::Config;

const EDIT_SYSTEM_PROMPT: &str = include_str!("../edit_prompt.txt");

/// About 250 words. The Ollama output cap (400 tokens) cannot hold a longer
/// rewrite, so longer selections are refused up front.
pub const MAX_SELECTION_CHARS: usize = 1500;

pub fn build_user_message(_instruction: &str, _selection: &str) -> String {
    todo!()
}

pub fn check_request(_cfg: &Config, _selection: Option<&str>) -> Result<(), String> {
    todo!()
}

pub fn check_result(_selection: &str, _raw: &str) -> Result<String, String> {
    todo!()
}

pub fn run(_instruction: &str, _selection: Option<&str>, _cfg: &Config) -> Result<String, String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn user_message_carries_instruction_and_text() {
        assert_eq!(
            build_user_message(" make it shorter ", "Hello there, how are you?"),
            "INSTRUCTION: make it shorter\n\nTEXT:\nHello there, how are you?"
        );
    }

    #[test]
    fn request_needs_ai_cleanup_and_a_usable_selection() {
        assert!(check_request(&cfg(), Some("Hello there")).is_ok());
        let off = Config {
            cleanup_enabled: false,
            ..cfg()
        };
        assert!(check_request(&off, Some("Hello"))
            .unwrap_err()
            .contains("Settings"));
        assert!(check_request(&cfg(), None)
            .unwrap_err()
            .contains("Select text first"));
        assert!(check_request(&cfg(), Some("  \n "))
            .unwrap_err()
            .contains("Select text first"));
        let long = "a".repeat(MAX_SELECTION_CHARS + 1);
        assert!(check_request(&cfg(), Some(&long))
            .unwrap_err()
            .contains("too long"));
        assert!(check_request(&cfg(), Some(&long[..MAX_SELECTION_CHARS])).is_ok());
    }

    #[test]
    fn result_passes_through_a_clean_edit() {
        assert_eq!(
            check_result("Hello there, how are you today?", "Hi, how are you?").unwrap(),
            "Hi, how are you?"
        );
    }

    #[test]
    fn result_drops_preamble_quotes_and_fences() {
        assert_eq!(
            check_result("long text here", "Here is the edited text:\nShort text.").unwrap(),
            "Short text."
        );
        assert_eq!(
            check_result("long text here", "\"Short text.\"").unwrap(),
            "Short text."
        );
        assert_eq!(
            check_result("long text here", "```\nShort text.\n```").unwrap(),
            "Short text."
        );
    }

    #[test]
    fn result_keeps_quotes_when_the_selection_was_quoted() {
        assert_eq!(
            check_result("\"Hello there\"", "\"Hi\"").unwrap(),
            "\"Hi\""
        );
    }

    #[test]
    fn result_keeps_a_first_line_that_only_looks_like_a_label() {
        let raw = "Results look good.\nNext steps are below.";
        assert_eq!(
            check_result("The results look good.\nNext steps are below, ok?", raw).unwrap(),
            raw
        );
    }

    #[test]
    fn result_rejects_empty_runaway_and_unchanged() {
        assert!(check_result("hello", "").unwrap_err().contains("empty"));
        assert!(check_result("hello", "``` ```")
            .unwrap_err()
            .contains("empty"));
        assert!(check_result("hello world", &"x".repeat(300))
            .unwrap_err()
            .contains("too long"));
        assert!(check_result("Hello world", " Hello world ")
            .unwrap_err()
            .contains("unchanged"));
    }

    #[test]
    fn run_refuses_before_calling_any_model() {
        // No selection: must return the notice without touching a provider.
        let err = run("make it shorter", None, &cfg()).unwrap_err();
        assert!(err.contains("Select text first"));
    }
}
```

Register the module: in `src/core/mod.rs` add `pub mod edit;` after `pub mod correction;`.

- [ ] **Step 7: Run to verify the edit tests fail**

Run: `cargo test core::edit 2>&1 | tail -20`
Expected: 8 tests FAIL with `not yet implemented`.

- [ ] **Step 8: Implement `src/core/edit.rs`**

Replace the four stub functions with:

```rust
pub fn build_user_message(instruction: &str, selection: &str) -> String {
    format!("INSTRUCTION: {}\n\nTEXT:\n{}", instruction.trim(), selection)
}

/// Checks before any model call. `Err` is the user-facing notice.
pub fn check_request(cfg: &Config, selection: Option<&str>) -> Result<(), String> {
    if !cfg.cleanup_enabled {
        return Err("Editing needs AI cleanup: turn it on in Settings.".into());
    }
    let selection = selection.map(str::trim).unwrap_or_default();
    if selection.is_empty() {
        return Err("Select text first, then hold the key with Shift.".into());
    }
    if selection.chars().count() > MAX_SELECTION_CHARS {
        return Err(format!(
            "The selection is too long to edit (max {MAX_SELECTION_CHARS} characters)."
        ));
    }
    Ok(())
}

/// Drop a short "Here is the edited text:" style first line. Only a line that
/// ends with a colon is dropped, so real text such as "Results look good."
/// is never mistaken for a label.
fn strip_edit_preamble(s: &str) -> String {
    if let Some((first, rest)) = s.split_once('\n') {
        let f = first.trim().to_ascii_lowercase();
        let label = [
            "here is",
            "here's",
            "sure",
            "edited",
            "rewritten",
            "translated",
            "translation",
        ]
        .iter()
        .any(|p| f.starts_with(p));
        if label && f.ends_with(':') && f.len() <= 60 {
            return rest.trim().to_string();
        }
    }
    s.trim().to_string()
}

/// Clean the model's reply and refuse unusable ones. `Err` is the notice.
pub fn check_result(selection: &str, raw: &str) -> Result<String, String> {
    let mut text = strip_edit_preamble(raw.replace("```", "").trim());
    if !selection.trim_start().starts_with(['"', '\'', '“', '‘']) {
        text = crate::core::cleanup::strip_surrounding_quotes(&text);
    }
    if text.is_empty() {
        return Err("The edit came back empty.".into());
    }
    // A rewrite never grows the text many-fold; this catches a runaway answer.
    if text.len() > selection.len().saturating_mul(6).max(200) {
        return Err("The edit came back far too long; your text was left alone.".into());
    }
    if text == selection.trim() {
        return Err("The edit left your text unchanged.".into());
    }
    Ok(text)
}

/// Selection + spoken instruction in, edited text out. `Err` is a notice for
/// the user; the caller leaves their text untouched.
pub fn run(instruction: &str, selection: Option<&str>, cfg: &Config) -> Result<String, String> {
    check_request(cfg, selection)?;
    let selection = selection.unwrap_or_default();
    let raw = crate::core::cleanup::edit_chat(
        EDIT_SYSTEM_PROMPT,
        &build_user_message(instruction, selection),
        cfg,
    )?;
    check_result(selection, &raw)
}
```

- [ ] **Step 9: Run all gates**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)" ; cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no clippy output; `test result: ok. 291 passed; 0 failed; 5 ignored` (281 + 2 provider + 8 edit).

- [ ] **Step 10: Commit**

```bash
git add src/edit_prompt.txt src/core/edit.rs src/core/cleanup.rs src/core/mod.rs
git commit -m "feat: voice-edit core and provider hook"
```

---

### Task 2: Detect Shift during a hold

**Files:**
- Modify: `src/platform/macos/tap.rs`

**Interfaces:**
- Produces (used by Task 4): `crate::platform::macos::tap::take_edit_held() -> bool` (true once per edit hold, clears itself).
- Produces (internal): `shift_with_ptt(flags: u64, watched: u32) -> bool`, `edit_flag_after(prev: bool, ptt_event: Option<bool>, flags: u64, watched: u32) -> bool`.

- [ ] **Step 1: Write the failing tests**

In `src/platform/macos/tap.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn shift_with_ptt_needs_both_keys_down() {
        assert!(shift_with_ptt(OPTION_FLAGS | SHIFT_FLAGS, 61));
        assert!(!shift_with_ptt(OPTION_FLAGS, 61));
        assert!(!shift_with_ptt(SHIFT_FLAGS, 61), "Shift alone is not a hold");
        assert!(shift_with_ptt(FUNCTION_FLAGS | SHIFT_FLAGS, 63));
    }

    #[test]
    fn edit_flag_follows_the_hold() {
        // Shift already down when the PTT key goes down.
        assert!(edit_flag_after(false, Some(true), OPTION_FLAGS | SHIFT_FLAGS, 61));
        // Shift pressed later: a flagsChanged for Shift, not for the PTT key.
        assert!(edit_flag_after(false, None, OPTION_FLAGS | SHIFT_FLAGS, 61));
        // A plain hold, and any release, never set it.
        assert!(!edit_flag_after(false, Some(true), OPTION_FLAGS, 61));
        assert!(!edit_flag_after(false, Some(false), 0, 61));
        // Releasing either key keeps an edit hold an edit hold until taken.
        assert!(edit_flag_after(true, Some(false), SHIFT_FLAGS, 61));
        assert!(edit_flag_after(true, None, OPTION_FLAGS, 61));
        // A new plain hold clears the previous flag.
        assert!(!edit_flag_after(true, Some(true), OPTION_FLAGS, 61));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test shift_with_ptt edit_flag 2>&1 | tail -10`
Expected: compile errors "cannot find function `shift_with_ptt`" / "cannot find value `SHIFT_FLAGS`".

- [ ] **Step 3: Implement**

After `const FUNCTION_FLAGS: u64 = 0x0080_0000;` add:

```rust
/// CGEventFlags maskShift
const SHIFT_FLAGS: u64 = 0x0002_0000;
```

After `modifier_event` add:

```rust
/// True while the PTT key and Shift are both down: the hold is a voice edit.
pub fn shift_with_ptt(flags: u64, watched: u32) -> bool {
    flags & SHIFT_FLAGS != 0 && flags & flags_mask_for_keycode(watched) != 0
}

/// Next value of the edit-hold flag after one flagsChanged event: a new hold
/// (PTT key down) clears it, Shift with the PTT key down sets it, releases
/// leave it for the worker to take.
pub fn edit_flag_after(prev: bool, ptt_event: Option<bool>, flags: u64, watched: u32) -> bool {
    let mut held = prev;
    if ptt_event == Some(true) {
        held = false;
    }
    if shift_with_ptt(flags, watched) {
        held = true;
    }
    held
}

/// Set by the tap when Shift is held during a PTT hold. One hold exists at a
/// time, so a single flag is enough.
static EDIT_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True when the hold that just ended was an edit hold; clears the flag.
pub fn take_edit_held() -> bool {
    EDIT_HELD.swap(false, std::sync::atomic::Ordering::SeqCst)
}
```

In `tap_callback`, in the `if event_type == EVENT_TYPE_FLAGS_CHANGED {` block, replace:

```rust
        let flags = CGEventGetFlags(event);
        if let Some(down) = modifier_event(keycode, flags, ctx.keycode) {
```
with:
```rust
        let flags = CGEventGetFlags(event);
        let ptt = modifier_event(keycode, flags, ctx.keycode);
        // Shift held during the hold marks it as a voice edit.
        EDIT_HELD.store(
            edit_flag_after(
                EDIT_HELD.load(std::sync::atomic::Ordering::SeqCst),
                ptt,
                flags,
                ctx.keycode,
            ),
            std::sync::atomic::Ordering::SeqCst,
        );
        if let Some(down) = ptt {
```

- [ ] **Step 4: Run all gates**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)"; cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no clippy output; `293 passed; 0 failed; 5 ignored`.

- [ ] **Step 5: Commit**

```bash
git add src/platform/macos/tap.rs
git commit -m "feat: flag PTT holds made with Shift as voice edits"
```

---

### Task 3: Read the selected text

**Files:**
- Modify: `src/platform/macos/secure.rs`, `src/platform/macos/mod.rs`
- Create: `src/platform/macos/selection.rs`

**Interfaces:**
- Produces (used by Task 4): `crate::platform::macos::selection::selected_text() -> Option<String>`. Blocks up to about 2 s (waits for the PTT keys to be released, then waits for the copy); call it from a worker thread, never the winit thread. Returns `None` for no selection, a password field, or when nothing could read it.
- Produces (internal): `secure::focused_string_attribute(attribute: &str) -> Option<String>` (`pub(super)`).

- [ ] **Step 1: Write the failing test for long strings**

The existing reader uses a 128-byte buffer, which silently drops any real selection. In `src/platform/macos/secure.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn long_strings_are_read_in_full() {
        // 10,000 bytes of UTF-8: far past the old 128-byte buffer.
        let long = "é".repeat(5000);
        // SAFETY: the CFString is created and released inside this test.
        unsafe {
            let cf = super::cf_string(&long);
            assert_eq!(super::cf_string_to_rust(cf).as_deref(), Some(long.as_str()));
            super::CFRelease(cf);
        }
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test long_strings_are_read 2>&1 | tail -6`
Expected: compile error "cannot find function `cf_string_to_rust`".

- [ ] **Step 3: Implement the reader and the focused-attribute helper in `secure.rs`**

Add to the CoreFoundation extern block:

```rust
    fn CFStringGetLength(s: CFTypeRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
```

Replace `copy_string_attribute` with:

```rust
/// UTF-8 contents of a CFString of any length (None when not a string).
unsafe fn cf_string_to_rust(value: CFTypeRef) -> Option<String> {
    if CFGetTypeID(value) != CFStringGetTypeID() {
        return None;
    }
    let len = CFStringGetLength(value);
    if len == 0 {
        return Some(String::new());
    }
    let max = CFStringGetMaximumSizeForEncoding(len, UTF8);
    if max < 0 {
        return None;
    }
    let cap = max + 1; // room for the NUL
    let mut buf = vec![0u8; cap as usize];
    if CFStringGetCString(value, buf.as_mut_ptr(), cap, UTF8) == 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

unsafe fn copy_string_attribute(element: CFTypeRef, name: &str) -> Option<String> {
    let value = copy_attribute(element, name)?;
    let out = cf_string_to_rust(value);
    CFRelease(value);
    out
}

/// String value of `attribute` on the focused UI element (None when
/// Accessibility cannot tell or the element has no such string).
pub(super) fn focused_string_attribute(attribute: &str) -> Option<String> {
    // SAFETY: every CF object created or copied here is released once.
    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return None;
        }
        // A hung target app must not stall the edit.
        AXUIElementSetMessagingTimeout(system, 0.25);
        let focused = copy_attribute(system, "AXFocusedUIElement");
        CFRelease(system);
        let focused = focused?;
        let value = copy_string_attribute(focused, attribute);
        CFRelease(focused);
        value
    }
}
```

- [ ] **Step 5: Run the test**

Run: `cargo test long_strings_are_read 2>&1 | tail -6`
Expected: PASS.

- [ ] **Step 6: Write the failing test for `selection.rs`**

Create `src/platform/macos/selection.rs` with the tests and a stub:

```rust
//! Read the text selected in the focused app (voice-edit input).
//!
//! Accessibility first (`AXSelectedText`: no keystroke, nothing touched).
//! Many web and Electron text boxes do not expose it, so fall back to
//! copying with Cmd+C and restoring the clipboard.

pub fn selected_text() -> Option<String> {
    todo!()
}

fn usable(_text: String) -> Option<String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_selection_is_not_usable() {
        assert_eq!(usable("  \n ".into()), None);
        assert_eq!(usable(String::new()), None);
        assert_eq!(usable("hi there".into()).as_deref(), Some("hi there"));
    }
}
```

Register it: in `src/platform/macos/mod.rs` add `pub mod selection;` after `pub mod panel;`.

Run: `cargo test blank_selection 2>&1 | tail -6`
Expected: FAIL with `not yet implemented`.

- [ ] **Step 7: Implement `selection.rs`**

Replace the stubs (keep the module doc and tests):

```rust
use arboard::Clipboard;
use std::ffi::c_void;
use std::time::{Duration, Instant};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateKeyboardEvent(source: *const c_void, keycode: u16, down: bool) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn CGEventSourceFlagsState(state_id: i32) -> u64;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// kVK_ANSI_C
const KEY_C: u16 = 8;
/// CGEventFlags maskCommand
const COMMAND_FLAG: u64 = 0x0010_0000;
/// kCGHIDEventTap
const HID_EVENT_TAP: u32 = 0;
/// kCGCombinedState
const COMBINED_STATE: i32 = 0;
/// Shift, Option and Fn: the keys of an edit hold.
const HOLD_KEYS: u64 = 0x0002_0000 | 0x0008_0000 | 0x0080_0000;

/// Selected text of the focused field. `None` when nothing is selected, the
/// field is a password field, or no method could read it. Blocks (waits for
/// the PTT keys to be released and for the copy), so call it from a worker
/// thread.
pub fn selected_text() -> Option<String> {
    if super::secure::password_field_focused() {
        return None;
    }
    if let Some(text) = super::secure::focused_string_attribute("AXSelectedText").and_then(usable)
    {
        return Some(text);
    }
    copy_selection_with_cmd_c().and_then(usable)
}

fn usable(text: String) -> Option<String> {
    (!text.trim().is_empty()).then_some(text)
}

/// Copy the selection with a synthetic Cmd+C, then restore the clipboard.
/// shortcut: a clipboard holding an image is replaced by an empty text one,
/// like `inject_text` does; fix both together if image clipboards matter.
fn copy_selection_with_cmd_c() -> Option<String> {
    // Cmd+C with Shift or Option still down would be a different shortcut
    // (Cmd+Shift+C opens the inspector in browsers).
    wait_for_keys_released(Duration::from_millis(1500));
    let mut cb = Clipboard::new().ok()?;
    let saved = cb.get_text().ok();
    // Empty marks "nothing copied yet", so a selection equal to the old
    // clipboard text is still detected.
    cb.set_text("").ok()?;
    post_cmd_c();
    let deadline = Instant::now() + Duration::from_millis(400);
    let mut copied = None;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        if let Ok(text) = cb.get_text() {
            if !text.is_empty() {
                copied = Some(text);
                break;
            }
        }
    }
    match saved {
        Some(text) => {
            let _ = cb.set_text(text);
        }
        None => {
            let _ = cb.clear();
        }
    }
    copied
}

fn wait_for_keys_released(max: Duration) {
    let deadline = Instant::now() + max;
    while Instant::now() < deadline {
        // SAFETY: plain query, no arguments held.
        if unsafe { CGEventSourceFlagsState(COMBINED_STATE) } & HOLD_KEYS == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn post_cmd_c() {
    // SAFETY: each event is created, posted and released right here.
    unsafe {
        for down in [true, false] {
            let event = CGEventCreateKeyboardEvent(std::ptr::null(), KEY_C, down);
            if event.is_null() {
                return;
            }
            CGEventSetFlags(event, COMMAND_FLAG);
            CGEventPost(HID_EVENT_TAP, event);
            CFRelease(event as *const c_void);
        }
    }
}
```

Add a hardware test inside `mod tests` (ignored; the live check in Task 5 runs it):

```rust
    /// Needs a front app with text selected. Run: cargo test cmd_c_fallback -- --ignored --nocapture
    #[test]
    #[ignore]
    fn cmd_c_fallback_reads_the_selection() {
        let text = copy_selection_with_cmd_c();
        eprintln!("COPIED {text:?}");
        assert!(text.is_some_and(|t| !t.trim().is_empty()));
    }
```

- [ ] **Step 8: Run all gates**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)"; cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no clippy output (a `clashing_extern_declarations` warning would mean a signature differs from `tap.rs` or `secure.rs`; fix the signature, do not allow the lint); `295 passed; 0 failed; 6 ignored`.

- [ ] **Step 9: Commit**

```bash
git add src/platform/macos/secure.rs src/platform/macos/selection.rs src/platform/macos/mod.rs
git commit -m "feat: read the selected text via Accessibility with a Cmd+C fallback"
```

---

### Task 4: Wire edit mode into the worker

**Files:**
- Modify: `src/daemon.rs`, `src/app/orchestrator.rs`, `src/app/mod.rs`

**Interfaces:**
- Consumes: `tap::take_edit_held()` (Task 2), `selection::selected_text()` (Task 3), `edit::run(...)` (Task 1).
- Produces: `DaemonEvent::Notice(String)`.

- [ ] **Step 1: Write the failing orchestrator test**

In `src/app/orchestrator.rs` `mod tests`, next to `tap_issue_notifies`, add:

```rust
    #[test]
    fn notice_changes_nothing() {
        let mut o = new_orchestrator();
        assert!(o
            .handle(&DaemonEvent::Notice("Select text first".into()))
            .is_empty());
        assert_eq!(o.phase(), Phase::Idle);
    }
```

Run: `cargo test notice_changes_nothing 2>&1 | tail -6`
Expected: compile error "no variant named `Notice`".

- [ ] **Step 2: Add the event**

`src/daemon.rs`, in `enum DaemonEvent`, after `OpenWindow,`:

```rust
    /// User-facing message from the worker (voice-edit could not run): shown
    /// as a native notification; the session state is untouched.
    Notice(String),
```

`src/app/orchestrator.rs`, in `handle`, after the `OpenWindow` arm:

```rust
            DaemonEvent::Notice(_) => Vec::new(),
```

`src/app/mod.rs`, in `daemon_event_variant_name`, after the `OpenWindow` arm:

```rust
        DaemonEvent::Notice(_) => "Notice",
```

and in the notification `match &event` block (next to the `TapIssue` arm) add:

```rust
            DaemonEvent::Notice(msg) => match crate::ui::notify::notify("wiflow", msg) {
                Ok(()) => tracing::info!("notification posted (notice)"),
                Err(e) => tracing::info!("notification failed ({e}) — tray note stands"),
            },
```

- [ ] **Step 3: Run the orchestrator test**

Run: `cargo test notice_changes_nothing 2>&1 | tail -6`
Expected: PASS.

- [ ] **Step 4: Branch the pipeline on edit mode in `src/daemon.rs`**

In `pipeline_on_worker`:

(a) After `let out = capture.stop();` add:

```rust
    // Taken at release: true when the hold was made with Shift (a voice edit).
    let edit_mode = crate::platform::macos::tap::take_edit_held();
```

(b) Immediately before the line `let cfg = crate::core::config::load_config();` that follows the `kept.is_empty()` block, add:

```rust
    // Voice-edit hold: read the selection while the instruction is transcribed.
    let selection = edit_mode
        .then(|| std::thread::spawn(crate::platform::macos::selection::selected_text));
```

(c) Immediately before the comment `// Cleanup via trait (route decision + context gate + chain).` add:

```rust
    if edit_mode {
        finish_edit(selection, &text, &cfg, duration_ms, rtf, proxy);
        return;
    }
```

(d) Add before `trait DaemonEventSender`:

```rust
/// Voice-edit tail of the pipeline: spoken instruction + selection in, edited
/// text out. Every failure leaves the user's text alone: a notice goes out and
/// the session ends with an empty `Done`.
fn finish_edit(
    selection: Option<std::thread::JoinHandle<Option<String>>>,
    instruction: &str,
    cfg: &crate::core::config::Config,
    duration_ms: u64,
    rtf: f64,
    proxy: &EventLoopProxy<DaemonEvent>,
) {
    let selected = selection.and_then(|h| h.join().ok().flatten());
    let end = |text: String| {
        let _ = proxy.send_event(DaemonEvent::Done {
            text,
            duration_ms,
            rtf,
        });
    };
    if instruction.trim().is_empty() {
        tracing::info!("edit hold: no instruction heard");
        return end(String::new());
    }
    let t0 = Instant::now();
    match crate::core::edit::run(instruction, selected.as_deref(), cfg) {
        Ok(text) => {
            tracing::info!(
                "edit done in {}ms ({} -> {} chars)",
                t0.elapsed().as_millis(),
                selected.as_deref().map_or(0, |s| s.chars().count()),
                text.chars().count()
            );
            end(text)
        }
        Err(notice) => {
            tracing::warn!("edit not applied: {notice}");
            let _ = proxy.send_event(DaemonEvent::Notice(notice));
            end(String::new())
        }
    }
}
```

- [ ] **Step 5: Run all gates**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)"; cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no clippy output (if clippy flags the `return end(...)` style, use `end(String::new()); return;`); `296 passed; 0 failed; 6 ignored`.

- [ ] **Step 6: Commit**

```bash
git add src/daemon.rs src/app/orchestrator.rs src/app/mod.rs
git commit -m "feat: run a voice edit when the PTT hold is made with Shift"
```

---

### Task 5: Live check, docs, spec update

**Files:**
- Modify: `README.md`, `memory.md`, `docs/superpowers/specs/2026-11-14-voice-edit-selection-design.md`

This task needs real macOS permissions (Input Monitoring + Accessibility for the app you run, BlackHole 2ch installed, Ollama running with `llama3.2:3b`). Do not run it while the user is typing; it takes over the keyboard focus. Back up and restore `~/Library/Application Support/wiflow/config.json`.

- [ ] **Step 1: Build and prepare tools**

```bash
cargo build --release
mkdir -p "$TMPDIR/vedit" && cd "$TMPDIR/vedit"
cat > ptt_edit.swift <<'EOF'
import CoreGraphics
import Foundation
// usage: ptt_edit <hold_ms> <shift 0|1>  — synthetic Right Option down, wait, up
let hold = Double(CommandLine.arguments[1])! / 1000
let shift = CommandLine.arguments[2] == "1"
let src = CGEventSource(stateID: .hidSystemState)
var down: CGEventFlags = [.maskAlternate, CGEventFlags(rawValue: 0x40)]
if shift { down.insert(.maskShift) }
let d = CGEvent(keyboardEventSource: src, virtualKey: 61, keyDown: true)!
d.type = .flagsChanged; d.flags = down; d.post(tap: .cghidEventTap)
Thread.sleep(forTimeInterval: hold)
let u = CGEvent(keyboardEventSource: src, virtualKey: 61, keyDown: false)!
u.type = .flagsChanged; u.flags = []; u.post(tap: .cghidEventTap)
EOF
swiftc -O ptt_edit.swift -o ptt_edit
say -a '?' | grep -i blackhole      # note the numeric id, call it BH
cp "$HOME/Library/Application Support/wiflow/config.json" config.backup.json
python3 - <<'EOF'
import json,os
p=os.path.expanduser("~/Library/Application Support/wiflow/config.json")
c=json.load(open(p)); c["mic_name"]="BlackHole 2ch"; json.dump(c,open(p,"w"),indent=2)
EOF
```

Quit any running Wiflow, then start the build under test: `target/release/wiflow-dictation --app &`.

- [ ] **Step 2: Edit in TextEdit (Accessibility path)**

```bash
osascript <<'EOF'
tell application "TextEdit"
  activate
  make new document
  set text of document 1 to "I was wondering if you could possibly let me know whether the meeting tomorrow is still going ahead as planned."
end tell
delay 0.5
tell application "System Events" to keystroke "a" using command down
EOF
./ptt_edit 3500 1 & sleep 0.8; say -a "$BH" "make it shorter"; wait; sleep 8
osascript -e 'tell application "TextEdit" to get text of document 1'
```
Expected: a shorter sentence than the original, with no quotes or "Here is" wrapper. Log (`~/Library/Logs/wiflow/wiflow.log`) shows `edit done in …ms (N -> M chars)`.

Then undo:
```bash
osascript -e 'tell application "System Events" to keystroke "z" using command down'; sleep 0.5
osascript -e 'tell application "TextEdit" to get text of document 1'
```
Expected: the original sentence is back.

- [ ] **Step 3: Check the Cmd+C fallback**

With the TextEdit text still selected (`keystroke "a" using command down` again):
```bash
cd - && cargo test cmd_c_fallback -- --ignored --nocapture 2>&1 | grep -E "COPIED|test result"
```
Expected: `COPIED Some("I was wondering …")` and the clipboard text you had before is unchanged (`pbpaste`).

- [ ] **Step 4: Check the failure paths**

- Nothing selected: click into TextEdit so the cursor sits with no selection, run `./ptt_edit 3000 1 & sleep 0.8; say -a "$BH" "make it shorter"; wait`. Expected: a native notification "Select text first, then hold the key with Shift." and the text unchanged.
- Plain dictation unchanged: `./ptt_edit 3000 0 & sleep 0.8; say -a "$BH" "hello world"; wait`. Expected: "Hello world." is pasted, no edit log lines.
- Stuck-mic regression: `cargo test named_mic -- --ignored` still passes (needs the mic to be free; run it after restoring the config).

- [ ] **Step 5: Restore and clean up**

```bash
pkill -f "target/release/wiflow-dictation"
cp "$TMPDIR/vedit/config.backup.json" "$HOME/Library/Application Support/wiflow/config.json"
osascript -e 'tell application "TextEdit" to close every document saving no'
```

- [ ] **Step 6: Docs**

`README.md`: after the paragraph that ends with "Use `--app` to start in the menu bar only", add:

```markdown
**Edit selected text by voice:** select text in any app, hold the push-to-talk
key (Right Option or Fn) together with Shift and say what to change: "make it
shorter", "fix the grammar", "translate to Spanish". The selection is replaced;
Cmd+Z undoes it. It uses your AI cleanup provider (Ollama by default), so AI
cleanup must be on. Selections up to about 250 words. In editors that copy the
whole line when nothing is selected (VS Code), select the text first.
```

`memory.md`: append a decision entry in the file's existing format:

```markdown
- 2026-11-14 — Voice-edit of selected text: hold PTT key + Shift, speak an instruction, selection replaced via the cleanup provider (no fallback chain, 8 s budget, edits not saved to history). Why: usage is short chat dictations; rewriting existing text saves the most steps. Ctrl+Space preset excluded (not on the event tap).
```

Spec updates in `docs/superpowers/specs/2026-11-14-voice-edit-selection-design.md`:
- Flow step 5 and the Components row for `core/cleanup.rs`: replace "calls the configured provider chain" with "calls the configured provider (auto: first with a key, else Ollama); there is no fallback chain, so a failed edit leaves the text alone".
- Testing section: replace the "Provider call: `edit_text` against a localhost fake model" bullet with "Provider choice: pure `edit_provider` test; the HTTP path is covered by the live check (no fake-model infrastructure exists, and cargo test must not call real models)".
- Failure table: add the row "Selection longer than 1500 characters: notice, no model call".
- Components table: add "`DaemonEvent::Notice`: user-facing worker message shown as a native notification, no state change".
- Risks: replace "Edited text appears in history like any dictation" with "Edits are not saved to history"; add "Editors that copy the whole line when nothing is selected (VS Code) will edit and paste that line at the cursor; Cmd+Z undoes it."

- [ ] **Step 7: Final gates and commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1 | grep -E "test result|FAILED"
git add README.md memory.md docs/superpowers/specs/2026-11-14-voice-edit-selection-design.md
git commit -m "docs: voice-edit usage, decision, and spec updates"
```
Expected: gates clean; `296 passed; 0 failed; 6 ignored`.

---

## Self-Review

**Spec coverage**

| Spec requirement | Task |
|---|---|
| Trigger: PTT key + Shift, bare-modifier presets only | 2 (flag), Global Constraints (Ctrl+Space excluded) |
| Instruction transcribed with light processing, no cleanup chain | 4 (edit branch returns before the cleanup route) |
| Selection read in parallel with STT, Accessibility then Cmd+C, password fields refused | 3, 4(b) |
| Open-ended instruction via the cleanup provider, 8 s budget | 1 |
| Result checks (empty, runaway, preamble, quotes, unchanged) | 1 |
| Paste over selection through the existing inject; Cmd+Z undoes | 4 (`Done { text }`), 5 step 2 |
| Failure table (no selection, cleanup off, provider error, empty instruction, rejected result, too long) | 1 (`check_request`, `check_result`), 4 (`finish_edit`, `Notice`) |
| Password field | 3 (`selected_text` returns `None`), plus the existing inject guard |
| Testing: unit, live, no stuck-mic regression | 1 to 3 tests, 5 steps 2 to 4 |
| Docs | 5 |

Deviations from the spec are listed in the header and written back to the spec in Task 5 step 6.

**Placeholder scan:** none; every code step has full code. `todo!()` appears only in the red-phase stubs that the next step replaces.

**Type consistency:** `edit::run(&str, Option<&str>, &Config) -> Result<String, String>` (Task 1) is called with exactly that shape in `finish_edit` (Task 4). `selection::selected_text() -> Option<String>` (Task 3) matches `JoinHandle<Option<String>>` in Task 4. `take_edit_held() -> bool` (Task 2) is used as `bool` in Task 4. `DaemonEvent::Notice(String)` is matched in the orchestrator, `daemon_event_variant_name` and the notification block. Test counts: 281 baseline, +10 (Task 1), +2 (Task 2), +2 (Task 3), +1 (Task 4) = 296 passed; ignored 5 → 6 (Task 3's hardware test).
