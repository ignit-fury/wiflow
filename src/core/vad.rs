use crate::core::audio::rms;
use webrtc_vad::{SampleRate, Vad as WebrtcVad, VadMode};

pub const VAD_SAMPLE_RATE: u32 = 16_000;
/// 30ms frames at 16kHz — the only size `trim_silence` feeds the detector.
pub const FRAME_SAMPLES: usize = 480;
/// Keep ~210ms of context audio around speech (7 * 30ms).
pub const PAD_FRAMES: usize = 7;

/// Low-pass taps for downsampling: Blackman-windowed sinc, 7 kHz cutoff.
/// Without it, 8–22 kHz content folds into the speech band at 16 kHz.
fn lowpass_taps(from_rate: u32) -> Vec<f32> {
    const N: usize = 101;
    let fc = 7_000.0 / from_rate as f64;
    let mid = (N / 2) as f64;
    let mut taps: Vec<f64> = (0..N)
        .map(|i| {
            let t = i as f64 - mid;
            let sinc = if t == 0.0 {
                2.0 * fc
            } else {
                (2.0 * std::f64::consts::PI * fc * t).sin() / (std::f64::consts::PI * t)
            };
            let x = 2.0 * std::f64::consts::PI * i as f64 / (N - 1) as f64;
            sinc * (0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos())
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    taps.iter_mut().for_each(|t| *t /= sum);
    taps.into_iter().map(|t| t as f32).collect()
}

/// Filtered sample at `center`; edges repeat the first/last sample.
fn filtered_at(samples: &[f32], taps: &[f32], center: usize) -> f32 {
    let half = taps.len() / 2;
    let last = samples.len() - 1;
    if center >= half && center + half <= last {
        let win = &samples[center - half..=center + half];
        return win.iter().zip(taps).map(|(s, t)| s * t).sum();
    }
    taps.iter()
        .enumerate()
        .map(|(j, t)| samples[(center + j).saturating_sub(half).min(last)] * t)
        .sum()
}

/// Resample any mono rate to 16kHz mono: low-pass first when downsampling,
/// then linear interpolation between filtered neighbours.
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
    let taps = (from_rate > VAD_SAMPLE_RATE).then(|| lowpass_taps(from_rate));
    let at = |k: usize| match &taps {
        Some(t) => filtered_at(samples, t, k),
        None => samples[k],
    };
    (0..out_len)
        .map(|i| {
            let pos = i as f64 / ratio;
            let lo = (pos.floor() as usize).min(samples.len() - 1);
            let frac = (pos - lo as f64) as f32;
            let hi = (lo + 1).min(samples.len() - 1);
            at(lo) * (1.0 - frac) + at(hi) * frac
        })
        .collect()
}

/// Below this RMS the input is room tone, not speech (measured room: 0.002).
pub const MIN_SPEECH_RMS: f32 = 0.01;

/// True when any 30 ms window reaches speech level. A whole-clip average
/// let long pauses pull quiet speech under the threshold and drop it.
pub fn has_speech_energy(samples: &[f32], rate: u32) -> bool {
    let window = (rate as usize * 30 / 1000).max(1);
    samples.chunks(window).any(|w| rms(w) >= MIN_SPEECH_RMS)
}

/// Single enforced entry point for STT input: energy gate → resample → trim.
/// Guarantees the 16kHz contract by construction (fixes Phase 2 review finding).
pub fn transcribe_ready(samples: &[f32], from_rate: u32, vad: &mut Vad) -> Vec<f32> {
    if !has_speech_energy(samples, from_rate) {
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

    fn tone(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn test_resample_removes_alias_band() {
        // 12 kHz cannot exist at 16 kHz; plain interpolation folds it to 4 kHz.
        let out = resample_to_16k(&tone(12_000.0, 48_000, 0.5), 48_000);
        assert!(rms(&out[200..out.len() - 200]) < 0.02, "alias leaked");
    }

    #[test]
    fn test_resample_keeps_speech_band() {
        let out = resample_to_16k(&tone(1_000.0, 44_100, 0.5), 44_100);
        let level = rms(&out[200..out.len() - 200]);
        assert!((level - 0.5 / 2f32.sqrt()).abs() < 0.02, "level {level}");
    }

    #[test]
    fn test_speech_gate_finds_short_quiet_speech_in_long_silence() {
        let mut sig = vec![0.0; 16_000 * 5];
        sig.extend(tone(300.0, 16_000, 0.3).iter().map(|s| s * 0.1));
        assert!(rms(&sig) < MIN_SPEECH_RMS);
        assert!(has_speech_energy(&sig, 16_000));
        assert!(!has_speech_energy(&vec![0.002; 16_000], 16_000));
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
        assert!(!has_speech_energy(&vec![0.0; 1600], 16_000));
        assert!(!has_speech_energy(&vec![0.001; 1600], 16_000));
    }

    #[test]
    fn test_energy_gate_accepts_loud() {
        let loud: Vec<f32> = (0..1600).map(|i| 0.5 * (i as f32 * 0.02).sin()).collect();
        assert!(has_speech_energy(&loud, 16_000));
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
