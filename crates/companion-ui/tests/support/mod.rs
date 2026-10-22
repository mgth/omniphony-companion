use companion_service::{
    controller::{Backend, Controller},
    manual_level,
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
    time::{Duration, Instant},
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
            output_filters_bypassed: false,
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
                // A synthetic room: surrounds mounted above the ear, a floor
                // subwoofer whose distance the layout does not state.
                azimuth_deg: Some([-30.0, 30.0, 0.0, -110.0, 110.0, 25.0][index]),
                elevation_deg: Some([0.0, 0.0, 0.0, 15.0, 15.0, -20.0][index]),
                distance_m: [Some(2.6), Some(2.6), Some(2.4), Some(1.9), Some(2.1), None][index],
                filter: None,
            })
            .collect(),
        })
    }
    /// A synthetic meter: each speaker sits a few dB from the reference and drifts
    /// slowly, so the preview's level band moves. No audio is played or recorded.
    fn manual_levels(
        &self,
        request: manual_level::Request,
        cancel: &AtomicBool,
        update: &mut dyn FnMut(manual_level::Reading),
    ) -> Result<(), String> {
        let offset_db = match request.output_id.as_str() {
            "Front right" => 2.5,
            "Center" => -4.0,
            "Surround left" => -1.5,
            "Surround right" => 0.3,
            _ => 0.0,
        };
        let start = Instant::now();
        while !cancel.load(Ordering::Relaxed) {
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed >= f64::from(manual_level::RUN_SECONDS) {
                break;
            }
            let phase = match elapsed {
                t if t < 0.4 => manual_level::Phase::CheckingInput,
                t if t < 0.9 => manual_level::Phase::Background,
                t if t < 1.4 => manual_level::Phase::Settling,
                _ => manual_level::Phase::Listening,
            };
            let db = offset_db + 0.4 * (elapsed * 0.7).sin();
            let level = (phase == manual_level::Phase::Listening)
                .then(|| {
                    calibration_core::manual_level::estimate(
                        0.01 * 10f64.powf(db / 20.0) + 0.0001,
                        0.0001,
                        0.0,
                    )
                })
                .flatten();
            update(manual_level::Reading {
                phase,
                output_gain_applied: Some(request.with_output_gain),
                level,
                remaining_seconds: manual_level::RUN_SECONDS - elapsed as u32,
                captured_at: Instant::now(),
            });
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
    /// The synthetic renderer switches to the speakers: discovery then reports none
    /// of the headphone reason.
    fn use_speaker_output(&self, endpoint: SocketAddr) -> Result<Snapshot, String> {
        self.discover(endpoint)
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
    let mut h = vec![0.0; 12001];
    h[4560] = -0.7;
    h[4944] = 0.35;
    let spectral = measurement_capture::spectral::analyze(
        &h,
        48000.0,
        4800,
        companion_service::controller::Settings::default().sweep,
    )
    .unwrap();
    let waveforms =
        measurement_capture::waveform::analyze(&h, 48000.0, 4800, &spectral, Some(-5.0));
    let segment = ImpulseSegment {
        role: "speaker".into(),
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
        spectral: Some(companion_service::review::SpectralReport {
            method: "synthetic-preview".into(),
            frequency_bounds_hz: [220.0, 9000.0],
            response_window_ms: [-100.0, 150.0],
            smoothing_octaves: 1.0 / 12.0,
            peak_band_octaves: 1.0 / 3.0,
            points: (0..64)
                .map(|i| {
                    let hz = 220.0 * (9000_f64 / 220.0).powf(i as f64 / 63.0);
                    let delay = -3.0 + 2.0 / (1.0 + (-5.0 * (hz / 2200.0).ln()).exp());
                    companion_service::review::SpectralPoint {
                        frequency_hz: hz,
                        magnitude_db: Some(-20.0 + 3.0 * (hz / 600.0).ln().sin()),
                        group_delay_ms: (i != 30).then_some(delay + 0.15 * (hz / 700.0).sin()),
                        peak_time_ms: (3..61).contains(&i).then_some(delay),
                        ambiguous_peak: i == 32,
                        time_resolution_ms: 2000.0 / (hz * 0.23),
                        estimated_snr_db: 35.0,
                    }
                })
                .collect(),
        }),
        spectral_error: None,
        waveforms,
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
        quality: None,
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
        capture_clipped_samples: 0,
        render_sample_rate: 48000,
        signal: companion_service::controller::Settings::default().sweep,
        sequence_window: None,
        original_accepted_for_analysis: true,
        original_rejection_reason: None,
        diagnostics: measurement_capture::diagnostics::AcquisitionDiagnostics {
            band_noise: None,
            method: "synthetic-preview".into(),
            noise_rms_dbfs: -50.0,
            total_clipped_samples: 0,
            reference_start_frames: None,
            segments: None,
            messages: vec![],
            valid_for_correction: false,
        },
        analysis_error: None,
        timing: Some(TimingReport {
            band_noise: None,
            warnings: vec![],
            method: "synthetic-preview".into(),
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
            method: "synthetic-preview".into(),
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
        role: ["Reference before", "Speaker", "Reference after"][i].into(),
        body_start_frame: 36000 + i * 72000,
        body_end_frame: 84000 + i * 72000,
        rms_dbfs: if i == 1 { -50.0 } else { -26.0 },
        signal_to_noise_db: if i == 1 { 0.0 } else { 24.0 },
        peak_dbfs: if i == 1 { -38.0 } else { -14.0 },
        clipped_samples: 0,
        overdrive_db: None,
        band_overdrive_db: None,
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

pub fn alignment(
    controller: &Controller,
    proposal: bool,
) -> companion_service::alignment::Alignment {
    use calibration_core::{
        CalibrationPolicy, CalibrationProfile, InstallationSnapshot, ObservationOrigin,
        OutputSettings, Speaker, SpeakerRole,
        repeatability::{QualifiedEstimate, RepeatEstimate},
    };
    use companion_service::{
        alignment::{Alignment, PassReport, Row},
        quality::{Grade, Indicator, Report},
    };
    let snapshot = controller.state.snapshot.clone().unwrap();
    let base = InstallationSnapshot {
        installation_id: "synthetic-room".into(),
        layout_revision: snapshot.revision.clone(),
        sample_rate_hz: 48000,
        speakers: snapshot
            .outputs
            .iter()
            .take(2)
            .map(|o| Speaker {
                id: o.id.clone(),
                label: o.name.clone(),
                role: SpeakerRole::FullRange,
                settings: OutputSettings {
                    gain_db: o.gain_db as f64,
                    delay_ms: o.delay_ms as f64,
                },
            })
            .collect(),
    };
    let mut target = base.clone();
    target.speakers[0].settings.gain_db -= 1.0;
    target.speakers[0].settings.delay_ms += 1.0;
    let rows = snapshot
        .outputs
        .iter()
        .take(if proposal { 2 } else { 3 })
        .enumerate()
        .map(|(i, o)| {
            let weak = i == 2;
            let arrival = [0.125, 0.325, 160.0][i];
            let level = [-6.5, -7.5, -25.0][i];
            let indicator = || Indicator {
                grade: if weak { Grade::Poor } else { Grade::Good },
                details: vec!["Synthetic presentation fixture".into()],
            };
            Row {
                sweep: None,
                reference_output: None,
                output_id: o.id.clone(),
                take_folders: vec![
                    format!("synthetic-pass-1-{i}").into(),
                    format!("synthetic-pass-2-{i}").into(),
                ],
                repeats: [0.0, 0.02]
                    .iter()
                    .map(|offset| RepeatEstimate {
                        arrival_ms: Some(arrival + offset),
                        level_db: Some(level),
                        minimum_snr_db: Some(if weak { 8.0 } else { 35.0 }),
                        reference_level_db: Some(0.0),
                        reasons: vec![],
                        codes: vec![],
                        output_gain_offset_db: 0.0,
                        minimum_snr_interval: None,
                    })
                    .collect(),
                quality: vec![Report {
                    method: "synthetic-preview".into(),
                    acquisition: indicator(),
                    timing: indicator(),
                    level: indicator(),
                    level_db: Some(level),
                    level_band_snr_db: Some(if weak { 8.0 } else { 35.0 }),
                    minimum_frequency_snr_db: None,
                }],
                qualified: (!weak).then_some(QualifiedEstimate {
                    arrival_ms: arrival,
                    level_db: level,
                    minimum_snr_db: 35.0,
                    arrival_repeat_spread_ms: 0.02,
                    level_repeat_spread_db: 0.10,
                }),
                // Saved as text only, like sessions before reason codes.
                reason: weak.then(|| {
                    "integrated level-band signal/noise is 8.0 dB (minimum 20 dB for correction)"
                        .into()
                }),
                reason_code: None,
                include_anyway: false,
            }
        })
        .collect();
    Alignment {
        level_probe: None,
        positions: vec![],
        schema_version: 1,
        directory: "synthetic-alignment".into(),
        endpoint: "127.0.0.1:9000".parse().unwrap(),
        settings: controller.settings.clone(),
        snapshot,
        rows,
        pass_reports: Some(vec![
            PassReport {
                analyzed_speakers: if proposal { 2 } else { 3 },
                error: None
            };
            2
        ]),
        verification_pass_reports: None,
        blockers: if proposal {
            vec![]
        } else {
            vec!["A speaker has not qualified for correction".into()]
        },
        blocker_codes: vec![],
        session: None,
        profile: proposal.then_some(CalibrationProfile {
            origin: ObservationOrigin::Simulated,
            schema_version: 1,
            source_session_id: "synthetic-preview".into(),
            source_batch_id: "synthetic-baseline".into(),
            reference_position_id: "reference-seat".into(),
            policy: CalibrationPolicy::default(),
            base_snapshot: base,
            target_snapshot: target,
        }),
        applied_snapshot: None,
        restored_snapshot: None,
        verification: None,
        verification_note: None,
        application_uncertain: false,
        physical_levels_changed: false,
        retake_snapshots: vec![],
        zeroed_snapshot: None,
        bass: None,
    }
}

/// A fuller synthetic room for the preview and chart tests: three repeatable speakers,
/// one silent, one noisy at a long delay. Values are invented; no capture happened.
pub fn room_alignment(controller: &Controller) -> companion_service::alignment::Alignment {
    use calibration_core::{
        CalibrationPolicy, CalibrationProfile, InstallationSnapshot, ObservationOrigin,
        OutputSettings, Speaker, SpeakerRole,
        repeatability::{QualifiedEstimate, RepeatEstimate},
    };
    use companion_service::{
        alignment::{Alignment, PassReport, Row},
        quality::{Grade, Indicator, Report},
    };
    let snapshot = controller.state.snapshot.clone().unwrap();
    // (arrival, level, grade, reason) per output; the subwoofer is muted and not aligned.
    type Fixture<'a> = (Option<f64>, Option<f64>, Grade, Option<&'a str>);
    let rows: [Fixture; 5] = [
        (Some(0.0), Some(-7.6), Grade::Good, None),
        (Some(0.32), Some(-4.2), Grade::Good, None),
        (Some(-1.85), Some(-3.0), Grade::Good, None),
        (
            None,
            None,
            Grade::Unavailable,
            Some("invalid calibration data: three reliable timing references were not found"),
        ),
        (
            Some(176.9),
            Some(-14.0),
            Grade::Poor,
            Some(
                "invalid calibration data: excited-band acquisition signal/noise below 15 dB; diagnostic result only",
            ),
        ),
    ];
    let latest = 0.32;
    let quietest = -7.6;
    let base = InstallationSnapshot {
        installation_id: "synthetic-room".into(),
        layout_revision: snapshot.revision.clone(),
        sample_rate_hz: 48000,
        speakers: snapshot
            .outputs
            .iter()
            .take(3)
            .map(|o| Speaker {
                id: o.id.clone(),
                label: o.name.clone(),
                role: SpeakerRole::FullRange,
                settings: OutputSettings {
                    gain_db: o.gain_db as f64,
                    delay_ms: o.delay_ms as f64,
                },
            })
            .collect(),
    };
    let mut target = base.clone();
    for (speaker, (arrival, level, ..)) in target.speakers.iter_mut().zip(&rows) {
        speaker.settings.delay_ms += latest - arrival.unwrap();
        speaker.settings.gain_db += quietest - level.unwrap();
    }
    let rows = snapshot
        .outputs
        .iter()
        .zip(rows)
        .enumerate()
        .map(|(i, (o, (arrival, level, grade, reason)))| {
            let indicator = || Indicator {
                grade,
                details: vec!["Synthetic presentation fixture".into()],
            };
            Row {
                sweep: None,
                reference_output: None,
                output_id: o.id.clone(),
                take_folders: vec![
                    format!("synthetic-room-pass-1-{i}").into(),
                    format!("synthetic-room-pass-2-{i}").into(),
                ],
                repeats: [0.0, 0.006]
                    .iter()
                    .map(|offset| RepeatEstimate {
                        arrival_ms: arrival.map(|a| a + offset),
                        level_db: level.map(|l| l + offset),
                        minimum_snr_db: Some(if reason.is_some() { 9.0 } else { 34.0 }),
                        reference_level_db: Some(0.0),
                        reasons: reason.map(|r| vec![r.to_owned()]).unwrap_or_default(),
                        codes: vec![],
                        output_gain_offset_db: 0.0,
                        minimum_snr_interval: None,
                    })
                    .collect(),
                quality: vec![Report {
                    method: "synthetic-preview".into(),
                    acquisition: indicator(),
                    timing: indicator(),
                    level: indicator(),
                    level_db: level,
                    level_band_snr_db: Some(if reason.is_some() { 9.0 } else { 34.0 }),
                    minimum_frequency_snr_db: None,
                }],
                qualified: reason.is_none().then(|| QualifiedEstimate {
                    arrival_ms: arrival.unwrap(),
                    level_db: level.unwrap(),
                    minimum_snr_db: 34.0,
                    arrival_repeat_spread_ms: 0.006,
                    level_repeat_spread_db: 0.006,
                }),
                reason: reason.map(str::to_owned),
                reason_code: None,
                include_anyway: false,
            }
        })
        .collect();
    Alignment {
        level_probe: None,
        positions: vec![],
        schema_version: 1,
        directory: "synthetic-room-alignment".into(),
        endpoint: "127.0.0.1:9000".parse().unwrap(),
        settings: controller.settings.clone(),
        snapshot,
        rows,
        pass_reports: Some(vec![
            PassReport {
                analyzed_speakers: 5,
                error: None
            };
            2
        ]),
        verification_pass_reports: None,
        blockers: vec![],
        blocker_codes: vec![],
        session: None,
        profile: Some(CalibrationProfile {
            origin: ObservationOrigin::Simulated,
            schema_version: 1,
            source_session_id: "synthetic-room".into(),
            source_batch_id: "synthetic-room-baseline".into(),
            reference_position_id: "reference-seat".into(),
            policy: CalibrationPolicy::default(),
            base_snapshot: base,
            target_snapshot: target,
        }),
        applied_snapshot: None,
        restored_snapshot: None,
        verification: None,
        verification_note: None,
        application_uncertain: false,
        physical_levels_changed: false,
        retake_snapshots: vec![],
        zeroed_snapshot: None,
        bass: None,
    }
}

/// Three aligned speakers, one silent and one repeatable but 60 ms late (another output
/// device), set aside with reason codes as a new session saves them. No proposal yet,
/// so "Align to it anyway" can rebuild one from these passes in `directory`.
pub fn late_device_alignment(
    controller: &Controller,
    directory: &std::path::Path,
) -> companion_service::alignment::Alignment {
    use calibration_core::{ExclusionReason, repeatability::RepeatEstimate};
    use companion_service::alignment::{Alignment, PassReport, Row};
    let snapshot = controller.state.snapshot.clone().unwrap();
    let rows = snapshot
        .outputs
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, o)| {
            let arrival = [0.0, 0.32, -1.85, 0.0, 60.0][i];
            let level = [-7.6, -4.2, -3.0, 0.0, -5.8][i];
            let silent = i == 3;
            let repeats = [0.0, 0.006]
                .iter()
                .map(|offset| {
                    let mut repeat = RepeatEstimate {
                        arrival_ms: (!silent).then_some(arrival + offset),
                        level_db: (!silent).then_some(level + offset),
                        minimum_snr_db: (!silent).then_some(34.0),
                        reference_level_db: (!silent).then_some(0.0),
                        reasons: vec![],
                        codes: vec![],
                        output_gain_offset_db: 0.0,
                        minimum_snr_interval: None,
                    };
                    if silent {
                        repeat.reject(ExclusionReason::MissingTimingReferences);
                    }
                    repeat
                })
                .collect::<Vec<_>>();
            let qualified = calibration_core::repeatability::qualify(&repeats).ok();
            let mut row = Row {
                sweep: None,
                reference_output: None,
                output_id: o.id.clone(),
                take_folders: vec![
                    format!("synthetic-late-pass-1-{i}").into(),
                    format!("synthetic-late-pass-2-{i}").into(),
                ],
                repeats,
                quality: vec![],
                qualified: qualified.clone(),
                reason: None,
                reason_code: None,
                include_anyway: false,
            };
            if silent {
                row.reason_code = Some(ExclusionReason::MissingTimingReferences);
                row.reason = row.reason_code.as_ref().map(ToString::to_string);
            } else if i == 4 {
                row.reason_code = Some(ExclusionReason::FarArrival {
                    ms: qualified.unwrap().arrival_ms,
                });
            }
            row
        })
        .collect();
    Alignment {
        level_probe: None,
        positions: vec![],
        schema_version: 1,
        directory: directory.to_owned(),
        endpoint: "127.0.0.1:9000".parse().unwrap(),
        settings: controller.settings.clone(),
        snapshot,
        rows,
        pass_reports: Some(vec![
            PassReport {
                analyzed_speakers: 5,
                error: None
            };
            2
        ]),
        verification_pass_reports: None,
        blockers: vec![],
        blocker_codes: vec![],
        session: None,
        profile: None,
        applied_snapshot: None,
        restored_snapshot: None,
        verification: None,
        verification_note: None,
        application_uncertain: false,
        physical_levels_changed: false,
        retake_snapshots: vec![],
        zeroed_snapshot: None,
        bass: None,
    }
}

/// A 14-output bench like the user's on 2026-10-20: Omniphony's normalised layout,
/// where FL and TFL share an azimuth, BL and SR share a position, LFE sits straight
/// below and two height speakers play through another output device, 143 ms late.
/// Seven speakers were measured after a first alignment, so arrivals include the
/// applied delays. Replaces the controller's renderer; values come from one real
/// session, but no capture happens here.
pub fn bench_alignment(controller: &mut Controller) -> companion_service::alignment::Alignment {
    use calibration_core::repeatability::{QualifiedEstimate, RepeatEstimate};
    use companion_service::alignment::{Alignment, PassReport, Row};
    // id, azimuth, elevation, layout distance, delay, gain, low, high, measured arrival.
    type Out = (
        &'static str,
        f32,
        f32,
        f32,
        f32,
        f32,
        Option<f32>,
        Option<f32>,
        Option<f64>,
    );
    let layout: [Out; 14] = [
        (
            "FL",
            -26.565,
            0.0,
            2.236,
            1.469,
            -11.7,
            Some(40.0),
            None,
            Some(0.017),
        ),
        (
            "FR",
            26.565,
            0.0,
            2.236,
            0.174,
            -11.9,
            Some(40.0),
            None,
            Some(0.012),
        ),
        (
            "C",
            0.0,
            0.0,
            2.0,
            0.216,
            -11.6,
            Some(40.0),
            None,
            Some(0.043),
        ),
        (
            "BC",
            180.0,
            -4.229,
            1.003,
            7.67,
            -11.4,
            Some(80.0),
            None,
            Some(0.040),
        ),
        (
            "BR",
            -135.0,
            0.0,
            1.414,
            6.971,
            -11.5,
            Some(40.0),
            None,
            Some(0.048),
        ),
        ("BL", 135.0, 0.0, 1.414, 0.0, 0.0, Some(40.0), None, None),
        (
            "TBL",
            -135.0,
            35.264,
            1.732,
            5.958,
            -12.1,
            Some(110.0),
            None,
            Some(0.038),
        ),
        (
            "TBR",
            135.0,
            35.264,
            1.732,
            5.541,
            -11.5,
            Some(110.0),
            None,
            Some(0.028),
        ),
        (
            "TFL",
            -26.565,
            24.095,
            2.449,
            0.0,
            0.0,
            Some(110.0),
            None,
            None,
        ),
        (
            "TFR",
            26.565,
            24.095,
            2.449,
            0.0,
            0.0,
            Some(110.0),
            None,
            None,
        ),
        ("SR", 135.0, 0.0, 1.414, 0.0, 0.0, Some(80.0), None, None),
        ("LFE", 0.0, -90.0, 0.5, 0.0, 0.0, None, Some(80.0), None),
        (
            "spk-12",
            -90.0,
            64.179,
            1.111,
            0.0,
            0.0,
            Some(110.0),
            None,
            Some(143.391),
        ),
        (
            "spk-13",
            90.0,
            61.26,
            1.140,
            0.0,
            0.0,
            Some(110.0),
            None,
            Some(143.322),
        ),
    ];
    let snapshot = controller.state.snapshot.as_mut().unwrap();
    snapshot.outputs = layout
        .iter()
        .enumerate()
        .map(
            |(index, &(id, az, el, d, delay, gain, low, high, _))| Output {
                id: id.into(),
                name: id.into(),
                index: index as u32,
                gain_db: gain,
                delay_ms: delay,
                muted: false,
                freq_low_hz: low,
                freq_high_hz: high,
                azimuth_deg: Some(az),
                elevation_deg: Some(el),
                distance_m: Some(d),
                filter: None,
            },
        )
        .collect();
    let snapshot = snapshot.clone();
    controller.settings.reference_output = Some("FL".into());
    controller.excluded_outputs = layout
        .iter()
        .filter(|o| o.8.is_none())
        .map(|o| o.0.to_owned())
        .collect();
    let rows = layout
        .iter()
        .filter_map(|&(id, .., arrival)| {
            let arrival = arrival?;
            let repeats = [0.0, 0.004]
                .iter()
                .map(|offset| RepeatEstimate {
                    arrival_ms: Some(arrival + offset),
                    level_db: Some(-14.0 + offset),
                    minimum_snr_db: Some(34.0),
                    reference_level_db: Some(0.0),
                    reasons: vec![],
                    codes: vec![],
                    output_gain_offset_db: 0.0,
                    minimum_snr_interval: None,
                })
                .collect();
            Some(Row {
                sweep: None,
                reference_output: None,
                output_id: id.into(),
                take_folders: vec![
                    format!("synthetic-bench-pass-1-{id}").into(),
                    format!("synthetic-bench-pass-2-{id}").into(),
                ],
                repeats,
                quality: vec![],
                qualified: Some(QualifiedEstimate {
                    arrival_ms: arrival,
                    level_db: -14.0,
                    minimum_snr_db: 34.0,
                    arrival_repeat_spread_ms: 0.004,
                    level_repeat_spread_db: 0.004,
                }),
                reason: None,
                reason_code: None,
                // The user chose to align to the late device.
                include_anyway: id.starts_with("spk"),
            })
        })
        .collect();
    Alignment {
        level_probe: None,
        positions: vec![],
        schema_version: 1,
        directory: "synthetic-bench-alignment".into(),
        endpoint: "127.0.0.1:9000".parse().unwrap(),
        settings: controller.settings.clone(),
        snapshot,
        rows,
        pass_reports: Some(vec![
            PassReport {
                analyzed_speakers: 9,
                error: None
            };
            2
        ]),
        verification_pass_reports: None,
        blockers: vec![],
        blocker_codes: vec![],
        session: None,
        profile: None,
        applied_snapshot: None,
        restored_snapshot: None,
        verification: None,
        verification_note: None,
        application_uncertain: false,
        physical_levels_changed: false,
        retake_snapshots: vec![],
        zeroed_snapshot: None,
        bass: None,
    }
}
