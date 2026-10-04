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
