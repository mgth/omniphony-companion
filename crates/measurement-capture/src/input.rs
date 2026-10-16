//! Input activity is independent of sweep detection and acoustic qualification.
use serde::Serialize;

pub const NO_SIGNAL: &str = "No microphone signal received: the selected input contains only digital silence. Check the input source, channel, microphone power and routing. Increasing speaker volume will not help.";

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct InputLevel {
    pub frames: usize,
    pub peak_dbfs: Option<f64>,
    pub rms_dbfs: Option<f64>,
    pub clipped_samples: usize,
    pub fault_flags: u32,
}
impl InputLevel {
    pub fn analyze(samples: &[f32], fault_flags: u32) -> Self {
        let (peak_dbfs, rms_dbfs, clipped_samples) = crate::levels(samples);
        Self {
            frames: samples.len(),
            peak_dbfs,
            rms_dbfs,
            clipped_samples,
            fault_flags: fault_flags
                | if samples.iter().any(|s| !s.is_finite()) {
                    crate::INVALID_SAMPLE
                } else {
                    0
                },
        }
    }

    /// No minimum loudness gate: quiet microphone noise is valid input activity.
    /// This does not prove that the selected source is the intended microphone.
    pub fn problem(&self) -> Option<&'static str> {
        if self.fault_flags != 0 {
            Some(
                "Microphone capture fault. Check the interface connection and restart the input check.",
            )
        } else if self.frames == 0 {
            Some("No microphone frames received. Check the input source and connection.")
        } else if self.peak_dbfs.is_none() {
            Some(NO_SIGNAL)
        } else if self.clipped_samples != 0 {
            Some("Microphone input is clipping. Lower the interface input gain before measuring.")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_is_distinct_from_quiet_activity_clipping_and_faults() {
        assert_eq!(
            InputLevel::analyze(&[0.0; 4800], 0).problem(),
            Some(NO_SIGNAL)
        );
        assert!(InputLevel::analyze(&[1e-8, -1e-8], 0).problem().is_none());
        assert!(
            InputLevel::analyze(&[], 0)
                .problem()
                .unwrap()
                .contains("frames")
        );
        assert!(
            InputLevel::analyze(&[1.0], 0)
                .problem()
                .unwrap()
                .contains("clipping")
        );
        assert!(
            InputLevel::analyze(&[f32::NAN], 0)
                .problem()
                .unwrap()
                .contains("fault")
        );
        assert!(
            InputLevel::analyze(&[0.1], crate::TIMESTAMP_GAP)
                .problem()
                .is_some()
        );
    }
}
