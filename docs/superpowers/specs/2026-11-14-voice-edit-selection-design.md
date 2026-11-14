# Voice-edit selected text — design

Date: 2026-11-14
Status: draft, awaiting review

## Goal

Select text in any app, hold the push-to-talk key together with Shift, say an
instruction ("make it shorter", "fix the grammar", "sound more polite",
"translate to Spanish"), and the selection is replaced with the edited text.

Why: usage history shows short chat-style dictations (median 5 words). The
biggest gain is saving the manual steps of rewriting and retyping text that
already exists, not longer dictation.

## Decisions

- **Trigger:** the push-to-talk key (Right Option or Fn, the bare-modifier
  presets that run on the event tap) held together with Shift. The key alone dictates exactly as today. No new hotkey is registered,
  and dictating over a selection keeps working.
- **Instructions:** open-ended. One prompt goes to the existing cleanup
  provider chain. There is no fixed command list.
- **Approach:** add an "edit" mode to the existing cleanup code. A separate
  Ollama-only module was rejected (duplicates provider and timeout code).
  Reusing today's cleanup was rejected (its safety check
  `candidate_preserves_input` rejects any change to the user's words).

## Flow

1. The user selects text, then holds the PTT key plus Shift and speaks.
2. The event tap notes that Shift was held at any point while the PTT key was
   down. This is a flag in shared state, set by the tap and read and cleared by
   the worker on key-up. `PttDown` and `PttUp` events are unchanged.
3. On key-up, in edit mode, the worker starts two things in parallel:
   speech-to-text on the instruction, and reading the selected text.
4. The instruction gets only the existing light `post_process` step. It skips
   the cleanup chain and the preserve-input guard.
5. `core::edit::edit_text(instruction, selection, cfg)` calls the configured
   provider chain with the system rule "apply the instruction to the text;
   output only the edited text".
6. The result is sent as `DaemonEvent::Done { text }`, so the existing
   inject step pastes it over the still-selected text. Cmd+Z in the target app
   undoes it. The recording pill, ducking, mic teardown and session lifecycle
   are unchanged.

## Components

| Piece | Change |
|---|---|
| `src/platform/macos/tap.rs` | While the PTT key is down, record whether Shift is down. Pure helper `edit_mode(flags)`, unit-tested. |
| `src/platform/macos/selection.rs` (new) | `selected_text() -> Option<String>`: read `AXSelectedText` of the focused element; fall back to Cmd+C and restore the clipboard. Returns `None` in a password field (reuses `password_field_focused`). |
| `src/core/edit.rs` (new) | `build_edit_prompt`, `check_result`, `edit_text`. |
| `src/daemon.rs` | In `pipeline_on_worker`, branch on edit mode: read selection in a thread while STT runs, skip cleanup for the instruction, call `edit_text`. |
| `src/core/cleanup.rs` | `CleanupBudget` gains a constructor taking a duration; the Ollama request timeout and token cap become parameters. Normal dictation keeps the 2 s budget and 1.5 s Ollama timeout. Edits use about 8 s. |
| Docs | README usage line, `memory.md` decision entry. `rules.md` §5 needs no change: still push-to-talk only, mic indicator unchanged. |

## Result checks (`check_result`)

- Strip wrappers such as "Here is the edited text:" and surrounding quotes,
  reusing `sanitize_llm_response`.
- Reject an empty result.
- Reject a result more than 6x the selection length when that exceeds 200
  characters (a runaway or chatty answer). "Make it longer" and bullets stay
  well inside this.

A rejected result means nothing is pasted and a notice is shown.

## Failure behavior

All failures leave the user's text untouched and show the existing warning
notification.

| Case | Behavior |
|---|---|
| Nothing selected | "Select text first, then hold the key with Shift." |
| Focused field is a password field | No selection read, no edit. |
| AI cleanup turned off in config | Notice pointing to Settings; edits need the cleanup model. |
| Provider unreachable, timed out or erroring | Notice with the provider error. |
| Instruction transcribes to nothing | Treated as "no speech detected": nothing happens. |
| Result rejected by `check_result` | Nothing pasted; notice. |

## Testing

- **Unit:** `edit_mode` flag detection, prompt building, `check_result` cases
  (wrapper, empty, runaway, normal), the `CleanupBudget` duration constructor.
- **Provider call:** `edit_text` against a localhost fake model, matching the
  existing rule of no real LLM calls in `cargo test`.
- **Live:** in TextEdit, select text, send a synthetic Right Option + Shift
  hold with a spoken instruction through BlackHole, and verify the text is
  replaced and Cmd+Z restores it. Also verify plain dictation is unchanged and
  that no stuck-mic regression appears (`cargo test named_mic -- --ignored`).

## Out of scope (v1)

- The Ctrl+Space preset. It is a registered global hotkey, not an event-tap
  key, so Shift cannot be detected the same way; Ctrl+Space dictates as today.

- Special pill label for edit mode.
- A voice command to undo an edit (Cmd+Z already works).
- A fixed list of named edit commands or per-command settings.
- Selection reading in apps where both Accessibility and Cmd+C fail; these get
  the "Select text first" notice.

## Risks

- Some apps hide the selection from Accessibility; the Cmd+C fallback covers
  most of them.
- `llama3.2:3b` is reliable for "shorter", "fix grammar" and "translate" and
  less reliable for complex instructions. The provider chain lets users point
  edits at a larger model.
- Edited text appears in history like any dictation.
