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
    };
    eprintln!(
        "Recording input channel {}. Playback: {} → {} → {}. Ctrl+C stops and saves a rejected take.",
        request.input_channel, request.reference_output, request.output, request.reference_output
    );
    let outcome = companion_service::measurement::measure(request, &cancelled, |_| {})?;
    println!("Raw recording and report: {}", outcome.directory.display());
    if let Some(reason) = outcome.rejection_reason {
        return Err(format!("take rejected: {reason}; see saved report").into());
    }
    if let Some(report) = outcome.timing {
        println!(
            "Timing diagnostic: relative peak {:.3} ms; clock drift {:.1} ppm. No correction proposed.",
            report.relative_peak_delay_ms, report.clock_drift_ppm
        );
    }
    Ok(())
}
