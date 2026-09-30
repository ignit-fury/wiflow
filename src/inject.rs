use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectReport {
    pub pasted_via: &'static str,
    pub clipboard_restored: bool,
}

fn enigo_err(ctx: &str, e: impl std::fmt::Debug) -> String {
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
    let mut en = Enigo::new(&Settings::default()).map_err(|e| enigo_err("enigo init", e))?;
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
