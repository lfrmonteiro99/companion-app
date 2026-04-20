// Cross-platform modules live in `awareness-core`. Re-exported so existing
// `crate::<mod>` paths (inside this crate's Linux-specific modules and in
// tests) and downstream users keep working unchanged.
pub use awareness_core::{
    api, api_vision, backend, budget, config, config_file, dedup, flow, gate, jsonl, memory,
};

pub mod a11y;
pub mod aggregator;
pub mod audio;
pub mod capture;
#[cfg(feature = "portal")]
pub mod capture_portal;
#[cfg(feature = "portal")]
pub mod capture_screenshot;
pub mod eval;
pub mod ocr;
pub mod setup;
pub mod tts;
pub mod vad;
pub mod whisper;
