use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// The literal dictation cleanup layer system prompt (verbatim, user-provided).
const SYSTEM_PROMPT: &str = include_str!("cleanup_prompt.txt");

pub const DEFAULT_ENDPOINT: &str = "http://localhost:11434";
pub const DEFAULT_MODEL: &str = "llama3.2:1b";
/// Filler-only sentinel the system prompt returns for empty/filler input.
pub const EMPTY_SENTINEL: &str = "EMPTY";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Build the Ollama /api/generate request body (pure, tested).
pub fn build_request_body(transcript: &str, model: &str) -> String {
    json!({
        "model": model,
        "system": SYSTEM_PROMPT,
        "prompt": transcript,
        "stream": false,
    })
    .to_string()
}

/// Parse Ollama's non-streaming response: {"response": "..."} (pure, tested).
pub fn parse_response(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("response")?
        .as_str()
        .map(|s| s.trim().to_string())
}

/// True when the cleaned text is the filler-only sentinel (or empty).
pub fn is_filler_result(cleaned: &str) -> bool {
    cleaned.trim() == EMPTY_SENTINEL || cleaned.trim().is_empty()
}

/// LLM cleanup via Ollama (local, $0). NEVER fails the pipeline: on skip
/// (disabled), unreachable Ollama, timeout, or parse error the input is
/// returned unchanged — the deterministic post_process output stands.
pub fn clean(transcript: &str, enabled: bool, model: &str, endpoint: &str) -> String {
    if !enabled || transcript.trim().is_empty() {
        return transcript.to_string();
    }
    match ollama_generate(transcript, model, endpoint) {
        Ok(response) => {
            if response.is_empty() {
                tracing::warn!("cleanup returned empty response, keeping deterministic output");
                transcript.to_string()
            } else {
                response
            }
        }
        Err(e) => {
            tracing::warn!(
                "cleanup skipped ({e}) — install Ollama + `ollama pull {model}` to enable"
            );
            transcript.to_string()
        }
    }
}

fn ollama_generate(transcript: &str, model: &str, endpoint: &str) -> Result<String, String> {
    let (host, port) = parse_endpoint(endpoint)?;
    let body = build_request_body(transcript, model);
    let mut stream = TcpStream::connect_timeout(
        &format!("{host}:{port}")
            .parse()
            .map_err(|e| format!("bad endpoint: {e:?}"))?,
        CONNECT_TIMEOUT,
    )
    .map_err(|e| format!("ollama unreachable at {host}:{port}: {e}"))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(READ_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let req = format!(
        "POST /api/generate HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .map_err(|e| format!("read: {e}"))?;
    // Split headers/body at the blank line; chunked encoding handled by
    // reading to EOF (Connection: close).
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&raw);
    parse_response(body).ok_or_else(|| format!("unparseable response: {}", truncate(body)))
}

/// "http://localhost:11434" → ("localhost", 11434). Only http (localhost).
fn parse_endpoint(endpoint: &str) -> Result<(String, u16), String> {
    let rest = endpoint
        .strip_prefix("http://")
        .ok_or("endpoint must be http:// (localhost)")?;
    match rest.rsplit_once(':') {
        Some((h, p)) => Ok((
            h.to_string(),
            p.parse().map_err(|e| format!("bad port: {e:?}"))?,
        )),
        None => Ok((rest.to_string(), 11434)),
    }
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
    fn test_build_request_body_shape() {
        let body = build_request_body("hello world", "llama3.2:1b");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["model"], "llama3.2:1b");
        assert_eq!(v["prompt"], "hello world");
        assert_eq!(v["stream"], false);
        assert!(v["system"]
            .as_str()
            .unwrap()
            .starts_with("You are a literal dictation cleanup layer"));
    }

    #[test]
    fn test_parse_response() {
        assert_eq!(
            parse_response(r#"{"response":"Hello world.","done":true}"#),
            Some("Hello world.".into())
        );
        assert_eq!(parse_response("not json"), None);
        assert_eq!(parse_response(r#"{"no_response":1}"#), None);
    }

    #[test]
    fn test_is_filler_result() {
        assert!(is_filler_result("EMPTY"));
        assert!(is_filler_result("  EMPTY  "));
        assert!(is_filler_result(""));
        assert!(!is_filler_result("Hello world."));
    }

    #[test]
    fn test_parse_endpoint() {
        assert_eq!(
            parse_endpoint("http://localhost:11434").unwrap(),
            ("localhost".into(), 11434)
        );
        assert_eq!(
            parse_endpoint("http://localhost").unwrap(),
            ("localhost".into(), 11434)
        );
        assert!(parse_endpoint("https://localhost").is_err());
    }

    #[test]
    fn test_clean_skip_paths() {
        // Disabled → input unchanged.
        assert_eq!(
            clean("hello", false, "m", "http://localhost:11434"),
            "hello"
        );
        // Empty transcript → unchanged (no LLM call).
        assert_eq!(clean("", true, "m", "http://localhost:11434"), "");
        // Unreachable Ollama (nothing listens on 11434 in tests) → unchanged.
        assert_eq!(
            clean("hello world", true, "llama3.2:1b", "http://localhost:11434"),
            "hello world"
        );
    }
}
