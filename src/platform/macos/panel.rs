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

use objc2::rc::Retained;
use objc2_app_kit::{
    NSFloatingWindowLevel, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

/// Resolve the winit window's NSWindow (borrowed view → owning window).
/// winit hands us the NSView; `-[NSView window]` resolves the NSWindow
/// (Retained: alive for the calls below). Main thread only (AppKit rule).
fn ns_window_of(window: &Window) -> Retained<NSWindow> {
    let handle = window.window_handle().expect("pill window handle").as_raw();
    let RawWindowHandle::AppKit(handle) = handle else {
        panic!("pill panel: non-AppKit handle — focus safety unavailable");
    };
    let ns_view: &NSView = unsafe { &*handle.ns_view.as_ptr().cast() };
    ns_view.window().expect("pill view has a window")
}

/// Configure an existing winit window as a non-activating floating pill:
///
/// - `NonactivatingPanel` style mask: showing/keying the window never
///   activates the app or steals key focus (the H21 hard requirement).
/// - Floating level: renders above normal windows incl. fullscreen spaces
///   handling via collection behavior.
/// - Ignores mouse events: clicks pass through (display-only surface).
/// - Stays visible across Spaces; never hides on app deactivate.
pub fn configure_pill_panel(window: &Window) {
    let ns_window = ns_window_of(window);
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

/// Show the pill window WITHOUT activating the app.
///
/// winit's `set_visible(true)` calls `makeKeyAndOrderFront:` unconditionally
/// (winit 0.30 macOS backend) — key status FORCES activation no matter what
/// style mask is set. `orderFront:` shows the (already non-activating,
/// click-through) panel without key status, so focus never moves. This is
/// the entire focus-safety mechanism; `GlWindow::show` must never be used
/// for the pill. Main thread only.
pub fn order_front_without_activating(window: &Window) {
    let ns_window = ns_window_of(window);
    ns_window.orderFront(None);
}
