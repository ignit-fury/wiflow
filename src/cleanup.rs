use serde_json::json;
use std::time::Duration;

/// The literal dictation cleanup layer system prompt (verbatim, user-provided).
const SYSTEM_PROMPT: &str = include_str!("cleanup_prompt.txt");

/// The literal context-synthesis system prompt (verbatim, user-provided):
/// two sentences describing what the user is doing and what they are about
/// to dictate, used only as a formatting hint by the cleanup model.
const CONTEXT_PROMPT: &str = include_str!("context_prompt.txt");

pub const OLLAMA_ENDPOINT: &str = "http://localhost:11434";
/// Filler-only sentinel the system prompt returns for empty/filler input.
pub const EMPTY_SENTINEL: &str = "EMPTY";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Strict circuit-breaker for the local cleanup call: a hanging 1B model must
/// never block injection for seconds — timeout Err falls back to the
/// deterministic offline transcript in `clean_chain`.
const OLLAMA_TIMEOUT: Duration = Duration::from_millis(800);

/// Provider chain (user-specified): Groq → OpenRouter → Ollama local →
/// deterministic output. Keys come from env first (`GROQ_API_KEY`,
/// `OPENROUTER_API_KEY`), then `~/Library/Application Support/wiflow/keys.json`
/// (`groq_api_key` / `openrouter_api_key` fields) — never stored in the repo.
pub fn groq_key() -> Option<String> {
    std::env::var("GROQ_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
        .or_else(|| key_from_file("groq_api_key"))
}

pub fn openrouter_key() -> Option<String> {
    std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
        .or_else(|| key_from_file("openrouter_api_key"))
}

/// keys.json field reader in the app support dir. Empty strings count as
/// missing (hand-edited placeholders must not be sent as auth headers).
fn key_from_file(field: &str) -> Option<String> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(keys_file_path()).ok()?).ok()?;
    v[field]
        .as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn keys_file_path() -> std::path::PathBuf {
    crate::config::app_support_dir().join("keys.json")
}

/// Save one API key field (`groq_api_key` / `openrouter_api_key`), preserving
/// the sibling field. Empty value clears the field. Atomic write (never
/// partial). Env vars still override the file at read time.
pub fn save_key(field: &str, value: &str) -> Result<(), String> {
    save_key_to(&keys_file_path(), field, value)
}

fn save_key_to(path: &std::path::Path, field: &str, value: &str) -> Result<(), String> {
    let mut map: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let v = value.trim();
    if v.is_empty() {
        map.remove(field);
    } else {
        map.insert(field.to_string(), serde_json::Value::String(v.to_string()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir keys: {e:?}"))?;
    }
    crate::config::atomic_write_json(path, &map)
}

/// Clear one API key field (sibling preserved).
pub fn clear_key(field: &str) -> Result<(), String> {
    save_key(field, "")
}

/// Installed local models for the Ollama picker, sorted/deduped. Empty when
/// Ollama is down (menu shows "not running"). Short timeout: menu builds on
/// the winit thread, so this must never hang.
pub fn list_ollama_models() -> Vec<String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(500))
        .build();
    let Ok(resp) = agent.get(&format!("{OLLAMA_ENDPOINT}/api/tags")).call() else {
        return Vec::new();
    };
    let v: serde_json::Value = resp.into_json().unwrap_or_default();
    let mut names: Vec<String> = v["models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.dedup();
    names
}

/// Native secure key prompt (tray app has no windows): osascript `display
/// dialog … with hidden answer`. None on cancel/empty/failure.
pub fn prompt_for_key(title: &str, prompt: &str) -> Option<String> {
    let script = format!(
        "display dialog \"{prompt}\" default answer \"\" with hidden answer with title \"{title}\""
    );
    let out = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None; // Cancel pressed.
    }
    parse_dialog_text(&String::from_utf8_lossy(&out.stdout))
}

/// Parse osascript dialog stdout (`text returned:secret, button
/// returned:OK`). None when canceled or empty.
pub fn parse_dialog_text(out: &str) -> Option<String> {
    let (_, after) = out.split_once("text returned:")?;
    let val = after.split(", button returned:").next().unwrap_or(after);
    let v = val.trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// Result of the cleanup chain: cleaned text (input unchanged when every
/// provider failed) + user-facing issues (rate limits, missing model).
#[derive(Debug, Clone, PartialEq)]
pub struct CleanupOutcome {
    pub text: String,
    pub issues: Vec<String>,
}

/// Cleanup providers for the LLM cleanup layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    Groq,
    OpenRouter,
    Ollama,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Provider::Groq => "Groq",
            Provider::OpenRouter => "OpenRouter",
            Provider::Ollama => "Ollama",
        }
    }

    /// Quota-exhausted wording (differs per provider: rate limit vs credits).
    fn quota_issue(self, fallback: Provider) -> String {
        let detail = match self {
            Provider::Groq => "tokens/rate limit exhausted",
            Provider::OpenRouter => "tokens/credits exhausted",
            Provider::Ollama => "quota exhausted",
        };
        format!(
            "{} {detail} — falling back to {}",
            self.name(),
            fallback.name()
        )
    }
}

/// Model id a provider uses (per-provider config field).
fn provider_model(p: Provider, cfg: &crate::config::Config) -> &str {
    match p {
        Provider::Groq => &cfg.cleanup_groq_model,
        Provider::OpenRouter => &cfg.cleanup_openrouter_model,
        Provider::Ollama => &cfg.cleanup_model,
    }
}

/// Try one provider + model: key (env → keys.json) + HTTP call. Err carries a
/// user-facing reason (missing key, HTTP status, unparseable response).
fn try_provider_model(p: Provider, model: &str, transcript: &str) -> Result<String, String> {
    match p {
        Provider::Groq => {
            let key = groq_key()
                .ok_or("no Groq API key — set GROQ_API_KEY or add groq_api_key to keys.json")?;
            chat_completion(
                "https://api.groq.com/openai/v1/chat/completions",
                &key,
                model,
                transcript,
                None,
            )
        }
        Provider::OpenRouter => {
            let key = openrouter_key().ok_or(
                "no OpenRouter API key — set OPENROUTER_API_KEY or add openrouter_api_key to keys.json",
            )?;
            chat_completion(
                "https://openrouter.ai/api/v1/chat/completions",
                &key,
                model,
                transcript,
                None,
            )
        }
        Provider::Ollama => ollama_chat(transcript, None, model),
    }
}

/// Explicit retry: after the primary model fails on a provider, retry the
/// SAME provider with `cleanup_fallback_model` before moving down the chain.
/// The fallback error stands when the fallback equals the primary.
fn try_provider_with_retry(
    p: Provider,
    cfg: &crate::config::Config,
    transcript: &str,
) -> Result<String, String> {
    let primary = provider_model(p, cfg);
    match try_provider_model(p, primary, transcript).map(|t| sanitize_llm_response(&t, transcript)) {
        Ok(t) => Ok(t),
        Err(e) => {
            let fallback = cfg.cleanup_fallback_model.as_str();
            if fallback != primary {
                tracing::warn!("retry with fallback model {fallback}: {e}");
                try_provider_model(p, fallback, transcript)
                    .map(|t| sanitize_llm_response(&t, transcript))
            } else {
                Err(e)
            }
        }
    }
}

/// Run the full provider chain. Never fails: worst case the input is
/// returned unchanged with issues recorded for the UI alert.
pub fn clean_chain(transcript: &str, cfg: &crate::config::Config) -> CleanupOutcome {
    let mut issues = Vec::new();
    if !cfg.cleanup_enabled || transcript.trim().is_empty() {
        return CleanupOutcome {
            text: transcript.to_string(),
            issues,
        };
    }
    let provider = if cfg.cleanup_provider.is_empty() {
        "auto"
    } else {
        cfg.cleanup_provider.as_str()
    };
    let text = match provider {
        "groq" => single_provider_chain(transcript, cfg, &[Provider::Groq], &mut issues),
        "openrouter" => {
            single_provider_chain(transcript, cfg, &[Provider::OpenRouter], &mut issues)
        }
        "ollama" => single_provider_chain(transcript, cfg, &[Provider::Ollama], &mut issues),
        _ => full_chain(transcript, cfg, &mut issues),
    };
    CleanupOutcome {
        text: text.unwrap_or_else(|| transcript.to_string()),
        issues,
    }
}

/// One chain step: try `p`; on failure push the user-facing issue and fall
/// back to `next` (when present). Returns Some(cleaned) on success.
fn chain_step(
    p: Provider,
    cfg: &crate::config::Config,
    transcript: &str,
    next: Option<Provider>,
    issues: &mut Vec<String>,
) -> Option<String> {
    match try_provider_with_retry(p, cfg, transcript) {
        Ok(text) => Some(text),
        Err(e) => {
            match next {
                Some(n) => {
                    if is_quota_error(&e) {
                        issues.push(p.quota_issue(n));
                    } else {
                        issues.push(format!(
                            "{} failed ({e}) — falling back to {}",
                            p.name(),
                            n.name()
                        ));
                    }
                }
                None => issues.push(format!("{} fallback failed: {e}", p.name())),
            }
            tracing::warn!("{} cleanup failed: {e}", p.name().to_lowercase());
            None
        }
    }
}

/// Auto chain (existing behavior): Groq → OpenRouter → Ollama, silently
/// skipping cloud providers with no key.
fn full_chain(
    transcript: &str,
    cfg: &crate::config::Config,
    issues: &mut Vec<String>,
) -> Option<String> {
    // 1. Groq (fastest cloud, generous free tier). No key → skip silently.
    if groq_key().is_some() {
        if let Some(text) = chain_step(
            Provider::Groq,
            cfg,
            transcript,
            Some(Provider::OpenRouter),
            issues,
        ) {
            return Some(text);
        }
    }
    // 2. OpenRouter (free-tier models available).
    if openrouter_key().is_some() {
        if let Some(text) = chain_step(
            Provider::OpenRouter,
            cfg,
            transcript,
            Some(Provider::Ollama),
            issues,
        ) {
            return Some(text);
        }
    }
    // 3. Ollama local ($0, offline). Background model check with guidance.
    chain_step(Provider::Ollama, cfg, transcript, None, issues)
}

/// Explicit provider: only that one, with Ollama fallback on quota errors
/// (user requirement: alert + switch to Ollama) — not other errors, unless
/// the provider IS ollama.
fn single_provider_chain(
    transcript: &str,
    cfg: &crate::config::Config,
    providers: &[Provider],
    issues: &mut Vec<String>,
) -> Option<String> {
    let p = providers[0];
    match try_provider_with_retry(p, cfg, transcript) {
        Ok(text) => Some(text),
        Err(e) => {
            if p != Provider::Ollama && is_quota_error(&e) {
                issues.push(p.quota_issue(Provider::Ollama));
                tracing::warn!("{} cleanup failed: {e}", p.name().to_lowercase());
                chain_step(Provider::Ollama, cfg, transcript, None, issues)
            } else {
                issues.push(format!("{} failed: {e}", p.name()));
                tracing::warn!("{} cleanup failed: {e}", p.name().to_lowercase());
                None
            }
        }
    }
}

/// 429 (rate limit), 402 (payment/credits), or 401 (quota/auth exhausted).
pub fn is_quota_error(err: &str) -> bool {
    err.contains("429") || err.contains("402") || err.contains("401")
}

/// OpenAI-compatible chat completion (Groq + OpenRouter share the shape).
/// `system: None` uses the cleanup SYSTEM_PROMPT (existing behavior).
fn chat_completion(
    url: &str,
    key: &str,
    model: &str,
    user_content: &str,
    system: Option<&str>,
) -> Result<String, String> {
    let agent = ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build();
    let resp = agent
        .post(url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_json(json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system.unwrap_or(SYSTEM_PROMPT)},
                {"role": "user", "content": user_content}
            ],
            "temperature": 0.0,
        }))
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            other => format!("{other}"),
        })?;
    let v: serde_json::Value = resp.into_json().map_err(|e| format!("bad json: {e:?}"))?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("unparseable response: {}", truncate(&v.to_string())))
}

fn ollama_chat(prompt: &str, system: Option<&str>, model: &str) -> Result<String, String> {
    // Background model check first: distinguish "not running" from
    // "model not pulled" so the alert tells the user exactly what to do.
    if !ollama_reachable() {
        return Err("Ollama not running — install from ollama.com and run `ollama serve`".into());
    }
    if !ollama_model_installed(model) {
        return Err(format!("model not installed — run `ollama pull {model}`"));
    }
    let agent = ureq::AgentBuilder::new().timeout(OLLAMA_TIMEOUT).build();
    let resp = agent
        .post(&format!("{OLLAMA_ENDPOINT}/api/generate"))
        .set("Content-Type", "application/json")
        .send_json(json!({
            "model": model,
            "system": system.unwrap_or(SYSTEM_PROMPT),
            "prompt": prompt,
            "stream": false,
            // Hardened for 1B SLMs: deterministic, generation-capped, stops
            // the moment the model reaches for XML or a new section.
            "options": {
                "temperature": 0.0,
                "num_predict": 100,
                "stop": ["</transcript>", "</Input>", "<Input>", "\n\n\n", "Context:"],
            },
            // Keep weights resident in unified memory (no reload latency).
            "keep_alive": -1,
        }))
        .map_err(|e| format!("generate failed: {e:?}"))?;
    let v: serde_json::Value = resp.into_json().map_err(|e| format!("bad json: {e:?}"))?;
    v["response"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("unparseable response: {}", truncate(&v.to_string())))
}

/// Background check: is Ollama running AND is the model pulled?
/// Returns Err with user-facing guidance (the "tell the user to install" path).
pub fn check_ollama_ready(model: &str) -> Result<(), String> {
    if !ollama_reachable() {
        return Err("Ollama not running — install from ollama.com and run `ollama serve`".into());
    }
    if !ollama_model_installed(model) {
        return Err(format!("model not installed — run `ollama pull {model}`"));
    }
    Ok(())
}

fn ollama_reachable() -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(2))
        .build();
    agent
        .get(&format!("{OLLAMA_ENDPOINT}/api/tags"))
        .call()
        .is_ok()
}

fn ollama_model_installed(model: &str) -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(2))
        .build();
    match agent.get(&format!("{OLLAMA_ENDPOINT}/api/tags")).call() {
        Ok(resp) => {
            let v: serde_json::Value = resp.into_json().unwrap_or_default();
            v["models"]
                .as_array()
                .map(|models| {
                    models
                        .iter()
                        .filter_map(|m| m["name"].as_str())
                        .any(|name| name == model || name.split(':').next() == Some(model))
                })
                .unwrap_or(false)
        }
        Err(_) => false,
    }
}

/// True when the cleaned text is the filler-only sentinel (or empty).
pub fn is_filler_result(cleaned: &str) -> bool {
    cleaned.trim() == EMPTY_SENTINEL || cleaned.trim().is_empty()
}

/// Flat, SLM-safe cleanup input: small models (1B–3B) echo nested XML back
/// into the output (`<Input>`, `<transcript>`…), so no angle-bracket tags
/// here. Non-empty, non-vague context → `[Context: …]` header + plain
/// `Transcript to clean:` line; otherwise the bare transcript.
pub fn format_cleanup_input(context: Option<&str>, transcript: &str) -> String {
    match context {
        Some(c) if !is_vague_context(c) => format!(
            "[Context: {}]\nTranscript to clean: {}",
            c.trim(),
            transcript
        ),
        _ => transcript.to_string(),
    }
}

/// Vague/placeholder context carries no signal — injecting it only confuses
/// the cleanup model (and gives 1B models scaffolding to echo), so the
/// caller passes the plain transcript instead. Besides exact placeholders,
/// synthesized paragraphs hedging on 2+ markers ("unknown field… not
/// visible… unclear") count as vague; a single marker inside otherwise
/// concrete text is kept.
fn is_vague_context(c: &str) -> bool {
    let t = c.trim();
    if t.is_empty() {
        return true;
    }
    let l = t.to_ascii_lowercase();
    if matches!(
        l.as_str(),
        "unknown" | "generic" | "n/a" | "none" | "empty"
    ) {
        return true;
    }
    const HEDGES: &[&str] = &[
        "unknown",
        "unclear",
        "not visible",
        "no specific",
        "not clear",
        "not shown",
        "cannot see",
        "can't see",
        "no names",
        "nothing visible",
    ];
    HEDGES.iter().filter(|m| l.contains(*m)).count() >= 2
}

/// Vague frontmost-app names: synthesizing context for these burns a second
/// sequential LLM call for zero signal, so skip it (cleanup proceeds alone).
fn is_vague_app(app: &str) -> bool {
    let t = app.trim();
    if t.is_empty() {
        return true;
    }
    matches!(
        t.to_ascii_lowercase().as_str(),
        "unknown" | "generic" | "n/a" | "none"
    )
}

/// Post-LLM sanitizer: strips the pseudo-XML / chatty wrappers lightweight
/// local models emit, so broken tags never reach the user's document.
///
/// Pipeline: extract `<transcript>` (else `<translated>`) inner → drop code
/// fences → strip every remaining `<…>` tag → strip preamble line/prefix →
/// strip surrounding quotes. `EMPTY` passes through (caller drops filler);
/// empty / non-alphanumeric / runaway-long results fall back to the plain
/// (header-stripped) transcript instead of injecting garbage.
pub fn sanitize_llm_response(raw: &str, fallback_transcript: &str) -> String {
    let fallback = plain_transcript_of(fallback_transcript);
    let mut text = raw.trim().to_string();
    if text.is_empty() {
        return fallback;
    }
    // 1. Most relevant tagged payload wins (1B models wrap + continue tags).
    if let Some(inner) = extract_tag_inner(&text, "transcript")
        .or_else(|| extract_tag_inner(&text, "translated"))
    {
        text = inner;
    }
    // 2. Code fences (``` … ```) — block or stray.
    if text.contains("```") {
        text = text.replace("```", "");
        text = text.trim().to_string();
    }
    // 3. Scaffolding blocks (<context>…</context>) go whole — their inner
    // notes are hints, not dictation. All other leftover <…> tags are then
    // stripped but their inner text kept (</Input>, stray <transcript>…).
    text = remove_tag_blocks(&text, "context");
    text = strip_tags(&text);
    text = text.trim().to_string();
    // 4. Echoed scaffolding (our flat "[Context: …]" header + "Transcript to
    // clean:" label) and conversational preamble ("Here is the cleaned
    // text:", "Cleaned: …"). Looped: preamble can sit above the header.
    for _ in 0..3 {
        let next = strip_flat_header(&strip_preamble(&text));
        if next == text {
            break;
        }
        text = next;
    }
    // 5. Surrounding quotes ('…', "…", “…”).
    text = strip_surrounding_quotes(&text).trim().to_string();
    if text.is_empty() {
        return fallback;
    }
    // Sentinel stays sentinel — the caller treats it as drop, not inject.
    if text == EMPTY_SENTINEL {
        return text;
    }
    // Corrupted: no word characters at all (only punctuation/symbols left).
    if !text.chars().any(|c| c.is_alphanumeric()) {
        return fallback;
    }
    // Runaway hallucination: far longer than what went in (cleanup only
    // fixes punctuation/caps — it never grows the text several-fold).
    if !fallback.is_empty() && text.len() > fallback.len().saturating_mul(4).max(200) {
        return fallback;
    }
    text
}

/// Case-insensitive first `<tag>…</tag>` inner text (trimmed). None when the
/// pair is missing.
fn extract_tag_inner(hay: &str, tag: &str) -> Option<String> {
    let lower = hay.to_ascii_lowercase();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let s = lower.find(open.as_str())?;
    let content_start = s + open.len();
    let e = lower[content_start..].find(close.as_str())?;
    Some(hay[content_start..content_start + e].trim().to_string())
}

/// Delete every `<tag>…</tag>` block including its inner text
/// (case-insensitive; unclosed opener deletes through end of string).
fn remove_tag_blocks(s: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = s.to_string();
    loop {
        let lower = out.to_ascii_lowercase();
        let Some(s0) = lower.find(open.as_str()) else {
            break;
        };
        let from = s0 + open.len();
        let e0 = match lower[from..].find(close.as_str()) {
            Some(e) => from + e + close.len(),
            None => out.len(),
        };
        out = format!("{} {}", &out[..s0], &out[e0..]);
    }
    out
}

/// Remove every `<…>` span; a `<` with no closing `>` is kept literally
/// ("a < b" survives).
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        match rest[lt..].find('>') {
            Some(gt) => rest = &rest[lt + gt + 1..],
            None => {
                out.push_str(&rest[lt..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Drop (or unwrap) a leading preamble line: "Here is the cleaned text:",
/// "Cleaned transcript: …", "Cleaned: \"hi\"", "Transcript: …". When the
/// prefix carries the payload after a colon it is kept, otherwise the bare
/// label line is dropped.
fn strip_preamble(s: &str) -> String {
    let mut text = s.to_string();
    for _ in 0..2 {
        let Some(nl) = text.find('\n') else { break };
        let (first, rest) = (&text[..nl], text[nl + 1..].trim_start().to_string());
        let fl = first.trim().to_ascii_lowercase();
        let is_label = fl.starts_with("here is")
            || fl.starts_with("here's")
            || fl.starts_with("cleaned")
            || fl.starts_with("transcript")
            || fl.starts_with("translated")
            || fl.starts_with("translation")
            || fl.starts_with("output")
            || fl.starts_with("result");
        if !is_label {
            // Bare trailing label ("Transcript:") with payload below.
            if first.trim().len() <= 60 && first.trim().ends_with(':') {
                text = rest;
                continue;
            }
            break;
        }
        if let Some(colon) = first.find(':') {
            let after = first[colon + 1..].trim();
            text = if after.is_empty() {
                rest
            } else {
                format!("{after}\n{rest}").trim().to_string()
            };
        } else {
            text = rest;
        }
    }
    // Single-line "Cleaned: hello" (no newline to split on).
    if !text.contains('\n') {
        let tl = text.to_ascii_lowercase();
        for prefix in [
            "cleaned transcript:",
            "cleaned text:",
            "cleaned:",
            "transcript:",
            "output:",
            "result:",
        ] {
            if tl.starts_with(prefix) {
                text = text[prefix.len()..].trim().to_string();
                break;
            }
        }
        if tl.starts_with("here is") || tl.starts_with("here's") {
            if let Some(colon) = text.find(':') {
                text = text[colon + 1..].trim().to_string();
            }
        }
    }
    text.trim().to_string()
}

/// Strip our own echoed input scaffolding: a leading "[Context: …]" header
/// (drop through the closing bracket; unclosed → drop the first line, or
/// everything when header-only) plus the "Transcript to clean:" label (any
/// case). Payload after the label is kept.
fn strip_flat_header(s: &str) -> String {
    let mut text = s.trim_start().to_string();
    if text.to_ascii_lowercase().starts_with("[context:") {
        if let Some(end) = text.find(']') {
            text = text[end + 1..].trim_start().to_string();
        } else if let Some(nl) = text.find('\n') {
            text = text[nl + 1..].trim_start().to_string();
        } else {
            return String::new();
        }
    }
    const LABEL: &str = "transcript to clean:";
    if text.to_ascii_lowercase().starts_with(LABEL) {
        text = text[LABEL.len()..].trim_start().to_string();
    }
    text
}

/// Strip one pair of surrounding quotes: "…" '…' “…” ‘…’.
fn strip_surrounding_quotes(s: &str) -> String {
    let t = s.trim();
    let pairs: &[(&str, &str)] = &[("\"", "\""), ("'", "'"), ("“", "”"), ("‘", "’")];
    for (o, c) in pairs {
        if t.len() >= o.len() + c.len() && t.starts_with(o) && t.ends_with(c) {
            return t[o.len()..t.len() - c.len()].trim().to_string();
        }
    }
    t.to_string()
}

/// Plain transcript for fallback: strips our own `[Context: …]` header (and
/// the legacy `<context>`/`<transcript>` wrappers) so a fallback never
/// injects formatting scaffolding.
fn plain_transcript_of(input: &str) -> String {
    let t = input.trim();
    if let Some(pos) = t.to_ascii_lowercase().find("transcript to clean:") {
        return t[pos + "transcript to clean:".len()..].trim().to_string();
    }
    if t.contains('<') {
        if let Some(inner) = extract_tag_inner(t, "transcript") {
            let rest = strip_tags(&inner);
            if !rest.trim().is_empty() {
                return rest.trim().to_string();
            }
        }
        let stripped = strip_tags(t).trim().to_string();
        if !stripped.is_empty() {
            return stripped;
        }
    }
    t.to_string()
}

/// Two-sentence context via the context model. "" when disabled, no/vague
/// app name, no cloud key, or any failure — never invent (the cleanup
/// proceeds without it). Cloud-only on purpose: on all-local setups a second
/// sequential 1B call costs ~1s and yields hedge-filled text ("unknown
/// field… unclear") that confuses the cleanup SLM, so it is skipped and the
/// plain transcript goes to cleanup.
pub fn synthesize_context(app_name: Option<&str>, cfg: &crate::config::Config) -> String {
    let Some(app) = app_name.filter(|a| !is_vague_app(a)) else {
        return String::new();
    };
    if !cfg.context_enabled {
        return String::new();
    }
    let Some(key) = groq_key() else {
        return String::new();
    };
    let prompt = format!("App: {app}");
    if let Ok(text) = chat_completion(
        "https://api.groq.com/openai/v1/chat/completions",
        &key,
        &cfg.context_model,
        &prompt,
        Some(CONTEXT_PROMPT),
    ) {
        return text;
    }
    String::new()
}

fn truncate(s: &str) -> String {
    if s.len() > 200 {
        format!("{}…", &s[..200])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_quota_error() {
        assert!(is_quota_error("HTTP 429"));
        assert!(is_quota_error("HTTP 402"));
        assert!(is_quota_error("HTTP 401"));
        assert!(!is_quota_error("HTTP 500"));
        assert!(!is_quota_error("network down"));
    }

    #[test]
    fn test_is_filler_result() {
        assert!(is_filler_result("EMPTY"));
        assert!(is_filler_result("  EMPTY  "));
        assert!(is_filler_result(""));
        assert!(!is_filler_result("Hello world."));
    }

    #[test]
    fn test_clean_chain_disabled_and_empty() {
        let cfg = crate::config::Config {
            cleanup_enabled: false,
            ..crate::config::Config::default()
        };
        let out = clean_chain("hello world", &cfg);
        assert_eq!(out.text, "hello world");
        assert!(out.issues.is_empty());
        let cfg = crate::config::Config::default();
        let out = clean_chain("", &cfg);
        assert_eq!(out.text, "");
        assert!(out.issues.is_empty());
    }

    #[test]
    fn test_clean_chain_ollama_provider_live() {
        // Explicit "ollama" provider: never calls the cloud APIs from tests
        // (the real Groq key lives in keys.json — live cloud verification is
        // Task 5's job). Ollama may or may not be running locally.
        let cfg = crate::config::Config {
            cleanup_provider: "ollama".into(),
            ..crate::config::Config::default()
        };
        let out = clean_chain("hello world", &cfg);
        assert!(!out.text.is_empty(), "cleaned text must not be empty");
        eprintln!("chain output: {:?}", out.text);
        if out.issues.is_empty() {
            eprintln!("ollama cleaned it live");
        } else {
            eprintln!("issues: {:?}", out.issues);
        }
    }

    #[test]
    fn test_keys_env_overrides_file() {
        // env wins over keys.json; missing both → None. Uses a throwaway key
        // file written to the REAL app dir (outside repo, gitignored by
        // location). A real keys.json is backed up and restored afterwards.
        let dir = crate::config::app_support_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let keys_path = dir.join("keys.json");
        let backup = std::fs::read_to_string(&keys_path).ok();
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
        match backup {
            Some(orig) => std::fs::write(&keys_path, orig).unwrap(),
            None => {
                let _ = std::fs::remove_file(&keys_path);
            }
        }
    }

    #[test]
    fn test_context_formatting() {
        let out = format_cleanup_input(
            Some("The user is dictating into Firefox. Likely a chat reply."),
            "hello world",
        );
        // Flat SLM-safe syntax — no angle-bracket tags (1B models echo them).
        assert!(out.contains("[Context: The user is dictating into Firefox."));
        assert!(out.contains("Transcript to clean: hello world"));
        assert!(!out.contains('<'));
        let bare = format_cleanup_input(None, "hello world");
        assert_eq!(bare, "hello world"); // no context → plain transcript
        // Vague context → plain transcript (no header injected).
        assert_eq!(format_cleanup_input(Some(""), "hi"), "hi");
        assert_eq!(format_cleanup_input(Some("unknown"), "hi"), "hi");
        assert_eq!(format_cleanup_input(Some("  Generic "), "hi"), "hi");
    }

    #[test]
    fn test_synthesize_context_disabled_or_missing_app() {
        let cfg = crate::config::Config::default();
        // No app name → empty context (never invent — prompt rule).
        assert_eq!(synthesize_context(None, &cfg), "");
        // Vague/generic apps skip the second LLM call entirely (latency).
        assert_eq!(synthesize_context(Some(""), &cfg), "");
        assert_eq!(synthesize_context(Some("unknown"), &cfg), "");
        assert_eq!(synthesize_context(Some("Generic"), &cfg), "");
    }

    #[test]
    fn test_sanitize_nested_input_tags() {
        let raw = "<Input><transcript>hello world</transcript><translated>hola mundo</translated></Input>";
        assert_eq!(sanitize_llm_response(raw, "fallback"), "hello world");
    }

    #[test]
    fn test_sanitize_strips_context_tags() {
        assert_eq!(
            sanitize_llm_response("<context>note about mail</context> hello world", "fallback"),
            "hello world"
        );
        assert_eq!(
            sanitize_llm_response("hello world </context> <Input>", "fallback"),
            "hello world"
        );
    }

    #[test]
    fn test_sanitize_preamble_quotes_and_fences() {
        assert_eq!(
            sanitize_llm_response("Cleaned: \"hello world\"", "fallback"),
            "hello world"
        );
        assert_eq!(
            sanitize_llm_response("Here is the cleaned text:\nhello world", "fallback"),
            "hello world"
        );
        assert_eq!(
            sanitize_llm_response("```hello world```", "fallback"),
            "hello world"
        );
        assert_eq!(
            sanitize_llm_response("<transcript>\"hello world\"</transcript>", "fallback"),
            "hello world"
        );
    }

    #[test]
    fn test_sanitize_strips_flat_header_echo() {
        // Exact user repro: 1B model echoed the flat input scaffolding back.
        let raw = "[Context: The user is working in an Electron-based application, but no specific window title, document, or content is visible. They are likely dictating text into an unknown field, and the specific topic, tone, and any names are unclear.]\nTranscript to clean: Hello there, today's plan is to make everything faster on v-flow.";
        assert_eq!(
            sanitize_llm_response(raw, "fallback"),
            "Hello there, today's plan is to make everything faster on v-flow."
        );
        // Label-only echo, mixed case.
        assert_eq!(
            sanitize_llm_response("TRANSCRIPT TO CLEAN: hello world", "fallback"),
            "hello world"
        );
        // Header without closing bracket still stripped, not injected.
        assert_eq!(
            sanitize_llm_response("[Context: some note\nhello world", "fallback"),
            "hello world"
        );
    }

    #[test]
    fn test_vague_synthesized_context_dropped() {
        // Synthesized paragraph full of hedges carries no signal → bare transcript.
        let vague = "The user is working in an Electron-based application, but no specific window title, document, or content is visible. They are likely dictating text into an unknown field, and the specific topic, tone, and any names are unclear.";
        assert_eq!(format_cleanup_input(Some(vague), "hi"), "hi");
        // End-to-end: vague context never becomes an injectable header.
        let input = format_cleanup_input(Some(vague), "hello world");
        assert_eq!(input, "hello world");
        assert_eq!(sanitize_llm_response(&input, &input), "hello world");
    }

    #[test]
    fn test_synthesize_context_skips_local_without_cloud_key() {
        // All-local setups must not burn a second sequential 1B call for
        // context (latency + vague text). Gated: only asserts when no cloud
        // key exists here (live-key machines skip like other live tests).
        if groq_key().is_some() {
            eprintln!("skipped (groq key present here)");
            return;
        }
        if !ollama_reachable() {
            eprintln!("skipped (ollama not running here)");
            return;
        }
        let cfg = crate::config::Config {
            context_enabled: true,
            ..crate::config::Config::default()
        };
        assert_eq!(synthesize_context(Some("Electron"), &cfg), "");
    }

    #[test]
    fn test_save_key_roundtrip_preserves_sibling() {
        let p = std::env::temp_dir().join("wiflow_keys_test.json");
        let _ = std::fs::remove_file(&p);
        save_key_to(&p, "groq_api_key", "gsk-test").unwrap();
        save_key_to(&p, "openrouter_api_key", "sk-or-test").unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["groq_api_key"], "gsk-test");
        assert_eq!(v["openrouter_api_key"], "sk-or-test");
        // Overwrite one, sibling untouched; clear removes the field.
        save_key_to(&p, "groq_api_key", "gsk-new").unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["groq_api_key"], "gsk-new");
        assert_eq!(v["openrouter_api_key"], "sk-or-test");
        save_key_to(&p, "groq_api_key", "").unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v.get("groq_api_key").is_none());
        assert_eq!(v["openrouter_api_key"], "sk-or-test");
        // Corrupt file never blocks a save (starts fresh, keeps valid keys).
        std::fs::write(&p, "{nope").unwrap();
        save_key_to(&p, "groq_api_key", "gsk-again").unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["groq_api_key"], "gsk-again");
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_parse_dialog_text() {
        assert_eq!(
            parse_dialog_text("text returned:sk-abc123, button returned:OK"),
            Some("sk-abc123".into())
        );
        assert_eq!(parse_dialog_text(""), None); // cancel / empty
        assert_eq!(parse_dialog_text("text returned:  , button returned:OK"), None);
        assert_eq!(parse_dialog_text("button returned:Cancel"), None);
    }

    #[test]
    fn test_sanitize_medium_complex_sentences() {
        // Medium-length complex dictation survives the pipeline byte-identical
        // when the model behaves; scaffolding echoes get stripped.
        let cases = [
            "Although the quarterly report, which Priya finalized late last night, shows revenue up twenty percent, we still need to cut travel before Thursday.",
            "When you finish the migration, which should take about three hours if the database cooperates, please tag release two point one and notify the on-call engineer.",
            "Because the client moved the deadline, itself already tight after the scope change, let's meet Wednesday after lunch instead of Thursday morning.",
        ];
        for want in cases {
            // Well-behaved model output passes through untouched.
            assert_eq!(sanitize_llm_response(want, "fallback"), want);
            // 1B echo with header + wrapper tags still yields the sentence.
            let echo = format!(
                "[Context: The user is dictating an email.]\nTranscript to clean: <transcript>\"{want}\"</transcript>"
            );
            assert_eq!(sanitize_llm_response(&echo, "fallback"), want);
            // Preamble + fences + quotes around a complex sentence.
            let chatty = format!("Here is the cleaned text:\n```\"{want}\"```");
            assert_eq!(sanitize_llm_response(&chatty, "fallback"), want);
        }
    }

    #[test]
    fn test_sanitize_empty_and_sentinel() {
        // Empty/corrupted → deterministic fallback (never inject tags).
        assert_eq!(sanitize_llm_response("", "hello world"), "hello world");
        assert_eq!(sanitize_llm_response("   ", "hello world"), "hello world");
        assert_eq!(sanitize_llm_response("<Input></Input>", "hello world"), "hello world");
        assert_eq!(sanitize_llm_response("!!! ???", "hello world"), "hello world");
        // Sentinel passes through so the caller drops filler instead of injecting it.
        assert_eq!(sanitize_llm_response("EMPTY", "um uh"), "EMPTY");
        assert_eq!(sanitize_llm_response("  EMPTY  ", "um uh"), "EMPTY");
        // Fallback never carries our own formatting header.
        let formatted =
            format_cleanup_input(Some("mail context here"), "hello world");
        assert_eq!(
            sanitize_llm_response("", &formatted),
            "hello world"
        );
        assert_eq!(
            sanitize_llm_response("<Input></Input>", &formatted),
            "hello world"
        );
    }

    #[test]
    fn test_check_ollama_ready_live() {
        // Live: Ollama may or may not be running (CI skips honestly).
        match check_ollama_ready("llama3.2:1b") {
            Ok(()) => {} // running + model installed
            Err(e) => eprintln!("skipped (ollama not ready here): {e}"),
        }
    }
}
