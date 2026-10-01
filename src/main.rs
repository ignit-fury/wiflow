mod app;
mod audio;
mod cleanup;
mod config;
mod daemon;
mod history;
mod hotkey;
mod inject;
mod stt;
mod tap;
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
    /// Skip cursor injection (headless/CI runs)
    #[arg(long)]
    no_inject: bool,
    /// Launch the menu-bar app (tray + global hotkey daemon)
    #[arg(long)]
    app: bool,
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

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    if args.app {
        app::run(config::load_config());
    }
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
                match stt::transcribe_shared(&model_path, &kept, &stt::read_prompt()) {
                    Ok(text) => {
                        let ms = t0.elapsed().as_millis();
                        let kept_ms = kept.len() as f64 / vad::VAD_SAMPLE_RATE as f64 * 1000.0;
                        let rtf = ms as f64 / kept_ms.max(1.0);
                        info!("transcribed in {ms}ms (RTF {rtf:.2})");
                        // Same cleanup chain as the daemon (Groq→OpenRouter→Ollama).
                        let cfg = config::load_config();
                        let outcome = cleanup::clean_chain(&text, &cfg);
                        for issue in &outcome.issues {
                            warn!("cleanup issue: {issue}");
                        }
                        let text = outcome.text;
                        if cleanup::is_filler_result(&text) {
                            info!("transcript empty or filler-only after cleanup");
                            return;
                        }
                        println!("TRANSCRIPT: {text}");
                        if text.trim().is_empty() {
                            info!("empty transcript, nothing to inject");
                        } else {
                            let entry = history::HistoryEntry {
                                text: text.clone(),
                                at_ms: now_ms(),
                                duration_ms,
                                rtf,
                            };
                            if let Err(e) = history::push_history(entry) {
                                warn!("history push failed: {e}");
                            }
                            if args.no_inject {
                                info!("--no-inject: skipping cursor injection");
                            } else {
                                match inject::inject_text(&text) {
                                    Ok(r) => info!(
                                        "injected via {} (clipboard restored: {})",
                                        r.pasted_via, r.clipboard_restored
                                    ),
                                    Err(e) => {
                                        warn!("inject failed ({e}) — text left on clipboard, press Cmd+V");
                                        inject::leave_on_clipboard(&text);
                                    }
                                }
                            }
                        }
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
        stt::shutdown();
        return;
    }
    println!(
        "Phase 5: tray + global-hotkey wiring lands here. Use --simulate-hold-ms 1500 for now."
    );
}
