# Dictation corrections

Local and cloud transcripts pass through the same correction stage before
injection. English cleanup removes simple fillers (`um`, `uh`, `erm`) and
obvious repeated function words such as `the the`. It preserves paragraphs,
grammatical repetitions such as `had had`, quoted text, commands, paths,
URLs, identifiers, acronyms, and numeric literals. Existing capitalization
and spoken math cleanup run here too.

Ambiguous self-corrections such as “Thursday, no actually Wednesday” still
use the configured AI cleanup chain. Its result is checked for changed
protected content, negation, unsupported numbers, added vocabulary, and
discarded text. A rejected result or provider failure returns the local
corrections. These checks are conservative heuristics, not a complete
semantic guarantee.

Cleanup provider requests share a two-second budget; Ollama generation
also retains its 800 ms timeout. Context synthesis has its existing,
separate timeout. Recording, Fn handling, STT provider selection, history,
and clipboard fallback keep their existing lifecycle.

## Personal word corrections

For technical terms that Whisper *mishears*, first add the intended phrase
to `~/Library/Application Support/wiflow/prompt.txt`, for example:

```text
Postfix calculation.
```

The app reads this vocabulary on each local transcription, including local
fallback from cloud. These are recognition hints, not forced replacements.
Cloud STT currently does not receive this vocabulary. Do not map an ordinary
phrase such as “to fix the calculation” to “postfix calculation”: both are
valid speech. A text-only cleanup model cannot recover which was spoken
reliably. Use an actual recording to confirm recognition improvements.

Optionally create
`~/Library/Application Support/wiflow/corrections.json`:

```json
[
  {"from": "v flow", "to": "Wiflow", "language": "en"},
  {"from": "sentance", "to": "sentence", "language": "en"}
]
```

Use pairs you have confirmed from your own transcripts. No guessed
dictionary is installed. The file is read for each transcription, so
changes take effect on the next recording.

Matches use whole words or phrases, ignore ASCII case, prefer longer
phrases, and apply once without cascading. They never cross punctuation
or line breaks or replace protected text. Source phrases contain letters,
spaces, and apostrophes; source and replacement are limited to 128 bytes.
The file supports up to 256 entries and 64 KiB. Missing or malformed files
leave the transcript available; malformed files produce a log warning.

Omit `language` to apply an entry to any language. A scoped entry requires
the selected language. Local English Whisper models resolve `auto` to
`en`; cloud `auto` does not guess English for fillers or scoped entries.

## Validation

The regression corpus covers preservation, dictionary matching,
self-correction, malformed files, AI rejection, and provider fallback.
The existing 12-case routing corpus now sends one case to AI rather than
three. This measures routing on a small fixed corpus, not accuracy on
personal recordings. Before release, follow the live recording and paste
checks in [manual-validation.md](manual-validation.md).
