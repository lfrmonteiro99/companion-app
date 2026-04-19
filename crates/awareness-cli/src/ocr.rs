use anyhow::Result;
use chrono::{DateTime, Utc};
use image::DynamicImage;

pub use awareness_core::types::OcrOutput;

/// Extract text from a screen frame using Tesseract OCR.
#[allow(dead_code)]
pub fn extract_text(image: &DynamicImage, captured_at: DateTime<Utc>) -> Result<OcrOutput> {
    #[cfg(feature = "full")]
    {
        use leptess::LepTess;
        use image::imageops::{grayscale, FilterType};

        // Preprocess: grayscale + 2x upscale. Modern UI fonts (Segoe UI,
        // Roboto) with sub-pixel rendering defeat tesseract at native
        // resolution. Upscaling + greyscale recovers most of the text.
        let gray = grayscale(image);
        let (w, h) = (gray.width(), gray.height());
        let upscaled = image::imageops::resize(
            &gray,
            w.saturating_mul(2),
            h.saturating_mul(2),
            FilterType::Lanczos3,
        );
        let processed = DynamicImage::ImageLuma8(upscaled);

        let mut buf = std::io::Cursor::new(Vec::new());
        processed
            .write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| anyhow::anyhow!("Failed to encode image: {e}"))?;

        let mut lt = LepTess::new(None, "por+eng")
            .map_err(|e| anyhow::anyhow!("Failed to init Tesseract: {e}"))?;
        lt.set_image_from_mem(buf.get_ref())
            .map_err(|e| anyhow::anyhow!("set_image_from_mem failed: {e}"))?;
        // PSM 11 = sparse text. Best for UI where text is scattered in
        // non-linear layouts (chat bubbles, menus, side panels).
        lt.set_variable(leptess::Variable::TesseditPagesegMode, "11")
            .map_err(|e| anyhow::anyhow!("set PSM failed: {e}"))?;
        let full_text = lt.get_utf8_text()
            .map_err(|e| anyhow::anyhow!("get_utf8_text failed: {e}"))?;

        // Title bar: take top 10% of the (upscaled) image.
        let crop_height = (processed.height() / 10).max(60);
        let title_bar_img = processed.crop_imm(0, 0, processed.width(), crop_height);

        let mut lt2 = LepTess::new(None, "por+eng")
            .map_err(|e| anyhow::anyhow!("Failed to init Tesseract (title bar): {e}"))?;
        let mut buf2 = std::io::Cursor::new(Vec::new());
        title_bar_img
            .write_to(&mut buf2, image::ImageFormat::Png)
            .map_err(|e| anyhow::anyhow!("Failed to encode title bar image: {e}"))?;
        lt2.set_image_from_mem(buf2.get_ref())
            .map_err(|e| anyhow::anyhow!("set_image_from_mem (title bar) failed: {e}"))?;
        let title_bar_text = lt2.get_utf8_text()
            .map_err(|e| anyhow::anyhow!("get_utf8_text (title bar) failed: {e}"))?;

        // Use full_text as fallback for app name since title bar OCR can miss it.
        let inferred_app_name = infer_app_name(&title_bar_text)
            .or_else(|| infer_app_name(&full_text));

        return Ok(OcrOutput {
            captured_at,
            full_text,
            title_bar_text,
            inferred_app_name,
        });
    }

    #[cfg(not(feature = "full"))]
    {
        let _ = image; // suppress unused warning
        Ok(OcrOutput {
            captured_at,
            full_text: String::new(),
            title_bar_text: String::new(),
            inferred_app_name: None,
        })
    }
}

/// Infer application name from title bar text using substring matching.
/// Always compiled regardless of feature flags.
#[allow(dead_code)]
pub fn infer_app_name(title_bar_text: &str) -> Option<String> {
    const APP_HINTS: &[(&str, &str)] = &[
        ("Visual Studio Code", "vscode"),
        ("VSCode", "vscode"),
        ("Code - Insiders", "vscode"),
        ("Cursor", "cursor"),
        ("Mozilla Firefox", "firefox"),
        ("Firefox", "firefox"),
        ("Chromium", "chrome"),
        ("Chrome", "chrome"),
        ("Slack", "slack"),
        ("Microsoft Teams", "teams"),
        ("Teams", "teams"),
        ("Claude", "claude"),
        ("ChatGPT", "chatgpt"),
        ("Outlook", "outlook"),
        ("Signal", "signal"),
        ("GNOME Terminal", "terminal"),
        ("Konsole", "terminal"),
        ("Terminal", "terminal"),
        ("Zoom Meeting", "zoom"),
        ("Google Meet", "meet"),
        ("Spotify", "spotify"),
        ("Instagram", "instagram"),
        ("YouTube", "youtube"),
        ("Twitter", "twitter"),
        ("WhatsApp", "whatsapp"),
    ];
    let lower = title_bar_text.to_lowercase();
    for (pattern, name) in APP_HINTS {
        if lower.contains(&pattern.to_lowercase()) {
            return Some(name.to_string());
        }
    }
    None
}
