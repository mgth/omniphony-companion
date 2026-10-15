use companion_service::{
    controller::{Backend, Controller},
    measurement::{MeasureProgress, MeasureRequest, TakeOutcome},
};
use measurement_capture::{InputConfiguration, InputDevice};
use omniphony_adapter::{Snapshot, protocol::Output};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

pub struct UiBench {
    pub starts: Arc<AtomicUsize>,
}
impl Backend for UiBench {
    fn inputs(&self) -> Result<Vec<InputDevice>, String> {
        Ok(vec![InputDevice {
            name: "Preview audio interface".into(),
            description: "Preview audio interface".into(),
            channel_names: vec!["Mic 1".into(), "Mic 2".into()],
            is_monitor: false,
            configurations: vec![InputConfiguration {
                channels: 2,
                min_sample_rate: 44100,
                max_sample_rate: 96000,
                sample_format: "f32".into(),
            }],
        }])
    }
    fn discover(&self, _: SocketAddr) -> Result<Snapshot, String> {
        Ok(Snapshot {
            instance_id: "ui-preview".into(),
            revision: "ui-preview:1".into(),
            sample_rate: 48000,
            master_gain_db: 0.0,
            max_delay_ms: 100.0,
            unavailable_reason: None,
            outputs: [
                "Front left",
                "Front right",
                "Center",
                "Surround left",
                "Surround right",
                "Subwoofer",
            ]
            .iter()
            .enumerate()
            .map(|(index, name)| Output {
                id: (*name).into(),
                name: (*name).into(),
                index: index as u32,
                gain_db: [-1.5, -2.0, 0.0, -0.5, -0.5, -3.0][index],
                delay_ms: [0.0, 0.35, 1.2, 3.1, 3.45, 0.0][index],
                muted: index == 5,
                freq_low_hz: Some(if index == 5 { 20.0 } else { 80.0 }),
                freq_high_hz: Some(if index == 5 { 120.0 } else { 20000.0 }),
            })
            .collect(),
        })
    }
    fn measure(
        &self,
        _: MeasureRequest,
        _: &AtomicBool,
        _: &mut dyn FnMut(MeasureProgress),
    ) -> Result<TakeOutcome, String> {
        self.starts.fetch_add(1, Ordering::Relaxed);
        Err(
            "UI preview only. No microphone or speaker was used, and no recording was created."
                .into(),
        )
    }
}
pub fn ready() -> (Controller, Arc<AtomicUsize>) {
    let starts = Arc::new(AtomicUsize::new(0));
    let backend = Arc::new(UiBench {
        starts: starts.clone(),
    });
    let mut c = Controller::new(backend.clone());
    let endpoint = "127.0.0.1:9000".parse().unwrap();
    c.state.inputs = backend.inputs().unwrap();
    c.state.snapshot = Some(backend.discover(endpoint).unwrap());
    c.state.endpoint = Some(endpoint);
    c.settings.device = Some("Preview audio interface".into());
    c.settings.reference_output = Some("Front left".into());
    (c, starts)
}

pub fn arrival_review() -> companion_service::review::SavedReview {
    use companion_service::review::{ImpulseReport, ImpulseSegment, SavedReview};
    use measurement_capture::timing::TimingReport;
    let segment = ImpulseSegment {
        role: "speaker",
        correlation_start_frame: 10000.0,
        candidate_start_frame: Some(9760.0),
        band_candidate_offset_ms: [Some(-5.0), Some(-5.1), Some(-4.9)],
        threshold_candidate_offset_ms: [[Some(-5.0); 3], [Some(-5.1); 3], [Some(-4.9); 3]],
        candidate_offset_ms: Some(-5.0),
        sensitivity_ms: Some(0.3),
        noise_db_relative_peak: -55.0,
        response_start_ms: -100.0,
        response_step_ms: 0.02,
        response: Vec::new(),
        envelope_step_ms: 0.2,
        envelope_db: (0..1250)
            .map(|i| {
                let ms = i as f32 * 0.2 - 100.0;
                let onset = -5.0;
                if ms < onset {
                    (-55.0 + 2.0 * (ms * 3.0).sin()).max(-80.0)
                } else {
                    (-0.7 * (ms - onset) - 8.0 * (ms * 1.2).sin().abs()).max(-80.0)
                }
            })
            .collect(),
    };
    SavedReview {
        schema_version: 2,
        directory: "sessions/synthetic-preview".into(),
        report_file: "sessions/synthetic-preview/analysis-preview.json".into(),
        origin: "microphone_capture".into(),
        output_id: "Front right".into(),
        reference_output_id: "Front left".into(),
        microphone_id: "Synthetic microphone".into(),
        position_id: "Preview seat".into(),
        input_channel: 1,
        analysis_start_frame: 0,
        capture_sample_rate: 48000,
        render_sample_rate: 48000,
        signal: companion_service::controller::Settings::default().sweep,
        sequence_window: None,
        original_accepted_for_analysis: true,
        original_rejection_reason: None,
        diagnostics: measurement_capture::diagnostics::AcquisitionDiagnostics {
            method: "synthetic-preview",
            noise_rms_dbfs: -50.0,
            total_clipped_samples: 0,
            reference_start_frames: None,
            segments: None,
            messages: vec![],
            valid_for_correction: false,
        },
        analysis_error: None,
        timing: Some(TimingReport {
            method: "synthetic-preview",
            reference_start_frames: [10000.0, 154000.0],
            target_start_frame: 82000.0,
            clock_drift_ppm: None,
            relative_peak_delay_ms: None,
            ambiguous_peaks: true,
            competing_peak_ratio: [0.9; 3],
            normalized_correlation: [0.4; 3],
            signal_to_noise_db: [28.0; 3],
            sweep_rms_dbfs: [-22.0; 3],
            noise_rms_dbfs: -50.0,
            valid_for_correction: false,
        }),
        impulse: Some(Arc::new(ImpulseReport {
            method: "synthetic-preview",
            bands_hz: [[300.0, 9000.0], [300.0, 2500.0], [2500.0, 9000.0]],
            segments: [segment.clone(), segment.clone(), segment],
            reference_similarity: Some(0.98),
            reference_alignment_residual_ms: Some(0.01),
            relative_arrival_ms: Some(2.4),
            clock_drift_ppm: Some(-20.0),
            uncertainty_ms: Some(0.61),
            unresolved_reasons: vec![],
            valid_for_correction: false,
        })),
        impulse_error: None,
        valid_for_correction: false,
    }
}

pub fn missing_target_review() -> companion_service::review::SavedReview {
    use companion_service::review::{SegmentDiagnostics, SegmentStatus};
    let mut review = arrival_review();
    review.original_accepted_for_analysis = false;
    review.original_rejection_reason =
        Some("three reliable timing references were not found".into());
    review.timing = None;
    review.impulse = None;
    review.analysis_error = Some("Original acquisition rejected; diagnostics only. Record a new take after resolving its cause.".into());
    review.diagnostics.reference_start_frames = Some([24000.0, 168000.0]);
    review.diagnostics.segments = Some(std::array::from_fn(|i| SegmentDiagnostics {
        role: ["Reference before", "Speaker", "Reference after"][i],
        body_start_frame: 36000 + i * 72000,
        body_end_frame: 84000 + i * 72000,
        rms_dbfs: if i == 1 { -50.0 } else { -26.0 },
        signal_to_noise_db: if i == 1 { 0.0 } else { 24.0 },
        peak_dbfs: if i == 1 { -38.0 } else { -14.0 },
        clipped_samples: 0,
        normalized_correlation: if i == 1 { 0.01 } else { 0.5 },
        status: if i == 1 {
            SegmentStatus::NoSweepDetected
        } else {
            SegmentStatus::Detected
        },
    }));
    review.diagnostics.messages = vec!["The reference sweeps were found, but no target sweep was detected above background. Check the selected speaker's output route, mute, amplifier and connection. Increasing the overall volume is not the first step.".into()];
    review
}
