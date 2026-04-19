// Cross-platform modules live in `awareness-core`. Re-exported so existing
// `crate::<mod>` paths (inside this crate's Linux-specific modules and in
// tests) and downstream users keep working unchanged.
pub use awareness_core::{
    api,
    api_vision,
    backend,
    budget,
    config,
    config_file,
    dedup,
    flow,
    gate,
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
pub mod eval;
pub mod tts;
