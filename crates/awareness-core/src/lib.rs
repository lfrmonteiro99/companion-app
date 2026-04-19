//! Cross-platform core of the awareness companion app.
//!
//! Everything in here must compile on Linux, Android, and anywhere else
//! tokio + reqwest work. Platform-specific bits (screen capture, audio,
//! OCR engines, TTS) live in the front-end crates (`awareness-cli` for
//! desktop, `core-rs` for Android) that depend on this one.

pub mod budget;
pub mod config_file;
pub mod dedup;
pub mod flow;
pub mod jsonl;
pub mod memory;
