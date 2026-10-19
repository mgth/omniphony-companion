//! First-run choices for the setup rail. A saved choice is never replaced: each default
//! applies only to a value that was never chosen (no device, no reference, an empty name).
use crate::controller::Settings;
use measurement_capture::{InputConfiguration, InputDevice};
use omniphony_adapter::{Snapshot, Sweep, protocol::Output};

pub const MICROPHONE_ID: &str = "mic-1";
pub const POSITION_ID: &str = "main-seat";
/// Chosen when the interface supports it: the usual renderer and interface rate.
pub const PREFERRED_SAMPLE_RATE: u32 = 48_000;

/// Which setup values were chosen automatically in this session, so the UI can say so.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Applied {
    pub device: bool,
    pub sample_rate: bool,
    pub reference: bool,
    pub microphone_id: bool,
    pub position_id: bool,
}

pub(crate) fn supported_format(format: &str) -> bool {
    matches!(format, "f32" | "i16" | "i32")
}
fn usable(c: &InputConfiguration) -> bool {
    supported_format(&c.sample_format)
        && (1..=64).contains(&c.channels)
        && c.min_sample_rate <= c.max_sample_rate
        && c.min_sample_rate <= 192_000
        && c.max_sample_rate >= 8_000
}

/// The device's preferred rate among its usable configurations: 48 kHz when supported,
/// otherwise the supported rate closest to it. `None` when nothing is usable.
pub fn preferred_rate(device: &InputDevice) -> Option<u32> {
    device
        .configurations
        .iter()
        .filter(|c| usable(c))
        .map(|c| {
            PREFERRED_SAMPLE_RATE
                .clamp(c.min_sample_rate.max(8_000), c.max_sample_rate.min(192_000))
        })
        .min_by_key(|rate| rate.abs_diff(PREFERRED_SAMPLE_RATE))
}

/// The first physical input (not a sink monitor) with a usable configuration, and the
/// rate to use with it.
pub fn input(inputs: &[InputDevice]) -> Option<(&InputDevice, u32)> {
    inputs
        .iter()
        .filter(|d| !d.is_monitor)
        .find_map(|d| preferred_rate(d).map(|rate| (d, rate)))
}

/// The centre speaker if there is one, else the front left, else the first speaker that
/// can play the sweep. Muted speakers and speakers that cannot play the sweep are skipped.
pub fn reference(snapshot: &Snapshot, sweep: Sweep) -> Option<String> {
    let playable = |o: &&Output| {
        !o.muted
            && sweep.start_hz >= o.freq_low_hz.unwrap_or(20.0)
            && sweep.end_hz <= o.freq_high_hz.unwrap_or(f32::MAX)
    };
    let named = |names: &[&str]| {
        snapshot.outputs.iter().filter(playable).find(|o| {
            names
                .iter()
                .any(|n| o.id.eq_ignore_ascii_case(n) || o.name.eq_ignore_ascii_case(n))
        })
    };
    named(&[
        "C",
        "Center",
        "Centre",
        "FC",
        "Front center",
        "Front centre",
    ])
    .or_else(|| named(&["FL", "Front left", "L", "Left"]))
    .or_else(|| snapshot.outputs.iter().find(playable))
    .map(|o| o.id.clone())
}

/// Fill empty microphone and position names; a typed name is kept.
pub fn identities(settings: &mut Settings, applied: &mut Applied) {
    if settings.microphone_id.trim().is_empty() {
        settings.microphone_id = MICROPHONE_ID.into();
        applied.microphone_id = true;
    }
    if settings.position_id.trim().is_empty() {
        settings.position_id = POSITION_ID.into();
        applied.position_id = true;
    }
}

/// Choose an input, its rate and the first channel when no device was ever chosen.
pub fn input_settings(settings: &mut Settings, inputs: &[InputDevice], applied: &mut Applied) {
    if settings.device.is_some() {
        return;
    }
    if let Some((device, rate)) = input(inputs) {
        settings.device = Some(device.name.clone());
        settings.sample_rate = rate;
        settings.input_channel = 1;
        applied.device = true;
        applied.sample_rate = true;
    }
}

/// Choose a timing reference when none was ever chosen.
pub fn reference_settings(settings: &mut Settings, snapshot: &Snapshot, applied: &mut Applied) {
    if settings.reference_output.is_none() {
        settings.reference_output = reference(snapshot, settings.sweep);
        applied.reference = settings.reference_output.is_some();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, monitor: bool, configurations: &[(u16, u32, u32, &str)]) -> InputDevice {
        InputDevice {
            name: name.into(),
            description: name.into(),
            channel_names: vec![],
            is_monitor: monitor,
            configurations: configurations
                .iter()
                .map(|&(channels, min, max, format)| InputConfiguration {
                    channels,
                    min_sample_rate: min,
                    max_sample_rate: max,
                    sample_format: format.into(),
                })
                .collect(),
        }
    }
    fn snapshot(outputs: &[(&str, &str, bool)]) -> Snapshot {
        Snapshot {
            instance_id: "defaults".into(),
            revision: "defaults:1".into(),
            sample_rate: 48000,
            master_gain_db: 0.0,
            max_delay_ms: 100.0,
            unavailable_reason: None,
            outputs: outputs
                .iter()
                .enumerate()
                .map(|(index, &(id, name, muted))| Output {
                    id: id.into(),
                    name: name.into(),
                    index: index as u32,
                    gain_db: 0.0,
                    delay_ms: 0.0,
                    muted,
                    freq_low_hz: None,
                    freq_high_hz: None,
                })
                .collect(),
        }
    }

    #[test]
    fn the_first_usable_physical_input_is_chosen_at_its_preferred_rate() {
        let inputs = [
            device("monitor", true, &[(2, 8000, 192000, "f32")]),
            device("no formats", false, &[(2, 8000, 192000, "u8")]),
            device("no configurations", false, &[]),
            device("usb mic", false, &[(1, 44100, 44100, "i16")]),
            device("interface", false, &[(2, 8000, 192000, "f32")]),
        ];
        let (chosen, rate) = input(&inputs).unwrap();
        assert_eq!((chosen.name.as_str(), rate), ("usb mic", 44100));
        assert_eq!(preferred_rate(&inputs[4]), Some(48000));
        assert_eq!(
            preferred_rate(&device("hi-res", false, &[(2, 88200, 192000, "i32")])),
            Some(88200)
        );
        assert!(input(&inputs[..3]).is_none());
    }

    #[test]
    fn the_reference_prefers_the_centre_then_the_front_left() {
        let sweep = Settings::default().sweep;
        let room = snapshot(&[
            ("spk-1", "Front left", false),
            ("spk-2", "Front right", false),
            ("spk-3", "Center", false),
        ]);
        assert_eq!(reference(&room, sweep).as_deref(), Some("spk-3"));
        let muted_centre = snapshot(&[("FR", "FR", false), ("C", "C", true), ("FL", "FL", false)]);
        assert_eq!(reference(&muted_centre, sweep).as_deref(), Some("FL"));
        let neither = snapshot(&[("Sub", "Sub", true), ("TFL", "TFL", false)]);
        assert_eq!(reference(&neither, sweep).as_deref(), Some("TFL"));
        assert_eq!(reference(&snapshot(&[]), sweep), None);
    }

    #[test]
    fn saved_choices_are_never_replaced() {
        let inputs = [device("interface", false, &[(2, 8000, 192000, "f32")])];
        let room = snapshot(&[("FL", "FL", false), ("C", "C", false)]);
        let saved = Settings {
            device: Some("unplugged interface".into()),
            input_channel: 2,
            sample_rate: 96000,
            reference_output: Some("FL".into()),
            microphone_id: "umik".into(),
            position_id: "sofa".into(),
            ..Settings::default()
        };
        let mut settings = saved.clone();
        let mut applied = Applied::default();
        input_settings(&mut settings, &inputs, &mut applied);
        reference_settings(&mut settings, &room, &mut applied);
        identities(&mut settings, &mut applied);
        assert_eq!(settings, saved);
        assert_eq!(applied, Applied::default());

        let mut fresh = Settings {
            device: None,
            input_channel: 3,
            sample_rate: 44100,
            reference_output: None,
            microphone_id: String::new(),
            position_id: "  ".into(),
            ..Settings::default()
        };
        input_settings(&mut fresh, &inputs, &mut applied);
        reference_settings(&mut fresh, &room, &mut applied);
        identities(&mut fresh, &mut applied);
        assert_eq!(fresh.device.as_deref(), Some("interface"));
        assert_eq!((fresh.input_channel, fresh.sample_rate), (1, 48000));
        assert_eq!(fresh.reference_output.as_deref(), Some("C"));
        assert_eq!(
            (fresh.microphone_id.as_str(), fresh.position_id.as_str()),
            ("mic-1", "main-seat")
        );
        assert_eq!(
            applied,
            Applied {
                device: true,
                sample_rate: true,
                reference: true,
                microphone_id: true,
                position_id: true,
            }
        );
    }
}
