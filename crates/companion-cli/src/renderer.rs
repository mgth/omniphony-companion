use crate::{AppResult, parent, read_json, write_json};
use clap::Subcommand;
use omniphony_adapter::{Client, SignalState, Snapshot, Sweep};
use std::{fs, net::SocketAddr, path::PathBuf, time::Duration};

#[derive(Subcommand)]
pub enum Command {
    /// Read physical output names, current settings and measurement availability.
    Discover {
        #[arg(long, default_value = "127.0.0.1:9000")]
        endpoint: SocketAddr,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Play one finite sweep through a real output; no microphone is recorded.
    Sweep {
        #[arg(long, default_value = "127.0.0.1:9000")]
        endpoint: SocketAddr,
        /// A previously discovered snapshot; stale configurations are refused.
        #[arg(long)]
        snapshot: PathBuf,
        #[arg(long)]
        output: String,
        #[arg(long, default_value_t = 1000)]
        duration_ms: u32,
        #[arg(long, default_value_t = -30.0, allow_hyphen_values = true)]
        peak_dbfs: f32,
        #[arg(long, default_value_t = 200.0)]
        start_hz: f32,
        #[arg(long, default_value_t = 10000.0)]
        end_hz: f32,
        /// New directory for the initial snapshot, signal request and result.
        #[arg(long)]
        out_dir: PathBuf,
    },
}

pub fn run(command: Command) -> AppResult<()> {
    match command {
        Command::Discover { endpoint, out } => {
            let mut client = Client::connect(endpoint, Duration::from_secs(2))?;
            let snapshot = client.discover()?;
            if let Some(path) = out {
                write_json(&path, &snapshot)?;
            }
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        }
        Command::Sweep {
            endpoint,
            snapshot,
            output,
            duration_ms,
            peak_dbfs,
            start_hz,
            end_hz,
            out_dir,
        } => {
            let snapshot: Snapshot = read_json(&snapshot)?;
            let signal = Sweep {
                duration_ms,
                peak_dbfs,
                start_hz,
                end_hz,
            };
            signal.validate(snapshot.sample_rate)?;
            fs::create_dir_all(parent(&out_dir))?;
            fs::create_dir(&out_dir)?;
            write_json(&out_dir.join("snapshot.json"), &snapshot)?;
            write_json(
                &out_dir.join("stimulus.json"),
                &serde_json::json!({
                    "protocol_version": omniphony_adapter::protocol::VERSION,
                    "signal_id": 1, "output_id": output, "signal": signal,
                    "microphone_capture": false,
                }),
            )?;
            let mut client = Client::connect(endpoint, Duration::from_secs(2))?;
            client.begin(&snapshot)?;
            eprintln!(
                "Playing a measurement sweep on {output}. No microphone capture. Ctrl+C disconnects the session."
            );
            let result: AppResult<_> = (|| {
                let mut progress = client.start(1, &output, signal)?;
                loop {
                    match progress.state {
                        SignalState::Completed => return Ok(progress),
                        SignalState::Cancelled | SignalState::Expired => {
                            return Err(format!("signal ended: {:?}", progress.state).into());
                        }
                        SignalState::Queued | SignalState::Rendering => {}
                    }
                    std::thread::sleep(Duration::from_millis(200));
                    progress = client.status()?.ok_or("renderer lost the active signal")?;
                }
            })();
            // End on both success and refusal. A broken connection also drops
            // ownership; the renderer has an independent lease and sample cap.
            let ended = client.end();
            let progress = result?;
            ended?;
            write_json(
                &out_dir.join("render-report.json"),
                &serde_json::json!({
                    "protocol_version": omniphony_adapter::protocol::VERSION,
                    "snapshot": snapshot, "signal": progress, "microphone_capture": false,
                }),
            )?;
            println!(
                "Rendered {} frames, including settling silence. This confirms renderer progress, not acoustic measurement.",
                progress.rendered_frames
            );
            println!("Artifacts: {}", out_dir.display());
        }
    }
    Ok(())
}
