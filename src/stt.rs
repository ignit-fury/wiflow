use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

#[allow(dead_code)]
pub struct Stt {
    ctx: WhisperContext,
}

#[allow(dead_code)]
fn num_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
        .min(8)
}

#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_load_missing_model_is_err() {
        assert!(Stt::load(Path::new("/nonexistent/ggml-base.en.bin")).is_err());
    }
}
