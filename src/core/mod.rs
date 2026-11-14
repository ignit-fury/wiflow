pub mod analyze;
pub mod audio;
#[cfg(test)]
pub mod baseline;
pub mod cleanup;
pub mod config;
pub mod correction;
#[allow(dead_code)] // consumed by the voice-edit worker in Task 4
pub mod edit;
pub mod groq_stt;
pub mod history;
pub mod hotkey;
pub mod stt;
pub mod traits;
pub mod vad;
