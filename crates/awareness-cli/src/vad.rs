/// Frame size for VAD at 16kHz: 480 samples = 30ms.
#[allow(dead_code)]
pub const VAD_FRAME_SAMPLES: usize = 480;

/// Voice Activity Detection wrapper.
#[allow(dead_code)]
pub struct VadDetector {
    #[cfg(feature = "full")]
    inner: webrtc_vad::Vad,
    #[cfg(not(feature = "full"))]
    _phantom: (),
}

impl VadDetector {
    /// Create a new VAD detector.
    ///
    /// With `full` feature: wraps `webrtc_vad::Vad` in Quality mode.
    /// Without `full` feature: no-op, `is_voice` always returns false.
    #[allow(dead_code)]
    pub fn new() -> Self {
        #[cfg(feature = "full")]
        {
            let mut vad = webrtc_vad::Vad::new();
            vad.set_mode(webrtc_vad::VadMode::Quality);
            VadDetector { inner: vad }
        }

        #[cfg(not(feature = "full"))]
        {
            VadDetector { _phantom: () }
        }
    }

    /// Returns true if voice is detected in this 30ms frame.
    ///
    /// `frame` must be exactly `VAD_FRAME_SAMPLES` (480) samples long.
    #[allow(dead_code)]
    pub fn is_voice(&mut self, frame: &[i16]) -> bool {
        #[cfg(feature = "full")]
        {
            if frame.len() != VAD_FRAME_SAMPLES {
                return false;
            }
            self.inner
                .is_voice_segment(frame)
                .unwrap_or(false)
        }

        #[cfg(not(feature = "full"))]
        {
            let _ = frame;
            false
        }
    }
}

impl Default for VadDetector {
    fn default() -> Self {
        Self::new()
    }
}
