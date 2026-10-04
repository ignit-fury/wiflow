use crate::core::audio::rms;
use webrtc_vad::{SampleRate, Vad as WebrtcVad, VadMode};

pub const VAD_SAMPLE_RATE: u32 = 16_000;
/// 30ms frames at 16kHz — the only size `trim_silence` feeds the detector.
pub const FRAME_SAMPLES: usize = 480;
/// Keep ~210ms of context audio around speech (7 * 30ms).
pub const PAD_FRAMES: usize = 7;

/// Linear-interpolate any mono rate to 16kHz mono.
pub fn resample_to_16k(samples: &[f32], from_rate: u32) -> Vec<f32> {
    if from_rate == 0 {
        return Vec::new();
    }
    if samples.is_empty() {
        return Vec::new();
    }
    if from_rate == VAD_SAMPLE_RATE {
        return samples.to_vec();
    }
    let ratio = VAD_SAMPLE_RATE as f64 / from_rate as f64;
    let out_len = ((samples.len() as f64) * ratio).round() as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 / ratio;
            let lo = pos.floor() as usize;
            let frac = (pos - lo as f64) as f32;
            let hi = (lo + 1).min(samples.len() - 1);
            samples[lo] * (1.0 - frac) + samples[hi] * frac
        })
        .collect()
}

/// Below this RMS the input is room tone, not speech (measured room: 0.002).
pub const MIN_SPEECH_RMS: f32 = 0.01;

pub fn has_speech_energy(samples: &[f32]) -> bool {
    rms(samples) >= MIN_SPEECH_RMS
}

/// Single enforced entry point for STT input: energy gate → resample → trim.
/// Guarantees the 16kHz contract by construction (fixes Phase 2 review finding).
pub fn transcribe_ready(samples: &[f32], from_rate: u32, vad: &mut Vad) -> Vec<f32> {
    if !has_speech_energy(samples) {
        return Vec::new();
    }
    let s16 = resample_to_16k(samples, from_rate);
    vad.trim_silence(&s16)
}

pub struct Vad {
    inner: WebrtcVad,
}

impl Vad {
    pub fn new() -> Self {
        Self {
            inner: WebrtcVad::new_with_rate_and_mode(SampleRate::Rate16kHz, VadMode::Aggressive),
        }
    }

    /// Return speech regions with ~210ms padding; empty vec when silence-only.
    /// Input MUST be 16kHz mono (use `resample_to_16k` first).
    pub fn trim_silence(&mut self, samples_16k: &[f32]) -> Vec<f32> {
        if samples_16k.is_empty() {
            return Vec::new();
        }
        let frames: Vec<&[f32]> = samples_16k.chunks(FRAME_SAMPLES).collect();
        let mut speech = vec![false; frames.len()];
        let mut buf = [0i16; FRAME_SAMPLES];
        for (i, f) in frames.iter().enumerate() {
            for (j, b) in buf.iter_mut().enumerate() {
                *b = if j < f.len() {
                    (f[j].clamp(-1.0, 1.0) * 32767.0) as i16
                } else {
                    0
                };
            }
            speech[i] = self.inner.is_voice_segment(&buf).unwrap_or(false);
        }
        if !speech.contains(&true) {
            return Vec::new();
        }
        let first = speech.iter().position(|&s| s).unwrap();
        let last = speech.iter().rposition(|&s| s).unwrap();
        let lo = first.saturating_sub(PAD_FRAMES) * FRAME_SAMPLES;
        let hi = ((last + PAD_FRAMES + 1) * FRAME_SAMPLES).min(samples_16k.len());
        samples_16k[lo..hi].to_vec()
    }
}

impl Default for Vad {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complex_tone_16k(secs: u32) -> Vec<f32> {
        // Loud harmonic complex with tremolo — speechlike energy for the detector.
        let n = (16_000 * secs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / 16_000.0;
                let tremolo = 0.6 + 0.4 * (2.0 * std::f32::consts::PI * 5.0 * t).sin();
                0.8 * tremolo
                    * ((2.0 * std::f32::consts::PI * 300.0 * t).sin()
                        + 0.5 * (2.0 * std::f32::consts::PI * 600.0 * t).sin()
                        + 0.25 * (2.0 * std::f32::consts::PI * 900.0 * t).sin())
                    / 1.75
            })
            .collect()
    }

    #[test]
    fn test_resample_same_rate_identity() {
        let v = vec![0.5, -0.25, 0.0, 1.0];
        assert_eq!(resample_to_16k(&v, 16_000), v);
    }

    #[test]
    fn test_resample_constant_preserved() {
        let v = vec![0.5; 480];
        let out = resample_to_16k(&v, 48_000);
        assert_eq!(out.len(), 160);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 0.01));
    }

    #[test]
    fn test_resample_empty() {
        assert!(resample_to_16k(&[], 44_100).is_empty());
    }

    #[test]
    fn test_resample_zero_rate_is_empty() {
        assert!(resample_to_16k(&[0.5; 10], 0).is_empty());
    }

    #[test]
    fn test_trim_silence_only_is_empty() {
        let mut v = Vad::new();
        assert!(v.trim_silence(&vec![0.0; 16_000]).is_empty());
    }

    #[test]
    fn test_trim_keeps_tone_core() {
        // 1s silence + 1s tone + 1s silence -> output keeps middle, drops edges.
        let mut sig = vec![0.0; 16_000];
        sig.extend(complex_tone_16k(1));
        sig.extend(vec![0.0; 16_000]);
        let mut v = Vad::new();
        let out = v.trim_silence(&sig);
        assert!(!out.is_empty(), "loud complex tone must survive trim");
        assert!(out.len() < sig.len(), "edge silence must be trimmed");
        assert!(out.len() >= 16_000 - PAD_FRAMES * FRAME_SAMPLES);
    }

    #[test]
    fn test_energy_gate_rejects_quiet() {
        assert!(!has_speech_energy(&vec![0.0; 1600]));
        assert!(!has_speech_energy(&vec![0.001; 1600]));
    }

    #[test]
    fn test_energy_gate_accepts_loud() {
        let loud: Vec<f32> = (0..1600).map(|i| 0.5 * (i as f32 * 0.02).sin()).collect();
        assert!(has_speech_energy(&loud));
    }

    #[test]
    fn test_pipeline_rejects_silence_at_any_rate() {
        let mut v = Vad::new();
        assert!(transcribe_ready(&vec![0.0; 4410], 44_100, &mut v).is_empty());
    }

    #[test]
    fn test_pipeline_keeps_tone_core() {
        let mut sig = vec![0.0; 16_000];
        sig.extend(complex_tone_16k(1));
        sig.extend(vec![0.0; 16_000]);
        let mut v = Vad::new();
        let out = transcribe_ready(&sig, 16_000, &mut v);
        assert!(!out.is_empty());
        assert!(out.len() < sig.len());
    }
}
