use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct Stt {
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
        WhisperContext::new_with_params(path, ctx_params)
            .map(|ctx| Self { ctx })
            .map_err(|e| format!("load model {}: {e:?}", path.display()))
    }

    /// Input MUST be 16kHz mono — callers pass `vad::transcribe_ready` output.
    /// Empty input short-circuits to Ok("") without touching the model.
    pub fn transcribe(&mut self, samples_16k: &[f32]) -> Result<String, String> {
        if samples_16k.is_empty() {
            return Ok(String::new());
        }
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(num_threads());
        params.set_print_progress(false);
        params.set_single_segment(true);
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| format!("create state: {e:?}"))?;
        state
            .full(params, samples_16k)
            .map_err(|e| format!("transcribe: {e:?}"))?;
        let n = state.full_n_segments();
        let mut text = String::new();
        for i in 0..n {
            if let Some(seg) = state.get_segment(i) {
                text.push_str(
                    &seg.to_str_lossy()
                        .map_err(|e| format!("segment text: {e:?}"))?,
                );
            }
        }
        Ok(text.trim().to_string())
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
    let path = model_path();
    if verify_model(&path) {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir models: {e:?}"))?;
    }
    let status = std::process::Command::new("curl")
        .args(["-fSL", "-C", "-", "-o"])
        .arg(&path)
        .arg(MODEL_URL)
        .status()
        .map_err(|e| format!("spawn curl: {e:?}"))?;
    if !status.success() || !verify_model(&path) {
        return Err(format!("download failed: {status}"));
    }
    Ok(path)
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
