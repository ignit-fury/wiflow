use crate::core::hotkey::{PttEvent, PushToTalk};
use tracing::{info, warn};

/// Subset of CLI flags consumed by the simulate-hold pipeline.
/// Built from `Args` in `main()` and passed to `run_simulate_hold`.
#[derive(Debug, Clone)]
pub struct SimulateArgs {
    pub hold_ms: u64,
    pub dump_wav: bool,
    pub device: Option<String>,
    pub model: Option<std::path::PathBuf>,
    pub no_inject: bool,
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
        w.write_sample(crate::core::audio::f32_to_i16(s))?;
    }
    w.finalize()?;
    Ok(())
}

fn maybe_dump_wav(args: &SimulateArgs, kept: &[f32]) {
    if args.dump_wav {
        match dump_wav(
            "/tmp/wiflow_hold.wav",
            kept,
            crate::core::vad::VAD_SAMPLE_RATE,
        ) {
            Ok(()) => info!("dumped /tmp/wiflow_hold.wav"),
            Err(e) => warn!("wav dump failed: {e}"),
        }
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn run_simulate_hold(args: &SimulateArgs) {
    let hold = args.hold_ms;
    let mut ptt = PushToTalk::new(300, 60_000);
    info!("simulate hold {hold}ms (no hotkey needed)");
    assert!(matches!(ptt.on_key_down(0), PttEvent::Started));
    let cap = match crate::core::audio::AudioCapture::start(args.device.clone()) {
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
        crate::core::audio::rms(&out.samples_mono)
    );
    let mut vad = crate::core::vad::Vad::new();
    let kept = crate::core::vad::transcribe_ready(&out.samples_mono, out.sample_rate, &mut vad);
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
    maybe_dump_wav(args, &kept);
    match ptt.on_key_up(out.duration_ms) {
        PttEvent::Transcribe { duration_ms } => {
            info!(
                "would transcribe {duration_ms}ms ({} vad samples)",
                kept.len()
            );
            let model_path = match &args.model {
                Some(p) => {
                    if !crate::core::stt::verify_model(p) {
                        warn!(
                            "custom model fails size check, attempting load anyway: {}",
                            p.display()
                        );
                    }
                    p.clone()
                }
                None => match crate::core::stt::ensure_model() {
                    Ok(p) => p,
                    Err(e) => {
                        warn!("model unavailable: {e}");
                        return;
                    }
                },
            };
            let t0 = std::time::Instant::now();
            // Hoisted ABOVE transcribe: reused for the STT language/
            // provider branch and the cleanup context below.
            let cfg = crate::core::config::load_config();
            let prompt = crate::core::stt::read_prompt();
            // Same STT provider branch as the daemon: "groq" = cloud
            // whisper-large-v3 (OPT-IN, tray alert + local fallback on
            // failure), otherwise local on-device whisper.
            let text = if cfg.stt_provider == "groq" {
                match crate::core::groq_stt::transcribe_cloud(
                    &kept,
                    crate::core::vad::VAD_SAMPLE_RATE,
                    &cfg,
                ) {
                    Ok(t) => {
                        info!("cloud stt (whisper-large-v3) done");
                        t
                    }
                    Err(e) => {
                        warn!("Groq STT failed: {e} — using local whisper");
                        match crate::core::stt::transcribe_shared(
                            &model_path,
                            &kept,
                            &prompt,
                            &cfg.stt_language,
                        ) {
                            Ok(t) => t,
                            Err(e2) => {
                                warn!("transcribe failed: {e2}");
                                return;
                            }
                        }
                    }
                }
            } else {
                match crate::core::stt::transcribe_shared(
                    &model_path,
                    &kept,
                    &prompt,
                    &cfg.stt_language,
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        warn!("transcribe failed: {e}");
                        return;
                    }
                }
            };
            let ms = t0.elapsed().as_millis();
            let kept_ms = kept.len() as f64 / crate::core::vad::VAD_SAMPLE_RATE as f64 * 1000.0;
            let rtf = ms as f64 / kept_ms.max(1.0);
            info!("transcribed in {ms}ms (RTF {rtf:.2})");
            // Same routing as the daemon: deterministic-clean transcripts
            // skip the LLM chain; complex ones run it (Groq→OpenRouter→
            // Ollama) with the focused-app context synthesized first.
            let route = crate::core::analyze::decide_route(&text, &cfg);
            let route_name = match &route {
                crate::core::analyze::CleanupRoute::Direct(_) => "deterministic",
                crate::core::analyze::CleanupRoute::Llm(_) => "llm",
            };
            info!(
                "cleanup route={route_name} reason={} score={}",
                crate::core::analyze::route_reason(&cfg, &route),
                crate::core::analyze::route_score(&route),
            );
            let (cleaned, issues) = match route {
                crate::core::analyze::CleanupRoute::Direct(a) => (a.text, Vec::new()),
                crate::core::analyze::CleanupRoute::Llm(a) => {
                    let key_present = crate::core::cleanup::groq_key().is_some();
                    let ctx = if a.wants_context()
                        && cfg.cleanup_enabled
                        && cfg.context_enabled
                        && key_present
                    {
                        let app = crate::platform::macos::context::focused_app_name();
                        if crate::core::analyze::context_allowed(
                            app.as_deref(),
                            &a,
                            cfg.cleanup_enabled,
                            cfg.context_enabled,
                            key_present,
                        ) {
                            crate::core::cleanup::synthesize_context(app.as_deref(), &cfg)
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    };
                    let input = crate::core::cleanup::format_cleanup_input(
                        if ctx.is_empty() { None } else { Some(&ctx) },
                        &text,
                    );
                    let outcome = crate::core::cleanup::clean_chain(&input, &cfg);
                    (outcome.text, outcome.issues)
                }
            };
            for issue in &issues {
                warn!("cleanup issue: {issue}");
            }
            let text = cleaned;
            if crate::core::cleanup::is_filler_result(&text) {
                info!("transcript empty or filler-only after cleanup");
                return;
            }
            println!("TRANSCRIPT: {text}");
            if text.trim().is_empty() {
                info!("empty transcript, nothing to inject");
            } else {
                let entry = crate::core::history::HistoryEntry {
                    text: text.clone(),
                    at_ms: now_ms(),
                    duration_ms,
                    rtf,
                };
                if let Err(e) = crate::core::history::push_history(entry) {
                    warn!("history push failed: {e}");
                }
                if args.no_inject {
                    info!("--no-inject: skipping cursor injection");
                } else {
                    match crate::platform::macos::inject::inject_text(&text) {
                        Ok(r) => info!(
                            "injected via {} (clipboard restored: {})",
                            r.pasted_via, r.clipboard_restored
                        ),
                        Err(e) => {
                            warn!("inject failed ({e}) — text left on clipboard, press Cmd+V");
                            crate::platform::macos::inject::leave_on_clipboard(&text);
                        }
                    }
                }
            }
        }
        e => info!("discarded: {:?}", e),
    }
    crate::core::stt::shutdown();
}
