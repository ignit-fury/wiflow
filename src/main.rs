use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "wiflow-dictation", about = "Push-to-talk dictation prototype")]
struct Args {
    /// List audio input devices and exit
    #[arg(long)]
    list_devices: bool,
    /// Dump captured audio to wav on release (debug)
    #[arg(long)]
    dump_wav: bool,
}

fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    if args.list_devices {
        info!("list-devices requested (wired in Task 2)");
    }
    println!("wiflow-dictation phase1 scaffold ok");
}
