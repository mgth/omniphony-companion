//! Acoustic distances for the room map: how much farther each measured speaker sounds
//! than the timing reference, from its arrival minus its own output delay. Display
//! only: each output path's electronic latency is included, nothing is sent to the
//! renderer and nothing here is calibration evidence.
use crate::alignment::Alignment;
use calibration_core::{reason::FAR_ARRIVAL_MS, repeatability::group_center};

/// Speed of sound at about 20 °C.
pub const SPEED_OF_SOUND_M_PER_MS: f64 = 0.343;
/// The nearest a speaker is ever placed: an anchor that would put one closer is raised.
pub const MIN_DISTANCE_M: f64 = 0.1;
/// Measured and layout distances further apart than this are both drawn.
pub const LAYOUT_MISMATCH_M: f64 = 0.3;

/// One measured output's distance, relative to the timing reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Acoustic {
    /// Metres farther than the reference (negative: nearer).
    Relative(f64),
    /// Its delay-free arrival is more than [`FAR_ARRIVAL_MS`] from the group: another
    /// output device or path, whose latency hides the distance.
    OtherDevice,
}

/// Relative acoustic distances of an alignment's repeatable speakers.
#[derive(Debug, Clone, PartialEq)]
pub struct Distances {
    pub reference_id: String,
    /// In alignment row order: every qualified row whose delay is known.
    pub outputs: Vec<(String, Acoustic)>,
}

impl Distances {
    /// `Δd = c·[(arrival − delay) − (arrival_ref − delay_ref)]`, where arrivals include
    /// the delays of the renderer state the takes were captured under. Retakes only
    /// lower gains, so a row whose delay differs in any recorded retake state is left
    /// out rather than guessed. None without a reference or a usable row.
    pub fn from_alignment(a: &Alignment) -> Option<Self> {
        let reference_id = a.settings.reference_output.clone()?;
        let delay = |id: &str| -> Option<f64> {
            let delay = a.snapshot.outputs.iter().find(|o| o.id == id)?.delay_ms;
            a.retake_snapshots
                .iter()
                .all(|s| {
                    s.outputs
                        .iter()
                        .find(|o| o.id == id)
                        .is_none_or(|o| (o.delay_ms - delay).abs() < 1e-3)
                })
                .then_some(delay as f64)
                .filter(|d| d.is_finite())
        };
        let path = |id: &str, arrival: f64| Some(arrival - delay(id)?);
        let arrival = |id: &str| {
            a.rows
                .iter()
                .find(|r| r.output_id == id)
                .and_then(|r| r.qualified.as_ref())
                .map(|q| q.arrival_ms)
                .filter(|v| v.is_finite())
        };
        // Arrivals are measured against the reference's own sweeps: zero when its row
        // did not qualify.
        let reference = path(&reference_id, arrival(&reference_id).unwrap_or(0.0))?;
        let relative: Vec<(String, f64)> = a
            .rows
            .iter()
            .filter_map(|r| {
                let ms = path(&r.output_id, arrival(&r.output_id)?)? - reference;
                Some((r.output_id.clone(), ms))
            })
            .collect();
        let mut sorted: Vec<f64> = relative.iter().map(|(_, ms)| *ms).collect();
        sorted.sort_by(f64::total_cmp);
        let center = group_center(&sorted)?;
        Some(Self {
            reference_id,
            outputs: relative
                .into_iter()
                .map(|(id, ms)| {
                    let acoustic = if (ms - center).abs() > FAR_ARRIVAL_MS {
                        Acoustic::OtherDevice
                    } else {
                        Acoustic::Relative(ms * SPEED_OF_SOUND_M_PER_MS)
                    };
                    (id, acoustic)
                })
                .collect(),
        })
    }

    fn relative(&self) -> impl Iterator<Item = (&str, f64)> {
        self.outputs.iter().filter_map(|(id, a)| match a {
            Acoustic::Relative(m) => Some((id.as_str(), *m)),
            Acoustic::OtherDevice => None,
        })
    }

    /// Whether the reference belongs to the group of measured speakers; when it is
    /// another device, a distance entered for it means nothing.
    pub fn reference_in_group(&self) -> bool {
        !self
            .outputs
            .iter()
            .any(|(id, a)| *id == self.reference_id && *a == Acoustic::OtherDevice)
    }

    /// The shortest reference distance that keeps every speaker of the group at least
    /// [`MIN_DISTANCE_M`] from the listener.
    pub fn min_reference_m(&self) -> f64 {
        let nearest = self.relative().map(|(_, m)| m).fold(0.0, f64::min);
        MIN_DISTANCE_M - nearest
    }

    /// The reference speaker's distance: `reference_m` when the user entered one, else
    /// the least-squares fit of the measured distances to the layout's, which is the
    /// mean of `layout − Δd`. Raised to [`Distances::min_reference_m`]. None when there
    /// is neither, or nothing measured to anchor.
    pub fn anchor(
        &self,
        layout_m: impl Fn(&str) -> Option<f64>,
        reference_m: Option<f64>,
    ) -> Option<f64> {
        let user = reference_m
            .filter(|m| m.is_finite() && *m > 0.0)
            .filter(|_| self.reference_in_group());
        let fitted = || {
            let offsets: Vec<f64> = self
                .relative()
                .filter_map(|(id, m)| Some(layout_m(id).filter(|l| l.is_finite())? - m))
                .collect();
            (!offsets.is_empty()).then(|| offsets.iter().sum::<f64>() / offsets.len() as f64)
        };
        let anchor = user.or_else(fitted)?;
        self.relative()
            .next()
            .map(|_| anchor.max(self.min_reference_m()))
    }

    /// Absolute distances once anchored, `None` for another device; outputs that were
    /// not measured are absent.
    pub fn absolute(&self, anchor_m: f64) -> impl Iterator<Item = (&str, Option<f64>)> {
        self.outputs.iter().map(move |(id, a)| {
            (
                id.as_str(),
                match a {
                    Acoustic::Relative(m) => Some(anchor_m + m),
                    Acoustic::OtherDevice => None,
                },
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bench of 2026-10-20 after a first alignment: arrivals include the applied
    /// delays; two height speakers sit on another output device, 143 ms late.
    fn bench() -> Alignment {
        let outputs = [
            ("FL", 1.469, Some(0.017), 2.236),
            ("FR", 0.174, Some(0.012), 2.236),
            ("C", 0.216, Some(0.043), 2.0),
            ("BC", 7.670, Some(0.040), 1.003),
            ("BR", 6.971, Some(0.048), 1.414),
            ("TBL", 5.958, Some(0.038), 1.732),
            ("TBR", 5.541, Some(0.028), 1.732),
            ("SR", 0.0, None, 1.414),
            ("spk-12", 0.0, Some(143.391), 1.111),
            ("spk-13", 0.0, Some(143.322), 1.140),
        ];
        let json = serde_json::json!({
            "schema_version": 1,
            "endpoint": "127.0.0.1:9000",
            "settings": { "reference_output": "FL" },
            "snapshot": {
                "instance_id": "renderer", "revision": "r1", "sample_rate": 48000,
                "master_gain_db": 0.0, "max_delay_ms": 200.0, "unavailable_reason": null,
                "outputs": outputs.iter().enumerate().map(|(i, (id, delay, _, distance))| serde_json::json!({
                    "id": id, "index": i, "name": id, "gain_db": -12.0, "delay_ms": delay,
                    "muted": false, "freq_low_hz": null, "freq_high_hz": null,
                    "distance_m": distance,
                })).collect::<Vec<_>>(),
            },
            "rows": outputs.iter().map(|(id, _, arrival, _)| serde_json::json!({
                "output_id": id, "take_folders": [], "repeats": [],
                "qualified": arrival.map(|a| serde_json::json!({
                    "arrival_ms": a, "level_db": -20.0, "minimum_snr_db": 30.0,
                    "arrival_repeat_spread_ms": 0.01, "level_repeat_spread_db": 0.01,
                })),
                "reason": arrival.is_none().then_some("not measured twice"),
                "include_anyway": id.starts_with("spk"),
            })).collect::<Vec<_>>(),
            "blockers": [], "session": null, "profile": null,
            "applied_snapshot": null, "restored_snapshot": null,
            "verification_note": null, "verification": null,
            "application_uncertain": false
        });
        serde_json::from_value(json).unwrap()
    }

    fn layout(a: &Alignment) -> impl Fn(&str) -> Option<f64> + '_ {
        |id| {
            a.snapshot
                .outputs
                .iter()
                .find(|o| o.id == id)
                .and_then(|o| o.distance_m)
                .map(f64::from)
        }
    }

    fn get(d: &Distances, anchor: f64, id: &str) -> Option<f64> {
        d.absolute(anchor).find(|(o, _)| *o == id).unwrap().1
    }

    #[test]
    fn arrival_minus_delay_gives_distance_relative_to_the_reference() {
        let a = bench();
        let d = Distances::from_alignment(&a).unwrap();
        assert_eq!(d.reference_id, "FL");
        let rel = |id: &str| match d.outputs.iter().find(|(o, _)| o == id).unwrap().1 {
            Acoustic::Relative(m) => m,
            Acoustic::OtherDevice => panic!("{id} is another device"),
        };
        assert_eq!(rel("FL"), 0.0);
        // FR arrives as early but with 1.3 ms less delay: 0.44 m farther.
        assert!((rel("FR") - 0.343 * (0.012 - 0.174 - 0.017 + 1.469)).abs() < 1e-6);
        assert!((rel("BC") + 2.113).abs() < 0.01);
        // Not measured twice: absent rather than guessed.
        assert!(d.outputs.iter().all(|(o, _)| o != "SR"));
        // Included anyway, but 143 ms late: another device, whatever the user chose.
        for id in ["spk-12", "spk-13"] {
            assert!(
                d.outputs.contains(&(id.into(), Acoustic::OtherDevice)),
                "{id}"
            );
        }
    }

    #[test]
    fn the_default_anchor_fits_the_layout_and_a_user_distance_overrides_it() {
        let a = bench();
        let d = Distances::from_alignment(&a).unwrap();
        let fitted = d.anchor(layout(&a), None).unwrap();
        // The mean of layout − Δd over the seven measured speakers.
        assert!((fitted - 2.62).abs() < 0.01, "{fitted}");
        assert!((get(&d, fitted, "FR").unwrap() - 3.07).abs() < 0.01);
        assert!((get(&d, fitted, "BC").unwrap() - 0.51).abs() < 0.01);
        assert_eq!(get(&d, fitted, "spk-12"), None);
        // The user's tape measure wins.
        assert_eq!(d.anchor(layout(&a), Some(3.0)), Some(3.0));
        // A reference so short that BC would sit behind the listener is raised.
        assert!((d.min_reference_m() - (MIN_DISTANCE_M + 2.113)).abs() < 0.01);
        let raised = d.anchor(layout(&a), Some(1.0)).unwrap();
        assert!((get(&d, raised, "BC").unwrap() - MIN_DISTANCE_M).abs() < 1e-9);
        // Nothing stated and nothing entered: no anchor.
        assert_eq!(d.anchor(|_| None, None), None);
        assert_eq!(d.anchor(|_| None, Some(2.5)), Some(2.5));
    }

    #[test]
    fn a_delay_changed_by_a_recorded_retake_state_drops_that_speaker() {
        let mut a = bench();
        let mut retake = a.snapshot.clone();
        retake.revision = "r2".into();
        retake.outputs[1].gain_db = -20.0;
        a.retake_snapshots = vec![retake.clone()];
        let d = Distances::from_alignment(&a).unwrap();
        assert!(d.outputs.iter().any(|(o, _)| o == "FR"));
        retake.outputs[1].delay_ms += 1.0;
        a.retake_snapshots = vec![retake];
        let d = Distances::from_alignment(&a).unwrap();
        assert!(d.outputs.iter().all(|(o, _)| o != "FR"));
    }

    #[test]
    fn a_reference_on_another_device_ignores_a_user_distance() {
        let mut a = bench();
        a.settings.reference_output = Some("spk-12".into());
        let d = Distances::from_alignment(&a).unwrap();
        assert!(!d.reference_in_group());
        assert!(
            d.outputs
                .contains(&("spk-12".into(), Acoustic::OtherDevice))
        );
        assert!(!d.outputs.contains(&("FL".into(), Acoustic::OtherDevice)));
        let fitted = d.anchor(layout(&a), None).unwrap();
        assert_eq!(d.anchor(layout(&a), Some(9.0)), Some(fitted));
        // No reference at all: nothing to measure against.
        a.settings.reference_output = None;
        assert_eq!(Distances::from_alignment(&a), None);
    }
}
