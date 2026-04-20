use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigFile {
    pub gate: GateSection,
    pub runtime: RuntimeSection,
    pub tts: TtsSection,
    pub vision: VisionSection,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct GateSection {
    pub frustration_keywords: Option<Vec<String>>,
    pub tuning: GateTuning,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct GateTuning {
    pub app_time_threshold_minutes: Option<u64>,
    pub periodic_check_minutes: Option<u64>,
    pub text_new_words_threshold: Option<usize>,
    pub text_change_cooldown_seconds: Option<u64>,
    pub voice_cooldown_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeSection {
    pub min_send_interval_seconds: Option<u64>,
    pub transcript_window_size: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct TtsSection {
    pub enabled: Option<bool>,
    pub command: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct VisionSection {
    /// Apps for which the vision backend should use the "sharp" (higher
    /// detail, more expensive) tier. Matched as a case-insensitive substring
    /// against `event.app`. When unset, a sensible default list is used.
    pub sharp_apps: Option<Vec<String>>,
}

pub fn default_sharp_apps() -> Vec<String> {
    [
        "vscode",
        "cursor",
        "code",
        "intellij",
        "pycharm",
        "webstorm",
        "sublime",
        "atom",
        "nvim",
        "neovim",
        "text_editor",
        "helix",
        "zed",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl ConfigFile {
    pub fn load_if_present(candidates: &[&Path]) -> Result<Option<Self>> {
        for path in candidates {
            if path.exists() {
                let raw =
                    std::fs::read_to_string(path).with_context(|| format!("reading {:?}", path))?;
                let parsed: ConfigFile =
                    toml::from_str(&raw).with_context(|| format!("parsing {:?}", path))?;
                tracing::info!("Loaded config from {:?}", path);
                return Ok(Some(parsed));
            }
        }
        Ok(None)
    }
}

pub fn default_frustration_keywords() -> Vec<String> {
    [
        "não percebo",
        "não funciona",
        "por amor",
        "merda",
        "caralho",
        "wtf",
        "why the hell",
        "this is broken",
        "not working",
        "foda-se",
        "impossível",
        "broken",
        "crash",
        "error",
        "failed",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, body: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("awareness-cfgfile-{}-{}", std::process::id(), name));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn empty_file_parses_to_defaults() {
        let p = write_tmp("empty.toml", "");
        let got = ConfigFile::load_if_present(&[p.as_path()])
            .unwrap()
            .unwrap();
        assert!(got.gate.frustration_keywords.is_none());
        assert!(got.gate.tuning.periodic_check_minutes.is_none());
        assert!(got.runtime.min_send_interval_seconds.is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn frustration_keywords_override() {
        let p = write_tmp(
            "keywords.toml",
            r#"[gate]
frustration_keywords = ["foo", "bar"]
"#,
        );
        let got = ConfigFile::load_if_present(&[p.as_path()])
            .unwrap()
            .unwrap();
        assert_eq!(
            got.gate.frustration_keywords.unwrap(),
            vec!["foo".to_string(), "bar".to_string()]
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn tuning_and_runtime_round_trip() {
        let p = write_tmp(
            "tuning.toml",
            r#"[gate.tuning]
periodic_check_minutes = 7
text_new_words_threshold = 3

[runtime]
min_send_interval_seconds = 42
"#,
        );
        let got = ConfigFile::load_if_present(&[p.as_path()])
            .unwrap()
            .unwrap();
        assert_eq!(got.gate.tuning.periodic_check_minutes, Some(7));
        assert_eq!(got.gate.tuning.text_new_words_threshold, Some(3));
        assert_eq!(got.runtime.min_send_interval_seconds, Some(42));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn unknown_field_is_rejected() {
        let p = write_tmp("unknown.toml", "garbage_field = 1\n");
        let err = ConfigFile::load_if_present(&[p.as_path()]).unwrap_err();
        assert!(
            err.to_string().contains("parsing")
                || err.chain().any(|c| c.to_string().contains("unknown")),
            "expected parse error, got {err}"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn load_if_present_returns_none_when_no_candidate_exists() {
        let missing = std::path::PathBuf::from("/definitely/does/not/exist/xyz.toml");
        let got = ConfigFile::load_if_present(&[missing.as_path()]).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn default_frustration_keywords_nonempty() {
        let kws = default_frustration_keywords();
        assert!(!kws.is_empty());
        assert!(kws.iter().any(|k| k == "broken"));
    }
}
