pub mod context;
pub mod duck;
pub mod inject;
pub mod media;
pub mod panel;
pub mod secure;
pub mod tap;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
}

/// Input Monitoring granted. Without it the Right Option / Fn tap only
/// sees keys while Wiflow itself is the front app.
pub fn input_monitoring_granted() -> bool {
    // SAFETY: plain query, no arguments.
    unsafe { CGPreflightListenEventAccess() }
}

/// Ask macOS for Input Monitoring (system prompt once, then adds Wiflow to
/// the list in System Settings). Returns the current grant.
pub fn request_input_monitoring() -> bool {
    // SAFETY: plain request, no arguments.
    unsafe { CGRequestListenEventAccess() }
}

/// Accessibility permission granted (paste needs it).
pub fn accessibility_trusted() -> bool {
    // SAFETY: plain query, no arguments.
    unsafe { AXIsProcessTrusted() != 0 }
}
