//! Native macOS notifications for wiflow (S4): transcription-complete
//! previews, errors, and permission prompts via Notification Center.
//!
//! Failure contract (H23): notification delivery must NEVER fail a
//! dictation session. Every caller ignores `Err` after the existing
//! tray-note fallback fires — the tray note is the guarantee, the banner
//! is best-effort. The osascript binary path is injectable so the failure
//! path is unit-testable without touching /usr/bin/osascript.

/// Maximum notification body length (Notification Center wraps ~3 lines;
/// longer transcripts get a capped preview, never a wall of text).
pub const MAX_BODY_CHARS: usize = 240;

/// Build the AppleScript source (pure — tested). Quotes/backslashes in user
/// transcripts are escaped so dictation content can never break out of the
/// string literal.
pub fn build_script(title: &str, body: &str) -> String {
    let body: String = body.chars().take(MAX_BODY_CHARS).collect();
    let body = if body.chars().count() >= MAX_BODY_CHARS {
        format!("{body}…")
    } else {
        body
    };
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "display notification \"{}\" with title \"{}\"",
        esc(&body),
        esc(title)
    )
}

/// Fire-and-forget notification. `Ok` = banner requested (delivery itself
/// is up to Notification Center / user settings); `Err` = osascript missing
/// or failed — the caller falls back to the tray note.
pub fn notify(title: &str, body: &str) -> Result<(), String> {
    notify_with(title, body, "osascript")
}

/// Same, with an injectable binary path (tests pass a bogus path to pin
/// the failure contract without touching the system binary).
pub fn notify_with(title: &str, body: &str, osascript: &str) -> Result<(), String> {
    let script = build_script(title, body);
    std::process::Command::new(osascript)
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("osascript spawn failed: {e}"))
        .and_then(|out| {
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "osascript failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_body_truncates_long_transcripts() {
        let long = "word ".repeat(100); // 500 chars
        let script = build_script("wiflow", &long);
        assert!(
            script.chars().count() <= MAX_BODY_CHARS + 100,
            "capped preview, got {} chars",
            script.chars().count()
        );
        assert!(script.contains('…'), "truncation is marked");
        let short = "hello world";
        assert!(
            build_script("wiflow", short).contains(short),
            "short bodies pass through intact"
        );
    }

    #[test]
    fn notification_script_escapes_quotes() {
        let evil = r#"say "hi" \ bye"#;
        let script = build_script("t", evil);
        // The AppleScript string literal must survive verbatim content:
        // exactly one display-notification statement, quotes escaped.
        assert_eq!(script.matches("display notification").count(), 1);
        assert!(script.contains("\\\"hi\\\""), "quotes escaped: {script}");
        assert!(script.contains("\\\\ bye"), "backslash escaped: {script}");
    }

    #[test]
    fn notify_failure_never_fails_session() {
        // Bogus binary → Err. The SESSION contract (H23) is that callers
        // ignore this and fall back to the tray note — pinned here at the
        // unit level; the call-site pattern (`let _ = …` + existing note)
        // is reviewed, not re-tested.
        let r = notify_with("wiflow", "test", "/nonexistent/osascript-xyz");
        assert!(r.is_err(), "missing binary must Err, not panic");
    }
}
