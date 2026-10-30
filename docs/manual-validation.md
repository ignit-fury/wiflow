# Dictation validation

## Automated long-audio regression

Generate a 60-second WAV with speech at 2, 33, and 51 seconds. This uses
macOS speech synthesis, writes temporary clips, and leaves the final WAV
at the supplied path. It does not record your microphone.

```sh
python3 scripts/generate-long-audio.py /tmp/wiflow-sixty-seconds.wav
WIFLOW_LONG_AUDIO_WAV=/tmp/wiflow-sixty-seconds.wav cargo test test_transcribe_long_recording_keeps_late_speech -- --ignored
```

Download the default `base.en` model through Wiflow first. The regression
uses that model on CPU, verifies its checksum, and requires the words
“morning”, “afternoon”, and “evening” in the transcript. You can set
`WIFLOW_TEST_MODEL` to another copy of the pinned base.en file.

This checks transcription across Whisper's 30-second chunks. It does not
check microphone capture, VAD, hotkeys, paste injection, or the watchdog.
The default test suite skips this test because it needs an installed
model and a generated speech fixture.

## Live acceptance checklist

### Technical vocabulary regression

Two optional CPU acoustic tests cover the reported confusion between
“postfix calculation” and “to fix the calculation”. Generate separate
16 kHz mono 16-bit WAV files with those phrases. For synthetic fixtures:

```sh
say -v Rishi -o /tmp/wiflow-postfix.aiff 'postfix calculation'
afconvert -f WAVE -d LEI16@16000 -c 1 /tmp/wiflow-postfix.aiff /tmp/wiflow-postfix.wav
say -v Samantha -o /tmp/wiflow-literal.aiff 'I need to fix the calculation'
afconvert -f WAVE -d LEI16@16000 -c 1 /tmp/wiflow-literal.aiff /tmp/wiflow-literal.wav
WIFLOW_SHORT_AUDIO_WAV=/tmp/wiflow-postfix.wav WIFLOW_TEST_PROMPT='Postfix calculation.' cargo test test_transcribe_postfix_calculation -- --ignored --nocapture
WIFLOW_LITERAL_AUDIO_WAV=/tmp/wiflow-literal.wav WIFLOW_TEST_PROMPT='Postfix calculation.' cargo test test_vocabulary_hint_does_not_replace_to_fix_calculation -- --ignored --nocapture
```

Set `WIFLOW_TEST_MODEL` to the installed `ggml-small.en.bin` to match the
reported configuration; the default is `base.en`. Omit `WIFLOW_TEST_PROMPT`
to use the saved `prompt.txt`; set it to an empty string for an unbiased
baseline. The technical-term test also checks VAD and deterministic cleanup.
Synthetic speech does not reproduce the user's recording or microphone.

### Microphone and app checks

Launch `cargo run --release -- --app`, select local transcription, and
disable cleanup/context for the initial checks. Use your chosen microphone
and inspect `~/Library/Logs/wiflow/wiflow.log` after failures.

| Check | Expected result | Status |
| --- | --- | --- |
| Dictate into Terminal, VS Code, Safari, and Slack | Text appears once at the focused cursor; focus stays in the target app | Pending user run |
| Dictate into a password field | Check clipboard fallback and notification when paste is rejected | Pending user run |
| Press for less than 300 ms | No transcription or paste; microphone closes | Pending user run |
| Press Esc while idle, recording, and in the settings hotkey recorder | Wiflow takes no action; recording continues until release/watchdog; use the recorder button to cancel key recording | Pending user run |
| Hold for 60 seconds; speak near the beginning and end | Recording stops; final phrase survives; next hold works | Pending user run |
| Record while music plays for more than 600 ms | Audio ducks/pauses, then returns to its prior volume/playback state | Pending user run |
| Release the hotkey while music plays | Audio returns to its prior state after the session completes | Pending user run |
| Change microphone and hotkey, save, reopen Settings | Settings persist; Test-record reports microphone activity | Pending user run |
| Leave Settings open without interacting | Redraws settle; window remains responsive when typing, hovering, or scrolling | Pending user run |
| Resize Settings, minimize/restore, and move between displays with different scaling | Content fits the window and redraws without clipping or GL errors | Pending user run |
| Run Test record, then stop interacting | Mic result appears when the background probe completes | Pending user run |
| Deny microphone or hotkey permissions | Guidance appears; app remains usable after permission recovery | Pending user run |
| Trigger a slow transcription | Tray spinner animates; pill hides after capture | Pending user run |
| Enable notifications with Focus/DND off | Transcription banner appears | Pending user run |
| Choose Groq with your own key | Cloud transcription succeeds; log names `whisper-large-v3-turbo` | Pending user run |
| Clear history, then reopen Settings | Deleted entries stay deleted | Pending user run |

Record date, app/provider, result, and relevant log lines when completing
a row. Synthetic-audio success does not close these live acceptance items.
