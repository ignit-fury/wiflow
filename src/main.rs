mod audio;
#[allow(dead_code)]
mod history;
mod hotkey;
#[allow(dead_code)]
mod inject;
mod stt;
mod vad;

use clap::Parser;
use hotkey::{PttEvent, PushToTalk};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "wiflow-dictation")]
struct Args {
    #[arg(long)]
    list_devices: bool,
    #[arg(long)]
    dump_wav: bool,
    #[arg(long)]
    device: Option<String>,
    /// Override model path (default: auto-download base.en to Application Support)
    #[arg(long)]
    model: Option<std::path::PathBuf>,
    /// Simulate hold of N ms without global hotkey (for headless test)
    #[arg(long)]
    simulate_hold_ms: Option<u64>,
}

fn dump_wav(path: &str, samples: &[f32], rate: u32) -> Result<(), Box<dyn std::error::Error>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample(crate::audio::f32_to_i16(s))?;
    }
    w.finalize()?;
    Ok(())
}

fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    if args.list_devices {
        for d in audio::list_devices() {
            println!("{d}");
        }
        return;
    }
    let mut ptt = PushToTalk::new(300, 60_000);
    if let Some(hold) = args.simulate_hold_ms {
        info!("simulate hold {hold}ms (no hotkey needed)");
        assert!(matches!(ptt.on_key_down(0), PttEvent::Started));
        let cap = match audio::AudioCapture::start(args.device.clone()) {
            Ok(c) => c,
            Err(e) => {
                warn!("capture failed (expected in CI without mic): {e}");
                return;
            }
        };
        std::thread::sleep(std::time::Duration::from_millis(hold.min(3000)));
        let out = cap.stop();
        info!(
            "captured {} samples @ {}Hz device-ms={} rms={:.3}",
            out.samples_mono.len(),
            out.sample_rate,
            out.duration_ms,
            audio::rms(&out.samples_mono)
        );
        let mut vad = vad::Vad::new();
        let kept = vad::transcribe_ready(&out.samples_mono, out.sample_rate, &mut vad);
        info!(
            "vad kept {}/{} raw @ {}Hz",
            kept.len(),
            out.samples_mono.len(),
            out.sample_rate
        );
        if kept.is_empty() {
            info!("no speech detected");
            return;
        }
        match ptt.on_key_up(out.duration_ms) {
            PttEvent::Transcribe { duration_ms } => {
                info!(
                    "would transcribe {duration_ms}ms ({} vad samples)",
                    kept.len()
                );
                let model_path = match &args.model {
                    Some(p) => {
                        if !stt::verify_model(p) {
                            warn!(
                                "custom model fails size check, attempting load anyway: {}",
                                p.display()
                            );
                        }
                        p.clone()
                    }
                    None => match stt::ensure_model() {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("model unavailable: {e}");
                            return;
                        }
                    },
                };
                let t0 = std::time::Instant::now();
                let mut stt = match stt::Stt::load(&model_path) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("stt load failed: {e}");
                        return;
                    }
                };
                let load_ms = t0.elapsed().as_millis();
                let t1 = std::time::Instant::now();
                match stt.transcribe(&kept) {
                    Ok(text) => {
                        let ms = t1.elapsed().as_millis();
                        let kept_ms = kept.len() as f64 / vad::VAD_SAMPLE_RATE as f64 * 1000.0;
                        let rtf = ms as f64 / kept_ms.max(1.0);
                        info!("model loaded in {load_ms}ms, transcribed in {ms}ms (RTF {rtf:.2})");
                        println!("TRANSCRIPT: {text}");
                    }
                    Err(e) => warn!("transcribe failed: {e}"),
                }
                if args.dump_wav {
                    match dump_wav("/tmp/wiflow_hold.wav", &kept, vad::VAD_SAMPLE_RATE) {
                        Ok(()) => info!("dumped /tmp/wiflow_hold.wav"),
                        Err(e) => warn!("wav dump failed: {e}"),
                    }
                }
            }
            e => info!("discarded: {:?}", e),
        }
        return;
    }
    println!(
        "Phase 5: tray + global-hotkey wiring lands here. Use --simulate-hold-ms 1500 for now."
    );
}
