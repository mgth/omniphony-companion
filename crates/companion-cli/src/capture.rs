use crate::{AppResult, read_json};
use clap::{Args, Subcommand};
use companion_service::measurement::MeasureRequest;
use omniphony_adapter::Sweep;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Subcommand)]
pub enum Command {
    /// List audio input names and supported configurations. Does not record.
    Devices,
    /// Reanalyze a saved microphone take without opening audio or the renderer.
    Review {
        #[arg(long)]
        take: PathBuf,
    },
    /// Recompute both retained alignment passes as a separate diagnostic report.
    ReviewAlignment {
        #[arg(long)]
        session: PathBuf,
    },
    /// Record one microphone channel with reference/target/reference playback.
    Measure(Box<Measure>),
}

#[derive(Args)]
pub struct Measure {
    #[arg(long, default_value = "127.0.0.1:9000")]
    endpoint: SocketAddr,
    #[arg(long)]
    snapshot: PathBuf,
    /// Exact input name returned by capture devices.
    #[arg(long)]
    device: String,
    /// One-based interface channel; the WAV contains only this channel.
    #[arg(long, default_value_t = 1)]
    input_channel: u16,
    #[arg(long, default_value_t = 48000)]
    sample_rate: u32,
    #[arg(long)]
    microphone_id: String,
    #[arg(long)]
    position_id: String,
    #[arg(long)]
    output: String,
    /// Keep the same physical reference output across compared takes.
    #[arg(long)]
    reference_output: String,
    /// Optional two-column frequency/relative-dB text file; saved, not applied.
    #[arg(long)]
    calibration: Option<PathBuf>,
    #[arg(long, default_value_t = 1000)]
    duration_ms: u32,
    #[arg(long, default_value_t = -30.0, allow_hyphen_values = true)]
    peak_dbfs: f32,
    #[arg(long, default_value_t = 200.0)]
    start_hz: f32,
    #[arg(long, default_value_t = 10000.0)]
    end_hz: f32,
    /// New directory for recording.wav and metadata; never overwrites a take.
    #[arg(long)]
    out_dir: PathBuf,
}

pub fn run(command: Command) -> AppResult<()> {
    match command {
        Command::Devices => println!(
            "{}",
            serde_json::to_string_pretty(&measurement_capture::devices()?)?
        ),
        Command::Measure(args) => measure(*args)?,
        Command::ReviewAlignment { session } => {
            let original = companion_service::alignment::load(&session)?;
            let reviewed = companion_service::alignment::reanalyze(&original)?;
            for row in &reviewed.rows {
                println!(
                    "{}: {}",
                    row.output_id,
                    match (&row.qualified, row.exclusion()) {
                        (Some(q), reason) => format!(
                            "{:+.3} ms / {:+.2} dB; repeat spread {:.3} ms / {:.2} dB{}",
                            q.arrival_ms,
                            q.level_db,
                            q.arrival_repeat_spread_ms,
                            q.level_repeat_spread_db,
                            reason.map(|r| format!("; {r}")).unwrap_or_default()
                        ),
                        (None, Some(reason)) => reason.to_string(),
                        (None, None) => "Unavailable".into(),
                    }
                );
                for quality in &row.quality {
                    print_quality(quality);
                }
            }
            println!("Diagnostic alignment: {}", reviewed.directory.display());
        }
        Command::Review { take } => {
            let review = companion_service::review::saved_take(&take)?;
            println!(
                "Offline review: {} (reference {})",
                review.output_id, review.reference_output_id
            );
            if review.origin == "sink_monitor_capture" {
                println!(
                    "Sink monitor: digital routing evidence, not an acoustic microphone measurement."
                );
            }
            if let Some(reason) = &review.original_rejection_reason {
                println!("Original take rejected: {reason}");
            }
            print_diagnostics(&review.diagnostics);
            if let Some(quality) = &review.quality {
                print_quality(quality);
            }
            if let Some(error) = &review.analysis_error {
                println!("{error}");
            }
            print_impulse(review.impulse.as_deref(), review.impulse_error.as_deref());
            println!("Review report: {}", review.report_file.display());
            println!(
                "Original recording and capture report preserved. No audio played; no correction applied."
            );
        }
    }
    Ok(())
}
fn measure(args: Measure) -> AppResult<()> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = cancelled.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let request = MeasureRequest {
        endpoint: args.endpoint,
        snapshot: read_json(&args.snapshot)?,
        device: args.device,
        input_channel: args.input_channel,
        sample_rate: args.sample_rate,
        microphone_id: args.microphone_id,
        position_id: args.position_id,
        output: args.output,
        reference_output: args.reference_output,
        calibration: args.calibration,
        sweep: Sweep {
            duration_ms: args.duration_ms,
            peak_dbfs: args.peak_dbfs,
            start_hz: args.start_hz,
            end_hz: args.end_hz,
        },
        out_dir: args.out_dir,
        bass_managed: false,
        bypass_output_filters: false,
    };
    eprintln!(
        "Recording input channel {}. Playback: {} → {} → {}. Ctrl+C stops and saves a rejected take.",
        request.input_channel, request.reference_output, request.output, request.reference_output
    );
    let outcome = companion_service::measurement::measure(request, &cancelled, |_| {})?;
    println!("Raw recording and report: {}", outcome.directory.display());
    if let Some(report) = &outcome.diagnostics {
        print_diagnostics(report);
    }
    print_quality(&outcome.quality());
    print_impulse(outcome.impulse.as_deref(), outcome.impulse_error.as_deref());
    if let Some(reason) = outcome.rejection_reason {
        return Err(format!(
            "capture fault: {reason}; diagnostic results retained in the saved report"
        )
        .into());
    }
    if let Some(report) = outcome.timing {
        println!(
            "Acquisition signal / noise: {:.1} dB. No correction proposed.",
            report.signal_to_noise_db[1]
        );
        if outcome.impulse.is_none() {
            if let Some(warning) = report.timing_warning() {
                println!("Captured; timing unresolved. {warning}");
            } else if let (Some(delay), Some(drift)) =
                (report.relative_peak_delay_ms, report.clock_drift_ppm)
            {
                println!(
                    "Timing diagnostic: relative peak {delay:.3} ms; clock drift {drift:.1} ppm."
                );
            }
        }
    }
    Ok(())
}

fn print_impulse(report: Option<&companion_service::review::ImpulseReport>, error: Option<&str>) {
    if let Some(report) = report {
        if let (Some(delay), Some(bound), Some(drift)) = (
            report.relative_arrival_ms,
            report.uncertainty_ms,
            report.clock_drift_ppm,
        ) {
            println!(
                "Arrival candidate: {delay:+.3} ms; sensitivity bound ±{bound:.3} ms; clock drift {drift:+.1} ppm."
            );
            println!("Repeatable candidate, not a validated direct path or correction.");
        } else {
            println!("Impulse response available; arrival unresolved:");
            for reason in &report.unresolved_reasons {
                println!("  {reason}");
            }
        }
    }
    if let Some(error) = error {
        println!("Impulse response unavailable: {error}");
    }
}

fn print_diagnostics(report: &measurement_capture::diagnostics::AcquisitionDiagnostics) {
    use measurement_capture::clipping::label;
    println!(
        "Acquisition background: {:.1} dBFS; clipped samples: {}.",
        report.noise_rms_dbfs, report.total_clipped_samples
    );
    if let Some(segments) = &report.segments {
        for segment in segments {
            let overdrive = match (segment.overdrive_db, segment.band_overdrive_db) {
                (Some(max), Some(band)) => format!(
                    " ({} over full scale; 500–2000 Hz band {})",
                    label(max),
                    label(band)
                ),
                (Some(max), None) => format!(" ({} over full scale)", label(max)),
                _ => String::new(),
            };
            println!(
                "{}: {}; RMS {:.1} dBFS; signal / noise {:.1} dB; peak {:.1} dBFS; {} clipped samples{overdrive}.",
                segment.role,
                segment.status.label(),
                segment.rms_dbfs,
                segment.signal_to_noise_db,
                segment.peak_dbfs,
                segment.clipped_samples
            );
        }
    }
    for message in &report.messages {
        println!("{message}");
    }
    println!("Approximate reference-pair windows; these levels are not speaker gain corrections.");
}

fn print_quality(q: &companion_service::quality::Report) {
    println!(
        "Experimental quality — acquisition: {}; timing: {}; level: {}",
        q.acquisition.grade.label(),
        q.timing.grade.label(),
        q.level.grade.label()
    );
    for indicator in [&q.acquisition, &q.timing, &q.level] {
        for detail in &indicator.details {
            println!("  {detail}");
        }
    }
}
