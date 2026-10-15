mod capture;
mod renderer;
mod simulation;

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use calibration_core::{
    CalibrationPolicy, CalibrationProfile, MeasurementBatch, Session, propose, verify,
};
use clap::{Parser, Subcommand};
use companion_service::{
    AppResult,
    files::{parent, read_json, write_json},
};

#[derive(Parser)]
#[command(
    version,
    about = "Omniphony calibration prototype: microphone capture and offline analysis"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Capture a microphone with acoustic timing references.
    Capture {
        #[command(subcommand)]
        command: capture::Command,
    },
    /// Discover Omniphony outputs or play an identified measurement sweep.
    Renderer {
        #[command(subcommand)]
        command: renderer::Command,
    },
    /// Run a deterministic simulated measurement/apply/verification cycle.
    Demo {
        /// New directory for four JSON artifacts; existing paths are refused.
        #[arg(long, default_value = "sessions/demo")]
        out_dir: PathBuf,
    },
    /// Validate and summarize an observation session.
    Inspect {
        #[arg(long)]
        session: PathBuf,
    },
    /// Calculate absolute gain/delay targets from validated observations.
    Plan {
        #[arg(long)]
        session: PathBuf,
        /// Output must not already exist.
        #[arg(long)]
        out: PathBuf,
    },
    /// Check a separate observation batch; exit 2 when tolerances are not met.
    Verify {
        #[arg(long)]
        session: PathBuf,
        #[arg(long)]
        profile: PathBuf,
        #[arg(long)]
        measurements: PathBuf,
        /// Output must not already exist, including when verification fails.
        #[arg(long)]
        out: PathBuf,
    },
}

fn run(args: Args) -> AppResult<ExitCode> {
    match args.command {
        Command::Capture { command } => capture::run(command)?,
        Command::Renderer { command } => renderer::run(command)?,
        Command::Demo { out_dir } => {
            let (session, profile, measurements, report) = simulation::run()?;
            fs::create_dir_all(parent(&out_dir))?;
            fs::create_dir(&out_dir)?;
            write_json(&out_dir.join("session.json"), &session)?;
            write_json(&out_dir.join("profile.json"), &profile)?;
            write_json(
                &out_dir.join("verification-measurements.json"),
                &measurements,
            )?;
            write_json(&out_dir.join("verification-report.json"), &report)?;
            println!("SIMULATION: no microphones, audio playback or renderer connection.");
            println!("Reference position: {}", report.reference_position_id);
            println!(
                "Arrival spread: {:.3} -> {:.3} ms",
                report.before.arrival_spread_ms, report.after.arrival_spread_ms
            );
            println!(
                "Level spread: {:.3} -> {:.3} dB",
                report.before.level_spread_db, report.after.level_spread_db
            );
            println!(
                "Verification: {}",
                if report.passed { "PASS" } else { "FAIL" }
            );
            println!("Artifacts: {}", out_dir.display());
            return Ok(if report.passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            });
        }
        Command::Inspect { session } => {
            let session: Session = read_json(&session)?;
            session.validate()?;
            println!(
                "Session: {} ({:?} observations)",
                session.id, session.baseline.origin
            );
            println!(
                "{} outputs, {} microphones, {} positions, {} observations",
                session.baseline.snapshot.speakers.len(),
                session.microphones.len(),
                session.positions.len(),
                session.baseline.observations.len()
            );
            println!("Reference position: {}", session.reference_position_id);
            for speaker in &session.baseline.snapshot.speakers {
                println!(
                    "{} [{:?}]: {:.2} dB, {:.3} ms",
                    speaker.id, speaker.role, speaker.settings.gain_db, speaker.settings.delay_ms
                );
            }
        }
        Command::Plan { session, out } => {
            let session: Session = read_json(&session)?;
            let profile = propose(&session, &CalibrationPolicy::default())?;
            write_json(&out, &profile)?;
            println!(
                "Profile written to {}. No renderer settings were changed.",
                out.display()
            );
        }
        Command::Verify {
            session,
            profile,
            measurements,
            out,
        } => {
            let session: Session = read_json(&session)?;
            let profile: CalibrationProfile = read_json(&profile)?;
            let measurements: MeasurementBatch = read_json(&measurements)?;
            let report = verify(&session, &profile, &measurements)?;
            write_json(&out, &report)?;
            println!(
                "Verification ({:?} observations): {}",
                report.origin,
                if report.passed { "PASS" } else { "FAIL" }
            );
            return Ok(if report.passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            });
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
