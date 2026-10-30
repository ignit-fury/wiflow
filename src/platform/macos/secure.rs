//! Password-field detection (prd.md §6): dictation is never typed into one.
//!
//! Main signal: the focused element's Accessibility role/subrole is
//! `AXSecureTextField` (native and most browser password inputs). Backup when
//! Accessibility cannot tell: macOS secure input, which password fields turn
//! on. Secure input alone is not trusted when Accessibility answers, because
//! some apps keep it on all the time (Terminal "Secure Keyboard Entry").

use std::ffi::c_void;

type CFTypeRef = *const c_void;

const SECURE_FIELD: &str = "AXSecureTextField";
const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8
const AX_OK: i32 = 0;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateSystemWide() -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(
        element: CFTypeRef,
        attribute: CFTypeRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(element: CFTypeRef, seconds: f32) -> i32;
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        alloc: CFTypeRef,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        external: u8,
    ) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut u8, size: isize, encoding: u32) -> u8;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFRelease(cf: CFTypeRef);
}

/// True when dictation must not be typed: a password field has focus.
pub fn password_field_focused() -> bool {
    // SAFETY: plain Carbon query, no arguments.
    let secure_input = unsafe { IsSecureEventInputEnabled() } != 0;
    let ax = focused_is_secure_field();
    let block = should_block(ax, secure_input);
    if block {
        tracing::info!(
            "password field detected (accessibility: {ax:?}, secure input: {secure_input})"
        );
    }
    block
}

/// `ax`: what Accessibility says about the focused element (None = unknown).
fn should_block(ax: Option<bool>, secure_input: bool) -> bool {
    ax.unwrap_or(secure_input)
}

fn focused_is_secure_field() -> Option<bool> {
    // SAFETY: every CF object created or copied here is released once.
    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return None;
        }
        // A hung target app must not stall the paste.
        AXUIElementSetMessagingTimeout(system, 0.25);
        let focused = copy_attribute(system, "AXFocusedUIElement");
        CFRelease(system);
        let focused = focused?;
        let role = copy_string_attribute(focused, "AXRole");
        let subrole = copy_string_attribute(focused, "AXSubrole");
        CFRelease(focused);
        if role.is_none() && subrole.is_none() {
            return None;
        }
        Some([role, subrole].iter().flatten().any(|v| v == SECURE_FIELD))
    }
}

unsafe fn cf_string(s: &str) -> CFTypeRef {
    CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0)
}

/// Copied value (caller releases), or None on any Accessibility error.
unsafe fn copy_attribute(element: CFTypeRef, name: &str) -> Option<CFTypeRef> {
    let attr = cf_string(name);
    if attr.is_null() {
        return None;
    }
    let mut value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, attr, &mut value);
    CFRelease(attr);
    (err == AX_OK && !value.is_null()).then_some(value)
}

unsafe fn copy_string_attribute(element: CFTypeRef, name: &str) -> Option<String> {
    let value = copy_attribute(element, name)?;
    let mut out = None;
    if CFGetTypeID(value) == CFStringGetTypeID() {
        let mut buf = [0u8; 128];
        if CFStringGetCString(value, buf.as_mut_ptr(), buf.len() as isize, UTF8) != 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            out = Some(String::from_utf8_lossy(&buf[..end]).into_owned());
        }
    }
    CFRelease(value);
    out
}

#[cfg(test)]
mod tests {
    use super::should_block;

    #[test]
    fn accessibility_answer_wins_over_secure_input() {
        assert!(should_block(Some(true), false), "password field");
        assert!(
            !should_block(Some(false), true),
            "Terminal Secure Keyboard Entry"
        );
        assert!(should_block(None, true), "unknown element, secure input on");
        assert!(!should_block(None, false));
    }
}
