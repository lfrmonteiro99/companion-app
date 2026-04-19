pub mod config;

// Cross-platform modules now live in the `awareness-core` crate.
// Re-exported so internal `crate::<mod>` paths and external users keep
// working unchanged.
pub use awareness_core::{
    budget,
    config_file,
    dedup,
    flow,
    jsonl,
    memory,
};

pub mod capture;
#[cfg(feature = "portal")]
pub mod capture_portal;
pub mod audio;
pub mod ocr;
pub mod a11y;
pub mod setup;
pub mod vad;
pub mod whisper;
pub mod aggregator;
pub mod gate;
pub mod api;
pub mod api_vision;
pub mod backend;
pub mod eval;
pub mod tts;
