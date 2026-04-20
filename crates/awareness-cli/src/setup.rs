use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

const FLAG: &str = "--force-renderer-accessibility";

/// Chromium-based launchers we wrap so they expose the renderer a11y tree.
/// Other apps (Slack, Discord, VS Code Electron, Obsidian) can be added later.
const TARGETS: &[&str] = &[
    "google-chrome.desktop",
    "google-chrome-stable.desktop",
    "chromium.desktop",
    "chromium-browser.desktop",
    "teams-for-linux.desktop",
    "code.desktop",
    "code-oss.desktop",
    "code-insiders.desktop",
    "cursor.desktop",
    "slack.desktop",
    "discord.desktop",
    // Snap naming convention: <snap>_<app>.desktop
    "teams-for-linux_teams-for-linux.desktop",
    "code_code.desktop",
    "code-insiders_code-insiders.desktop",
    "slack_slack.desktop",
    "discord_discord.desktop",
];

const SEARCH_PATHS: &[&str] = &[
    "/usr/share/applications",
    "/var/lib/snapd/desktop/applications",
    "/var/lib/flatpak/exports/share/applications",
];

/// First-run setup: inject --force-renderer-accessibility into user overrides
/// of Chromium/Electron launchers. Idempotent via a marker file — this runs at
/// most once per user profile. Non-fatal: any error is logged and skipped so
/// the main CLI keeps starting.
pub fn ensure_a11y_launchers() {
    if let Err(e) = run() {
        tracing::warn!("a11y launcher setup failed (non-fatal): {e}");
    }
}

fn run() -> Result<()> {
    let state_dir = state_dir()?;
    let marker = state_dir.join("a11y_setup_done");
    if marker.exists() {
        return Ok(());
    }

    let home = std::env::var("HOME").context("HOME not set")?;
    let user_apps = PathBuf::from(&home).join(".local/share/applications");
    fs::create_dir_all(&user_apps)?;

    let mut wrapped: Vec<String> = Vec::new();

    for name in TARGETS {
        let dest = user_apps.join(name);
        if dest.exists() {
            // Respect user's own override — don't touch it.
            continue;
        }
        let src = match SEARCH_PATHS
            .iter()
            .map(|b| PathBuf::from(b).join(name))
            .find(|p| p.exists())
        {
            Some(p) => p,
            None => continue,
        };

        let content =
            fs::read_to_string(&src).with_context(|| format!("read {}", src.display()))?;
        let patched = inject_flag(&content);
        fs::write(&dest, patched).with_context(|| format!("write {}", dest.display()))?;
        tracing::info!("a11y setup: wrapped {} → {:?}", name, dest);
        wrapped.push(name.to_string());
    }

    // Mark done even when nothing was found — this machine simply has no
    // Chromium/Electron apps in standard locations; don't rescan on every run.
    fs::write(&marker, serde_json::to_string(&wrapped).unwrap_or_default())?;

    if !wrapped.is_empty() {
        tracing::info!(
            "a11y setup: {} launcher(s) wrapped. Close & reopen Teams/Chrome once for it to take effect.",
            wrapped.len()
        );
    }
    Ok(())
}

fn state_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let dir = PathBuf::from(&home).join(".config/awareness-cli");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn inject_flag(content: &str) -> String {
    let mut out = String::with_capacity(content.len() + 128);
    for line in content.lines() {
        match line.strip_prefix("Exec=") {
            Some(rest) if !rest.contains(FLAG) => {
                // Insert flag after the program name, before any existing args
                // or field codes (%U, %F, %u, %f).
                let mut parts = rest.splitn(2, ' ');
                let prog = parts.next().unwrap_or("");
                let tail = parts.next().unwrap_or("");
                if tail.is_empty() {
                    out.push_str(&format!("Exec={} {}", prog, FLAG));
                } else {
                    out.push_str(&format!("Exec={} {} {}", prog, FLAG, tail));
                }
            }
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// Revert previously wrapped launchers. Deletes the user-level .desktop copies
/// that we wrote and removes the marker. Preserves any user-customised copies
/// we never touched (the `dest.exists()` guard above ensures we never wrote
/// over those).
#[allow(dead_code)]
pub fn revert() -> Result<()> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let user_apps = PathBuf::from(&home).join(".local/share/applications");
    let marker = state_dir()?.join("a11y_setup_done");

    let wrapped: Vec<String> = match fs::read_to_string(&marker) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => TARGETS.iter().map(|s| s.to_string()).collect(),
    };

    for name in wrapped {
        let p = user_apps.join(&name);
        if p.exists() {
            let _ = fs::remove_file(&p);
            tracing::info!("a11y revert: removed {}", p.display());
        }
    }
    let _ = fs::remove_file(&marker);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_flag_into_simple_exec() {
        let input = "[Desktop Entry]\nExec=teams-for-linux\nName=Teams\n";
        let out = inject_flag(input);
        assert!(out.contains("Exec=teams-for-linux --force-renderer-accessibility"));
        assert!(out.contains("Name=Teams"));
    }

    #[test]
    fn inject_flag_preserves_field_codes() {
        let input = "Exec=/usr/bin/google-chrome-stable %U\n";
        let out = inject_flag(input);
        assert!(
            out.contains("Exec=/usr/bin/google-chrome-stable --force-renderer-accessibility %U")
        );
    }

    #[test]
    fn inject_flag_is_idempotent() {
        let input = "Exec=teams --force-renderer-accessibility %U\n";
        let out = inject_flag(input);
        // Should remain unchanged — already has the flag.
        assert_eq!(out.trim_end(), input.trim_end());
    }
}
