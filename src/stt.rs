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

impl Stt {
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut ctx_params = WhisperContextParameters::new();
        // Explicit: Metal GPU with `metal` feature (default would also be true via _gpu).
        ctx_params.use_gpu(true);
        let ctx = WhisperContext::new_with_params(path, ctx_params)
            .map_err(|e| format!("load model {}: {e:?}", path.display()))?;
        // Create the Metal state once — allocates kv-cache, compute buffers, and
        // compiles Metal pipelines here so `transcribe` never pays that cost again
        // (~200 ms saved per transcription; eliminates per-cycle ggml_metal_init
        // and whisper_init_state overhead).
        let state = ctx
            .create_state()
            .map_err(|e| format!("create state: {e:?}"))?;
        Ok(Self { state, ctx })
    }

    /// Input MUST be 16kHz mono — callers pass `vad::transcribe_ready` output.
    /// Empty input short-circuits to Ok("") without touching the model.
    /// Reuses the cached WhisperState: no Metal re-init, no buffer re-allocation.
    pub fn transcribe(&mut self, samples_16k: &[f32]) -> Result<String, String> {
        if samples_16k.is_empty() {
            return Ok(String::new());
        }
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(num_threads());
        params.set_print_progress(false);
        params.set_single_segment(true);
        // Skip timestamp token computation — wiflow never uses timestamps.
        params.set_no_timestamps(true);
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
        Ok(post_process(&text))
    }
}

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
pub fn post_process(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(trimmed.len());
    let mut capitalize_next = true;
    for i in 0..len {
        let c = chars[i];
        if c == 'i' {
            let prev_boundary = i == 0
                || chars[i - 1].is_whitespace()
                || matches!(chars[i - 1], '(' | '[' | '{' | '"' | '“' | '‘');
            let next = chars.get(i + 1).copied();
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
                out.push('I');
                capitalize_next = false;
                continue;
            }
        }
        if capitalize_next && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            capitalize_next = false;
            continue;
        }
        if matches!(c, '.' | '?' | '!') {
            // Sentence-ending punctuation only when followed by whitespace or
            // end-of-text — a '.' before a digit is a decimal point ("1.0").
            let next_is_space = chars.get(i + 1).map(|n| n.is_whitespace()).unwrap_or(true);
            if next_is_space {
                capitalize_next = true;
            }
        }
        out.push(c);
    }
    out
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
        assert!(transcribe_shared(Path::new("/nonexistent/ggml.bin"), &[0.1; 160]).is_err());
        // Retry allowed: second call re-attempts (no poisoned cache).
        assert!(transcribe_shared(Path::new("/nonexistent/ggml.bin"), &[0.1; 160]).is_err());
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
    #[ignore]
    fn test_transcribe_tone_with_real_model() {
        let path = model_path();
        if !verify_model(&path) {
            eprintln!("skipped: model missing");
            return;
        }
        let mut stt = Stt::load(&path).expect("load");
        let tone: Vec<f32> = (0..16_000).map(|i| 0.5 * (i as f32 * 0.02).sin()).collect();
        let text = stt.transcribe(&tone).expect("transcribe must not error");
        eprintln!("tone transcript: {text:?}");
    }
}
