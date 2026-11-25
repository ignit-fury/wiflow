# Fallback when an API key runs out of tokens — design

Date: 2026-11-25
Status: approved design, not implemented

## Goal

When Groq or OpenRouter says a key is out of tokens, Wiflow stops using
that key until the limit resets and uses a fallback the user chooses:
local Ollama or the auto chain. Dictation cleanup and voice-edit both
follow it. The user's saved provider never changes.

## Today

- Dictation cleanup, provider `auto`: Groq, then OpenRouter, then Ollama,
  tried in that order on every dictation.
- Dictation cleanup, explicit Groq or OpenRouter: on HTTP 429, 402 or 401
  it falls back to Ollama for that one dictation.
- Every dictation tries the exhausted key first and waits for the refusal.
- Voice-edit (`edit_chat`) has no fallback: the edit fails.
- Groq speech-to-text already falls back to local Whisper on any error.

## Behaviour

### What counts as out of tokens

HTTP 429 (rate or token limit) and HTTP 402 (no credits). The key is
then skipped for the time in the response's `Retry-After` header (whole
seconds). Without a usable header the time is 60 s. It is capped at
1 hour.

HTTP 401 (rejected key) is not a limit. That one dictation or edit uses
the fallback setting, with the "update your key" message, but no skip
period starts: the next request tries the key again.

### New setting

`cleanup_limit_fallback` in `Config`, values:

| Value | Label in Settings | Used while a key is skipped |
|---|---|---|
| `"ollama"` (default) | Local Ollama | Ollama only |
| `"auto"` | Auto chain | Groq, then OpenRouter, then Ollama, leaving out every skipped provider and any provider without a key |

The default keeps today's fallback. Unknown or empty values read as
`"ollama"`. Settings shows it as a radio pair, "When an API key runs out
of tokens", next to the cleanup provider choice.

### Which provider is used

For each dictation cleanup and each voice-edit:

1. Start from the configured provider (`cleanup_provider`).
2. If it is `auto`, run the auto chain and leave out skipped providers.
3. If it is Groq or OpenRouter and that provider is skipped, go straight
   to the fallback setting, without a request to the skipped provider.
4. If a request returns 429 or 402, start the skip period for that
   provider and continue with the fallback setting in the same call.
5. Ollama is never skipped.

Voice-edit uses the same order. It keeps its own budget (8 s) and its
"edit not applied" messages when every provider fails.

### Messages

- When a skip period starts, one notification: for example "Groq is out
  of tokens; using Ollama for about 1 min." The tray note shows the
  same text.
- While the provider stays skipped, no further messages.
- When the time is up, the next dictation tries the provider again. If it
  is still limited, a new skip period starts with a new notification.

## Components

| Piece | Change |
|---|---|
| `core/cleanup.rs` | A small in-memory store of skipped providers: provider to "skip until" instant, behind a `Mutex`. Functions to start a skip period, ask whether a provider is skipped (with an injectable "now" for tests), and take the pending notice. |
| `core/cleanup.rs` | `chat_completion` keeps the `Retry-After` value in its error string, for example `HTTP 429 (retry after 12s)`. A pure `skip_for(err) -> Option<Duration>` turns an error string into a skip period (429/402 only, default 60 s, cap 1 h). |
| `core/cleanup.rs` | `full_chain`, `single_provider_chain` and `edit_chat` consult the store and the fallback setting as described above. |
| `core/config.rs` | `cleanup_limit_fallback: String`, default `"ollama"`, serde default for old config files. |
| `ui/settings.rs` | The radio pair. |
| `daemon.rs` | After a cleanup or edit, take the pending notice and send it as `DaemonEvent::Notice` (a banner), so the existing tray-only `CleanupIssue` path is unchanged. |

## Error handling

- The store lives in memory only; a restart clears it.
- A poisoned store lock is treated as "nothing skipped", so cleanup still
  runs.
- If the fallback also fails, today's messages and raw-text paste apply.

## Testing

Unit tests, test-first:

- `skip_for`: 429 and 402 with and without `Retry-After`, the 60 s
  default, the 1 h cap, and `None` for 401, 500 and network errors.
- The store: a skipped provider is reported skipped until its instant and
  not after; Ollama is never skipped; a notice is produced once per skip
  period.
- Provider order: explicit Groq while skipped goes to Ollama with
  `"ollama"`, and to OpenRouter (when it has a key) with `"auto"`; the
  auto chain leaves out skipped providers; voice-edit follows the same
  order.
- Config: old config files load with `"ollama"`.

Then `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
and the full `cargo test`. A live check starts a skip period by hand (no
real limit is needed) and confirms the notification and that the next
dictation does not call the skipped provider.

## Out of scope

- Groq speech-to-text (already falls back to local Whisper).
- Remembering skip periods across restarts.
- Parsing Groq's `x-ratelimit-reset-*` headers; `Retry-After` is enough.
