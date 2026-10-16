//! Device I/O lives here, outside the calibration policy core.
//! Callbacks only convert samples, push into a bounded queue and update flags.
pub mod calibration;
pub mod diagnostics;
pub mod impulse;
#[cfg(target_os = "linux")]
mod pipewire_capture;
pub mod sequence;
pub mod spectral;
pub mod timing;
pub mod waveform;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use rtrb::{Consumer, Producer, RingBuffer};
use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const MAX_SECONDS: u32 = 30;
pub const QUEUE_OVERFLOW: u32 = 1;
pub const STREAM_ERROR: u32 = 2;
pub const INVALID_SAMPLE: u32 = 4;
pub const TIMESTAMP_GAP: u32 = 8;
pub const LENGTH_LIMIT: u32 = 16;

#[derive(Debug, Clone, Serialize)]
pub struct InputDevice {
    /// Stable selector passed back to Capture::start; not a transient node ID.
    pub name: String,
    pub description: String,
    pub channel_names: Vec<String>,
    pub is_monitor: bool,
    pub configurations: Vec<InputConfiguration>,
}
impl InputDevice {
    pub fn label(&self) -> String {
        if self.is_monitor {
            format!("{} (sink monitor)", self.description)
        } else {
            self.description.clone()
        }
    }
    pub fn channel_label(&self, channel: u16) -> String {
        match channel
            .checked_sub(1)
            .and_then(|i| self.channel_names.get(usize::from(i)))
        {
            Some(name) => format!("{channel} · {name}"),
            None => format!("Channel {channel}"),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct InputConfiguration {
    pub channels: u16,
    pub min_sample_rate: u32,
    pub max_sample_rate: u32,
    pub sample_format: String,
}
pub fn devices() -> Result<Vec<InputDevice>> {
    #[cfg(target_os = "linux")]
    if let Ok(inputs) = pipewire_capture::devices() {
        return Ok(inputs);
    }
    let host = cpal::default_host();
    host.input_devices()?
        .map(|device| {
            Ok(InputDevice {
                name: device.name()?,
                description: device.name()?,
                channel_names: Vec::new(),
                is_monitor: false,
                configurations: device
                    .supported_input_configs()?
                    .map(|c| InputConfiguration {
                        channels: c.channels(),
                        min_sample_rate: c.min_sample_rate().0,
                        max_sample_rate: c.max_sample_rate().0,
                        sample_format: c.sample_format().to_string(),
                    })
                    .collect(),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureMetadata {
    pub host: String,
    pub device: String,
    /// One-based channel in the interface stream. The stored WAV is mono.
    pub input_channel: u16,
    pub stream_channels: u16,
    pub sample_rate: u32,
    pub sample_format: String,
    pub is_monitor: bool,
    pub channel_name: Option<String>,
    pub continuity_check: String,
    pub frames: usize,
    pub fault_flags: u32,
    pub peak_dbfs: Option<f64>,
    pub rms_dbfs: Option<f64>,
    pub clipped_samples: usize,
}

pub struct Capture {
    stream: CaptureStream,
    consumer: Consumer<f32>,
    faults: Arc<AtomicU32>,
    samples: Vec<f32>,
    metadata: CaptureMetadata,
    max_frames: usize,
}
enum CaptureStream {
    Cpal(cpal::Stream),
    #[cfg(target_os = "linux")]
    PipeWire(Box<pipewire_capture::Stream>),
}
pub struct Recording {
    pub metadata: CaptureMetadata,
    pub samples: Vec<f32>,
}

struct Sink {
    producer: Producer<f32>,
    faults: Arc<AtomicU32>,
    channels: usize,
    channel: usize,
    rate: u32,
    previous: Option<(cpal::StreamInstant, usize)>,
}
impl Sink {
    fn accept<T>(&mut self, data: &[T], timestamp: cpal::StreamInstant)
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        if !data.len().is_multiple_of(self.channels) {
            self.faults.fetch_or(STREAM_ERROR, Ordering::Relaxed);
            return;
        }
        let frames = data.len() / self.channels;
        if let Some((previous, previous_frames)) = self.previous {
            let expected = previous_frames as f64 / self.rate as f64;
            // Host timestamps are diagnostic, not the acoustic time reference.
            // A conservative 5 ms discontinuity threshold also catches stalls.
            if timestamp
                .duration_since(&previous)
                .is_none_or(|d| (d.as_secs_f64() - expected).abs() > 0.005)
            {
                self.faults.fetch_or(TIMESTAMP_GAP, Ordering::Relaxed);
            }
        }
        self.previous = Some((timestamp, frames));
        self.accept_samples(data);
    }
    fn accept_samples<T>(&mut self, data: &[T])
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        for frame in data.chunks_exact(self.channels) {
            let sample = f32::from_sample(frame[self.channel]);
            if !sample.is_finite() {
                self.faults.fetch_or(INVALID_SAMPLE, Ordering::Relaxed);
            }
            if self.producer.push(sample).is_err() {
                self.faults.fetch_or(QUEUE_OVERFLOW, Ordering::Relaxed);
            }
        }
    }
}
fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut sink: Sink,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let faults = sink.faults.clone();
    Ok(device.build_input_stream(
        config,
        move |data: &[T], info| sink.accept(data, info.timestamp().capture),
        move |_| {
            faults.fetch_or(STREAM_ERROR, Ordering::Relaxed);
        },
        None,
    )?)
}

impl Capture {
    /// Opening/starting input is explicit. Enumeration never records audio.
    pub fn start(device_name: &str, input_channel: u16, sample_rate: u32) -> Result<Self> {
        Self::start_bounded(
            device_name,
            input_channel,
            sample_rate,
            sample_rate as usize * MAX_SECONDS as usize,
        )
    }
    /// Continuous batches have a separate bounded mono capture budget.
    pub fn start_bounded(
        device_name: &str,
        input_channel: u16,
        sample_rate: u32,
        max_frames: usize,
    ) -> Result<Self> {
        if max_frames == 0
            || max_frames > 4_000_000.max(sample_rate as usize * MAX_SECONDS as usize)
        {
            return Err("capture exceeds the frame budget".into());
        }
        if input_channel == 0 || !(8_000..=192_000).contains(&sample_rate) {
            return Err("choose a one-based channel and a sample rate in 8000..=192000".into());
        }
        #[cfg(target_os = "linux")]
        if device_name.starts_with("pipewire:") {
            return pipewire_capture::start(device_name, input_channel, sample_rate, max_frames);
        }
        let host = cpal::default_host();
        let mut matches = host
            .input_devices()?
            .filter(|d| d.name().is_ok_and(|n| n == device_name));
        let device = matches
            .next()
            .ok_or("input device name not found; list devices again")?;
        if matches.next().is_some() {
            return Err("input device name is ambiguous; give the interface a unique name".into());
        }
        let config = device
            .supported_input_configs()?
            .filter(|c| {
                c.channels() >= input_channel
                    && c.channels() <= 64
                    && c.min_sample_rate().0 <= sample_rate
                    && c.max_sample_rate().0 >= sample_rate
                    && matches!(
                        c.sample_format(),
                        SampleFormat::F32 | SampleFormat::I16 | SampleFormat::I32
                    )
            })
            .min_by_key(|c| {
                (
                    c.channels(),
                    match c.sample_format() {
                        SampleFormat::F32 => 0,
                        SampleFormat::I32 => 1,
                        _ => 2,
                    },
                )
            })
            .ok_or("no supported f32/i32/i16 input configuration for this channel and rate")?
            .with_sample_rate(cpal::SampleRate(sample_rate));
        let (producer, consumer) = RingBuffer::new(sample_rate as usize * 4);
        let faults = Arc::new(AtomicU32::new(0));
        let sink = Sink {
            producer,
            faults: faults.clone(),
            channels: config.channels() as usize,
            channel: (input_channel - 1) as usize,
            rate: sample_rate,
            previous: None,
        };
        let stream = match config.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config.config(), sink)?,
            SampleFormat::I16 => build::<i16>(&device, &config.config(), sink)?,
            SampleFormat::I32 => build::<i32>(&device, &config.config(), sink)?,
            _ => return Err("unsupported input sample format".into()),
        };
        let metadata = CaptureMetadata {
            host: host.id().name().into(),
            device: device_name.into(),
            input_channel,
            stream_channels: config.channels(),
            sample_rate,
            sample_format: config.sample_format().to_string(),
            is_monitor: false,
            channel_name: None,
            continuity_check: "device timestamps".into(),
            frames: 0,
            fault_flags: 0,
            peak_dbfs: None,
            rms_dbfs: None,
            clipped_samples: 0,
        };
        let samples = Vec::with_capacity(max_frames);
        stream.play()?;
        Ok(Self {
            stream: CaptureStream::Cpal(stream),
            consumer,
            faults,
            samples,
            metadata,
            max_frames,
        })
    }
    /// Drain on the orchestration thread at least every 200 ms.
    pub fn poll(&mut self) {
        match &self.stream {
            CaptureStream::Cpal(stream) => {
                let _ = stream;
            }
            #[cfg(target_os = "linux")]
            CaptureStream::PipeWire(stream) => stream.poll(),
        }
        drain(
            &mut self.consumer,
            &mut self.samples,
            self.max_frames,
            &self.faults,
        );
    }
    pub fn frames(&self) -> usize {
        self.samples.len()
    }
    pub fn faults(&self) -> u32 {
        self.faults.load(Ordering::Relaxed)
    }
    pub fn finish(self) -> Recording {
        let Self {
            stream,
            mut consumer,
            faults,
            mut samples,
            mut metadata,
            max_frames,
        } = self;
        drop(stream);
        drain(&mut consumer, &mut samples, max_frames, &faults);
        let (peak, rms, clipped) = levels(&samples);
        metadata.frames = samples.len();
        metadata.fault_flags = faults.load(Ordering::Relaxed);
        metadata.peak_dbfs = peak;
        metadata.rms_dbfs = rms;
        metadata.clipped_samples = clipped;
        Recording { metadata, samples }
    }
}
fn drain(consumer: &mut Consumer<f32>, samples: &mut Vec<f32>, max: usize, faults: &AtomicU32) {
    // Snapshot the available count: the producer cannot prolong this loop.
    let count = consumer.slots();
    for _ in 0..count {
        if let Ok(sample) = consumer.pop() {
            if samples.len() < max {
                samples.push(sample);
            } else {
                faults.fetch_or(LENGTH_LIMIT, Ordering::Relaxed);
            }
        }
    }
}
pub fn levels(samples: &[f32]) -> (Option<f64>, Option<f64>, usize) {
    let mut peak = 0.0_f64;
    let mut energy = 0.0;
    let mut clipped = 0;
    for &sample in samples {
        if !sample.is_finite() {
            continue;
        }
        peak = peak.max(sample.abs() as f64);
        energy += (sample as f64).powi(2);
        if sample.abs() >= 0.999 {
            clipped += 1;
        }
    }
    let db = |v: f64| (v > 0.0).then(|| 20.0 * v.log10());
    (
        db(peak),
        db((energy / samples.len().max(1) as f64).sqrt()),
        clipped,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_channel_selection_and_overflow_are_explicit() {
        let (producer, mut consumer) = RingBuffer::new(2);
        let faults = Arc::new(AtomicU32::new(0));
        let mut sink = Sink {
            producer,
            faults: faults.clone(),
            channels: 2,
            channel: 1,
            rate: 48000,
            previous: None,
        };
        sink.accept_samples(&[1.0_f32, 0.25, 1.0, -0.5, 1.0, 0.0]);
        assert_eq!(faults.load(Ordering::Relaxed), QUEUE_OVERFLOW);
        let mut samples = Vec::with_capacity(1);
        drain(&mut consumer, &mut samples, 1, &faults);
        assert_eq!(samples, [0.25]);
        assert_eq!(
            faults.load(Ordering::Relaxed),
            LENGTH_LIMIT | QUEUE_OVERFLOW
        );
    }
    #[test]
    fn level_checks_distinguish_silence_clipping_and_invalid_samples() {
        assert_eq!(levels(&[0.0; 10]), (None, None, 0));
        let (peak, _, clipped) = levels(&[0.5, -1.0, f32::NAN]);
        assert_eq!(peak, Some(0.0));
        assert_eq!(clipped, 1);
    }
}
