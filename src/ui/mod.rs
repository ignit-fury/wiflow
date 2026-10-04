//! Pure-Rust UI layer (S4): winit windows + egui painting.
//!
//! The UI observes App state and emits intent commands; it never drives the
//! daemon or the PTT machine directly (H20).

pub mod gl;
pub mod notify;
pub mod pill;
pub mod settings;
