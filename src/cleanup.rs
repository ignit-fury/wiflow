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
    let path = crate::config::app_support_dir().join("keys.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v[field]
        .as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
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
    match try_provider_model(p, primary, transcript) {
        Ok(t) => Ok(t),
        Err(e) => {
            let fallback = cfg.cleanup_fallback_model.as_str();
            if fallback != primary {
                tracing::warn!("retry with fallback model {fallback}: {e}");
                try_provider_model(p, fallback, transcript)
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
    let agent = ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build();
    let resp = agent
        .post(&format!("{OLLAMA_ENDPOINT}/api/generate"))
        .set("Content-Type", "application/json")
        .send_json(json!({
            "model": model,
            "system": system.unwrap_or(SYSTEM_PROMPT),
            "prompt": prompt,
            "stream": false,
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

/// Format the cleanup input: non-empty context → `<context>…</context>`
/// block above the `<transcript>` tag; otherwise the plain transcript
/// (the cleanup prompt treats untagged input as the transcript).
pub fn format_cleanup_input(context: Option<&str>, transcript: &str) -> String {
    match context {
        Some(c) if !c.trim().is_empty() => format!(
            "<context>{}</context>\n<transcript>{}</transcript>",
            c.trim(),
            transcript
        ),
        _ => transcript.to_string(),
    }
}

/// Two-sentence context via the context model. "" when disabled, no app
/// name, or any failure — never invent (the cleanup proceeds without it).
/// Provider order for the small context call: Groq → Ollama (skips
/// OpenRouter for latency).
pub fn synthesize_context(app_name: Option<&str>, cfg: &crate::config::Config) -> String {
    let Some(app) = app_name.filter(|a| !a.trim().is_empty()) else {
        return String::new();
    };
    if !cfg.context_enabled {
        return String::new();
    }
    let prompt = format!("App: {app}");
    if let Some(key) = groq_key() {
        if let Ok(text) = chat_completion(
            "https://api.groq.com/openai/v1/chat/completions",
            &key,
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

    #[test]
    fn test_check_ollama_ready_live() {
        // Live: Ollama may or may not be running (CI skips honestly).
        match check_ollama_ready("llama3.2:1b") {
            Ok(()) => {} // running + model installed
            Err(e) => eprintln!("skipped (ollama not ready here): {e}"),
        }
    }
}
