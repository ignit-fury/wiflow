//! Non-activating floating-panel configuration for the recording pill.
//!
//! A winit window ALWAYS activates its app on show — fatal for a dictation
//! pill (focus would leave the target app on every press). The fix is one
//! AppKit style-mask bit (`NSNonactivatingPanelMask`) plus a floating level
//! and click-through: no new windows, no second process, no focus theft.
//!
//! MUST run on the main (winit event-loop) thread, right after the window
//! is created and before it is shown. winit exposes no non-activating API,
//! hence the small objc2 surface here (preferred over a hand-rolled
//! `objc_msgSend` — same calls, checked signatures).

use objc2_app_kit::{NSFloatingWindowLevel, NSView, NSWindowCollectionBehavior, NSWindowStyleMask};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

/// Configure an existing winit window as a non-activating floating pill:
///
/// - `NonactivatingPanel` style mask: showing/keying the window never
///   activates the app or steals key focus (the H21 hard requirement).
/// - Floating level: renders above normal windows incl. fullscreen spaces
///   handling via collection behavior.
/// - Ignores mouse events: clicks pass through (display-only surface).
/// - Stays visible across Spaces; never hides on app deactivate.
pub fn configure_pill_panel(window: &Window) {
    let handle = window.window_handle().expect("pill window handle").as_raw();
    let RawWindowHandle::AppKit(handle) = handle else {
        tracing::warn!("pill panel: non-AppKit handle — focus safety unavailable");
        return;
    };
    // Borrowed view → owning window handle. winit hands us the NSView;
    // `-[NSView window]` resolves the NSWindow (Retained: alive for the
    // configuration calls below).
    let ns_view: &NSView = unsafe { &*handle.ns_view.as_ptr().cast() };
    let ns_window = ns_view.window().expect("pill view has a window");
    ns_window.setStyleMask(ns_window.styleMask() | NSWindowStyleMask::NonactivatingPanel);
    ns_window.setLevel(NSFloatingWindowLevel);
    ns_window.setIgnoresMouseEvents(true);
    ns_window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    ns_window.setHidesOnDeactivate(false);
    tracing::info!("pill panel: non-activating floating configured");
}
