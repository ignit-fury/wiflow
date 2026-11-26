# Wiflow

Wiflow is a macOS menu-bar dictation app written in Rust. Hold a hotkey,
speak, then release to transcribe and paste into the focused app.

Local transcription uses Whisper with Metal acceleration and a CPU fallback.
You can choose Groq for cloud transcription and configure Groq, OpenRouter,
or local Ollama for text cleanup. Settings also include a microphone picker,
recording pill, audio ducking, and a browser for your last 50 transcripts.

## Run

Use macOS 13+ on Apple Silicon, Rust 1.81 or newer, and Xcode Command Line
Tools (`xcode-select --install`). From the project directory:

```sh
cargo run --release
```

This opens the Wiflow window (Home, History, Settings, Permissions) and a
menu-bar icon. Closing the window keeps Wiflow running in the menu bar; reopen
it with **Open Wiflow…**. Use `--app` to start in the menu bar only (the login
item does this). Grant Microphone permission and the
Accessibility/Input Monitoring permissions requested for your hotkey and
paste injection in System Settings → Privacy & Security. After granting
permissions, restart Wiflow if macOS still blocks recording or hotkeys.

The default hotkey is Ctrl+Space. Fn and Right Option are also available.
For Fn, set System Settings → Keyboard → “Press 🌐 key to” to “Do Nothing”.
Focus a text field, hold the hotkey, speak, and release. Wiflow leaves Esc
unassigned; taps shorter than 300 ms are discarded. Wiflow stops recording
at 60 seconds. If injection fails, check the clipboard for your transcript.

On first local transcription, Wiflow downloads `base.en` (about 148 MB).
Settings offers `tiny.en` (78 MB) and `small.en` (488 MB). Download URLs use
a pinned upstream revision; Wiflow checks file size and SHA-256 before
installing a model. Interrupted downloads remain in `.bin.part` files and
resume on the next attempt. Existing models also undergo verification.

## Providers and local data

Speech recognition defaults to local: audio stays on your Mac. Selecting
Groq transcription sends audio to Groq. Cleanup and context are enabled by
default; configured cloud cleanup providers receive transcript/context
text. Disable those options in Settings for a fully local workflow, or
choose local Ollama. Cloud providers require your own credentials.

Add API keys through Settings. Wiflow also reads `GROQ_API_KEY` and
`OPENROUTER_API_KEY`; nonempty environment values override saved keys.
Keep credentials outside the repository.

| Data | Location |
| --- | --- |
| Settings, keys, history, vocabulary | `~/Library/Application Support/wiflow/` |
| Whisper models | `~/Library/Application Support/wiflow/models/` |
| Logs | `~/Library/Logs/wiflow/wiflow.log` |

Add recognition hints to `prompt.txt` in the app support directory.
Use Settings to clear transcript history. Review logs when recording,
transcription, or paste fails; they can contain transcript text.

## Development

Sentence cleanup and optional personal word replacements are described in
[corrections.md](docs/corrections.md). Both local and cloud transcription
use the same conservative cleanup and AI fallback checks.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Device and recording diagnostics:

```sh
cargo run -- --list-devices
cargo run -- --simulate-hold-ms 1500 --no-inject --dump-wav
```

The headless command records from the microphone and writes trimmed audio
to `/tmp/wiflow_hold.wav`. It currently caps capture at three seconds;
use the app for the live 60-second watchdog check.

See [architecture.md](architecture.md) for the module boundaries and
session lifecycle, [manual validation](docs/manual-validation.md) for
release checks and the long-audio regression, and [task.md](task.md) for
remaining work. Older phase documents describe their state at the time.

## Package

```sh
sh packaging/build-app.sh
sh packaging/make-dmg.sh
```

The build script creates an ad-hoc-signed `target/Wiflow.app`. Follow
[NOTARIZE.md](docs/NOTARIZE.md) for signing and notarization before distribution.
