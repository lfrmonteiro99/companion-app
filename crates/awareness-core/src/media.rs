//! Identifies media/scroll apps. Used to gate the loopback-audio lane (1b)
//! so a reel's soundtrack is only transcribed when a media app is foreground.

const MEDIA_APP_HINTS: &[&str] = &[
    "instagram", "tiktok", "musically", "ugc.trill", "youtube",
    "facebook", "twitter", "snapchat", "pinterest", "reddit",
    "threads", "bluesky",
];

/// True when `app` looks like a media/scroll app (case-insensitive substring).
pub fn is_media_app(app: Option<&str>) -> bool {
    match app {
        Some(a) => { let l = a.to_lowercase(); MEDIA_APP_HINTS.iter().any(|h| l.contains(h)) }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_media() {
        assert!(is_media_app(Some("Instagram")));
        assert!(is_media_app(Some("com.instagram.android")));
        assert!(is_media_app(Some("YouTube")));
    }
    #[test]
    fn rejects_non_media_and_none() {
        assert!(!is_media_app(Some("vscode")));
        assert!(!is_media_app(None));
        assert!(!is_media_app(Some("")));
    }
}
