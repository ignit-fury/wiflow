use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

pub struct Stt {
    // State before ctx: Rust drops fields in declaration order, so
    // whisper_free_state runs before whisper_free at the C level.
    state: WhisperState,
    // Never read in Rust after `load` — load-bearing: keeps the C context
    // alive because `WhisperState`'s raw pointer references it.
    #[allow(dead_code)]
    ctx: WhisperContext,
}

fn num_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
        .min(8)
}

/// Compute backend for model load: Metal first, CPU as fallback.
/// Test-pinned policy (`backend_selection_policy`); `Stt::load` implements
/// it as try-Metal-then-fallback since availability is only proven by trying.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Backend {
    Metal,
    Cpu,
}

/// Pure backend decision: Metal when available, CPU otherwise.
/// (`Stt::load` implements the retry; this pins the policy for tests.)
#[allow(dead_code)]
pub(crate) fn backend_for(metal_ok: bool) -> Backend {
    if metal_ok {
        Backend::Metal
    } else {
        Backend::Cpu
    }
}

/// Does this load error smell like a Metal/GPU init failure (worth a CPU
/// retry) rather than a model-file problem (retry would fail identically)?
/// Conservative by design: only explicit GPU markers trigger the retry.
/// NOTE: hard aborts (`ggml_abort` inside Metal teardown/init) kill the
/// process and can never reach this classifier — the fallback covers
/// error-returning init failures only.
fn is_metal_init_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("metal") || m.contains("ggml_metal") || m.contains("mtl") || m.contains("gpu")
}

impl Stt {
    pub fn load(path: &Path) -> Result<Self, String> {
        // Metal first (fast path, unchanged behavior on success); on a
        // Metal/GPU init failure, retry the SAME model on CPU instead of
        // failing the cycle (diagram GPU/CPU fallback). File errors
        // (missing/corrupt model) are NOT retried — a CPU reload would fail
        // identically after re-parsing gigabytes.
        match Self::load_with_gpu(path, true) {
            Ok(stt) => Ok(stt),
            Err(e) if is_metal_init_error(&e) => {
                tracing::warn!("Metal backend failed ({e}) — retrying on CPU");
                Self::load_with_gpu(path, false).map_err(|e2| {
                    format!("cpu fallback failed after metal failure: {e2} (metal: {e})")
                })
            }
            Err(e) => Err(e),
        }
    }

    fn load_with_gpu(path: &Path, gpu: bool) -> Result<Self, String> {
        let mut ctx_params = WhisperContextParameters::new();
        // Explicit GPU flag (was hardcoded `true`): Metal with the `metal`
        // feature, CPU otherwise (default would also be true via _gpu).
        ctx_params.use_gpu(gpu);
        let ctx = WhisperContext::new_with_params(path, ctx_params)
            .map_err(|e| format!("load model {}: {e:?}", path.display()))?;
        // Create the backend state once — allocates kv-cache, compute
        // buffers, and compiles Metal pipelines here so `transcribe` never
        // pays that cost again (~200 ms saved per transcription; eliminates
        // per-cycle ggml_metal_init and whisper_init_state overhead).
        let state = ctx
            .create_state()
            .map_err(|e| format!("create state: {e:?}"))?;
        Ok(Self { state, ctx })
    }

    /// Input MUST be 16kHz mono — callers pass `vad::transcribe_ready` output.
    /// Empty input short-circuits to Ok("") without touching the model.
    /// Reuses the cached WhisperState: no Metal re-init, no buffer re-allocation.
    /// `initial_prompt` biases recognition with domain terms (names, jargon);
    /// empty string = no prompt (zero cost).
    /// `language` = "auto" (detect) or an ISO code ("en"); "auto"/empty skips
    /// set_language (whisper-rs FullParams::set_language takes Option<&str>).
    pub fn transcribe(
        &mut self,
        samples_16k: &[f32],
        initial_prompt: &str,
        language: &str,
    ) -> Result<String, String> {
        if samples_16k.is_empty() {
            return Ok(String::new());
        }
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(num_threads());
        params.set_print_progress(false);
        params.set_single_segment(true);
        // Skip timestamp token computation — wiflow never uses timestamps.
        params.set_no_timestamps(true);
        if !initial_prompt.is_empty() {
            params.set_initial_prompt(initial_prompt);
        }
        if language != "auto" && !language.is_empty() {
            params.set_language(Some(language));
        }
        self.state
            .full(params, samples_16k)
            .map_err(|e| format!("transcribe: {e:?}"))?;
        let n = self.state.full_n_segments();
        let mut text = String::new();
        for i in 0..n {
            if let Some(seg) = self.state.get_segment(i) {
                text.push_str(
                    &seg.to_str_lossy()
                        .map_err(|e| format!("segment text: {e:?}"))?,
                );
            }
        }
        // Cleanup pipeline: hallucination tokens → capitalization/i-fix →
        // spoken math ("A plus B" → "A+B"). Empty after token strip → Ok("")
        // (callers treat empty as no-text, nothing injected).
        let stripped = strip_hallucination_tokens(&text);
        let processed = post_process(&stripped);
        Ok(plus_to_symbol(&processed))
    }
}

static STT: OnceLock<Mutex<Option<(PathBuf, Stt)>>> = OnceLock::new();

/// Load once, reload on model switch, retry after failure (Err never sticks).
/// First-implemented fix for the Phase 4 `OnceLock<Result>` Err-sticks finding.
pub fn transcribe_shared(
    model_path: &Path,
    samples: &[f32],
    initial_prompt: &str,
    language: &str,
) -> Result<String, String> {
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
        .transcribe(samples, initial_prompt, language)
}

/// Read the initial-prompt vocabulary (prompt.txt); missing file → empty.
/// Null bytes stripped (set_initial_prompt panics on them).
pub fn read_prompt() -> String {
    std::fs::read_to_string(crate::core::config::prompt_path())
        .unwrap_or_default()
        .chars()
        .filter(|c| *c != '\0')
        .collect::<String>()
        .trim()
        .to_string()
}

/// Drop the cached WhisperContext before process exit: whisper.cpp's C++
/// static device destructor (`ggml_metal_device_free` → `ggml_metal_rsets_free`)
/// aborts (SIGABRT) when the residency set still holds entries from a leaked
/// context (crash report 2026-09-30, repro: load via static + exit without drop).
/// No-op when nothing was loaded. Call before every normal exit path.
pub fn shutdown() {
    if let Some(slot) = STT.get() {
        let mut guard = match slot.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        *guard = None;
    }
}

pub const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
/// Verified 2026-09-30 via HEAD (HTTP 200, content-length).
pub const MODEL_SIZE: u64 = 147_964_211;
pub const MODEL_NAME: &str = "ggml-base.en.bin";

pub fn models_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/wiflow/models")
}

pub fn model_path() -> PathBuf {
    models_dir().join(MODEL_NAME)
}

pub fn verify_model(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() == MODEL_SIZE)
        .unwrap_or(false)
}

/// Download base.en on first use (curl ships with macOS — no HTTP dep).
/// Skips download when a size-verified model already exists.
pub fn ensure_model() -> Result<PathBuf, String> {
    ensure_model_variant("base")
}

pub const SMALL_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin";
/// Size-gated like base.en; verified 2026-09-30 via HEAD (HTTP 200).
/// Never downloaded by tests — size-gate assertions only.
pub const SMALL_MODEL_SIZE: u64 = 487_614_201;
pub const SMALL_MODEL_NAME: &str = "ggml-small.en.bin";

pub const TINY_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin";
/// Verified 2026-10-01 via HEAD (HTTP 200, content-length 77,704,715).
pub const TINY_MODEL_SIZE: u64 = 77_704_715;
pub const TINY_MODEL_NAME: &str = "ggml-tiny.en.bin";

/// Variant-aware paths: "tiny", "small", or anything else (default "base").
pub fn model_path_for(variant: &str) -> (PathBuf, &'static str, u64) {
    if variant == "small" {
        (
            models_dir().join(SMALL_MODEL_NAME),
            SMALL_MODEL_URL,
            SMALL_MODEL_SIZE,
        )
    } else if variant == "tiny" {
        (
            models_dir().join(TINY_MODEL_NAME),
            TINY_MODEL_URL,
            TINY_MODEL_SIZE,
        )
    } else {
        (model_path(), MODEL_URL, MODEL_SIZE)
    }
}

pub fn ensure_model_variant(variant: &str) -> Result<PathBuf, String> {
    let (path, url, size) = model_path_for(variant);
    if std::fs::metadata(&path)
        .map(|m| m.len() == size)
        .unwrap_or(false)
    {
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
        || !std::fs::metadata(&path)
            .map(|m| m.len() == size)
            .unwrap_or(false)
    {
        return Err(format!("download failed: {status}"));
    }
    Ok(path)
}

/// Simple post-processing pass (pre-AI step — deterministic, offline):
/// - Capitalizes the first letter of sentences (text start and after '.', '?', '!')
/// - Capitalizes lowercase 'i' when standalone ("I") or in common contractions ("I'm", "I've", "I'll", "I'd")
/// - Preserves punctuation, whitespace, and numbers (e.g. "1.0")
pub fn post_process(text: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Cow::Borrowed(trimmed);
    }
    // Copy-on-first-change: the output buffer is allocated only when a
    // rewrite actually happens, so already-clean text costs zero allocation.
    // `prev` always holds the RAW consumed char (rewrites must not leak into
    // boundary checks); `pos` is the byte offset of the current char for the
    // prefix copy.
    let mut out: Option<String> = None;
    macro_rules! ensure {
        ($pos:expr) => {
            out.get_or_insert_with(|| {
                let mut s = String::with_capacity(trimmed.len());
                s.push_str(&trimmed[..$pos]);
                s
            })
        };
    }
    let mut capitalize_next = true;
    let mut prev: Option<char> = None;
    // Square-bracket spans are opaque to rewrites (markdown "[ok]", "[i]")
    // but transparent to the flag: "[ok] fine" keeps "[ok]" and still caps
    // the sentence. Depth-counted so nesting/unbalanced stays safe.
    let mut bracket_depth: u32 = 0;
    let mut iter = trimmed.char_indices().peekable();
    while let Some((pos, c)) = iter.next() {
        if c == '[' {
            bracket_depth = bracket_depth.saturating_add(1);
        } else if c == ']' {
            bracket_depth = bracket_depth.saturating_sub(1);
        }
        if bracket_depth > 0 && c != ']' && c != '[' {
            if let Some(o) = out.as_mut() {
                o.push(c);
            }
            prev = Some(c);
            continue;
        }
        if c == 'i' {
            let prev_boundary = match prev {
                None => true,
                Some(p) => p.is_whitespace() || matches!(p, '(' | '[' | '{' | '"' | '“' | '‘'),
            };
            let next = iter.peek().map(|(_, n)| *n);
            let is_standalone = match next {
                None => true,
                Some(n) => {
                    n.is_whitespace()
                        || matches!(
                            n,
                            ')' | ']' | '}' | '"' | '”' | '’' | ',' | '.' | '?' | '!' | ':' | ';'
                        )
                }
            };
            let is_contraction = matches!(next, Some('\''));
            if prev_boundary && (is_standalone || is_contraction) {
                ensure!(pos).push('I');
                capitalize_next = false;
                prev = Some('i');
                continue;
            }
        }
        // Any alphanumeric opens the sentence (digits included: "1.0 plus"
        // must not capitalize "plus"); only lowercase alpha rewrites.
        if capitalize_next && c.is_alphanumeric() {
            if c.is_alphabetic() {
                let mut up = c.to_uppercase();
                if up.next() != Some(c) || up.next().is_some() {
                    ensure!(pos).extend(c.to_uppercase());
                }
            }
            capitalize_next = false;
            prev = Some(c);
            continue;
        }
        if matches!(c, '.' | '?' | '!') {
            // Sentence-ending punctuation only when followed by whitespace or
            // end-of-text — a '.' before a digit is a decimal point ("1.0").
            let next_is_space = iter.peek().map(|(_, n)| n.is_whitespace()).unwrap_or(true);
            if next_is_space {
                capitalize_next = true;
            }
        }
        if let Some(o) = out.as_mut() {
            o.push(c);
        }
        prev = Some(c);
    }
    match out {
        Some(o) => Cow::Owned(o),
        None => Cow::Borrowed(trimmed),
    }
}

/// Remove bracketed ALL-CAPS Whisper tokens ([BLANK_AUDIO], [MUSIC],
/// [APPLAUSE], [BLANK_AUDIO]) that silent/noisy holds produce — the user's
/// history shows them getting pasted into documents. Lowercase bracket
/// content (markdown links, [ok]) is preserved. One trailing space after a
/// stripped token is consumed.
pub(crate) fn strip_hallucination_tokens(text: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    // Borrowed fast path: no '[' means no token possible — zero allocation.
    // Byte walk below (no Vec<char>): '[' / ']' / ' ' are ASCII so they can
    // never match inside a multibyte sequence; non-ASCII bytes fail the
    // token check exactly like the old char comparison did.
    if !text.contains('[') {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut removed = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(rel) = text[i + 1..].find(']') {
                let inner = &text[i + 1..i + 1 + rel];
                let is_token =
                    !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_uppercase() || b == b'_');
                if is_token {
                    removed = true;
                    i += rel + 2;
                    if i < bytes.len() && bytes[i] == b' ' {
                        i += 1;
                    }
                    continue;
                }
            }
        }
        if bytes[i] < 0x80 {
            out.push(bytes[i] as char);
            i += 1;
        } else {
            let ch = text[i..].chars().next().expect("char boundary");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    if removed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(text)
    }
}

/// Alphanumeric core of a word, ignoring SURROUNDING punctuation only:
/// "(A" → "A", "B," → "B". Inner punctuation is untouched ("1.0" → "1.0",
/// so decimals never match the single-letter plus check).
fn core_word(w: &str) -> &str {
    let s = w.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    s.trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
}

fn is_single_alnum(w: &str) -> bool {
    let c = core_word(w);
    c.chars().count() == 1 && c.chars().next().is_some_and(|x| x.is_ascii_alphanumeric())
}

/// Spoken math → symbol: "A plus B" → "A+B", "A plus B, then" → "A+B, then",
/// "A plus B plus C" → "A+B+C". Only between single alphanumeric
/// letters/digits (punctuation attached to the letter is kept); normal prose
/// ("the cost plus tax") is untouched.
pub(crate) fn plus_to_symbol(text: &str) -> String {
    // Fast path: no "plus" word and spacing already regular (ASCII, single
    // spaces, none at the ends) → output equals input, single copy.
    // Anything else (tabs, newlines, non-ASCII whitespace, runs) falls
    // through to the general path below.
    if text.is_ascii()
        && !text.split_whitespace().any(|w| w == "plus")
        && !text.starts_with(char::is_whitespace)
        && !text.ends_with(char::is_whitespace)
        && !text.contains("  ")
        && !text.contains(['\t', '\n', '\x0B', '\x0C', '\r'])
    {
        return text.to_string();
    }
    // Lazy word stream (no Vec<&str>): first three words buffered in a fixed
    // array to preserve the <3-words fast path, then chained back.
    let mut words = text.split_whitespace();
    let (Some(w0), Some(w1), third) = (words.next(), words.next(), words.next()) else {
        return text.to_string();
    };
    let rest = third.into_iter().chain(words);
    let mut words = [w0, w1].into_iter().chain(rest).peekable();
    // Output never exceeds input: '+' replaces " plus " and runs collapse.
    let mut out = String::with_capacity(text.len());
    let mut prev: Option<&str> = None;
    while let Some(w) = words.next() {
        if w == "plus"
            && prev.is_some_and(is_single_alnum)
            && words.peek().is_some_and(|n| is_single_alnum(n))
        {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push('+');
        } else {
            out.push_str(w);
            out.push(' ');
        }
        prev = Some(w);
    }
    // Trailing space exists only when the last word was pushed normally —
    // skip the copy otherwise.
    if out.ends_with(char::is_whitespace) {
        out.trim_end().to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_load_missing_model_is_err() {
        assert!(Stt::load(Path::new("/nonexistent/ggml-base.en.bin")).is_err());
    }

    #[test]
    fn backend_selection_policy() {
        assert_eq!(backend_for(true), Backend::Metal);
        assert_eq!(backend_for(false), Backend::Cpu);
    }

    #[test]
    fn metal_init_classifier() {
        // GPU smells → retry-worthy.
        assert!(is_metal_init_error("ggml_metal_init failed: no device"));
        assert!(is_metal_init_error(
            "load model x.bin: Metal context creation failed (mtl)"
        ));
        assert!(is_metal_init_error("GPU backend unavailable"));
        // File problems → fail fast, never retry (a CPU reload would fail
        // identically after re-parsing gigabytes).
        assert!(!is_metal_init_error(
            "load model /nonexistent/ggml.bin: failed to open file"
        ));
        assert!(!is_metal_init_error("create state: out of memory"));
        assert!(!is_metal_init_error(""));
    }

    #[test]
    fn test_model_path_name() {
        assert_eq!(model_path().file_name().unwrap(), MODEL_NAME);
    }

    #[test]
    fn test_verify_model_size_gate() {
        let p = std::env::temp_dir().join("wiflow_verify_test.bin");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(MODEL_SIZE).unwrap(); // sparse — instant, no disk use
        drop(f);
        assert!(verify_model(&p));
        std::fs::remove_file(&p).unwrap();
        assert!(!verify_model(&p));
    }

    #[test]
    fn test_transcribe_shared_bad_path_is_err() {
        assert!(
            transcribe_shared(Path::new("/nonexistent/ggml.bin"), &[0.1; 160], "", "").is_err()
        );
        // Retry allowed: second call re-attempts (no poisoned cache).
        assert!(
            transcribe_shared(Path::new("/nonexistent/ggml.bin"), &[0.1; 160], "", "").is_err()
        );
    }

    #[test]
    fn test_read_prompt_missing_is_empty() {
        // Default machine state: prompt.txt not yet created → empty prompt.
        let p = crate::core::config::prompt_path();
        let existed = p.exists();
        if existed {
            let saved = std::fs::read_to_string(&p).unwrap();
            assert!(!read_prompt().contains('\0'));
            let _ = saved;
        } else {
            assert_eq!(read_prompt(), "");
        }
    }

    #[test]
    fn test_model_path_for_variants() {
        let (base_path, base_url, base_size) = model_path_for("base");
        assert_eq!(base_path.file_name().unwrap(), MODEL_NAME);
        assert_eq!(base_url, MODEL_URL);
        assert_eq!(base_size, MODEL_SIZE);
        let (small_path, small_url, small_size) = model_path_for("small");
        assert_eq!(small_path.file_name().unwrap(), SMALL_MODEL_NAME);
        assert_eq!(small_url, SMALL_MODEL_URL);
        assert_eq!(small_size, SMALL_MODEL_SIZE);
        // Unknown variant falls back to base (never a download-by-typo).
        assert_eq!(model_path_for("bogus").1, MODEL_URL);
    }

    #[test]
    fn test_small_model_size_gate() {
        // Sparse file: size-gate only, no 465MB download, instant.
        let p = std::env::temp_dir().join("wiflow_small_gate_test.bin");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(SMALL_MODEL_SIZE).unwrap();
        drop(f);
        let (path, _, size) = model_path_for("small");
        assert_eq!(path.file_name().unwrap(), SMALL_MODEL_NAME);
        assert!(std::fs::metadata(&p)
            .map(|m| m.len() == size)
            .unwrap_or(false));
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_post_process_empty() {
        assert_eq!(post_process(""), "");
        assert_eq!(post_process("   "), "");
    }

    #[test]
    fn test_post_process_capitalizes_sentences() {
        assert_eq!(post_process("hello world"), "Hello world");
        assert_eq!(
            post_process("first one. second one? third! done"),
            "First one. Second one? Third! Done"
        );
    }

    #[test]
    fn test_post_process_fixes_standalone_i() {
        assert_eq!(post_process("i think i can"), "I think I can");
        assert_eq!(post_process("and i'm here"), "And I'm here");
        assert_eq!(post_process("i've got it"), "I've got it");
    }

    #[test]
    fn test_post_process_preserves_words_with_i() {
        // 'i' inside words or non-boundary positions must NOT be capitalized.
        assert_eq!(post_process("wifi is fast"), "Wifi is fast");
        assert_eq!(post_process("it's fine"), "It's fine");
        assert_eq!(post_process("value 1.0 stays"), "Value 1.0 stays");
    }

    #[test]
    fn test_tiny_model_variant() {
        let (path, url, size) = model_path_for("tiny");
        assert_eq!(path.file_name().unwrap(), TINY_MODEL_NAME);
        assert_eq!(url, TINY_MODEL_URL);
        assert_eq!(size, TINY_MODEL_SIZE);
        // Sparse-file size gate: instant, no 75MB download.
        let p = std::env::temp_dir().join("wiflow_tiny_gate_test.bin");
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(TINY_MODEL_SIZE).unwrap();
        drop(f);
        assert!(std::fs::metadata(&p)
            .map(|m| m.len() == size)
            .unwrap_or(false));
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn test_strip_hallucination_tokens() {
        assert_eq!(strip_hallucination_tokens("[BLANK_AUDIO]"), "");
        assert_eq!(strip_hallucination_tokens("[MUSIC]"), "");
        assert_eq!(
            strip_hallucination_tokens("[BLANK_AUDIO] hello world"),
            "hello world"
        );
        assert_eq!(
            strip_hallucination_tokens("hello [MUSIC] world"),
            "hello world"
        );
        // Lowercase bracket content preserved (markdown links, [ok]).
        assert_eq!(
            strip_hallucination_tokens("see [the docs] here"),
            "see [the docs] here"
        );
        assert_eq!(strip_hallucination_tokens("[ok]"), "[ok]");
        // Unbalanced bracket untouched.
        assert_eq!(
            strip_hallucination_tokens("[BLANK_AUDIO hello"),
            "[BLANK_AUDIO hello"
        );
    }

    #[test]
    fn test_unified_pass_whitespace_and_unicode() {
        // Whitespace runs still collapse (math stage splits on them).
        assert_eq!(post_process("hello   world"), "Hello   world");
        assert_eq!(
            crate::core::baseline::deterministic_clean("hello   world"),
            "Hello world"
        );
        // Multibyte text through the byte-walk strip + stream post-process.
        assert_eq!(
            crate::core::baseline::deterministic_clean("café [MUSIC] naïve"),
            "Café naïve"
        );
        assert_eq!(post_process("über alles"), "Über alles");
        // Token directly adjacent to multibyte chars.
        assert_eq!(strip_hallucination_tokens("[BLANK_AUDIO]café"), "café");
    }

    #[test]
    fn test_borrowed_fast_path() {
        use std::borrow::Cow;
        // Already-clean text borrows (zero allocation); dirty text owns.
        assert!(matches!(post_process("Hello world"), Cow::Borrowed(_)));
        assert!(matches!(post_process("  Hello world  "), Cow::Borrowed(_)));
        assert!(matches!(post_process("hello world"), Cow::Owned(_)));
        assert!(matches!(post_process("i think"), Cow::Owned(_)));
        assert!(matches!(
            strip_hallucination_tokens("hello"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            strip_hallucination_tokens("[ok] stays"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            strip_hallucination_tokens("[MUSIC] hi"),
            Cow::Owned(_)
        ));
        // Borrowed content still equals the old owned bytes.
        let b: Cow<'_, str> = post_process("Hello world");
        assert_eq!(b, "Hello world");
        assert_eq!(strip_hallucination_tokens("[BLANK_AUDIO]"), "");
    }

    #[test]
    fn test_plus_fast_path() {
        // No "plus" + regular spacing → single copy, identical bytes.
        assert_eq!(plus_to_symbol("Hello world today"), "Hello world today");
        // Irregular spacing still normalizes via the general path.
        assert_eq!(plus_to_symbol("Hello   world"), "Hello world");
        assert_eq!(plus_to_symbol("Hello\tworld"), "Hello world");
        assert_eq!(plus_to_symbol("Hello\nworld"), "Hello world");
        assert_eq!(plus_to_symbol("Hello\r\nworld"), "Hello world");
        // Non-breaking space is whitespace too (never fast-pathed).
        assert_eq!(plus_to_symbol("Hello world"), "Hello world");
    }

    #[test]
    fn test_leading_digits_open_sentence() {
        // Phase 7: a leading number must not capitalize the next word.
        assert_eq!(post_process("1.0 plus 2.0"), "1.0 plus 2.0");
        assert_eq!(post_process("20% off today"), "20% off today");
        assert_eq!(post_process("3 blind mice"), "3 blind mice");
        // Quoted/parenthesized starts still capitalize inside.
        assert_eq!(post_process("\"hello\" she said"), "\"Hello\" she said");
    }

    #[test]
    fn test_bracket_spans_opaque_to_rewrites() {
        // Phase 7: markdown bracket content preserved byte-identical, while
        // sentence capitalization still applies outside the brackets.
        assert_eq!(post_process("[ok] fine"), "[ok] Fine");
        assert_eq!(post_process("see [the docs] here"), "See [the docs] here");
        assert_eq!(post_process("[i] think so"), "[i] Think so");
        assert_eq!(post_process("a [b [c] d"), "A [b [c] d");
        assert_eq!(post_process("see [the docs here"), "See [the docs here");
    }

    #[test]
    fn test_plus_streaming_edges() {
        // Leading "plus" never converts (needs a left neighbor).
        assert_eq!(plus_to_symbol("plus A plus B"), "plus A+B");
        assert_eq!(plus_to_symbol("A plus"), "A plus");
        assert_eq!(plus_to_symbol("plus plus plus"), "plus plus plus");
        assert_eq!(plus_to_symbol("A  plus   B"), "A+B");
        assert_eq!(plus_to_symbol("a plus b plus c plus d"), "a+b+c+d");
    }

    #[test]
    fn test_plus_to_symbol() {
        assert_eq!(plus_to_symbol("A plus B"), "A+B");
        assert_eq!(plus_to_symbol("a plus b"), "a+b");
        assert_eq!(plus_to_symbol("A plus B, then why"), "A+B, then why");
        assert_eq!(plus_to_symbol("A plus B plus C"), "A+B+C");
        assert_eq!(plus_to_symbol("(A) plus [B]"), "(A)+[B]");
        assert_eq!(plus_to_symbol("1 plus 2 equals 3"), "1+2 equals 3");
        // Prose untouched: multi-letter words, decimals.
        assert_eq!(plus_to_symbol("the cost plus tax"), "the cost plus tax");
        assert_eq!(plus_to_symbol("1.0 plus 2.0"), "1.0 plus 2.0");
        assert_eq!(plus_to_symbol("hello"), "hello");
        assert_eq!(plus_to_symbol(""), "");
    }

    #[test]
    #[ignore]
    fn test_transcribe_tone_with_real_model() {
        let path = model_path();
        if !verify_model(&path) {
            eprintln!("skipped: model missing");
            return;
        }
        let mut stt = Stt::load(&path).expect("load");
        let tone: Vec<f32> = (0..16_000).map(|i| 0.5 * (i as f32 * 0.02).sin()).collect();
        let text = stt
            .transcribe(&tone, "", "")
            .expect("transcribe must not error");
        eprintln!("tone transcript: {text:?}");
    }
}
