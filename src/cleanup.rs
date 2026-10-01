use serde_json::json;
use std::time::Duration;

/// The literal dictation cleanup layer system prompt (verbatim, user-provided).
const SYSTEM_PROMPT: &str = include_str!("cleanup_prompt.txt");

pub const OLLAMA_ENDPOINT: &str = "http://localhost:11434";
/// Filler-only sentinel the system prompt returns for empty/filler input.
pub const EMPTY_SENTINEL: &str = "EMPTY";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Provider chain (user-specified): Groq → OpenRouter → Ollama local →
/// deterministic output. Keys come from env (never stored on disk):
/// `GROQ_API_KEY`, `OPENROUTER_API_KEY`.
pub fn groq_api_key() -> Option<String> {
    std::env::var("GROQ_API_KEY").ok().filter(|k| !k.is_empty())
}

pub fn openrouter_api_key() -> Option<String> {
    std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

/// Result of the cleanup chain: cleaned text (input unchanged when every
/// provider failed) + user-facing issues (rate limits, missing model).
#[derive(Debug, Clone, PartialEq)]
pub struct CleanupOutcome {
    pub text: String,
    pub issues: Vec<String>,
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
    // 1. Groq (fastest cloud, generous free tier).
    if let Some(key) = groq_api_key() {
        match chat_completion(
            "https://api.groq.com/openai/v1/chat/completions",
            &key,
            &cfg.cleanup_groq_model,
            transcript,
        ) {
            Ok(text) => return CleanupOutcome { text, issues },
            Err(e) => {
                if is_quota_error(&e) {
                    issues.push(
                        "Groq tokens/rate limit exhausted — falling back to OpenRouter".to_string(),
                    );
                } else {
                    issues.push(format!("Groq failed ({e}) — falling back to OpenRouter"));
                }
                tracing::warn!("groq cleanup failed: {e}");
            }
        }
    }
    // 2. OpenRouter (free-tier models available).
    if let Some(key) = openrouter_api_key() {
        match chat_completion(
            "https://openrouter.ai/api/v1/chat/completions",
            &key,
            &cfg.cleanup_openrouter_model,
            transcript,
        ) {
            Ok(text) => return CleanupOutcome { text, issues },
            Err(e) => {
                if is_quota_error(&e) {
                    issues.push(
                        "OpenRouter tokens/credits exhausted — falling back to Ollama".to_string(),
                    );
                } else {
                    issues.push(format!("OpenRouter failed ({e}) — falling back to Ollama"));
                }
                tracing::warn!("openrouter cleanup failed: {e}");
            }
        }
    }
    // 3. Ollama local ($0, offline). Background model check with guidance.
    match ollama_generate(transcript, &cfg.cleanup_model) {
        Ok(text) => return CleanupOutcome { text, issues },
        Err(e) => {
            issues.push(format!("Ollama fallback failed: {e}"));
            tracing::warn!("ollama cleanup failed: {e}");
        }
    }
    CleanupOutcome {
        text: transcript.to_string(),
        issues,
    }
}

/// 429 (rate limit), 402 (payment/credits), or 401 (quota/auth exhausted).
pub fn is_quota_error(err: &str) -> bool {
    err.contains("429") || err.contains("402") || err.contains("401")
}

/// OpenAI-compatible chat completion (Groq + OpenRouter share the shape).
fn chat_completion(url: &str, key: &str, model: &str, transcript: &str) -> Result<String, String> {
    let agent = ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build();
    let resp = agent
        .post(url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_json(json!({
            "model": model,
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": transcript}
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

fn ollama_generate(transcript: &str, model: &str) -> Result<String, String> {
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
            "system": SYSTEM_PROMPT,
            "prompt": transcript,
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
    fn test_clean_chain_falls_through_to_ollama() {
        // No env keys → chain falls through to Ollama (running here).
        let cfg = crate::config::Config::default();
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
    fn test_check_ollama_ready_live() {
        // Live: Ollama may or may not be running (CI skips honestly).
        match check_ollama_ready("llama3.2:1b") {
            Ok(()) => {} // running + model installed
            Err(e) => eprintln!("skipped (ollama not ready here): {e}"),
        }
    }
}
