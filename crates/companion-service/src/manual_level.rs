//! Finite physical-level noise and a microphone meter; never writes output settings.
use crate::{AppResult, microphone::InputRequest};
use calibration_core::manual_level::{self, Level};
use measurement_capture::{Capture, live_level::Meter};
use omniphony_adapter::{Client, SignalState, Snapshot, protocol::LevelNoise};
use std::{
    net::SocketAddr,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const RUN_SECONDS: u32 = 60;
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub endpoint: SocketAddr,
    pub snapshot: Snapshot,
    pub input: InputRequest,
    pub microphone_id: String,
    pub position_id: String,
    pub reference_output: String,
    pub peak_dbfs: f32,
}
impl Context {
    pub fn output(&self, id: &str) -> Result<&omniphony_adapter::protocol::Output, String> {
        LevelNoise {
            duration_seconds: RUN_SECONDS,
            peak_dbfs: self.peak_dbfs,
            bypass_output_gain: false,
        }
        .validate(self.snapshot.sample_rate)?;
        if let Some(reason) = &self.snapshot.unavailable_reason {
            return Err(reason.clone());
        }
        let output = self
            .snapshot
            .outputs
            .iter()
            .find(|o| o.id == id)
            .ok_or("Choose a connected speaker.")?;
        if output.muted {
            return Err("Unmute the speaker first.".into());
        }
        if output.freq_low_hz.unwrap_or(20.0) > LevelNoise::LOW_HZ
            || output.freq_high_hz.unwrap_or(f32::MAX) < LevelNoise::HIGH_HZ
        {
            return Err(
                "This speaker does not cover 500–2000 Hz. Subwoofer matching is not available yet."
                    .into(),
            );
        }
        Ok(output)
    }
    /// [`Self::output`], within the output ceiling for the noise played with the
    /// output's gain or without it (the master gain applies either way).
    pub fn playable(
        &self,
        id: &str,
        with_output_gain: bool,
    ) -> Result<&omniphony_adapter::protocol::Output, String> {
        let output = self.output(id)?;
        let gain = if with_output_gain {
            output.gain_db
        } else {
            0.0
        };
        if self.peak_dbfs + self.snapshot.master_gain_db + gain > -6.0 {
            return Err(
                "Lower the test level: existing digital gains exceed the output ceiling.".into(),
            );
        }
        Ok(output)
    }
}
#[derive(Debug, Clone)]
pub struct Request {
    pub context: Context,
    pub output_id: String,
    /// Check mode: play the noise through the output's gain (Omniphony's level
    /// correction) and compare speakers as corrected. Off, the noise skips that
    /// gain when the renderer can, so the physical volume controls are matched.
    pub with_output_gain: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    CheckingInput,
    Background,
    Settling,
    Listening,
}
#[derive(Debug, Clone)]
pub struct Reading {
    pub phase: Phase,
    /// Whether the renderer plays the noise through the output's gain; known once
    /// it is connected. An older renderer always does.
    pub output_gain_applied: Option<bool>,
    pub level: Option<Level>,
    pub remaining_seconds: u32,
    pub captured_at: Instant,
}
#[derive(Debug, Clone)]
pub struct Run {
    pub request: Request,
    pub reading: Option<Reading>,
    pub error: Option<String>,
    pub completed: bool,
}
#[derive(Debug, Clone)]
pub struct Reference {
    pub context: Context,
    pub level: Level,
}

/// Worker owns both the microphone and lease. Drop/disconnect is an additional
/// renderer-side stop; the frontend never owns a playback timer or keepalive.
pub fn run(
    request: Request,
    cancel: &AtomicBool,
    update: &mut dyn FnMut(Reading),
) -> AppResult<()> {
    let context = &request.context;
    let output = context.playable(&request.output_id, request.with_output_gain)?;
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    update(Reading {
        phase: Phase::CheckingInput,
        output_gain_applied: None,
        level: None,
        remaining_seconds: RUN_SECONDS,
        captured_at: Instant::now(),
    });
    let mut client = Client::connect(context.endpoint, Duration::from_millis(500))?;
    if client.discover()? != context.snapshot {
        return Err("Renderer configuration changed. Reconnect before matching levels.".into());
    }
    // The level is compared without the gains the renderer applied to the noise.
    let bypass_output_gain = !request.with_output_gain
        && client.protocol_version() >= omniphony_adapter::protocol::NOISE_GAINS_VERSION;
    let digital_trim = f64::from(
        context.snapshot.master_gain_db
            + if bypass_output_gain {
                0.0
            } else {
                output.gain_db
            },
    );
    let input = &context.input;
    let mut capture = Capture::start_bounded(
        &input.device,
        input.input_channel,
        input.sample_rate,
        input.sample_rate as usize * 2,
    )?;
    if capture.is_monitor() {
        return Err("Choose a physical microphone input, not a sink monitor.".into());
    }
    // Exclusively silence programme first, then measure its quiet background.
    client.begin(&context.snapshot)?;
    let result = listen(
        &mut client,
        &mut capture,
        input.sample_rate,
        (digital_trim, bypass_output_gain),
        request,
        cancel,
        update,
    );
    // Attempt an explicit stop on every path; drop also disconnects the owner.
    let _ = client.end();
    result
}
fn listen(
    client: &mut Client,
    capture: &mut Capture,
    rate: u32,
    (digital_trim, bypass_output_gain): (f64, bool),
    request: Request,
    cancel: &AtomicBool,
    update: &mut dyn FnMut(Reading),
) -> AppResult<()> {
    let mut meter = Meter::new(rate)?;
    let mut last_audio = Instant::now();
    let mut next_status = Instant::now();
    let baseline = Instant::now();
    while baseline.elapsed() < Duration::from_millis(1600) {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        drain(capture, &mut meter, &mut last_audio)?;
        if Instant::now() >= next_status {
            client.status()?;
            update(Reading {
                phase: Phase::Background,
                output_gain_applied: Some(!bypass_output_gain),
                level: None,
                remaining_seconds: RUN_SECONDS,
                captured_at: Instant::now(),
            });
            next_status = Instant::now() + Duration::from_millis(250);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let background = meter.power().filter(|p| meter.ready() && *p > 1e-18)
        .ok_or("Microphone is silent or background capture is incomplete. Check the input before playing noise.")?;
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    let signal = LevelNoise {
        duration_seconds: RUN_SECONDS,
        peak_dbfs: request.context.peak_dbfs,
        bypass_output_gain,
    };
    client.start_noise(1, &request.output_id, signal)?;
    meter = Meter::new(rate)?;
    let started = Instant::now();
    let mut rendering_since = None;
    let mut last_rendered = 0;
    let mut last_progress = started;
    let mut next_update = started;
    next_status = started;
    let result: AppResult<()> = (|| {
        while started.elapsed() < Duration::from_secs(u64::from(RUN_SECONDS)) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            drain(capture, &mut meter, &mut last_audio)?;
            if Instant::now() >= next_status {
                let p = client.noise_status()?;
                match p.state {
                    SignalState::Completed => break,
                    SignalState::Cancelled | SignalState::Expired => {
                        return Err(
                            "Renderer stopped the level noise. Reconnect and try again.".into()
                        );
                    }
                    SignalState::Rendering => {
                        rendering_since.get_or_insert_with(Instant::now);
                    }
                    SignalState::Queued => {}
                }
                if p.rendered_frames > last_rendered {
                    last_progress = Instant::now();
                    last_rendered = p.rendered_frames;
                }
                if last_progress.elapsed() > Duration::from_secs(2) {
                    return Err("Renderer is not advancing the noise signal.".into());
                }
                next_status = Instant::now() + Duration::from_millis(250);
            }
            if Instant::now() >= next_update {
                let settled = rendering_since
                    .is_some_and(|t| t.elapsed() >= Duration::from_secs(2))
                    && meter.ready();
                update(Reading {
                    output_gain_applied: Some(!bypass_output_gain),
                    phase: if settled {
                        Phase::Listening
                    } else {
                        Phase::Settling
                    },
                    level: if settled {
                        meter
                            .power()
                            .and_then(|p| manual_level::estimate(p, background, digital_trim))
                    } else {
                        None
                    },
                    remaining_seconds: RUN_SECONDS
                        .saturating_sub(started.elapsed().as_secs() as u32),
                    captured_at: Instant::now(),
                });
                next_update = Instant::now() + Duration::from_millis(100);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })();
    let stopped = client.stop_noise();
    result?;
    stopped?;
    Ok(())
}
fn drain(capture: &mut Capture, meter: &mut Meter, last_audio: &mut Instant) -> AppResult<()> {
    if capture.drain_live(|s| meter.push(s)) > 0 {
        *last_audio = Instant::now();
    }
    if capture.faults() != 0 || meter.invalid {
        return Err("Microphone capture fault; level noise stopped.".into());
    }
    if meter.clipped {
        return Err("Microphone clipping; level noise stopped. Lower the interface input gain or speaker volume.".into());
    }
    if last_audio.elapsed() > Duration::from_secs(1) {
        return Err("Microphone stopped delivering frames; level noise stopped.".into());
    }
    Ok(())
}
