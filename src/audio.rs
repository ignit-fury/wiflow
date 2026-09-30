use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{
    traits::{Consumer, Producer, Split},
    HeapCons, HeapProd, HeapRb,
};
use tracing::{info, warn};

// Phase 1: AudioCapture/rms consumed by Task 4 wiring.
#[derive(Debug)]
pub struct AudioError(pub String);

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "audio error: {}", self.0)
    }
}
impl std::error::Error for AudioError {}

#[derive(Debug)]
pub struct CapturedAudio {
    /// Mono samples at NATIVE device rate (see `sample_rate`).
    /// Resampling to 16kHz happens in `vad::resample_to_16k`, not here.
    pub samples_mono: Vec<f32>,
    pub sample_rate: u32,
    pub duration_ms: u64,
}

pub fn list_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut out = Vec::new();
    if let Some(d) = host.default_input_device() {
        out.push(d.name().unwrap_or_else(|_| "<default>".into()));
    }
    match host.input_devices() {
        Ok(devs) => {
            for d in devs {
                if let Ok(name) = d.name() {
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
        }
        Err(e) => warn!("input_devices failed: {e}"),
    }
    if out.is_empty() {
        out.push("<no-input-device>".into());
    }
    out
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

pub fn i16_to_f32(s: i16) -> f32 {
    s as f32 / 32768.0
}

pub fn u16_to_f32(s: u16) -> f32 {
    (s as f32 - 32768.0) / 32768.0
}

pub struct AudioCapture {
    stream: cpal::Stream,
    consumer: HeapCons<f32>,
    started: std::time::Instant,
    sample_rate: u32,
}

impl AudioCapture {
    pub fn start(device_name: Option<String>) -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = match device_name {
            Some(n) => host
                .input_devices()
                .map_err(|e| AudioError(e.to_string()))?
                .find(|d| d.name().map(|dn| dn == n).unwrap_or(false))
                .ok_or_else(|| AudioError(format!("device not found: {n}")))?,
            None => host
                .default_input_device()
                .ok_or_else(|| AudioError("no default input".into()))?,
        };
        let mut supported: Vec<_> = device
            .supported_input_configs()
            .map_err(|e| AudioError(e.to_string()))?
            .collect();
        supported.sort_by_key(|c| {
            let r = c.min_sample_rate().0 as i32 - 16000;
            r.abs()
        });
        let cfg = supported
            .into_iter()
            .next()
            .ok_or_else(|| AudioError("no supported config".into()))?;
        let sample_format = cfg.sample_format();
        let min_rate = cfg.min_sample_rate().0;
        let max_rate = cfg.max_sample_rate().0;
        // Clamp to 16kHz: 192kHz devices blew the 70s ringbuf to ~53MB (Phase 1 review).
        let sample_rate = 16_000.clamp(min_rate, max_rate);
        let config = cfg.with_sample_rate(cpal::SampleRate(sample_rate)).config();
        // Split halves: producer owns the write side, so the realtime callback
        // never locks. Capacity 70s exceeds the 60s PTT auto-stop, therefore
        // drop-newest on full is unreachable in practice (and preferable to
        // blocking the audio thread).
        let (mut producer, consumer): (HeapProd<f32>, HeapCons<f32>) =
            HeapRb::<f32>::new(sample_rate as usize * 70).split();
        // NOTE: `producer` is moved into exactly one match arm (only one arm runs).
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    for &s in data {
                        let _ = producer.try_push(s);
                    }
                },
                |err| warn!("audio stream error: {err}"),
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    for &s in data {
                        let _ = producer.try_push(i16_to_f32(s));
                    }
                },
                |err| warn!("audio stream error: {err}"),
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    for &s in data {
                        let _ = producer.try_push(u16_to_f32(s));
                    }
                },
                |err| warn!("audio stream error: {err}"),
                None,
            ),
            fmt => return Err(AudioError(format!("unsupported sample format: {fmt:?}"))),
        }
        .map_err(|e| AudioError(e.to_string()))?;
        stream.play().map_err(|e| AudioError(e.to_string()))?;
        info!("capture started @ {sample_rate}Hz");
        Ok(Self {
            stream,
            consumer,
            started: std::time::Instant::now(),
            sample_rate,
        })
    }

    pub fn stop(mut self) -> CapturedAudio {
        drop(self.stream);
        let samples: Vec<f32> = self.consumer.pop_iter().collect();
        let duration_ms = self.started.elapsed().as_millis() as u64;
        CapturedAudio {
            samples_mono: samples,
            sample_rate: self.sample_rate,
            duration_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rms_silence_is_zero() {
        assert_eq!(rms(&[0.0; 160]), 0.0);
    }

    #[test]
    fn test_rms_full_scale_sine_positive() {
        let samples: Vec<f32> = (0..160).map(|i| (i as f32 * 0.1).sin()).collect();
        let v = rms(&samples);
        assert!(v > 0.1 && v < 1.0, "rms was {v}");
    }

    #[test]
    fn test_list_devices_returns_vec() {
        let devs = list_devices();
        assert!(
            !devs.is_empty(),
            "expected at least default device, got {:?}",
            devs
        );
    }

    #[test]
    fn test_start_bogus_device_is_err() {
        assert!(AudioCapture::start(Some("no-such-device-xyz".into())).is_err());
    }

    #[test]
    fn test_i16_to_f32_endpoints() {
        assert_eq!(i16_to_f32(0), 0.0);
        assert!((i16_to_f32(i16::MAX) - 1.0).abs() < 0.001);
        assert!((i16_to_f32(i16::MIN) + 1.0).abs() < 0.001);
    }

    #[test]
    fn test_u16_to_f32_endpoints() {
        assert!((u16_to_f32(32768) - 0.0).abs() < 0.001);
        assert!((u16_to_f32(u16::MAX) - 1.0).abs() < 0.01);
        assert!((u16_to_f32(u16::MIN) + 1.0).abs() < 0.01);
    }
}
