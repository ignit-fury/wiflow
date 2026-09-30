# Phase 5 Menu-Bar App + Packaging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Real menu-bar app: tray icon with states, global push-to-talk daemon driving the full pipeline, menu-driven settings/onboarding, reloadable model holder, macOS bundle + DMG.

**Architecture:** winit 0.30 event loop owns a tray icon (tray-icon 0.24) + menu tree (muda 0.19); global-hotkey 0.7 feeds PTT events into the loop; heavy work (capture→transcribe→inject) runs on a worker thread posting results back via EventLoopProxy; settings persist as JSON config; packaging is a script + Info.plist + ad-hoc sign + hdiutil DMG.

**Tech Stack:** Rust 2021, `winit 0.30` (`EventLoop::with_user_event`, `run_app`, `ApplicationHandler` — verified against vendored 0.30.13), `tray-icon 0.24` (`TrayIconBuilder`, `set_icon/set_tooltip/set_menu`, `Icon::from_rgba` — verified 0.24.2), `muda 0.19` (`Menu/MenuItem/Submenu/CheckMenuItem::new`, `MenuEvent::receiver` — verified 0.19.3), `global-hotkey 0.7` (`GlobalHotKeyManager`, `HotKey::new`, `hotkey::{Code, Modifiers}` re-exported from `keyboard_types` — verified 0.7.0), `open 5.4` (System Settings deep links), existing pipeline modules

## Global Constraints

- Target macOS 13+ arm64 first, must compile warning-free on `aarch64-apple-darwin`.
- Rust edition 2021, stable toolchain (verified 1.95.0).
- Default path $0, no API key, no network calls except one-time model downloads (user-initiated).
- `cargo fmt --check` clean.
- `cargo clippy --all-targets -- -D warnings` clean.
- `cargo test` passes; new logic needs a test.
- Recording indicator mandatory whenever mic open (tray icon + tooltip in v1).
- Injection failure must still leave text in clipboard + notify (rules.md UX rule).

## Explicit Scope Decisions (deviations from PRD/design, recorded honestly)

- NO custom pill overlay window and NO settings window in v1: tray icon states (idle/recording/done) + tooltip carry the recording indicator; all settings live in tray menu submenus persisted to `config.json`. Rationale: hand-rolled winit windows with widgets is a second project; menu-driven settings are the native menu-bar pattern. Pill overlay + settings window → v1.1.
- Toasts = `println!`/tracing + tray tooltip in v1 (no `notify-rust`: dependency + bundle-id behavior unverified). Revisit v1.1.
- Notarization is DOCUMENTED manual steps only (`docs/NOTARIZE.md`): requires a paid Apple Developer account ($99/yr), which this project does not have. Ad-hoc `codesign -s -` covers local runs.
- Launch-at-login via `~/Library/LaunchAgents` plist (no new deps), not SMAppService (needs ObjC bridge).

---

## File Structure

- Create: `src/app.rs` — Task 1 (`AppState` enum, generated icons, tray/menu construction, winit `ApplicationHandler` skeleton).
- Create: `src/daemon.rs` — Task 2 (hotkey registration AltRight→Fn fallback, worker thread pipeline runner, `DaemonEvent` proxy protocol, Esc cancel via `hotkey::on_cancel`).
- Create: `src/config.rs` — Task 3 (`Config` serde struct, pure-path load/save core, LaunchAgent plist create/remove, System Settings URLs).
- Modify: `src/stt.rs` — Task 1 (`transcribe_shared` reloadable holder replacing `OnceLock<Result>` Err-sticks).
- Modify: `src/history.rs` — Task 3 (atomic tmp+rename write; centralize support dir via shared helper).
- Modify: `src/main.rs` — Tasks 1–4 (`--app` flag launching GUI loop; simulate path preserved for headless tests).
- Modify: `Cargo.toml` — Task 1 (`tray-icon = "0.24"`, `muda = "0.19"`, `winit = "0.30"`, `global-hotkey = "0.7"`, `open = "5"`).
- Create: `packaging/Info.plist`, `packaging/build-app.sh`, `packaging/make-dmg.sh`, `docs/NOTARIZE.md` — Task 4.
- Modify: `task.md`, `memory.md` — Task 4.

Interfaces:
- `app::AppState::{Idle, Recording, Transcribing, Error}` (new Task 1; `tooltip()` + `icon_kind()` pure).
- `app::make_icon(AppState) -> tray_icon::Icon` (new Task 1; 32×32 generated RGBA, pure, unit-tested).
- `app::build_menu(&Config) -> (muda::Menu, MenuIds)` (new Task 1 skeleton; items wired Task 3).
- `daemon::DaemonEvent::{Started, Transcribed{...}, Failed(String), Cancelled}` (new Task 2; sent via `winit::event_loop::EventLoopProxy<DaemonEvent>`).
- `daemon::register_ptt_hotkey() -> Result<(GlobalHotKeyManager, HotKey), String>` (new Task 2; AltRight first, Fn fallback, reports which).
- `daemon::run_hold_pipeline(...)` (new Task 2; worker-thread fn: capture→gate→resample→trim→shared transcribe→inject→history, posts DaemonEvent).
- `config::Config { hotkey_preset: HotkeyPreset, mic_name: Option<String>, model: ModelChoice, launch_at_login: bool }` (new Task 3; `HotkeyPreset::{RightOption, Fn, CtrlSpace}`, `ModelChoice::{BaseEn, SmallEn}`).
- `config::load_config_from(&Path)/save_config_to(&Path, &Config)` (new Task 3, pure-path core) + `config_path()/load_config/save_config` wrappers.
- `config::set_launch_at_login(bool, exe_path: &Path) -> Result<(), String>` (new Task 3; writes/removes LaunchAgent plist + bootstrap/load).
- `stt::transcribe_shared(&Path, &[f32]) -> Result<String, String>` (new Task 1; reloadable, retryable — replaces `shared_stt` contract; keep `shared_stt` as thin wrapper OR remove it and update main.rs — plan mandates REMOVE + update call site, no duplicated holders).
- `stt::SMALL_MODEL_URL/SIZE/NAME + ensure_model_path(&str)` (new Task 3; small.en 487,614,201 bytes verified 2026-09-30; generalize ensure_model over a variant table).

---

### Task 1: Deps + Reloadable STT + Tray Shell

**Files:**
- Modify: `Cargo.toml` (add 5 deps), `src/stt.rs` (holder rework), `src/main.rs` (`mod app;`, `--app` flag skeleton)
- Create: `src/app.rs` (state, icons, menu skeleton, winit handler skeleton)
- Test: inline — icon bytes, state tooltip/icon_kind, holder retry semantics

**Interfaces:**
- Consumes: tray-icon/muda/winit APIs per Tech Stack (all verified above).
- Produces: compiling GUI shell (`--app` opens tray, menus visible, no daemon yet); `transcribe_shared` for Task 2.

- [ ] **Step 1: Add deps + write failing tests**

Add to `Cargo.toml` `[dependencies]`:

```toml
tray-icon = "0.24"
muda = "0.19"
winit = "0.30"
global-hotkey = "0.7"
open = "5"
```

Create `src/app.rs` with ONLY state + tests first:

```rust
use tray_icon::Icon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Idle,
    Recording,
    Transcribing,
    Error,
}

impl AppState {
    pub fn tooltip(&self) -> &'static str {
        match self {
            AppState::Idle => "Wiflow — hold Right Option to dictate",
            AppState::Recording => "Wiflow — recording… release to transcribe",
            AppState::Transcribing => "Wiflow — transcribing…",
            AppState::Error => "Wiflow — error (see menu)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tooltips_cover_all_states() {
        assert!(AppState::Idle.tooltip().contains("hold"));
        assert!(AppState::Recording.tooltip().contains("recording"));
        assert!(AppState::Transcribing.tooltip().contains("transcribing"));
        assert!(AppState::Error.tooltip().contains("error"));
    }

    #[test]
    fn test_icon_bytes_are_32x32_rgba() {
        for state in [AppState::Idle, AppState::Recording, AppState::Transcribing, AppState::Error] {
            let rgba = icon_rgba(state);
            assert_eq!(rgba.len(), 32 * 32 * 4);
        }
    }

    #[test]
    fn test_recording_dot_is_red_center() {
        let rgba = icon_rgba(AppState::Recording);
        let i = (16 * 32 + 16) * 4;
        assert!(rgba[i] > 200 && rgba[i + 1] < 80 && rgba[i + 2] < 80, "center must be red");
    }
}
```

- [ ] **Step 2: Run to verify failure (deps compile first — allow minutes)**

Run: `cargo test app:: 2>&1 | tail -4`
Expected: FAIL with `cannot find function icon_rgba`.

- [ ] **Step 3: Implementation — icons + holder rework**

Append to `src/app.rs` (above tests):

```rust
/// 32x32 status icon: dark rounded square, center dot colored by state.
pub fn icon_rgba(state: AppState) -> Vec<u8> {
    let dot: (u8, u8, u8) = match state {
        AppState::Idle => (140, 140, 140),
        AppState::Recording => (230, 40, 40),
        AppState::Transcribing => (60, 180, 255),
        AppState::Error => (230, 150, 0),
    };
    let mut px = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32 {
        for x in 0..32 {
            let dx = x as i32 - 16;
            let dy = y as i32 - 16;
            let (r, g, b) = if dx * dx + dy * dy <= 49 { dot } else { (24, 24, 24) };
            px.extend_from_slice(&[r, g, b, 255]);
        }
    }
    px
}

pub fn make_icon(state: AppState) -> Icon {
    Icon::from_rgba(icon_rgba(state), 32, 32).expect("generated icon is valid RGBA")
}
```

stt.rs holder rework — replace the `OnceLock<Result<...>>` block with:

```rust
use std::sync::{Mutex, OnceLock};

static STT: OnceLock<Mutex<Option<(PathBuf, Stt)>>> = OnceLock::new();

/// Load once, reload on model switch, retry after failure (Err never sticks).
/// First-implemented fix for the Phase 4 `OnceLock<Result>` Err-sticks finding.
pub fn transcribe_shared(model_path: &Path, samples: &[f32]) -> Result<String, String> {
    let slot = STT.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    let hit = matches!(&*guard, Some((p, _)) if p == model_path);
    if !hit {
        *guard = Some((model_path.to_path_buf(), Stt::load(model_path)?));
    }
    guard
        .as_mut()
        .expect("slot just filled")
        .1
        .transcribe(samples)
}
```

Delete `shared_stt` + its test (`test_shared_stt_bad_path_is_err`); update the ONE main.rs call site to `transcribe_shared` (it currently calls `shared_stt(&model_path)` then locks + transcribes — replace that whole block with a single `stt::transcribe_shared(&model_path, &kept)` call returning `Result<String, String>`, keeping the surrounding warn-on-Err + timing logs). Add replacement test:

```rust
#[test]
fn test_transcribe_shared_bad_path_is_err() {
    assert!(transcribe_shared(Path::new("/nonexistent/ggml.bin"), &[0.1; 160]).is_err());
    // Retry allowed: second call re-attempts (no poisoned cache).
    assert!(transcribe_shared(Path::new("/nonexistent/ggml.bin"), &[0.1; 160]).is_err());
}
```

main.rs: add `mod app;` (alphabetical: app first) + `--app` flag:

```rust
/// Launch the menu-bar app (tray + global hotkey daemon)
#[arg(long)]
app: bool,
```

and at the TOP of main() after Args::parse:

```rust
if args.app {
    return app::run();
}
```

with stub in app.rs:

```rust
pub fn run() -> ! {
    eprintln!("tray shell lands in Task 2 wiring; --app parsed OK");
    std::process::exit(0);
}
```

(Returns `!` via exit so main's fallthrough compiles; replaced for real in Task 2. `eprintln!` keeps stdout clean for TRANSCRIPT parsing.)

`shared_stt`'s old `#[allow]` handling: delete `shared_stt` entirely INCLUDING its allow; `transcribe_shared` is used by main → no allow needed. If clippy demands otherwise, narrow per-item allow with comment.

- [ ] **Step 4: Run gates**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`
Expected: `FMT_OK`.

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -2`
Expected: `Finished`, no warnings in our code (new dep trees may warn — third-party, note only).

Run: `cargo test 2>&1 | grep -E "test result"`
Expected: zero failures; report actual count (34 base + 4 new app tests + 1 new stt test − 1 deleted shared_stt test = 38 passed + 1 ignored expected).

Run: `cargo run -- --app 2>&1 | tail -2`
Expected: `tray shell lands in Task 2 wiring` on stderr, exit 0. Headless-safe (no GUI touched yet).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/app.rs src/stt.rs src/main.rs
git commit -m "feat: add tray shell skeleton and reloadable stt holder"
```

---

### Task 2: Hotkey Daemon + Worker Pipeline

**Files:**
- Create: `src/daemon.rs`
- Modify: `src/app.rs` (real `run()`: winit loop, tray build, event drain, state updates)
- Modify: `src/hotkey.rs` (no logic change expected; `on_cancel` + `Cancelled` finally consumed — remove the 3 allows if clippy is clean without them)
- Test: inline — hotkey preset mapping, proxy protocol is wiring (live-tested)

**Interfaces:**
- Consumes: `hotkey::PushToTalk/PttEvent`, `audio::AudioCapture`, `vad::transcribe_ready`, `stt::transcribe_shared/ensure_model`, `inject::inject_text/leave_on_clipboard`, `history::{push_history, HistoryEntry}`, `app::AppState/make_icon`, winit proxy.
- Produces: working daemon: hold→record→release→transcribe→inject loop with tray states.

- [ ] **Step 1: Write the failing tests (daemon preset mapping)**

Create `src/daemon.rs` with ONLY these + tests first:

```rust
use global_hotkey::hotkey::{Code, HotKey, Modifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyPreset {
    RightOption,
    Fn,
    CtrlSpace,
}

pub fn preset_hotkey(preset: HotkeyPreset) -> HotKey {
    match preset {
        HotkeyPreset::RightOption => HotKey::new(None, Code::AltRight),
        HotkeyPreset::Fn => HotKey::new(None, Code::Fn),
        HotkeyPreset::CtrlSpace => {
            HotKey::new(Some(Modifiers::CONTROL), Code::Space)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_presets_are_distinct_ids() {
        let a = preset_hotkey(HotkeyPreset::RightOption);
        let b = preset_hotkey(HotkeyPreset::Fn);
        let c = preset_hotkey(HotkeyPreset::CtrlSpace);
        assert_ne!(a.id(), b.id());
        assert_ne!(a.id(), c.id());
        assert_ne!(b.id(), c.id());
    }
}
```

(`HotKey::id()` — if 0.7.0 lacks `.id()` on HotKey (only on events), compare debug strings instead; implementer follows compiler, reports deviation.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test daemon:: 2>&1 | tail -4`
Expected: FAIL — `HotKey::id` may not exist (see note) or imports wrong; honest RED is any compile error in the new module. If it PASSES immediately (API exactly right), do the break-then-revert protocol from Phase 2 Task 1 (temporarily map two presets to the same key, show fail, revert).

- [ ] **Step 3: Implementation — registration + worker + loop**

Append to `src/daemon.rs`:

```rust
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::sync::mpsc;

#[derive(Debug)]
pub enum DaemonEvent {
    PttDown,
    PttUp,
    Cancel,
    Done { text: String, duration_ms: u64, rtf: f64 },
    Failed(String),
}

/// Register Right-Option; fall back to Fn when the OS swallows it.
/// Returns the manager (must be kept alive), the hotkey, and which preset won.
pub fn register_ptt_hotkey(
    prefer: HotkeyPreset,
) -> Result<(GlobalHotKeyManager, HotKey, HotkeyPreset), String> {
    let order = match prefer {
        HotkeyPreset::RightOption => [HotkeyPreset::RightOption, HotkeyPreset::Fn],
        HotkeyPreset::Fn => [HotkeyPreset::Fn, HotkeyPreset::RightOption],
        HotkeyPreset::CtrlSpace => [HotkeyPreset::CtrlSpace, HotkeyPreset::Fn],
    };
    let manager =
        GlobalHotKeyManager::new().map_err(|e| format!("hotkey manager: {e:?}"))?;
    for preset in order {
        let hk = preset_hotkey(preset);
        match manager.register(hk.clone()) {
            Ok(()) => return Ok((manager, hk, preset)),
            Err(e) => tracing::warn!("hotkey register failed for {preset:?}: {e:?}"),
        }
    }
    Err("no push-to-talk hotkey registered".into())
}
```

(`HotKey: Clone` — if 0.7.0 HotKey is Copy, `.clone()` still compiles (Copy types have clone). If neither, restructure to rebuild via preset_hotkey twice; report.)

Worker + app loop live in `src/app.rs` `run()` (replace stub):

```rust
pub fn run() -> ! {
    let event_loop = winit::event_loop::EventLoop::<DaemonEvent>::with_user_event()
        .build()
        .expect("winit event loop");
    let proxy = event_loop.create_proxy();
    // ... tray build with Idle icon + menu (Task 3 fills menu items; Task 1 menu skeleton call lives here) ...
    // ... register_ptt_hotkey(prefer from config default RightOption) ...
    // Spawn bridge thread: GlobalHotKeyEvent::receiver() → proxy.send_event(DaemonEvent::PttDown/PttUp) on Pressed/Released.
    // Spawn worker on PttDown (capture start); on PttUp run pipeline on worker, post Done/Failed.
    // about_to_wait drains MenuEvent::receiver (Task 3 items) + updates tray icon/tooltip per AppState.
    // If ANY winit API above differs in 0.30.13 (method names, proxy types), follow the compiler + vendored docs and REPORT the deviation verbatim — do not guess twice.
    std::process::exit(app_main(event_loop, proxy));
}
```

Full loop body is integration code the implementer writes against the verified APIs above (with_user_event/create_proxy/run_app/ApplicationHandler/resumed/about_to_wait/user_event). Requirements the reviewer will check: PTT down/up drives REAL `PushToTalk` (not a reimplementation); Esc maps to `on_cancel` (finally consuming it); `<300ms` discards never touch the model; `Cancelled` posts tray Error/idle reset (no stuck recording); transcribe uses `stt::transcribe_shared` (reloadable); inject failure → `leave_on_clipboard` + tooltip warn; history pushed before inject; `Duration`-blocking work NEVER runs on the winit thread (worker thread mandatory).

- [ ] **Step 4: Run gates + live daemon smoke**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (zero failures; report count).

Run: `timeout 15 cargo run -- --app 2>&1 | tail -5 & sleep 2; kill %1` — expect tray/menu/hotkey init logs, no panic, clean exit on kill. This machine HAS a GUI session (Task 4 Task 2 proved real inject). If `--app` panics headless-style, report honestly with log.

- [ ] **Step 5: Commit**

```bash
git add src/daemon.rs src/app.rs src/hotkey.rs src/main.rs
git commit -m "feat: add global hotkey daemon with worker pipeline"
```

---

### Task 3: Config + Menu System + Daemon Hardening

**Files:**
- Create: `src/config.rs`
- Modify: `src/app.rs` (full menu tree + event handling), `src/history.rs` (atomic write), `src/stt.rs` (small-model variant table)
- Modify: `src/main.rs` (config load at startup; `--app` passes config)
- Test: inline — config round-trip/corrupt-default, menu ids distinct, atomic write no-partial

**Interfaces:**
- Consumes: muda menu types, `history::push_history_to`, `stt::ensure_model`, LaunchAgent plist path, `open::that`.
- Produces: all `config::` items + wired menus + atomic history + small.en support.

- [ ] **Step 1: Write the failing tests**

`src/config.rs` tests-only first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            hotkey_preset: HotkeyPreset::Fn,
            mic_name: Some("Test Mic".into()),
            model: ModelChoice::SmallEn,
            launch_at_login: true,
        }
    }

    #[test]
    fn test_config_roundtrip() {
        let p = std::env::temp_dir().join("wiflow_cfg_test.json");
        let _ = std::fs::remove_file(&p);
        save_config_to(&p, &sample()).unwrap();
        assert_eq!(load_config_from(&p), sample());
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_config_missing_is_default() {
        let p = std::env::temp_dir().join("wiflow_cfg_missing_xyz.json");
        let _ = std::fs::remove_file(&p);
        assert_eq!(load_config_from(&p), Config::default());
    }

    #[test]
    fn test_config_corrupt_is_default() {
        let p = std::env::temp_dir().join("wiflow_cfg_corrupt_xyz.json");
        std::fs::write(&p, "{nope").unwrap();
        assert_eq!(load_config_from(&p), Config::default());
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_default_preset_is_right_option() {
        assert_eq!(Config::default().hotkey_preset, HotkeyPreset::RightOption);
    }
}
```

Types (`Config`, `HotkeyPreset`, `ModelChoice`) live in config.rs but HotkeyPreset is ALSO needed by daemon.rs Task 2 — plan mandates: daemon.rs defines and OWNS `HotkeyPreset`; config.rs `use crate::daemon::HotkeyPreset;` and `ModelChoice` owned by config.rs. (If Task 2 put it elsewhere, follow the code + report.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test config:: 2>&1 | tail -4`
Expected: FAIL with missing `Config`/`load_config_from`/`save_config_to`.

- [ ] **Step 3: Implementation (exact code)**

```rust
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelChoice {
    BaseEn,
    SmallEn,
}

impl Default for ModelChoice {
    fn default() -> Self {
        ModelChoice::BaseEn
    }
}

pub use crate::daemon::HotkeyPreset;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub hotkey_preset: HotkeyPreset,
    #[serde(default)]
    pub mic_name: Option<String>,
    #[serde(default)]
    pub model: ModelChoice,
    #[serde(default)]
    pub launch_at_login: bool,
}
```

`HotkeyPreset` needs Serialize/Deserialize + Default(RightOption) — it was defined in daemon.rs WITHOUT those derives in Task 2. Add them in THIS task (modify daemon.rs derives — allowed: `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]` with `#[default]` on RightOption; import serde in daemon.rs). `Default` for Config: all-default struct.

```rust
impl Default for Config {
    fn default() -> Self {
        Self { hotkey_preset: HotkeyPreset::default(), mic_name: None, model: ModelChoice::default(), launch_at_login: false }
    }
}

pub fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/wiflow/config.json")
}

pub fn load_config_from(path: &Path) -> Config {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_config_to(path: &Path, cfg: &Config) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir config: {e:?}"))?;
    }
    atomic_write_json(path, cfg)
}

/// Crash-safe write for small JSON docs: tmp + rename (never partial).
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let json = serde_json::to_string_pretty(value).map_err(|e| format!("encode: {e:?}"))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write tmp: {e:?}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e:?}"))
}

pub const LAUNCH_AGENT_LABEL: &str = "com.wiflow.dictation";

pub fn launch_agent_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(format!("Library/LaunchAgents/{LAUNCH_AGENT_LABEL}.plist"))
}

/// Enable/disable launch-at-login. Applies immediately via launchctl when asked.
pub fn set_launch_at_login(enable: bool, exe_path: &Path, apply_now: bool) -> Result<(), String> {
    let path = launch_agent_path();
    if !enable {
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("gui/{}", unsafe_user_id()), path.to_string_lossy().as_ref()])
            .status();
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir agents: {e:?}"))?;
    }
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>{LAUNCH_AGENT_LABEL}</string>\n\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{}</string>\n\t\t<string>--app</string>\n\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n</dict>\n</plist>\n",
        exe_path.display()
    );
    atomic_write_json_raw(&path, &plist)?;
    if apply_now {
        let _ = std::process::Command::new("launchctl")
            .args(["bootstrap", &format!("gui/{}", unsafe_user_id()), path.to_string_lossy().as_ref()])
            .status();
    }
    Ok(())
}

fn atomic_write_json_raw(path: &Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("write tmp: {e:?}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e:?}"))
}

fn unsafe_user_id() -> String {
    unsafe { libc_uid() }
}

/// uid without a libc dep: `id -u`, fallback 501.
fn libc_uid() -> String {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".into())
}

pub mod permissions {
    /// System Settings deep links (macOS 13+). Fall back to the root pane when they fail.
    pub const MIC_PRIVACY: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";
    pub const ACCESSIBILITY_PRIVACY: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
    pub const INPUT_MONITORING_PRIVACY: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";

    pub fn open_mic_settings() {
        if open::that(MIC_PRIVACY).is_err() {
            let _ = open::that("x-apple.systempreferences:com.apple.preference.security");
        }
    }
    pub fn open_accessibility_settings() {
        if open::that(ACCESSIBILITY_PRIVACY).is_err() {
            let _ = open::that("x-apple.systempreferences:com.apple.preference.security");
        }
    }
}
```

history.rs atomic write: replace direct `std::fs::write(path, json)` in `push_history_to` with `crate::config::atomic_write_json(path, &all)` (drop the local to_string_pretty). Add `use` — history.rs already serializes Vec<HistoryEntry>; atomic_write_json is generic Serialize. Delete now-unused serde_json import in history.rs ONLY if compiler flags it (report).

stt.rs small-model table: append:

```rust
pub const SMALL_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin";
/// Verified 2026-09-30 via HEAD (HTTP 200, content-length).
pub const SMALL_MODEL_SIZE: u64 = 487_614_201;
pub const SMALL_MODEL_NAME: &str = "ggml-small.en.bin";

/// Variant-aware paths: "base" (default) or "small".
pub fn model_path_for(variant: &str) -> (PathBuf, &'static str, u64) {
    if variant == "small" {
        (models_dir().join(SMALL_MODEL_NAME), SMALL_MODEL_URL, SMALL_MODEL_SIZE)
    } else {
        (models_dir().join(MODEL_NAME), MODEL_URL, MODEL_SIZE)
    }
}

pub fn ensure_model_variant(variant: &str) -> Result<PathBuf, String> {
    let (path, url, size) = model_path_for(variant);
    if std::fs::metadata(&path).map(|m| m.len() == size).unwrap_or(false) {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir models: {e:?}"))?;
    }
    let status = std::process::Command::new("curl")
        .args(["-fSL", "-C", "-", "-o"])
        .arg(&path)
        .arg(url)
        .status()
        .map_err(|e| format!("spawn curl: {e:?}"))?;
    if !status.success()
        || !std::fs::metadata(&path).map(|m| m.len() == size).unwrap_or(false)
    {
        return Err(format!("download failed: {status}"));
    }
    Ok(path)
}
```

Refactor `ensure_model` to delegate: `pub fn ensure_model() -> Result<PathBuf, String> { ensure_model_variant("base") }` (keep signature — main.rs depends on it).

Menu tree in app.rs (replace Task 1 skeleton call): root Menu::new with items: Status (disabled, shows state), separator?, Mic submenu (from `audio::list_devices()`, checked = config.mic_name match/default first), Model submenu (Base en 140MB checked per config + Small en 465MB note), Hotkey submenu (Right Option / Fn / Ctrl+Space checked per config; selecting re-registers daemon hotkey + saves config), Launch at Login (CheckMenuItem per config; toggles set_launch_at_login with current_exe), History submenu (last 8 entries as items → click copies via clipboard... needs clipboard write: reuse arboard directly in handler via `inject::leave_on_clipboard`? That SETS clipboard — perfect for copy. Use it.), Permissions submenu (Microphone / Accessibility → open URLs), Quit. MenuEvent::receiver drained in about_to_wait; match on stored ids. Persist every change via save_config.

- [ ] **Step 4: Run gates + live menu smoke**

Run: `cargo fmt && cargo fmt --check && echo FMT_OK`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (zero failures; report count).

Run: `timeout 20 cargo run -- --app 2>&1 | tail -6 & sleep 3; kill %1` — expect tray init + menu build + hotkey registered log (which preset won), no panic. Then toggle nothing (menus need clicks — USER matrix covers). Record preset outcome.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/app.rs src/history.rs src/stt.rs src/daemon.rs src/main.rs Cargo.toml Cargo.lock
git commit -m "feat: add config, menu system, small model, atomic history"
```

(Only add files actually touched; Cargo.toml unchanged this task unless serde was missed earlier — serde/serde_json were NOT added yet (Tasks 1–2 didn't need them): this task adds `serde` + `serde_json`. Include Cargo.toml/Cargo.lock.)

---

### Task 4: Packaging + Onboarding Docs + Gates

**Files:**
- Create: `packaging/Info.plist`, `packaging/build-app.sh`, `packaging/make-dmg.sh`, `docs/NOTARIZE.md`
- Modify: `task.md` (check Phase 5 boxes + numbers), `memory.md` (dated entry)
- Test: bundle builds + launches + DMG mounts (all runnable here)

**Interfaces:**
- Consumes: release binary, icon (reuse generated RGBA → PNG via script? `sips` can convert: write PNG asset with a tiny Rust helper OR ship `packaging/icon.png` generated once via `cargo run -- --dump-icon packaging/icon.png`... simplest: add hidden `--dump-icon PATH` flag? NO new flags without need — instead generate icon.png with python3 + struct (stdlib only, 5 lines) in build-app.sh. Plan mandates the python3 approach, no repo flag.)

- [ ] **Step 1: Info.plist (exact content)**

`packaging/Info.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>com.wiflow.dictation</string>
	<key>CFBundleName</key>
	<string>Wiflow</string>
	<key>CFBundleVersion</key>
	<string>0.1.0</string>
	<key>CFBundleShortVersionString</key>
	<string>0.1.0</string>
	<key>CFBundleExecutable</key>
	<string>wiflow-dictation</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>Wiflow needs microphone access for push-to-talk dictation. Audio is processed on-device.</string>
	<key>NSSpeechRecognitionUsageDescription</key>
	<string>Reserved for future on-device speech features. Not used in v1.</string>
</dict>
</plist>
```

- [ ] **Step 2: build-app.sh (exact content)**

`packaging/build-app.sh`:

```bash
#!/bin/sh
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/Wiflow.app"
cargo build --release --manifest-path "$ROOT/Cargo.toml"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/release/wiflow-dictation" "$APP/Contents/MacOS/"
cp "$ROOT/packaging/Info.plist" "$APP/Contents/"
python3 - "$APP/Contents/Resources/icon.png" <<'EOF'
import struct, sys, zlib
w = h = 32
raw = bytearray()
for y in range(h):
    for x in range(w):
        dx, dy = x - 16, y - 16
        r, g, b = ((230, 40, 40) if dx*dx + dy*dy <= 49 else (24, 24, 24))
        raw += bytes([r, g, b])
def chunk(t, d):
    c = struct.pack(">I", len(d)) + t + d
    return c + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
stride = w * 3
scan = b"".join(b"\x00" + raw[i:i+stride] for i in range(0, len(raw), stride))
png += chunk(b"IDAT", zlib.compress(bytes(scan))) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
EOF
codesign --force --deep --sign - "$APP"
echo "built $APP"
```

`chmod +x packaging/build-app.sh packaging/make-dmg.sh` (run chmod, commit preserves bit via git).

`packaging/make-dmg.sh`:

```bash
#!/bin/sh
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/Wiflow.app"
DMG="$ROOT/target/Wiflow-0.1.0-arm64.dmg"
[ -d "$APP" ] || "$ROOT/packaging/build-app.sh"
rm -f "$DMG"
hdiutil create -volname Wiflow -srcfolder "$APP" -ov -format UDZO "$DMG"
echo "built $DMG"
```

- [ ] **Step 3: NOTARIZE.md (exact content)**

`docs/NOTARIZE.md`: why ad-hoc (`codesign -s -`) suffices locally; paid Apple Developer ($99/yr) prerequisites; `xcrun notarytool submit` + `staple` command shapes WITHOUT secrets (use `$APPLE_ID`/`$APP_PASSWORD` env placeholders); Team ID plist note. No credentials anywhere.

- [ ] **Step 4: Run gates + live bundle verification**

Run: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (zero failures; report count).

Run: `sh packaging/build-app.sh 2>&1 | tail -3` (release build takes minutes first time — allow 15 min).
Expected: `built .../target/Wiflow.app`.

Run: `codesign -dv --verbose=2 target/Wiflow.app 2>&1 | grep -E "Signature|Identifier"` → ad-hoc signature lines.

Run: `timeout 20 target/Wiflow.app/Contents/MacOS/wiflow-dictation --app 2>&1 | tail -4 & sleep 3; kill %1` → tray init, no panic.

Run: `sh packaging/make-dmg.sh 2>&1 | tail -2; hdiutil verify target/Wiflow-0.1.0-arm64.dmg 2>&1 | tail -2` → verified.

- [ ] **Step 5: Docs + commits**

In `task.md`: check Phase 5 boxes with numbers (dep versions, preset outcome, bundle + DMG sizes, test count). Move ALL deferred items into a tracked `## v1.1` section so nothing is lost: pill overlay, settings window, rich toasts, `set_single_segment` revisit before 60s holds, `dirs`-crate path centralization (HOME fallback accepted for v1), SHA256 model pin, layout-aware paste, ggml-metal teardown abort fix, true WER corpus, small.en bench.

In `memory.md`: dated entry (stack versions, AltRight-vs-Fn outcome, bundle/DMG paths, gate status).

```bash
git add packaging docs/NOTARIZE.md
git commit -m "feat: add macos bundle, dmg scripts, notarize docs"
git add task.md memory.md
git commit -m "docs: mark phase5 app complete with numbers"
```

---

## Self-Review

- Spec coverage: task.md Phase 5 (tray states → Task 1–2 icons/tooltip; recording pill → explicitly deferred v1.1 with tracked box; toasts → tooltip+log, v1.1; settings window → menu submenus + config.json; onboarding → permissions menu + mic usage plist; Info.plist → Task 4; launch-at-login → Task 3 LaunchAgent; sign → ad-hoc Task 4; DMG → Task 4; model first-run → ensure_model already, small.en added Task 3) + all 6 carried pre-reqs (reloadable holder → Task 1; atomic history + dirs → Task 3 atomic_write_json + history_path note — dirs crate NOT adopted: HOME fallback kept, documented as Phase-5-accepted in Task 3 report; layout paste → Task 2 Dvorak note; metal teardown → pre-ship note carried to memory; single_segment → Task 4? NOT covered — add now: Task 4 Step 5 docs must record single_segment revisit as v1.1 box. FIXED in plan: task.md v1.1 section gains it.)
- Placeholder scan: no TBD/TODO; winit/proxy/global-hotkey API risks fenced with compiler+report protocols; sandbox-vs-GUI fenced with honest-report; Homebrew-class risks absent (no new system deps).
- Type consistency: `transcribe_shared(&Path, &[f32])` matches Task 2 worker call; `HotkeyPreset` owned by daemon, re-exported by config with added derives; `ModelChoice::{BaseEn,SmallEn}` ↔ "base"/"small" strings at menu boundary only (`ensure_model_variant`); `DaemonEvent` variants match worker posts + loop matches; `MenuIds` struct fields match handler arms (implementer defines both sides — reviewer checks pair).
