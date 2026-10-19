//! Expected wall-clock time of a capture run from the sweep, the outputs and the passes.
//! The capture loops use the same timing constants, so the estimate follows them.
use crate::measurement::MeasureProgress;
use omniphony_adapter::{Sweep, protocol::SETTLE_MS};
use std::time::Duration;

/// Input frames gathered before the renderer session begins (a tenth of a second).
pub const OPEN_INPUT: Duration = Duration::from_millis(100);
/// Metered quiet lead-in before the first sweep.
pub const LEAD_IN: Duration = Duration::from_millis(600);
/// Quiet tail recorded after the last sweep.
pub const TAIL: Duration = Duration::from_millis(600);
/// Renderer status polling period; completion is noticed up to this late.
pub const STATUS_POLL: Duration = Duration::from_millis(200);
/// Audio analysed around each take's three segments (the batch windows' margins).
const TAKE_MARGIN: Duration = Duration::from_millis(400);
/// Offline analysis time per second of take audio, measured on the session of
/// 16 Oct 2026 (nine 1 s sweeps at 48 kHz took 3.5 s each). Machine dependent.
pub const ANALYSIS_PER_AUDIO_SECOND: f64 = 0.7;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    pub passes: usize,
    pub outputs: usize,
    /// Renderer sequence of one pass: 2 × outputs + 1 segments, each a sweep with settle.
    pub sequence: Duration,
    /// Offline analysis of one take.
    pub analysis_per_take: Duration,
    /// One pass: input opening, lead-in, sequence, status latency, tail and analysis.
    pub per_pass: Duration,
    pub total: Duration,
}

/// A run of `passes` continuous batches over `outputs` speakers (a single take is a
/// batch of one: reference, speaker, reference).
pub fn estimate(sweep: Sweep, outputs: usize, passes: usize) -> Estimate {
    let segment = Duration::from_millis(u64::from(sweep.duration_ms) + 2 * u64::from(SETTLE_MS));
    let sequence = segment * (2 * outputs as u32 + 1);
    let analysis_per_take = (segment * 3 + TAKE_MARGIN).mul_f64(ANALYSIS_PER_AUDIO_SECOND);
    let per_pass =
        OPEN_INPUT + LEAD_IN + sequence + STATUS_POLL + TAIL + analysis_per_take * outputs as u32;
    Estimate {
        passes,
        outputs,
        sequence,
        analysis_per_take,
        per_pass,
        total: per_pass * passes as u32,
    }
}
pub fn batch(sweep: Sweep, outputs: usize) -> Estimate {
    estimate(sweep, outputs, 1)
}
/// Two passes, as alignment and its verification capture them.
pub fn alignment(sweep: Sweep, outputs: usize) -> Estimate {
    estimate(sweep, outputs, 2)
}

impl Estimate {
    /// Time left while pass `pass` (from 0) reports `progress` for speaker `target`.
    pub fn remaining(
        &self,
        pass: usize,
        target: usize,
        progress: Option<&MeasureProgress>,
    ) -> Duration {
        let later = self.per_pass * self.passes.saturating_sub(pass + 1) as u32;
        let analysis = self.analysis_per_take * self.outputs as u32;
        let this = match progress {
            None | Some(MeasureProgress::Preparing | MeasureProgress::OpeningInput) => {
                self.per_pass
            }
            Some(MeasureProgress::LeadIn | MeasureProgress::CheckingInput(_)) => {
                self.per_pass.saturating_sub(OPEN_INPUT)
            }
            Some(MeasureProgress::Playing(p)) => {
                let played = p.rendered_frames as f64 / p.total_frames.max(1) as f64;
                self.sequence.mul_f64(1.0 - played.clamp(0.0, 1.0)) + STATUS_POLL + TAIL + analysis
            }
            Some(MeasureProgress::Tail) => TAIL + analysis,
            // Takes are saved and analysed in order, from the first speaker.
            Some(MeasureProgress::Saving | MeasureProgress::Analyzing) => {
                self.analysis_per_take * self.outputs.saturating_sub(target) as u32
            }
        };
        this + later
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omniphony_adapter::{Progress, SignalState};

    fn sweep(duration_ms: u32) -> Sweep {
        Sweep {
            duration_ms,
            peak_dbfs: -30.0,
            start_hz: 200.0,
            end_hz: 10000.0,
        }
    }

    #[test]
    fn the_estimate_follows_sweep_outputs_and_passes() {
        // The 16 Oct 2026 session: nine speakers, 1 s sweeps, two passes. Each pass
        // recorded for 30.1 s and analysed for 31.5 s on the development machine.
        let a = alignment(sweep(1000), 9);
        assert_eq!(a.sequence, Duration::from_millis(28_500));
        assert_eq!(
            OPEN_INPUT + LEAD_IN + a.sequence + STATUS_POLL + TAIL,
            Duration::from_millis(30_000)
        );
        assert!((a.analysis_per_take.as_secs_f64() - 3.43).abs() < 0.01);
        assert!((a.per_pass.as_secs_f64() - 60.9).abs() < 0.1, "{a:?}");
        assert_eq!(a.total, a.per_pass * 2);
        let single = batch(sweep(1000), 1);
        assert_eq!(single.sequence, Duration::from_millis(4_500));
        assert!(batch(sweep(500), 9).total < batch(sweep(1000), 9).total);
        assert!(batch(sweep(1000), 4).total < batch(sweep(1000), 9).total);
    }

    #[test]
    fn the_remaining_time_falls_through_each_stage_and_pass() {
        let a = alignment(sweep(1000), 4);
        let playing = |fraction: f64| {
            let total = 48_000 * 1_500 / 1_000 * 9;
            MeasureProgress::Playing(Progress {
                signal_id: 1,
                output_id: "FL".into(),
                reference_output_id: Some("C".into()),
                sequence_outputs: vec!["FL".into(); 4],
                signal: sweep(1000),
                state: SignalState::Rendering,
                rendered_frames: (total as f64 * fraction) as u64,
                total_frames: total,
            })
        };
        let stages = [
            (0, 0, None),
            (0, 0, Some(MeasureProgress::LeadIn)),
            (0, 0, Some(playing(0.25))),
            (0, 2, Some(playing(0.75))),
            (0, 3, Some(MeasureProgress::Tail)),
            (0, 0, Some(MeasureProgress::Analyzing)),
            (0, 3, Some(MeasureProgress::Analyzing)),
            (1, 0, Some(MeasureProgress::OpeningInput)),
            (1, 3, Some(MeasureProgress::Analyzing)),
        ];
        let left: Vec<_> = stages
            .iter()
            .map(|(pass, target, progress)| a.remaining(*pass, *target, progress.as_ref()))
            .collect();
        assert_eq!(left[0], a.total);
        assert!(left.windows(2).all(|w| w[0] > w[1]), "{left:?}");
        assert_eq!(left[7], a.per_pass);
        assert_eq!(left[8], a.analysis_per_take);
        assert!(
            batch(sweep(1000), 4).remaining(5, 9, Some(&MeasureProgress::Analyzing))
                == Duration::ZERO
        );
    }
}
