mod audio;
mod hotkey;

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
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
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
        // Use the MEASURED clock, not the requested hold: sleep bound (3000ms) and
        // stream-setup latency diverge from `hold` (Phase 1 review).
        match ptt.on_key_up(out.duration_ms) {
            PttEvent::Transcribe { duration_ms } => {
                info!("would transcribe {duration_ms}ms");
                if args.dump_wav {
                    match dump_wav("/tmp/wiflow_hold.wav", &out.samples_mono, out.sample_rate) {
                        Ok(()) => info!("dumped /tmp/wiflow_hold.wav"),
                        Err(e) => warn!("wav dump failed: {e}"),
                    }
                }
            }
            e => info!("discarded: {:?}", e),
        }
        return;
    }
    println!("Phase1: global-hotkey wiring lands here. Use --simulate-hold-ms 1500 for now.");
    println!(
        "Next: global-hotkey 0.6 GlobalHotKeyManager + winit event loop (Task 4 follow-up on user approval)."
    );
}
