/// Return the name of the currently focused (frontmost) application via
/// `osascript`. `None` when the call fails or returns an empty string.
pub fn focused_app_name() -> Option<String> {
    let out = std::process::Command::new("osascript")
        .args(["-e", "tell application \"System Events\" to get name of first application process whose frontmost is true"])
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

use crate::core::traits::ContextProvider;

/// Production `ContextProvider`: osascript System Events query. Lives here
/// (not in core) per H25 — core owns the trait.
pub struct OsascriptContext;

impl ContextProvider for OsascriptContext {
    fn focused_app(&self) -> Option<String> {
        focused_app_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_osascript_context_returns_app_or_none() {
        let ctx = OsascriptContext;
        // In CI/headless this may return None; on a desktop it returns Some.
        // Either is valid — the contract is Option<String>.
        let _ = ctx.focused_app();
    }
}
