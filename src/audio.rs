use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{
    traits::{Consumer, RingBuffer},
    HeapRb,
};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

// Phase 1: AudioCapture/rms consumed by Task 4 wiring; allow dead_code until then.
#[allow(dead_code)]
#[derive(Debug)]
pub struct AudioError(pub String);

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "audio error: {}", self.0)
    }
}
impl std::error::Error for AudioError {}

#[allow(dead_code)]
#[derive(Debug)]
pub struct CapturedAudio {
    pub samples_16k_mono: Vec<f32>,
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

#[allow(dead_code)]
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

#[allow(dead_code)]
pub struct AudioCapture {
    stream: cpal::Stream,
    ring: Arc<Mutex<HeapRb<f32>>>,
    started_ms: u64,
    sample_rate: u32,
}

#[allow(dead_code)]
fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[allow(dead_code)]
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
        let sample_rate = cfg.max_sample_rate().0.max(cfg.min_sample_rate().0);
        let config = cfg.with_sample_rate(cpal::SampleRate(sample_rate)).config();
        let ring = Arc::new(Mutex::new(HeapRb::<f32>::new(sample_rate as usize * 70)));
        let ring_clone = ring.clone();
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _| {
                    if let Ok(mut rb) = ring_clone.lock() {
                        for &s in data {
                            let _ = rb.push_overwrite(s);
                        }
                    }
                },
                |err| warn!("audio stream error: {err}"),
                None,
            )
            .map_err(|e| AudioError(e.to_string()))?;
        stream.play().map_err(|e| AudioError(e.to_string()))?;
        info!("capture started @ {sample_rate}Hz");
        Ok(Self {
            stream,
            ring,
            started_ms: now_ms(),
            sample_rate,
        })
    }

    pub fn stop(self) -> CapturedAudio {
        drop(self.stream);
        let samples: Vec<f32> = self
            .ring
            .lock()
            .map(|rb| rb.iter().copied().collect())
            .unwrap_or_default();
        let duration_ms = now_ms().saturating_sub(self.started_ms);
        CapturedAudio {
            samples_16k_mono: samples,
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
    #[allow(clippy::len_zero)]
    fn test_list_devices_returns_vec() {
        let devs = list_devices();
        assert!(
            devs.len() >= 1,
            "expected at least default device, got {:?}",
            devs
        );
    }
}
