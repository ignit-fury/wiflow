# Architecture — Rust Voice Dictation

## 1. Pipeline
```
[global-hotkey] → [cpal capture 16kHz mono] → [ringbuf] → [VAD] → [whisper-rs Metal]
 → [post-process] → [arboard clipboard + enigo inject] → [history + toast]
```

Push-to-talk: keydown starts capture thread, keyup stops, flushes ringbuf → VAD → STT → inject. Esc cancels.

## 2. Crate Map
| Stage | Crate | Notes |
|---|---|---|
| Audio in | `cpal`, `ringbuf`, `hound` (debug dump only) | 16kHz, mono, f32 → i16 |
| VAD | `webrtc-vad` (v1) → `ort` + Silero (v1.1 if accuracy needs it) | Trim silence, drop <300ms |
| STT | `whisper-rs` (whisper.cpp, `metal` feature) | Models in `~/Library/Application Support/Dictation/models/` |
| Hotkey | `global-hotkey` | Push-to-talk, remappable |
| Tray UI | `tray-icon` + `winit` or `tauri` (decide at build) | Menu-bar only, no dock icon |
| Inject | `arboard` (clipboard) + `enigo` (Cmd+V) | Preserve + restore clipboard |
| Settings | `serde` + `toml` + `directories` | `config.toml` |
| Logging | `tracing` + `tracing-subscriber` | File log in dev, os_log in prod |

## 3. Module Layout (planned)
```
src/
  main.rs        // app bootstrap, tray, permissions
  hotkey.rs      // global-hotkey wrapper, push-to-talk state machine
  audio.rs       // cpal capture → ringbuf, device select
  vad.rs         // VAD trait + webrtc impl, Silero behind feature
  stt.rs         // Whisper trait + whisper-rs impl, model manager/download
  inject.rs      // clipboard save → set → Cmd+V → restore
  history.rs     // last-50 store (SQLite or JSON, TBD)
  settings.rs    // config load/save
  ui.rs          // tray, pill overlay, toasts, onboarding
core/            // pure-Rust, no OS UI — portable to Win/Linux later
platform/macos/  // Info.plist keys, permissions checks, launch-at-login
```

`core` must not depend on `winit`/`tauri`. OS injection behind `trait TextInjector` so Linux-Wayland pain stays isolated.

## 4. macOS Specifics
- `Info.plist`: `NSMicrophoneUsageDescription`, `NSSpeechRecognitionUsageDescription` (if ever using Apple STT), `LSUIElement=true` (menu-bar, no dock).
- Permissions runtime checks: mic (`AVAudioSession` / `cpal` error mapping), Accessibility (for `enigo`), Input Monitoring (for global hotkey). Deep-link to System Settings on denial.
- Notarization + hardened runtime required for global hotkey + injection outside dev.
- Launch at login via `SMAppService` (macOS 13+) or login-item helper.
- Metal: build `whisper.cpp` with Metal on aarch64-apple-darwin for 3-5x speedup. CI must cache this.

## 5. Data Flow & Performance
- Capture thread writes f32 frames to lock-free SPSC ringbuf (no alloc in hot loop).
- On keyup: drain → resample to 16kHz i16 → VAD segments → concat with 200ms padding → single Whisper full-transcribe call (streaming partials deferred to v1.1).
- Target: 10s audio → <2s transcribe on M1 base.en. Model download once (142MB base, 466MB small) with progress + SHA verify + resume.
- No audio leaves machine in default config. Cloud path (if enabled) sends only the VAD-trimmed segment over TLS, no storage.

## 6. Error Handling
- Mic busy / unplugged mid-record → stop, toast, keep partial.
- STT OOM (small model on 8GB) → auto-fall back to base + suggest setting change.
- Inject fails → clipboard already has text, toast "Pasted to clipboard — Cmd+V to paste".
- Model missing/corrupt → re-download prompt, blocking transcribe with clear UI.

## 7. Testing Strategy
- Unit: VAD trim, resample, history ring, settings serde.
- Integration: golden wav files → assert WER threshold for base.en.
- Manual: hotkey in VS Code, Safari, Slack, password field, mic unplug, permission denied.
- Bench: `criterion` for resample + VAD; Whisper RTF logged per run.

## 8. Future Hooks (not v1)
- `STT` trait allows Groq API impl behind `cloud` feature flag.
- `PostProcessor` trait allows Ollama / OpenRouter cleanup impl.
- Streaming partials via whisper.cpp streaming or Parakeet ONNX.
