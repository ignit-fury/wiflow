# Key-limit fallback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When Groq or OpenRouter answers HTTP 429/402, skip that provider until its limit resets and use the fallback the user chose (local Ollama or the auto chain), for dictation cleanup and voice-edit.

**Architecture:** All provider choice moves into two small pure functions in `src/core/cleanup.rs`: `route` (which providers to try, in order) and `walk` (try them, record limits, write today's issue texts). An in-memory `Skips` store remembers each limited provider's "skip until" instant and one pending notice, which the daemon sends as a banner. A new config field picks the fallback.

**Tech Stack:** Rust 2021, ureq 2.12 (HTTP), egui (Settings), serde (config). macOS only.

Spec: `docs/superpowers/specs/2026-11-25-key-limit-fallback-design.md`.

## Global Constraints

- Only HTTP 429 and HTTP 402 start a skip period. HTTP 401 never does.
- Skip time: the `Retry-After` header in whole seconds; 60 s when missing or not a number; never more than 3600 s.
- Ollama is never skipped and is always the last provider tried.
- `cleanup_limit_fallback` values: `"ollama"` (default) and `"auto"`. Any other value behaves as `"ollama"`.
- Settings label: "When an API key runs out of tokens:" with radios "Local Ollama" (`"ollama"`) and "Auto chain" (`"auto"`).
- Notice text: `"{Provider} is out of tokens; using {Fallback} for about {N} min."` with N = seconds rounded up to whole minutes, at least 1.
- With no skip active, provider order and issue texts stay exactly as today.
- Skip state lives in memory only. Keys are never logged.
- Before every commit: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` all pass.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Work on branch `tune-and-live-pill`. Do not push.

## File map

| File | Change |
|---|---|
| `src/core/cleanup.rs` | `status_is`, `status_error`, `skip_for`, `Skips` store, `take_limit_notice`, `route`, `walk`; `clean_chain` and `edit_chat` use them; `chain_step`, `full_chain`, `single_provider_chain`, `edit_provider` removed |
| `src/core/config.rs` | `cleanup_limit_fallback` field |
| `src/ui/settings.rs` | Radio pair |
| `src/daemon.rs` | Send the pending notice after cleanup and after an edit |
| `README.md`, `docs/corrections.md`, `memory.md`, the spec | Docs |

---

### Task 1: Limit detection and the skip store

**Files:**
- Modify: `src/core/cleanup.rs` (helpers near `is_quota_error`, about line 670; `chat_completion` error mapping, about line 720; tests module)

**Interfaces:**
- Produces (all private to `cleanup.rs` except `take_limit_notice`):
  - `fn status_is(err: &str, code: u16) -> bool`
  - `fn status_error(code: u16, retry_after: Option<&str>) -> String`
  - `fn skip_for(err: &str) -> Option<Duration>`
  - `struct Skips` with `fn is_skipped(&self, p: Provider, now: Instant) -> bool` and `fn start(&mut self, p: Provider, d: Duration, now: Instant, next: Provider)`; field `notice: Option<String>`
  - `fn skips() -> std::sync::MutexGuard<'static, Skips>`
  - `pub fn take_limit_notice() -> Option<String>`

- [ ] **Step 1: Write the failing tests** (add inside `mod tests` in `src/core/cleanup.rs`)

```rust
    #[test]
    fn limit_errors_set_a_skip_time() {
        assert_eq!(skip_for("HTTP 429 (retry after 12s)"), Some(Duration::from_secs(12)));
        assert_eq!(skip_for("HTTP 429"), Some(Duration::from_secs(60)));
        assert_eq!(skip_for("HTTP 402"), Some(Duration::from_secs(60)));
        assert_eq!(
            skip_for("HTTP 429 (retry after 99999s)"),
            Some(Duration::from_secs(3600))
        );
        for e in ["HTTP 401", "HTTP 500", "timeout", "model not installed"] {
            assert_eq!(skip_for(e), None, "{e}");
        }
    }

    #[test]
    fn status_errors_keep_retry_after_seconds_only() {
        assert_eq!(status_error(429, Some(" 12 ")), "HTTP 429 (retry after 12s)");
        assert_eq!(
            status_error(429, Some("Wed, 21 Oct 2026 07:28:00 GMT")),
            "HTTP 429"
        );
        assert_eq!(status_error(500, None), "HTTP 500");
        // A wait of 401 s is a limit, not a rejected key.
        let e = status_error(429, Some("401"));
        assert!(!status_is(&e, 401) && status_is(&e, 429), "{e}");
        assert!(!Provider::Groq.quota_issue(&e, Provider::Ollama).contains("API key"));
    }

    #[test]
    fn skip_period_ends_and_names_the_fallback() {
        let now = Instant::now();
        let mut s = Skips::default();
        s.start(Provider::Groq, Duration::from_secs(12), now, Provider::Ollama);
        assert!(s.is_skipped(Provider::Groq, now + Duration::from_secs(11)));
        assert!(!s.is_skipped(Provider::Groq, now + Duration::from_secs(12)));
        assert!(!s.is_skipped(Provider::OpenRouter, now));
        assert_eq!(
            s.notice.take().as_deref(),
            Some("Groq is out of tokens; using Ollama for about 1 min.")
        );
        s.start(Provider::OpenRouter, Duration::from_secs(150), now, Provider::Groq);
        assert_eq!(
            s.notice.as_deref(),
            Some("OpenRouter is out of tokens; using Groq for about 3 min.")
        );
        s.notice = None;
        s.start(Provider::Ollama, Duration::from_secs(60), now, Provider::Ollama);
        assert!(!s.is_skipped(Provider::Ollama, now));
        assert_eq!(s.notice, None);
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test skip 2>&1 | grep -E "^error" | head -5`
Expected: `error[E0425]: cannot find function 'skip_for'` and `cannot find type 'Skips'`.

- [ ] **Step 3: Implement**

At the top of `src/core/cleanup.rs`, change the time import to:

```rust
use std::time::{Duration, Instant};
```

Replace `is_quota_error` and `key_hint` with versions built on `status_is`, and add the new helpers next to them:

```rust
/// True when `err` carries this HTTP status ("HTTP 429 …"). Matching the
/// prefix keeps a Retry-After of 401 s from reading as a rejected key.
fn status_is(err: &str, code: u16) -> bool {
    err.contains(&format!("HTTP {code}"))
}

/// 429 (rate limit), 402 (payment/credits), or 401 (quota/auth exhausted).
pub fn is_quota_error(err: &str) -> bool {
    status_is(err, 429) || status_is(err, 402) || status_is(err, 401)
}

/// Error text for an HTTP status. A numeric Retry-After is kept so the
/// skip period can use it; a date form is ignored (60 s default applies).
fn status_error(code: u16, retry_after: Option<&str>) -> String {
    match retry_after.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(s) => format!("HTTP {code} (retry after {s}s)"),
        None => format!("HTTP {code}"),
    }
}

const DEFAULT_SKIP: Duration = Duration::from_secs(60);
const MAX_SKIP: Duration = Duration::from_secs(3600);

/// How long to skip a provider after this error: only token or credit
/// limits (429/402) count; a rejected key (401) does not fix itself in time.
fn skip_for(err: &str) -> Option<Duration> {
    if !(status_is(err, 429) || status_is(err, 402)) {
        return None;
    }
    let secs = err
        .split_once("(retry after ")
        .and_then(|(_, rest)| rest.split_once("s)"))
        .and_then(|(n, _)| n.parse::<u64>().ok());
    Some(secs.map_or(DEFAULT_SKIP, Duration::from_secs).min(MAX_SKIP))
}

/// Cloud providers skipped after a token limit, until the limit resets,
/// plus the one notice to show when a skip period starts. Memory only.
#[derive(Debug, Default)]
struct Skips {
    until: Vec<(Provider, Instant)>,
    notice: Option<String>,
}

impl Skips {
    fn is_skipped(&self, p: Provider, now: Instant) -> bool {
        self.until.iter().any(|&(q, t)| q == p && now < t)
    }

    /// Skip `p` for `d` from `now`; `next` is what is used meanwhile.
    fn start(&mut self, p: Provider, d: Duration, now: Instant, next: Provider) {
        if p == Provider::Ollama {
            return;
        }
        self.until.retain(|&(q, _)| q != p);
        self.until.push((p, now + d));
        self.notice = Some(format!(
            "{} is out of tokens; using {} for about {} min.",
            p.name(),
            next.name(),
            d.as_secs().div_ceil(60).max(1)
        ));
    }
}

static SKIPS: std::sync::Mutex<Skips> = std::sync::Mutex::new(Skips {
    until: Vec::new(),
    notice: None,
});

fn skips() -> std::sync::MutexGuard<'static, Skips> {
    SKIPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The notice from a skip period that just started, once.
pub fn take_limit_notice() -> Option<String> {
    skips().notice.take()
}

/// Tells the user what to fix when a provider rejects the API key.
fn key_hint(err: &str) -> &'static str {
    if status_is(err, 401) {
        " (API key rejected; update it in AI Keys & Models in the menu bar)"
    } else {
        ""
    }
}
```

In `Provider::quota_issue`, change the first match arm guard from `err.contains("401")` to `status_is(err, 401)`.

In `chat_completion`, replace the error mapping:

```rust
        .map_err(|e| match e {
            ureq::Error::Status(code, resp) => status_error(code, resp.header("retry-after")),
            other => format!("{other}"),
        })?;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test skip && cargo test status_errors && cargo test quota`
Expected: all pass (including the existing `test_is_quota_error` and `rejected_key_is_not_reported_as_rate_limit`).

This is a binary crate, so items used only by tests fail clippy with `dead_code`. Put `#[cfg_attr(not(test), allow(dead_code))] // used from Task 3` above `fn skip_for`, `struct Skips`, `impl Skips`, `static SKIPS` and `fn skips`, and `#[cfg_attr(not(test), allow(dead_code))] // used from Task 4` above `pub fn take_limit_notice`. Task 3 removes the first five, Task 4 the last.

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
git add src/core/cleanup.rs
git commit -m "feat: remember when an API key hits its token limit

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The fallback setting

**Files:**
- Modify: `src/core/config.rs` (struct after `cleanup_provider`, about line 43; `Default` impl about line 108; `sample()` in tests about line 328)
- Modify: `src/ui/settings.rs` (after the "Cleanup with:" row, about line 703)

**Interfaces:**
- Produces: `Config::cleanup_limit_fallback: String` (`"ollama"` default)

- [ ] **Step 1: Write the failing test** (in `mod tests` of `src/core/config.rs`)

```rust
    #[test]
    fn old_config_falls_back_to_ollama_on_key_limits() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.cleanup_limit_fallback, "ollama");
        assert_eq!(Config::default().cleanup_limit_fallback, "ollama");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test old_config_falls_back 2>&1 | grep -E "^error" | head -3`
Expected: `error[E0609]: no field 'cleanup_limit_fallback'`.

- [ ] **Step 3: Implement**

In `Config`, after `cleanup_provider`:

```rust
    /// Used while a cloud key is skipped after a token limit (HTTP 429/402):
    /// "ollama" (default) or "auto" (the other cloud provider with a key,
    /// then Ollama). Any other value behaves as "ollama".
    #[serde(default = "default_limit_fallback")]
    pub cleanup_limit_fallback: String,
```

Next to the other `default_*` functions:

```rust
fn default_limit_fallback() -> String {
    "ollama".into()
}
```

In `impl Default for Config`, after `cleanup_provider: "auto".into(),`:

```rust
            cleanup_limit_fallback: "ollama".into(),
```

In the tests' `sample()`, after `cleanup_provider: "auto".into(),`:

```rust
            cleanup_limit_fallback: "auto".into(),
```

In `src/ui/settings.rs`, inside `ui.add_enabled_ui(vm.config.cleanup_enabled, |ui| {`, right after the `ui.horizontal` block that ends the "Cleanup with:" radios:

```rust
            ui.horizontal(|ui| {
                ui.label("When an API key runs out of tokens:");
                for (value, name) in [("ollama", "Local Ollama"), ("auto", "Auto chain")] {
                    ui.radio_value(
                        &mut vm.config.cleanup_limit_fallback,
                        value.to_string(),
                        name,
                    );
                }
            });
```

- [ ] **Step 4: Run the tests**

Run: `cargo test config`
Expected: PASS, including `test_config_roundtrip`.

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
git add src/core/config.rs src/ui/settings.rs
git commit -m "feat: setting for what to use when an API key runs out of tokens

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Route cleanup and voice-edit around skipped providers

**Files:**
- Modify: `src/core/cleanup.rs`: `clean_chain` (about line 490), remove `chain_step`, `full_chain`, `single_provider_chain`, `edit_provider` (about lines 524–633), rewrite `edit_chat` (about line 637); tests module (remove `edit_provider_follows_config_then_keys`)

**Interfaces:**
- Consumes: `skip_for`, `skips()`, `Skips::start`, `Skips::is_skipped` (Task 1); `Config::cleanup_limit_fallback` (Task 2); existing `try_provider_with_retry`, `try_provider_model`, `provider_model`, `Provider::quota_issue`, `is_quota_error`, `key_hint`, `CleanupBudget::for_edit`
- Produces:
  - `fn route(configured: &str, fallback: &str, has_key: impl Fn(Provider) -> bool, skipped: impl Fn(Provider) -> bool) -> (Vec<Provider>, bool)`
  - `fn walk(order: &[Provider], strict: bool, issues: &mut Vec<String>, try_one: impl FnMut(Provider) -> Result<String, String>, on_limit: impl FnMut(Provider, Duration, Provider)) -> Result<String, (Provider, String)>`
  - `clean_chain` and `edit_chat` keep their public signatures.

- [ ] **Step 1: Write the failing tests** (in `mod tests`; delete `edit_provider_follows_config_then_keys`, whose cases are covered here)

```rust
    use Provider::{Groq, Ollama, OpenRouter};

    #[test]
    fn route_without_limits_matches_todays_order() {
        let keys = |_: Provider| true;
        let no_keys = |_: Provider| false;
        let free = |_: Provider| false;
        assert_eq!(route("auto", "ollama", keys, free), (vec![Groq, OpenRouter, Ollama], false));
        assert_eq!(route("", "auto", no_keys, free), (vec![Ollama], false));
        assert_eq!(route("groq", "ollama", keys, free), (vec![Groq, Ollama], true));
        assert_eq!(route("ollama", "auto", keys, free), (vec![Ollama], true));
        assert_eq!(
            route("openrouter", "auto", keys, free),
            (vec![OpenRouter, Groq, Ollama], true)
        );
        assert_eq!(route("groq", "bogus", keys, free), (vec![Groq, Ollama], true));
    }

    #[test]
    fn route_leaves_out_skipped_providers() {
        let keys = |_: Provider| true;
        let groq_limited = |p: Provider| p == Groq;
        assert_eq!(route("groq", "ollama", keys, groq_limited), (vec![Ollama], false));
        assert_eq!(route("groq", "auto", keys, groq_limited), (vec![OpenRouter, Ollama], false));
        assert_eq!(route("auto", "ollama", keys, groq_limited), (vec![OpenRouter, Ollama], false));
        assert_eq!(
            route("groq", "auto", |p| p != OpenRouter, groq_limited),
            (vec![Ollama], false)
        );
    }

    #[test]
    fn walk_records_limits_and_keeps_issue_texts() {
        let mut issues = Vec::new();
        let mut limits = Vec::new();
        let out = walk(
            &[Groq, Ollama],
            false,
            &mut issues,
            |p| match p {
                Groq => Err("HTTP 429 (retry after 12s)".into()),
                _ => Ok("clean".into()),
            },
            |p, d, next| limits.push((p, d, next)),
        );
        assert_eq!(out, Ok("clean".to_string()));
        assert_eq!(limits, [(Groq, Duration::from_secs(12), Ollama)]);
        assert_eq!(issues, ["Groq tokens/rate limit exhausted — falling back to Ollama"]);
    }

    #[test]
    fn walk_explicit_provider_falls_back_only_on_key_or_limit_errors() {
        let mut tried = Vec::new();
        let mut issues = Vec::new();
        let out = walk(
            &[Groq, Ollama],
            true,
            &mut issues,
            |p| {
                tried.push(p);
                Err("HTTP 500".into())
            },
            |_, _, _| panic!("500 is not a limit"),
        );
        assert_eq!(out, Err((Groq, "HTTP 500".to_string())));
        assert_eq!(tried, [Groq]);
        assert_eq!(issues, ["Groq failed: HTTP 500"]);

        let mut issues = Vec::new();
        let out = walk(
            &[Groq, Ollama],
            true,
            &mut issues,
            |p| match p {
                Groq => Err("HTTP 401".into()),
                _ => Err("down".into()),
            },
            |_, _, _| panic!("401 starts no skip"),
        );
        assert_eq!(out, Err((Ollama, "down".to_string())));
        assert_eq!(issues.len(), 2);
        assert!(issues[0].contains("API key"), "{issues:?}");
        assert_eq!(issues[1], "Ollama fallback failed: down");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test route_ 2>&1 | grep -E "^error" | head -3`
Expected: `error[E0425]: cannot find function 'route'`.

- [ ] **Step 3: Implement**

Add above `clean_chain`:

```rust
fn has_key(p: Provider) -> bool {
    match p {
        Provider::Groq => groq_key().is_some(),
        Provider::OpenRouter => openrouter_key().is_some(),
        Provider::Ollama => true,
    }
}

/// Providers to try, in order, for one cleanup or edit. `configured` is
/// `cleanup_provider`, `fallback` is `cleanup_limit_fallback`. Skipped
/// providers are left out; Ollama is never skipped and always last. The
/// bool is true when the first entry is the explicitly chosen provider:
/// its non-key, non-limit errors end the call (today's rule).
fn route(
    configured: &str,
    fallback: &str,
    has_key: impl Fn(Provider) -> bool,
    skipped: impl Fn(Provider) -> bool,
) -> (Vec<Provider>, bool) {
    let auto = |leave_out: Option<Provider>| -> Vec<Provider> {
        [Provider::Groq, Provider::OpenRouter]
            .into_iter()
            .filter(|&p| Some(p) != leave_out && has_key(p) && !skipped(p))
            .chain([Provider::Ollama])
            .collect()
    };
    let chosen = match configured {
        "groq" => Provider::Groq,
        "openrouter" => Provider::OpenRouter,
        "ollama" => return (vec![Provider::Ollama], true),
        _ => return (auto(None), false),
    };
    let rest = if fallback == "auto" {
        auto(Some(chosen))
    } else {
        vec![Provider::Ollama]
    };
    if skipped(chosen) {
        (rest, false)
    } else {
        (std::iter::once(chosen).chain(rest).collect(), true)
    }
}

/// Try `order` until one provider succeeds. A token limit (429/402) calls
/// `on_limit(provider, skip time, next provider)`. With `strict`, the first
/// provider falls back only on key or limit errors. Issue texts match the
/// old chain. Err carries the last provider tried and its error.
fn walk(
    order: &[Provider],
    strict: bool,
    issues: &mut Vec<String>,
    mut try_one: impl FnMut(Provider) -> Result<String, String>,
    mut on_limit: impl FnMut(Provider, Duration, Provider),
) -> Result<String, (Provider, String)> {
    let mut last = (Provider::Ollama, String::from("no provider to try"));
    for (i, &p) in order.iter().enumerate() {
        let e = match try_one(p) {
            Ok(text) => return Ok(text),
            Err(e) => e,
        };
        tracing::warn!("{} cleanup failed: {e}", p.name().to_lowercase());
        let first_strict = strict && i == 0;
        let next = order
            .get(i + 1)
            .copied()
            .filter(|_| !first_strict || is_quota_error(&e));
        if let Some(d) = skip_for(&e) {
            on_limit(p, d, next.unwrap_or(Provider::Ollama));
        }
        match next {
            Some(n) if is_quota_error(&e) => issues.push(p.quota_issue(&e, n)),
            Some(n) => issues.push(format!(
                "{} failed ({e}) — falling back to {}",
                p.name(),
                n.name()
            )),
            None if first_strict => issues.push(format!("{} failed: {e}", p.name())),
            None => issues.push(format!("{} fallback failed: {e}", p.name())),
        }
        last = (p, e);
        if next.is_none() {
            break;
        }
    }
    Err(last)
}

fn start_skip(p: Provider, d: Duration, next: Provider) {
    skips().start(p, d, Instant::now(), next);
}

fn route_for(cfg: &crate::core::config::Config) -> (Vec<Provider>, bool) {
    let now = Instant::now();
    route(
        &cfg.cleanup_provider,
        &cfg.cleanup_limit_fallback,
        has_key,
        |p| skips().is_skipped(p, now),
    )
}
```

Replace the body of `clean_chain` from `let provider = …` to the end with:

```rust
    let (order, strict) = route_for(cfg);
    let text = walk(
        &order,
        strict,
        &mut issues,
        |p| try_provider_with_retry(p, cfg, transcript, &budget),
        start_skip,
    )
    .ok();
    CleanupOutcome {
        text: text.unwrap_or_else(|| transcript.to_string()),
        issues,
    }
}
```

Delete `chain_step`, `full_chain`, `single_provider_chain` and `edit_provider` with their doc comments.

Replace `edit_chat` with:

```rust
/// One edit, routed like dictation cleanup (skipped keys left out, the
/// fallback setting applies). A failed edit leaves the user's text alone
/// and says why.
pub fn edit_chat(
    system: &str,
    user: &str,
    cfg: &crate::core::config::Config,
) -> Result<String, String> {
    let budget = CleanupBudget::for_edit();
    let (order, strict) = route_for(cfg);
    walk(
        &order,
        strict,
        &mut Vec::new(),
        |p| try_provider_model(p, provider_model(p, cfg), user, Some(system), &budget),
        start_skip,
    )
    .map_err(|(p, e)| format!("{} edit failed: {e}{}", p.name(), key_hint(&e)))
}
```

Remove the five `// used from Task 3` allow lines added in Task 1 (keep the one on `take_limit_notice`).

- [ ] **Step 4: Run the tests**

Run: `cargo test route_ && cargo test walk_ && cargo test`
Expected: all pass. If `cargo test` fails, the old chain had an order or message this plan missed: fix `route`/`walk`, not the old test.

- [ ] **Step 5: Gates and commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
git add src/core/cleanup.rs
git commit -m "feat: skip a limited API key and use the chosen fallback

Dictation cleanup and voice-edit now share one route: a provider that
answered 429/402 is left out until its limit resets, and the fallback
setting decides between Ollama and the auto chain. Voice-edit falls back
instead of failing. With no limit active, order and messages are as
before.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Banner, live check and docs

**Files:**
- Modify: `src/daemon.rs` (after the cleanup issues loop, about line 390; voice-edit `match crate::core::edit::run(...)`, about line 455)
- Modify: `src/core/cleanup.rs` (one ignored test)
- Modify: `README.md`, `docs/corrections.md`, `memory.md`, `docs/superpowers/specs/2026-11-25-key-limit-fallback-design.md`

**Interfaces:**
- Consumes: `crate::core::cleanup::take_limit_notice() -> Option<String>`; existing `DaemonEvent::Notice(String)` (shown as a notification by the app)

- [ ] **Step 1: Send the notice from the daemon**

In `src/daemon.rs`, right after:

```rust
    for issue in &issues {
        let _ = proxy.send_event(DaemonEvent::CleanupIssue(issue.clone()));
    }
```

add:

```rust
    if let Some(notice) = crate::core::cleanup::take_limit_notice() {
        let _ = proxy.send_event(DaemonEvent::Notice(notice));
    }
```

In the voice-edit function, replace `match crate::core::edit::run(instruction, selected.as_deref(), cfg) {` with:

```rust
    let result = crate::core::edit::run(instruction, selected.as_deref(), cfg);
    if let Some(notice) = crate::core::cleanup::take_limit_notice() {
        let _ = proxy.send_event(DaemonEvent::Notice(notice));
    }
    match result {
```

Remove the `// used from Task 4` allow line above `take_limit_notice` in `src/core/cleanup.rs`.

- [ ] **Step 2: Add the live check** (in `mod tests` of `src/core/cleanup.rs`)

```rust
    /// Needs Ollama running. A skipped Groq is never called: with a working
    /// or a rejected Groq key alike, the only provider used is Ollama.
    /// `cargo test live_skipped_groq_uses_ollama -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_skipped_groq_uses_ollama() {
        start_skip(Groq, Duration::from_secs(30), Ollama);
        assert_eq!(
            take_limit_notice().as_deref(),
            Some("Groq is out of tokens; using Ollama for about 1 min.")
        );
        let cfg = crate::core::config::Config {
            cleanup_provider: "groq".into(),
            ..Default::default()
        };
        let out = clean_chain("um so the meeting is on friday", &cfg);
        println!("{out:?}");
        assert!(out.issues.is_empty(), "Groq was called: {:?}", out.issues);
        assert!(!out.text.trim().is_empty());
    }
```

Run: `cargo test live_skipped_groq_uses_ollama -- --ignored --nocapture`
Expected: PASS and the printed text is a cleaned sentence.

- [ ] **Step 3: Docs**

`README.md`, after the paragraph that starts "Add API keys from the menu bar", add:

```markdown
When Groq or OpenRouter says a key is out of tokens (HTTP 429 or 402),
Wiflow stops using it until the limit resets (the provider's wait time,
or 1 minute) and shows one notification. Meanwhile dictation and
voice-edit use the fallback chosen in Settings under "When an API key
runs out of tokens": local Ollama (default) or the auto chain.
```

`docs/corrections.md`, at the end of the paragraph that starts "Ambiguous self-corrections", add the sentence: "A provider that reports a token limit is skipped until the limit resets; see the README."

`memory.md`, append:

```markdown
- 2026-11-25 — Key-limit fallback: a cloud provider that answers 429/402
  is skipped in memory for its Retry-After (default 60 s, cap 1 h); the
  `cleanup_limit_fallback` setting picks Ollama (default) or the auto
  chain; voice-edit now falls back too. 401 starts no skip. Why: user
  wants dictation to keep working when a key runs out of tokens.
```

In the spec, set `Status: implemented` and in "Error handling" replace the poisoned-lock line with: "A poisoned store lock is recovered (its data is plain instants), so cleanup still runs." Then add at the end:

```markdown
## As built

- The live check is an ignored test (`live_skipped_groq_uses_ollama`)
  that starts a skip period in code; the banner uses the existing
  `DaemonEvent::Notice` path.
```

- [ ] **Step 4: Gates and commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
git add src/daemon.rs src/core/cleanup.rs README.md docs/corrections.md memory.md docs/superpowers/specs/2026-11-25-key-limit-fallback-design.md
git commit -m "feat: notify when a key hits its limit; docs and live check

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 5: Rebuild the app**

```bash
cargo build --release
pkill -TERM -f 'Wiflow.app/Contents/MacOS/wiflow-dictation'
sh packaging/build-app.sh
open target/Wiflow.app --args --app
```

Expected: `codesign -dv --verbose=2 target/Wiflow.app 2>&1 | grep Authority` prints `Authority=Wiflow Dev`, and the log shows no "input monitoring not granted".
