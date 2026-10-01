# Phase 6 Cloud Providers + Context + Settings-Models Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provider switcher (Groq/OpenRouter/Ollama), Groq key wired, context-synthesis layer (focused app → 2-sentence context → cleanup hint), image-specified per-task models, and Groq cloud STT (whisper-large-v3) with language setting.

**Architecture:** Keys live in `~/Library/Application Support/wiflow/keys.json` (outside repo, chmod 600, env overrides). `cleanup_provider` config selects Auto chain (Groq→OpenRouter→Ollama) or a single provider with Ollama fallback on quota. Context synthesis: focused-app name via `osascript` subprocess → context model → `<context>` block prepended to cleanup input. STT: `stt_provider` local|groq; Groq path = in-memory wav + multipart POST; `stt_language` feeds both.

**Tech Stack:** Rust 2021, `ureq 2.12` (json feature, existing), `hound` (in-memory wav), `osascript(1)` subprocess, existing cleanup/system-prompt infra

## Global Constraints

- **NEVER commit API keys or the keys.json file.** Keys live in `~/Library/Application Support/wiflow/keys.json` (outside the repo) or env vars. The user's Groq key (pasted in chat) goes into keys.json with chmod 600 and NOTHING in git.
- Target macOS 13+ arm64 first, warning-free; edition 2021, stable 1.95.0.
- `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test` zero failures.
- Cloud STT/audio upload is OPT-IN only (default `stt_provider = "local"`); privacy rule: audio never leaves device in default config.
- Cleanup chain never fails the pipeline: worst case deterministic output + alert.

---

## File Structure

- Create: `src/context_prompt.txt` — Task 3 (user's context-synthesis prompt verbatim).
- Create: `src/groq_stt.rs` — Task 4 (wav encode + multipart POST + response parse).
- Modify: `src/cleanup.rs` — Tasks 1–3 (keys file, provider selection, retry model, context synthesis, `<context>` formatting).
- Modify: `src/config.rs` — Tasks 1–4 (`cleanup_provider`, `cleanup_fallback_model`, `context_enabled`, `context_model`, `stt_provider`, `stt_language`).
- Modify: `src/app.rs` — Tasks 1, 3, 4 (provider submenu, context toggle, transcription submenu, background checks).
- Modify: `src/daemon.rs` — Task 4 (CleanupIssue already exists; stt provider branch in worker).
- Modify: `src/main.rs` — Task 4 (simulate path stt provider branch).
- Modify: `src/stt.rs` — Task 4 (language param on local transcribe).
- Modify: `task.md`, `memory.md` — Task 5.

Interfaces (produced across tasks):
- `cleanup::groq_key() -> Option<String>` (Task 1: env `GROQ_API_KEY` → keys.json `groq_api_key`).
- `cleanup::openrouter_key() -> Option<String>` (same shape, `openrouter_api_key`).
- `cleanup::clean_chain(transcript: &str, cfg: &Config) -> CleanupOutcome` (signature unchanged; behavior per provider config + retry model + context).
- `cleanup::synthesize_context(app_name: Option<&str>, cfg: &Config) -> String` (Task 3; "" when disabled/unavailable).
- `groq_stt::encode_wav16(&[f32], u32) -> Vec<u8>` (Task 4, pure, tested).
- `groq_stt::build_multipart(boundary: &str, wav: &[u8], model: &str, language: Option<&str>) -> Vec<u8>` (Task 4, pure, tested).
- `groq_stt::parse_transcript(body: &str) -> Option<String>` (Task 4, pure, tested).
- `groq_stt::transcribe_cloud(samples: &[f32], rate: u32, cfg: &Config) -> Result<String, String>` (Task 4).
- `stt::Stt::transcribe(&mut self, samples, initial_prompt)` — gains language via params: signature change to `transcribe(&mut self, samples_16k: &[f32], initial_prompt: &str, language: &str)` (Task 4; "auto" → skip set_language).
- `DaemonEvent::CleanupIssue(String)` exists.

---

### Task 1: Keys File + Provider Switcher

**Files:**
- Modify: `src/cleanup.rs` (keys file readers, provider selection)
- Modify: `src/config.rs` (`cleanup_provider: String`, default "auto")
- Modify: `src/app.rs` (Cleanup Provider submenu + handler)
- Modify: `src/daemon.rs` (worker: post provider-switch issues — already generic via CleanupIssue)

**Interfaces:**
- Consumes: existing `CleanupOutcome`, `DaemonEvent::CleanupIssue`.
- Produces: `cleanup::groq_key()/openrouter_key()` (env → keys.json), provider selection; menu submenu.

- [ ] **Step 1: Write failing tests (append to cleanup tests)**

```rust
#[test]
fn test_keys_env_overrides_file() {
    // env wins over keys.json; missing both → None. Uses a throwaway key file
    // written to the REAL app dir (outside repo, gitignored by location).
    let dir = crate::config::app_support_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let keys_path = dir.join("keys.json");
    std::fs::write(
        &keys_path,
        r#"{"groq_api_key":"file-key","openrouter_api_key":"file-or-key"}"#,
    )
    .unwrap();
    std::env::remove_var("GROQ_API_KEY");
    std::env::remove_var("OPENROUTER_API_KEY");
    assert_eq!(groq_key(), Some("file-key".into()));
    assert_eq!(openrouter_key(), Some("file-or-key".into()));
    std::env::set_var("GROQ_API_KEY", "env-key");
    assert_eq!(groq_key(), Some("env-key".into()));
    std::env::remove_var("GROQ_API_KEY");
    let _ = std::fs::remove_file(&keys_path);
}
```

NOTE: env-var mutation in tests races across the test binary — the test above is the ONLY one touching GROQ_API_KEY env; if parallel tests interfere, gate with a mutex or `--test-threads=1` note in report.

- [ ] **Step 2: Run for RED** — `cargo test test_keys_env_overrides_file 2>&1 | tail -4` → FAIL (no groq_key/app_support_dir).

- [ ] **Step 3: Implement**

In `config.rs` add:

```rust
/// App support dir (shared home for config/history/keys).
pub fn app_support_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/wiflow")
}
```

Refactor `config_path()`/`prompt_path()` to use it (behavior identical). In `cleanup.rs` replace `groq_api_key()`/`openrouter_api_key()` with:

```rust
fn key_from_file(field: &str) -> Option<String> {
    let path = crate::config::app_support_dir().join("keys.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v[field].as_str().map(|s| s.to_string()).filter(|s| !s.is_empty())
}

pub fn groq_key() -> Option<String> {
    std::env::var("GROQ_API_KEY").ok().filter(|k| !k.is_empty()).or_else(|| key_from_file("groq_api_key"))
}

pub fn openrouter_key() -> Option<String> {
    std::env::var("OPENROUTER_API_KEY").ok().filter(|k| !k.is_empty()).or_else(|| key_from_file("openrouter_api_key"))
}
```

Keep old names as thin wrappers if call sites use them (`groq_api_key()` → `groq_key()`; plan mandates RENAME call sites, no duplicates).

Provider selection — add to `config.rs`:

```rust
/// "auto" = Groq → OpenRouter → Ollama chain; explicit "groq" / "openrouter" /
/// "ollama" = only that provider (with Ollama fallback on quota errors).
#[serde(default)]
pub cleanup_provider: String,
```

Default impl gains `cleanup_provider: "auto".into()`.

In `cleanup.rs` `clean_chain` gains provider branching:

```rust
let provider = if cfg.cleanup_provider.is_empty() { "auto" } else { cfg.cleanup_provider.as_str() };
match provider {
    "groq" => single_provider_chain(transcript, cfg, &[Provider::Groq], &mut issues),
    "openrouter" => single_provider_chain(transcript, cfg, &[Provider::OpenRouter], &mut issues),
    "ollama" => single_provider_chain(transcript, cfg, &[Provider::Ollama], &mut issues),
    _ => { /* existing full chain: Groq → OpenRouter → Ollama */ }
}
```

Implement via a `Provider` enum + one `try_provider(p, cfg, transcript) -> Result<String, String>` fn (refactor the existing inline Groq/OpenRouter/Ollama blocks into it — DRY). Explicit single-provider chains still fall back to Ollama on quota (user requirement: alert + switch to Ollama), but NOT on other errors unless the provider IS ollama.

app.rs: Cleanup Provider submenu (CheckMenuItems: Auto (chain), Groq only, OpenRouter only, Ollama only) + ids + handler switching `cleanup_provider` + save + menu_dirty. Add after the cleanup toggle row.

- [ ] **Step 4: Gates + live check** — fmt/clippy/test green. `cargo run -- --list-devices` unaffected. Report test-threads caveat if env test flaked.

- [ ] **Step 5: Commit** — `git add src/cleanup.rs src/config.rs src/app.rs` → "feat: provider switcher and keys file"

---

### Task 2: Image Models + Explicit Retry

**Files:**
- Modify: `src/config.rs` (defaults per image + `cleanup_fallback_model`)
- Modify: `src/cleanup.rs` (retry step in the chain)

**Interfaces:**
- Consumes: Task 1 provider machinery.
- Produces: retry-model step; image-default models.

- [ ] **Step 1: Failing test** —

```rust
#[test]
fn test_image_model_defaults() {
    let cfg = crate::config::Config::default();
    assert_eq!(cfg.cleanup_groq_model, "openai/gpt-oss-20b");
    assert_eq!(cfg.cleanup_fallback_model, "qwen/qwen3.6-27b");
    assert_eq!(cfg.context_model, "qwen/qwen3.6-27b");
    assert_eq!(cfg.stt_provider, "local");
    assert_eq!(cfg.stt_language, "auto");
}
```

- [ ] **Step 2: RED** — FAIL (fields missing).

- [ ] **Step 3: Implement** — config defaults per the image: `cleanup_groq_model = "openai/gpt-oss-20b"` (Post-Processing Model), NEW `cleanup_fallback_model = "qwen/qwen3.6-27b"` (Post-Processing Fallback — explicit retry), `context_model = "qwen/qwen3.6-27b"` (Context Model), `stt_provider = "local"`, `stt_language = "auto"` (Task 4 fields added here, implemented Task 4). Update serde default fns + Default impl + sample() test fixture.

Chain update: after the primary model fails on a provider, retry the SAME provider with `cleanup_fallback_model` before moving to the next provider:

```rust
fn try_provider_with_retry(p: Provider, cfg: &Config, transcript: &str) -> Result<String, String> {
    let primary = provider_model(p, cfg);
    match try_provider_model(p, &primary, transcript) {
        Ok(t) => Ok(t),
        Err(e) => {
            let fallback = &cfg.cleanup_fallback_model;
            if *fallback != primary {
                tracing::warn!("retry with fallback model {fallback}: {e}");
                try_provider_model(p, fallback, transcript)
            } else {
                Err(e)
            }
        }
    }
}
```

- [ ] **Step 4: Gates** — green.
- [ ] **Step 5: Commit** — "feat: image-default models with explicit fallback retry"

---

### Task 3: Context Synthesis Layer

**Files:**
- Create: `src/context_prompt.txt` (user's context-synthesis prompt verbatim)
- Modify: `src/cleanup.rs` (`synthesize_context`, `<context>` formatting, chain wiring)
- Modify: `src/app.rs` (focused-app name helper, context toggle in menu, background behavior)
- Modify: `src/config.rs` (`context_enabled: bool` default true, `context_model` — added Task 2)

**Interfaces:**
- Consumes: Task 1/2 provider machinery.
- Produces: `cleanup::synthesize_context(Option<&str>, &Config) -> String`; formatted input for all providers.

- [ ] **Step 1: Save the prompt verbatim** — `src/context_prompt.txt` = the user's context-synthesis prompt EXACTLY ("You are a context synthesis assistant inside a speech-to-text pipeline..." through "...Describe the activity as you would otherwise." including examples and rules).

- [ ] **Step 2: Failing tests**

```rust
#[test]
fn test_context_formatting() {
    let out = format_cleanup_input(Some("The user is dictating into Firefox. Likely a chat reply."), "hello world");
    assert!(out.contains("<context>"));
    assert!(out.contains("</context>"));
    assert!(out.contains("<transcript>hello world</transcript>"));
    let bare = format_cleanup_input(None, "hello world");
    assert_eq!(bare, "hello world"); // no context → plain transcript (prompt: no tags = transcript)
}

#[test]
fn test_synthesize_context_disabled_or_missing_app() {
    let cfg = crate::config::Config::default();
    // No app name → empty context (never invent — prompt rule).
    assert_eq!(synthesize_context(None, &cfg), "");
}
```

- [ ] **Step 3: RED** then implement:

```rust
const CONTEXT_PROMPT: &str = include_str!("context_prompt.txt");

pub fn format_cleanup_input(context: Option<&str>, transcript: &str) -> String {
    match context {
        Some(c) if !c.trim().is_empty() => {
            format!("<context>{}</context>\n<transcript>{}</transcript>", c.trim(), transcript)
        }
        _ => transcript.to_string(),
    }
}

/// Two-sentence context via the context model (first available provider in
/// the chain). "" when disabled, no app name, or any failure — never invent.
pub fn synthesize_context(app_name: Option<&str>, cfg: &crate::config::Config) -> String {
    let Some(app) = app_name.filter(|a| !a.trim().is_empty()) else { return String::new(); };
    if !cfg.context_enabled {
        return String::new();
    }
    let prompt = format!("App: {app}");
    // Provider order for the small context call: Groq → Ollama (skip OpenRouter for latency).
    if let Some(key) = groq_key() {
        if let Ok(text) = chat_completion(
            "https://api.groq.com/openai/v1/chat/completions",
            key,
            &cfg.context_model,
            &prompt,
            Some(CONTEXT_PROMPT),
        ) {
            return text;
        }
    }
    if ollama_reachable() && ollama_model_installed(&cfg.cleanup_model) {
        if let Ok(text) = ollama_chat(&prompt, Some(CONTEXT_PROMPT), &cfg.cleanup_model) {
            return text;
        }
    }
    String::new()
}
```

Refactor `chat_completion` to take an optional system prompt override: `chat_completion(url, key, model, user_content, system: Option<&str>)` — None → the cleanup SYSTEM_PROMPT (existing behavior). `ollama_chat` = ollama_generate with a system override, same pattern. Update all existing call sites to pass None.

Chain wiring: `clean_chain` gains `context: Option<&str>`? — plan mandates: `clean_chain(transcript, cfg)` signature UNCHANGED; the CALLERS (daemon worker + main simulate) synthesize context first and pass the FORMATTED input:

```rust
let app = focused_app_name(); // app.rs helper, None when unavailable
let ctx = crate::cleanup::synthesize_context(app.as_deref(), &cfg);
let input = crate::cleanup::format_cleanup_input(if ctx.is_empty() { None } else { Some(&ctx) }, &text);
let outcome = crate::cleanup::clean_chain(&input, &cfg);
```

focused-app helper (app.rs, reusable by daemon):

```rust
/// Frontmost app name via System Events (osascript subprocess, ~200ms).
/// None when the query fails or returns empty.
pub fn focused_app_name() -> Option<String> {
    let out = std::process::Command::new("osascript")
        .args(["-e", "tell application \"System Events\" to get name of first application process whose frontmost is true"])
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}
```

Put `focused_app_name` in daemon.rs (worker uses it; app.rs doesn't need it directly) — plan mandates daemon.rs ownership. app.rs menu: "Context Inference" CheckMenuItem toggle (context_enabled) next to the cleanup toggle.

- [ ] **Step 4: Gates + live verify** — green; LIVE: with the Groq key present (Task 1 keys.json), a throwaway test synthesize_context("Terminal") → two-sentence output; report output verbatim. Latency note: context call adds ~0.3-2s per dictation (worker thread, tray shows Transcribing).
- [ ] **Step 5: Commit** — "feat: context synthesis layer with focused app detection"

---

### Task 4: Groq Cloud STT + Language

**Files:**
- Create: `src/groq_stt.rs`
- Modify: `src/stt.rs` (language param), `src/daemon.rs` (provider branch + focused_app + CleanupIssue), `src/main.rs` (simulate branch), `src/app.rs` (Transcription submenu)

**Interfaces:**
- Consumes: `hound`, `ureq`, config fields from Task 2.
- Produces: all `groq_stt::` items; `stt::Stt::transcribe(&mut self, &[f32], &str, &str)`.

- [ ] **Step 1: Failing tests (in groq_stt.rs tests module)**

```rust
#[test]
fn test_encode_wav16_shape() {
    let samples = vec![0.0f32, 0.5, -0.5, 1.0];
    let wav = encode_wav16(&samples, 16_000);
    assert!(wav.len() > 44, "must include RIFF header");
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
}

#[test]
fn test_build_multipart_shape() {
    let wav = vec![1u8, 2, 3];
    let body = build_multipart("BOUNDARY123", &wav, "whisper-large-v3", None);
    let s = String::from_utf8_lossy(&body);
    assert!(s.contains("--BOUNDARY123"));
    assert!(s.contains("name=\"model\""));
    assert!(s.contains("whisper-large-v3"));
    assert!(s.contains("name=\"file\"; filename=\"audio.wav\""));
    assert!(s.contains("Content-Type: audio/wav"));
    assert!(s.ends_with(b"--BOUNDARY123--\r\n") || s.ends_with("--BOUNDARY123--\r\n"));
}

#[test]
fn test_parse_transcript() {
    assert_eq!(parse_transcript(r#"{"text":"Hello world."}"#), Some("Hello world.".into()));
    assert_eq!(parse_transcript("not json"), None);
}
```

- [ ] **Step 2: RED** then implement:

```rust
use std::io::Write as _;

/// Encode mono f32 samples to a 16-bit WAV in memory (pure, tested).
pub fn encode_wav16(samples: &[f32], rate: u32) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::new(&mut cursor, spec).expect("wav writer");
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16).expect("sample");
    }
    w.finalize().expect("finalize");
    cursor.into_inner()
}

/// Build a multipart/form-data body for Groq audio transcription (pure, tested).
pub fn build_multipart(boundary: &str, wav: &[u8], model: &str, language: Option<&str>) -> Vec<u8> {
    let mut body = Vec::new();
    let mut part = |field: &str, value: &str| {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"\r\n\r\n{value}\r\n").as_bytes());
    };
    part("model", model);
    if let Some(lang) = language {
        part("language", lang);
    }
    part("response_format", "json");
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

pub fn parse_transcript(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("text")?
        .as_str()
        .map(|s| s.trim().to_string())
}

/// Cloud STT via Groq whisper-large-v3 (OPT-IN: audio leaves the device only
/// when stt_provider == "groq" — privacy rule).
pub fn transcribe_cloud(samples: &[f32], rate: u32, cfg: &crate::config::Config) -> Result<String, String> {
    let key = crate::cleanup::groq_key().ok_or("no Groq key (set GROQ_API_KEY)")?;
    let wav = encode_wav16(samples, rate);
    let boundary = "wiflow-audio-boundary-7f3a";
    let body = build_multipart(boundary, &wav, "whisper-large-v3", lang_opt(&cfg.stt_language));
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(30)).build();
    let resp = agent
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .send_bytes(&body)
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            other => format!("{other}"),
        })?;
    let text_raw = resp.into_string().map_err(|e| format!("read: {e:?}"))?;
    parse_transcript(&text_raw).ok_or_else(|| format!("unparseable: {}", text_raw))
}

fn lang_opt(lang: &str) -> Option<&str> {
    if lang.is_empty() || lang == "auto" { None } else { Some(lang) }
}
```

`stt.rs` language: `transcribe(&mut self, samples_16k, initial_prompt, language: &str)` — after set_no_timestamps: `if language != "auto" && !language.is_empty() { params.set_language(language) }` (whisper-rs `set_language` exists on FullParams; check vendored — if missing, use the `language` param in FullParams::new? report). Update ALL call sites (daemon + main simulate pass `&cfg.stt_language`).

Worker branch (daemon.rs pipeline_on_worker): before `transcribe_shared`:

```rust
let cfg = crate::config::load_config(); // hoist ABOVE transcribe (reuse for prompt/language/provider)
let text = if cfg.stt_provider == "groq" {
    match crate::groq_stt::transcribe_cloud(&kept, crate::vad::VAD_SAMPLE_RATE, &cfg) {
        Ok(t) => {
            tracing::info!("cloud stt (whisper-large-v3) done");
            t
        }
        Err(e) => {
            let _ = proxy.send_event(DaemonEvent::CleanupIssue(format!("Groq STT failed: {e} — using local whisper")));
            match crate::stt::transcribe_shared(&model_path, &kept, &crate::stt::read_prompt(), &cfg.stt_language) {
                Ok(t) => t,
                Err(e2) => { let _ = proxy.send_event(DaemonEvent::Failed(format!("transcribe failed: {e2}"))); return; }
            }
        }
    }
} else {
    match crate::stt::transcribe_shared(&model_path, &kept, &crate::stt::read_prompt(), &cfg.stt_language) {
        Ok(t) => t,
        Err(e) => { let _ = proxy.send_event(DaemonEvent::Failed(format!("transcribe failed: {e}"))); return; }
    }
};
```

(Read `initial_prompt` ONCE into a variable — the current code calls read_prompt() inline; hoist.) Also focused-app context synthesis wiring per Task 3 (hoist cfg, synthesize before cleanup).

app.rs Transcription submenu: "Transcription: Local whisper" / "Transcription: Groq cloud (whisper-large-v3)" CheckMenuItems (stt_provider) + "Language: Auto-detect" submenu? — plan mandates: provider items only in the menu; `stt_language` stays config.json-only (documented in memory). Handler switches `stt_provider` + save + menu_dirty + CleanupIssue-free (no alert needed).

- [ ] **Step 3: Gates + LIVE verification (the user's requirement — test fully)**

With the user's Groq key in keys.json (Task 1 writes it): throwaway test `groq_stt::transcribe_cloud(&tone_16k, 16_000, &cfg)` → REAL cloud transcript; report output + RTF verbatim. If the key is invalid/expired → report the HTTP error honestly (user rotates). Also chain-with-key live test: `clean_chain` → Groq primary (gpt-oss-20b) → report output.

- [ ] **Step 4: Commit** — "feat: groq cloud stt with language setting"

---

### Task 5: Full Test + Docs + Complete

**Files:**
- Modify: `task.md`, `memory.md`

- [ ] **Step 1: Full gates** — fmt/clippy/test green; report counts.
- [ ] **Step 2: Full live pass** — app run (config provider "auto", Groq key in keys.json): startup background check (Ollama running here → no alert), posted-keys PTT round-trip → capture → STT → context → cleanup → inject; report every log line verbatim. Also explicit-provider configs (groq/openrouter/ollama) via config swap: each registers/behaves; report.
- [ ] **Step 3: task.md** — new Phase 6 section: all boxes checked with numbers (models, provider outcomes, live outputs); memory.md dated entry (keys.json location + chmod 600, chain behavior, context latency, cloud STT privacy note, llama/gpt-oss model defaults per image).
- [ ] **Step 4: Commits** — "docs: mark phase6 cloud providers complete" + final.

---

## Self-Review

- Spec coverage: provider switcher (Task 1) ✓; Groq key (Task 1 keys.json + Task 5 live) ✓; context synthesis prompt (Task 3, verbatim + focused-app input) ✓; image models (Task 2: post-processing openai/gpt-oss-20b, fallback qwen/qwen3.6-27b explicit retry, context qwen/qwen3.6-27b; Task 4: transcription whisper-large-v3 via Groq + auto language) ✓; alert-on-exhaustion (existing CleanupIssue + quota detection) ✓; background model check (Task 3/4 check_ollama_ready at startup + fallback) ✓; full test (Task 5) ✓.
- Placeholder scan: none — every step has code; the whisper-rs set_language existence is fenced with a report protocol; live Groq calls fenced with honest-error reporting (key may be invalid — user rotates).
- Type consistency: `clean_chain(transcript, cfg)` signature unchanged across tasks; `chat_completion(url, key, model, user_content, system: Option<&str>)` — all call sites updated Task 3; `transcribe(&mut self, &[f32], &str, &str)` — daemon + main + tests updated Task 4; `CleanupOutcome { text, issues }` unchanged; `cleanup_provider`/`stt_provider` strings match menu/config values.
- SECURITY: the user's Groq key NEVER enters any committed file — keys.json lives in the app dir (outside the repo); the plan text contains NO key material.
