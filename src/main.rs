mod app;
mod core;
mod daemon;
pub mod logfile;
mod platform;
mod ptt;
pub mod ui;

use clap::Parser;

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

fn main() {
    logfile::init();
    let args = Args::parse();

    if args.list_devices {
        for d in crate::core::audio::list_devices() {
            println!("{d}");
        }
        return;
    }

    if args.app {
        app::run(crate::core::config::load_config());
    }

    if let Some(hold) = args.simulate_hold_ms {
        let sim = app::headless::SimulateArgs {
            hold_ms: hold,
            dump_wav: args.dump_wav,
            device: args.device.clone(),
            model: args.model.clone(),
            no_inject: args.no_inject,
        };
        app::headless::run_simulate_hold(&sim);
        return;
    }

    println!(
        "Phase 5: tray + global-hotkey wiring lands here. Use --simulate-hold-ms 1500 for now."
    );
}
