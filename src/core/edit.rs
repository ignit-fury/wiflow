//! Voice-edit: rewrite the selected text by a spoken instruction.
//!
//! Pure logic plus one provider call. The worker reads the selection and
//! passes it in; nothing here touches the OS (rules.md §3).

use crate::core::config::Config;

const EDIT_SYSTEM_PROMPT: &str = include_str!("../edit_prompt.txt");

/// About 250 words. The Ollama output cap (400 tokens) cannot hold a longer
/// rewrite, so longer selections are refused up front.
pub const MAX_SELECTION_CHARS: usize = 1500;

pub fn build_user_message(instruction: &str, selection: &str) -> String {
    format!(
        "INSTRUCTION: {}\n\nTEXT:\n{}",
        instruction.trim(),
        selection
    )
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
        assert_eq!(check_result("\"Hello there\"", "\"Hi\"").unwrap(), "\"Hi\"");
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
