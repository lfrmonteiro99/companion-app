use std::collections::{HashSet, VecDeque};

/// Perceptual hash deduplication for screen frames.
#[allow(dead_code)]
pub struct PerceptualDedup {
    last_hash: Option<u64>,
    threshold: u32,
}

impl PerceptualDedup {
    #[allow(dead_code)]
    pub fn new(threshold: u32) -> Self {
        Self {
            last_hash: None,
            threshold,
        }
    }

    /// Returns true if this frame should be processed (hash differs enough from last).
    ///
    /// Keep if no previous hash, or if Hamming distance (XOR popcount) exceeds threshold.
    /// Always updates `last_hash`.
    #[allow(dead_code)]
    pub fn should_keep(&mut self, hash: u64) -> bool {
        let keep = match self.last_hash {
            None => true,
            Some(prev) => (prev ^ hash).count_ones() > self.threshold,
        };
        self.last_hash = Some(hash);
        keep
    }
}

/// Text deduplication using Jaccard similarity on character trigrams.
#[allow(dead_code)]
pub struct TextDedup {
    window: VecDeque<String>,
    max_size: usize,
    threshold: f32,
}

impl TextDedup {
    #[allow(dead_code)]
    pub fn new(max_size: usize, threshold: f32) -> Self {
        Self {
            window: VecDeque::with_capacity(max_size),
            max_size,
            threshold,
        }
    }

    /// Returns true if this text should be processed (not too similar to recent texts).
    ///
    /// Discards the text if its max Jaccard similarity against any text in the window
    /// exceeds `threshold`. Otherwise adds it to the window (evicting oldest if full).
    #[allow(dead_code)]
    pub fn should_keep(&mut self, text: &str) -> bool {
        if self.window.is_empty() {
            self.window.push_back(text.to_string());
            return true;
        }

        let max_similarity = self
            .window
            .iter()
            .map(|w| Self::jaccard_trigrams(w, text))
            .fold(0.0_f32, f32::max);

        if max_similarity > self.threshold {
            return false;
        }

        if self.window.len() >= self.max_size {
            self.window.pop_front();
        }
        self.window.push_back(text.to_string());
        true
    }

    /// Jaccard similarity on character trigrams of two strings.
    ///
    /// Returns 1.0 if both are empty, 0.0 if only one is empty.
    pub fn jaccard_trigrams(a: &str, b: &str) -> f32 {
        let trigrams_a: HashSet<&str> = trigrams(a).collect();
        let trigrams_b: HashSet<&str> = trigrams(b).collect();

        if trigrams_a.is_empty() && trigrams_b.is_empty() {
            return 1.0;
        }
        if trigrams_a.is_empty() || trigrams_b.is_empty() {
            return 0.0;
        }

        let intersection = trigrams_a.intersection(&trigrams_b).count();
        let union = trigrams_a.union(&trigrams_b).count();

        intersection as f32 / union as f32
    }
}

/// Iterator yielding character trigram slices from a string.
fn trigrams(s: &str) -> impl Iterator<Item = &str> {
    // Collect char byte offsets so we can slice safely.
    let indices: Vec<usize> = s
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(s.len()))
        .collect();

    (0..indices.len().saturating_sub(3)).map(move |i| &s[indices[i]..indices[i + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- PerceptualDedup ---

    #[test]
    fn perceptual_dedup_first_frame_is_kept() {
        let mut dedup = PerceptualDedup::new(8);
        assert!(dedup.should_keep(0xDEAD_BEEF_CAFE_1234));
    }

    #[test]
    fn perceptual_dedup_identical_hash_is_dropped() {
        let mut dedup = PerceptualDedup::new(8);
        let hash = 0xDEAD_BEEF_CAFE_1234_u64;
        dedup.should_keep(hash); // first — kept
        assert!(!dedup.should_keep(hash)); // identical — dropped
    }

    #[test]
    fn perceptual_dedup_very_different_hash_is_kept() {
        let mut dedup = PerceptualDedup::new(8);
        dedup.should_keep(0x0000_0000_0000_0000); // first
                                                  // All bits flipped: Hamming distance = 64 >> threshold of 8
        assert!(dedup.should_keep(0xFFFF_FFFF_FFFF_FFFF));
    }

    // --- TextDedup ---

    #[test]
    fn text_dedup_first_text_is_kept() {
        let mut dedup = TextDedup::new(5, 0.85);
        assert!(dedup.should_keep("hello world"));
    }

    #[test]
    fn text_dedup_identical_text_is_dropped() {
        let mut dedup = TextDedup::new(5, 0.85);
        dedup.should_keep("hello world"); // first — kept
        assert!(!dedup.should_keep("hello world")); // identical — dropped
    }

    #[test]
    fn text_dedup_completely_different_text_is_kept() {
        let mut dedup = TextDedup::new(5, 0.85);
        dedup.should_keep("hello world");
        assert!(dedup.should_keep("the quick brown fox jumps over the lazy dog"));
    }

    // --- jaccard_trigrams ---

    #[test]
    fn jaccard_identical_strings() {
        let sim = TextDedup::jaccard_trigrams("hello", "hello");
        assert!((sim - 1.0).abs() < 1e-6, "expected 1.0, got {sim}");
    }

    #[test]
    fn jaccard_empty_strings() {
        let sim = TextDedup::jaccard_trigrams("", "");
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "expected 1.0 for both empty, got {sim}"
        );
    }

    #[test]
    fn jaccard_one_empty_string() {
        let sim = TextDedup::jaccard_trigrams("hello", "");
        assert!((sim - 0.0).abs() < 1e-6, "expected 0.0, got {sim}");
    }

    #[test]
    fn jaccard_hello_vs_world_less_than_half() {
        let sim = TextDedup::jaccard_trigrams("hello", "world");
        assert!(sim < 0.5, "expected < 0.5, got {sim}");
    }
}
