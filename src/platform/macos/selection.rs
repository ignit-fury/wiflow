//! Read the text selected in the focused app (voice-edit input).
//!
//! Accessibility first (`AXSelectedText`: no keystroke, nothing touched).
//! Many web and Electron text boxes do not expose it, so fall back to
//! copying with Cmd+C and restoring the clipboard.

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
    pick(
        super::secure::focused_string_attribute("AXSelectedText"),
        copy_selection_with_cmd_c,
    )
}

/// An AX answer is final, even an empty one: Cmd+C with no selection copies
/// the whole line in code editors. Cmd+C only when AX has no answer at all.
fn pick(ax: Option<String>, cmd_c: impl FnOnce() -> Option<String>) -> Option<String> {
    match ax {
        Some(text) => usable(text),
        None => cmd_c().and_then(usable),
    }
}

/// Current selection by Accessibility only (never a keystroke). `None` when
/// AX cannot say.
pub fn selected_text_ax() -> Option<String> {
    super::secure::focused_string_attribute("AXSelectedText")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_selection_is_not_usable() {
        assert_eq!(usable("  \n ".into()), None);
        assert_eq!(usable(String::new()), None);
        assert_eq!(usable("hi there".into()).as_deref(), Some("hi there"));
    }

    #[test]
    fn ax_answer_is_final_and_cmd_c_only_runs_without_one() {
        let boom = || -> Option<String> { panic!("Cmd+C must not run") };
        assert_eq!(pick(Some(String::new()), boom), None);
        assert_eq!(pick(Some("sel".into()), boom).as_deref(), Some("sel"));
        assert_eq!(
            pick(None, || Some("copied".into())).as_deref(),
            Some("copied")
        );
        assert_eq!(pick(None, || Some("  ".into())), None);
    }

    /// Needs a front app with text selected. Run: cargo test cmd_c_fallback -- --ignored --nocapture
    #[test]
    #[ignore]
    fn cmd_c_fallback_reads_the_selection() {
        let text = copy_selection_with_cmd_c();
        eprintln!("COPIED {text:?}");
        assert!(text.is_some_and(|t| !t.trim().is_empty()));
    }
}
