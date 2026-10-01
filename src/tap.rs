use crate::daemon::{DaemonEvent, HotkeyPreset};
use winit::event_loop::EventLoopProxy;

// CoreGraphics / CoreFoundation FFI — zero new dependencies.
// CGEventTap intercepts RAW keyboard events (including bare modifier
// flagsChanged) that RegisterEventHotKey cannot see.
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: i32,
        place: i32,
        options: i32,
        mask: u64,
        callback: usize,
        user_info: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn CGEventTapEnable(tap: *mut std::ffi::c_void, enable: bool);
    fn CFMachPortCreateRunLoopSource(
        alloc: *mut std::ffi::c_void,
        port: *mut std::ffi::c_void,
        order: i64,
    ) -> *mut std::ffi::c_void;
    fn CFRunLoopAddSource(
        rl: *mut std::ffi::c_void,
        source: *mut std::ffi::c_void,
        mode: *const std::ffi::c_void,
    );
    fn CFRunLoopGetCurrent() -> *mut std::ffi::c_void;
    fn CFRunLoopRun();
    fn CFMachPortInvalidate(port: *mut std::ffi::c_void);
    fn CGEventGetIntegerValueField(event: *mut std::ffi::c_void, field: i32) -> i64;
    fn CGEventGetFlags(event: *mut std::ffi::c_void) -> u64;
    static kCFRunLoopDefaultMode: *const std::ffi::c_void;
}

const K_CG_SESSION_EVENT_TAP: i32 = 1;
const K_CG_HEAD_INSERT_EVENT_TAP: i32 = 0;
const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: i32 = 1;
/// kCGEventFlagsChanged
const EVENT_TYPE_FLAGS_CHANGED: u32 = 12;
/// kCGKeyboardEventKeycode field id
const FIELD_KEYCODE: i32 = 9;
/// kVK_RightOption
pub const KEYCODE_RIGHT_OPTION: u32 = 61;
/// kVK_Function (globe/Fn key)
pub const KEYCODE_FN: u32 = 63;
/// CGEventFlags maskAlternate (option key down indicator)
const OPTION_FLAGS: u64 = 0x0008_0000;
/// CGEventFlags maskFunction (Fn key down indicator)
const FUNCTION_FLAGS: u64 = 0x0080_0000;

/// Which bare-modifier keycodes a preset watches (None = combo preset,
/// those ride global-hotkey instead).
pub fn keycode_for_preset(preset: HotkeyPreset) -> Option<u32> {
    match preset {
        HotkeyPreset::RightOption => Some(KEYCODE_RIGHT_OPTION),
        HotkeyPreset::Fn => Some(KEYCODE_FN),
        _ => None,
    }
}

pub fn flags_mask_for_keycode(keycode: u32) -> u64 {
    if keycode == KEYCODE_RIGHT_OPTION {
        OPTION_FLAGS
    } else {
        FUNCTION_FLAGS
    }
}

/// Pure keycode/flags → down-state mapping for the watched modifier.
/// `None` when the changed key is not the watched one.
pub fn modifier_event(keycode: i64, flags: u64, watched: u32) -> Option<bool> {
    if keycode == watched as i64 {
        Some(flags & flags_mask_for_keycode(watched) != 0)
    } else {
        None
    }
}

struct TapCtx {
    keycode: u32,
    proxy: EventLoopProxy<DaemonEvent>,
}

unsafe extern "C" fn tap_callback(
    _cg_proxy: *mut std::ffi::c_void,
    event_type: u32,
    event: *mut std::ffi::c_void,
    user_info: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    if event_type == EVENT_TYPE_FLAGS_CHANGED {
        let ctx = &*(user_info as *const TapCtx);
        let keycode = CGEventGetIntegerValueField(event, FIELD_KEYCODE);
        if let Some(down) = modifier_event(keycode, CGEventGetFlags(event), ctx.keycode) {
            let ev = if down {
                DaemonEvent::PttDown
            } else {
                DaemonEvent::PttUp
            };
            let _ = ctx.proxy.send_event(ev);
        }
    }
    event
}

/// Live tap handle: dropping it (or `stop`) invalidates the mach port and
/// stops listening. The tap thread's runloop exits when the port dies.
pub struct ModifierTap {
    port: *mut std::ffi::c_void,
    #[allow(dead_code)]
    source: *mut std::ffi::c_void,
}

unsafe impl Send for ModifierTap {}

impl ModifierTap {
    pub fn stop(&mut self) {
        if !self.port.is_null() {
            unsafe { CFMachPortInvalidate(self.port) };
            self.port = std::ptr::null_mut();
        }
    }
}

impl Drop for ModifierTap {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn a listen-only CGEventTap for a bare-modifier preset (RightOption/Fn).
/// The tap runs on its own thread + runloop (never the winit thread) and
/// posts PttDown/PttUp into the same proxy the global-hotkey bridge uses.
/// Requires the Input Monitoring permission (Privacy_ListenEvent); returns
/// Err with guidance when the tap cannot be created.
pub fn spawn(
    preset: HotkeyPreset,
    proxy: EventLoopProxy<DaemonEvent>,
) -> Result<ModifierTap, String> {
    let keycode = keycode_for_preset(preset).ok_or("preset is not a bare modifier")?;
    let ctx = Box::into_raw(Box::new(TapCtx { keycode, proxy }));
    let mask = 1u64 << 12; // kCGEventFlagsChanged only
    let port = unsafe {
        CGEventTapCreate(
            K_CG_SESSION_EVENT_TAP,
            K_CG_HEAD_INSERT_EVENT_TAP,
            K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
            mask,
            tap_callback as *const std::ffi::c_void as usize,
            ctx as *mut std::ffi::c_void,
        )
    };
    if port.is_null() {
        return Err(
            "CGEventTapCreate failed — grant Input Monitoring (System Settings → Privacy & Security → Input Monitoring) and restart wiflow"
                .into(),
        );
    }
    unsafe { CGEventTapEnable(port, true) };
    let source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null_mut(), port, 0) };
    if source.is_null() {
        unsafe { CFMachPortInvalidate(port) };
        return Err("CFMachPortCreateRunLoopSource failed".into());
    }
    let source_for_thread = SendPtr(source);
    std::thread::Builder::new()
        .name("wiflow-tap".into())
        .spawn(move || unsafe {
            CFRunLoopAddSource(
                CFRunLoopGetCurrent(),
                source_for_thread.get(),
                kCFRunLoopDefaultMode,
            );
            CFRunLoopRun();
        })
        .map_err(|e| {
            unsafe { CFMachPortInvalidate(port) };
            format!("tap thread spawn: {e:?}")
        })?;
    Ok(ModifierTap { port, source })
}

/// Newtype wrapper: raw pointer that is Send (single tap thread owns it;
/// the runloop there owns the source until the mach port is invalidated).
#[derive(Debug)]
struct SendPtr(*mut std::ffi::c_void);
unsafe impl Send for SendPtr {}
impl SendPtr {
    fn get(&self) -> *mut std::ffi::c_void {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modifier_event_mapping() {
        assert_eq!(modifier_event(61, OPTION_FLAGS, 61), Some(true));
        assert_eq!(modifier_event(61, 0, 61), Some(false));
        assert_eq!(modifier_event(63, FUNCTION_FLAGS, 63), Some(true));
        assert_eq!(modifier_event(63, 0, 63), Some(false));
        // Different key or watched key → None (ignore).
        assert_eq!(modifier_event(49, OPTION_FLAGS, 61), None);
        assert_eq!(modifier_event(61, OPTION_FLAGS, 63), None);
        assert_eq!(modifier_event(0, 0, 61), None);
    }

    #[test]
    fn test_keycode_for_preset() {
        assert_eq!(keycode_for_preset(HotkeyPreset::RightOption), Some(61));
        assert_eq!(keycode_for_preset(HotkeyPreset::Fn), Some(63));
        // Combo presets ride global-hotkey, not the tap.
        assert_eq!(keycode_for_preset(HotkeyPreset::CtrlSpace), None);
    }
}
