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

/// Accessibility permission granted (paste needs it).
pub fn accessibility_trusted() -> bool {
    // SAFETY: plain query, no arguments.
    unsafe { AXIsProcessTrusted() != 0 }
}
