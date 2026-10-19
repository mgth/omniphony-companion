//! Typed reasons a speaker receives no correction target, or a proposal is blocked.
//!
//! Each code has one technical `Display` (Lab, logs and the legacy text fields of saved
//! sessions) and one short `human()` sentence for the everyday view. Text written before
//! these codes existed, and acquisition messages that are still produced as text, are
//! classified once by [`ExclusionReason::from_message`]; nothing downstream parses text.
use serde::{Deserialize, Serialize};
use std::fmt;

/// Arrival change between the two passes above which a speaker is not repeatable.
pub const ARRIVAL_REPEAT_LIMIT_MS: f64 = 0.15;
/// Level change between the two passes above which a speaker is not repeatable.
pub const LEVEL_REPEAT_LIMIT_DB: f64 = 0.5;
/// Repeatable arrivals further than this from the group are set aside by default.
pub const FAR_ARRIVAL_MS: f64 = 20.0;
/// Clipping overdrive estimates saturate here (`measurement_capture::clipping`).
pub const MAX_OVERDRIVE_DB: f64 = 20.0;
/// Headroom added to a clipping overdrive in the suggested gain reduction.
pub const CLIPPING_MARGIN_DB: f64 = 3.0;

/// Which signal/noise floor was not met.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnrBand {
    /// Excited-band confidence of each sweep in a take.
    Acquisition,
    /// Integrated 500–2000 Hz level band used for the correction.
    Level,
}
impl SnrBand {
    pub fn minimum_db(self) -> f64 {
        match self {
            Self::Acquisition => 15.0,
            Self::Level => 20.0,
        }
    }
}

/// How a reason should be presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Nothing usable was captured: check the wiring or the gain.
    Severe,
    /// Captured, but not trusted for a correction.
    Warning,
    /// Repeatable, but left out of the correction by default; the user may include it.
    SetAside,
}

/// Why one speaker receives no correction target. Its gain and delay stay unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ExclusionReason {
    /// The input carried only digital silence.
    NoSignal,
    /// The reference/target/reference sweep sequence was not found in the recording.
    MissingTimingReferences,
    Clipped {
        /// How far the capture overshot full scale, in dB, saturating at
        /// [`MAX_OVERDRIVE_DB`]. Absent when unknown: older text, or no measurable
        /// plateau along the sweep.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overdrive_db: Option<f64>,
    },
    LowSnr {
        band: SnrBand,
        /// Absent when classified from older text that did not state the value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        db: Option<f64>,
    },
    /// The reference speaker's level changed within a take or between takes.
    UnstableReference,
    /// Arrival or level changed between the two passes (absolute differences).
    RepeatSpread {
        ms: f64,
        db: f64,
    },
    /// No single direct arrival could be resolved (echoes, band disagreement, ambiguity).
    NoClearArrival {
        detail: String,
    },
    /// Fewer than two complete captures of this speaker.
    NotMeasuredTwice,
    Cancelled,
    /// The recording itself failed or was incomplete.
    CaptureFailed {
        detail: String,
    },
    /// A saved take does not belong to this alignment.
    SessionMismatch {
        detail: String,
    },
    /// Repeatable, but its arrival is this far from the median of the repeatable speakers.
    FarArrival {
        ms: f64,
    },
    Other {
        detail: String,
    },
}

impl ExclusionReason {
    /// Classify free text from an older session or from an acquisition message.
    /// The first match wins, so causes that make the others meaningless come first.
    pub fn from_message(text: &str) -> Self {
        let text = without_error_prefix(text);
        let lower = text.to_ascii_lowercase();
        let has = |needle: &str| lower.contains(needle);
        let detail = || text.to_owned();
        if has("digital silence") || has("no microphone signal") {
            Self::NoSignal
        } else if has("timing references were not found")
            || has("no target sweep")
            || has("synchronized timing evidence")
        {
            Self::MissingTimingReferences
        } else if has("clip") {
            Self::Clipped {
                overdrive_db: number_before(&lower, " db over full scale")
                    .map(|db| db.clamp(0.0, MAX_OVERDRIVE_DB)),
            }
        } else if has("signal/noise") || has("weak target") {
            Self::LowSnr {
                band: if has("excited-band") {
                    SnrBand::Acquisition
                } else {
                    SnrBand::Level
                },
                db: number_after(&lower, "signal/noise is "),
            }
        } else if has("reference level") {
            Self::UnstableReference
        } else if has("arrival changed by") {
            Self::RepeatSpread {
                ms: number_after(&lower, "arrival changed by ").unwrap_or(f64::MAX),
                db: 0.0,
            }
        } else if has("level changed by") {
            Self::RepeatSpread {
                ms: 0.0,
                db: number_after(&lower, "level changed by ").unwrap_or(f64::MAX),
            }
        } else if has("cancel") {
            Self::Cancelled
        } else if has("two independent") || has("two qualified") {
            Self::NotMeasuredTwice
        } else if has("different renderer") || has("identity differs") {
            Self::SessionMismatch { detail: detail() }
        } else if has("arrival")
            || has("peak")
            || has("impulse")
            || has("waveform alignment")
            || has("clock ratio")
            || has("ambiguous")
        {
            Self::NoClearArrival { detail: detail() }
        } else if has("fault")
            || has("did not complete")
            || has("did not start")
            || has("too short")
            || has("truncated")
            || has("lead-in")
            || has("incomplete")
        {
            Self::CaptureFailed { detail: detail() }
        } else {
            Self::Other { detail: detail() }
        }
    }

    pub fn severity(&self) -> Severity {
        match self {
            Self::NoSignal | Self::MissingTimingReferences | Self::Clipped { .. } => {
                Severity::Severe
            }
            Self::FarArrival { .. } => Severity::SetAside,
            _ => Severity::Warning,
        }
    }
    pub fn is_set_aside(&self) -> bool {
        self.severity() == Severity::SetAside
    }

    /// One short sentence for the everyday view; `Display` keeps the technical detail.
    pub fn human(&self) -> String {
        match self {
            Self::NoSignal => "Microphone silent. Check the input and its power.".into(),
            Self::MissingTimingReferences => "No sound picked up. Wired and unmuted?".into(),
            Self::Clipped { overdrive_db: None } => {
                "Clipped. Lower the mic gain or test level.".into()
            }
            Self::Clipped {
                overdrive_db: Some(db),
            } => format!(
                "Clipped by {}. Lower the mic gain or test level by {}{:.0} dB.",
                if *db >= MAX_OVERDRIVE_DB {
                    format!("more than {MAX_OVERDRIVE_DB:.0} dB")
                } else {
                    format!("about {:.0} dB", db.max(1.0))
                },
                if *db >= MAX_OVERDRIVE_DB {
                    "at least "
                } else {
                    ""
                },
                (db + CLIPPING_MARGIN_DB).ceil()
            ),
            Self::LowSnr { .. } => "Too noisy to trust.".into(),
            Self::UnstableReference => {
                "Reference speaker level drifted. Keep the mic gain fixed.".into()
            }
            Self::RepeatSpread { ms, .. } if *ms > ARRIVAL_REPEAT_LIMIT_MS => {
                "Timing moved between runs. Keep still and measure again.".into()
            }
            Self::RepeatSpread { .. } => {
                "Level moved between runs. Keep still and measure again.".into()
            }
            Self::NoClearArrival { .. } => "No clear arrival. Echoes or an obstacle?".into(),
            Self::NotMeasuredTwice => "Not measured twice. Measure again.".into(),
            Self::Cancelled => "Measurement was stopped before the end.".into(),
            Self::CaptureFailed { .. } => "Recording failed. Measure again.".into(),
            Self::SessionMismatch { .. } => "Saved take does not match this session.".into(),
            Self::FarArrival { ms } => format!(
                "Set aside: {:.0} ms {} than the others.",
                ms.abs(),
                if *ms > 0.0 { "later" } else { "earlier" }
            ),
            Self::Other { .. } => "Not reliable enough to correct.".into(),
        }
    }
}

impl fmt::Display for ExclusionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSignal => write!(
                f,
                "no microphone signal: the input contains only digital silence"
            ),
            Self::MissingTimingReferences => {
                write!(f, "three reliable timing references were not found")
            }
            Self::Clipped { overdrive_db: None } => write!(f, "clipped recording"),
            Self::Clipped {
                overdrive_db: Some(db),
            } if *db >= MAX_OVERDRIVE_DB => write!(
                f,
                "clipped recording, more than {MAX_OVERDRIVE_DB:.0} dB over full scale"
            ),
            Self::Clipped {
                overdrive_db: Some(db),
            } => write!(f, "clipped recording, {db:.1} dB over full scale"),
            Self::LowSnr {
                band: SnrBand::Acquisition,
                db,
            } => match db {
                Some(db) => write!(
                    f,
                    "excited-band acquisition signal/noise is {db:.1} dB (minimum 15 dB); diagnostic result only"
                ),
                None => write!(
                    f,
                    "excited-band acquisition signal/noise below 15 dB; diagnostic result only"
                ),
            },
            Self::LowSnr {
                band: SnrBand::Level,
                db,
            } => match db {
                Some(db) => write!(
                    f,
                    "integrated level-band signal/noise is {db:.1} dB (minimum 20 dB for correction)"
                ),
                None => write!(
                    f,
                    "integrated level-band signal/noise below 20 dB (minimum for correction)"
                ),
            },
            Self::UnstableReference => write!(f, "reference level changed during or between takes"),
            Self::RepeatSpread { ms, db } => {
                let mut parts = Vec::new();
                if *ms > ARRIVAL_REPEAT_LIMIT_MS {
                    parts.push(format!(
                        "arrival changed by {ms:.3} ms between repeats (limit {ARRIVAL_REPEAT_LIMIT_MS:.3} ms)"
                    ));
                }
                if *db > LEVEL_REPEAT_LIMIT_DB {
                    parts.push(format!(
                        "level changed by {db:.2} dB between repeats (limit {LEVEL_REPEAT_LIMIT_DB:.2} dB)"
                    ));
                }
                if parts.is_empty() {
                    parts.push("estimates changed between repeats".into());
                }
                write!(f, "{}", parts.join("; "))
            }
            Self::NotMeasuredTwice => {
                write!(f, "two independent complete target captures are required")
            }
            Self::Cancelled => write!(f, "capture cancelled"),
            Self::FarArrival { ms } => write!(
                f,
                "repeatable arrival is {ms:+.1} ms from the median of the repeatable speakers (limit ±{FAR_ARRIVAL_MS:.0} ms); set aside, settings unchanged"
            ),
            Self::NoClearArrival { detail }
            | Self::CaptureFailed { detail }
            | Self::SessionMismatch { detail }
            | Self::Other { detail } => write!(f, "{detail}"),
        }
    }
}

/// Older sessions saved core errors with their (sometimes repeated) error prefix.
pub fn without_error_prefix(mut text: &str) -> &str {
    text = text.trim();
    while let Some(rest) = text.strip_prefix("invalid calibration data: ") {
        text = rest;
    }
    text
}

/// The first number following `prefix`, as written by the older text formats.
fn number_after(text: &str, prefix: &str) -> Option<f64> {
    let rest = &text[text.find(prefix)? + prefix.len()..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok().filter(|v: &f64| v.is_finite())
}

/// The number written just before `suffix`, as in "about 4 dB over full scale".
fn number_before(text: &str, suffix: &str) -> Option<f64> {
    let head = &text[..text.find(suffix)?];
    let start = head
        .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map_or(0, |i| i + 1);
    head[start..].parse().ok().filter(|v: &f64| v.is_finite())
}

/// Why no proposal can be made for the session as a whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Blocker {
    /// Fewer than two speakers are included in the correction.
    TooFewQualified {
        count: usize,
        /// Repeatable speakers set aside for a far arrival, which the user may include.
        #[serde(default)]
        set_aside: usize,
    },
    /// The shared reference level differs across the included speakers.
    ReferenceDrift,
    Cancelled,
    /// A pass did not return every selected speaker in order.
    IncompleteBatch,
    /// An offline reanalysis, which can never become a correction.
    Diagnostic,
    /// A pass failed; the text comes from the acquisition.
    Capture {
        detail: String,
    },
    /// The proposal policy refused the included subset.
    Proposal {
        detail: String,
    },
    /// A blocker saved as text before these codes existed.
    Other {
        detail: String,
    },
}

impl Blocker {
    /// Derived blockers are recomputed from the saved evidence whenever the proposal is
    /// updated; the others describe the acquisition and are kept.
    pub fn is_derived(&self) -> bool {
        matches!(
            self,
            Self::TooFewQualified { .. } | Self::ReferenceDrift | Self::Proposal { .. }
        )
    }

    /// One short sentence for the everyday view; `Display` keeps the technical detail.
    pub fn human(&self) -> String {
        match self {
            Self::TooFewQualified { set_aside, .. } if *set_aside > 0 => {
                "Fewer than two speakers can be aligned. Include a set-aside speaker to align to it."
                    .into()
            }
            Self::TooFewQualified { .. } => "Fewer than two speakers measured reliably.".into(),
            Self::ReferenceDrift => {
                "The reference speaker's level drifted. Keep the microphone and its gain fixed."
                    .into()
            }
            Self::Cancelled => "Measurement was stopped before the end.".into(),
            Self::IncompleteBatch => "Not every speaker was measured. Measure again.".into(),
            Self::Diagnostic => "Diagnostic reanalysis only. Measure again to correct.".into(),
            Self::Capture { detail } | Self::Proposal { detail } | Self::Other { detail } => {
                detail.clone()
            }
        }
    }
}

impl fmt::Display for Blocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewQualified {
                count,
                set_aside: 0,
            } => write!(
                f,
                "Only {count} speaker(s) qualify; relative correction requires at least two."
            ),
            Self::TooFewQualified { count, set_aside } => write!(
                f,
                "Only {count} speaker(s) can be aligned; relative correction requires at least two. {set_aside} repeatable speaker(s) are set aside for a far arrival; include one to align to it."
            ),
            Self::ReferenceDrift => write!(
                f,
                "Reference level changed across qualified speakers; keep microphone and input gain fixed."
            ),
            Self::Cancelled => write!(
                f,
                "Alignment cancelled; incomplete measurements cannot make a proposal."
            ),
            Self::IncompleteBatch => write!(
                f,
                "The batch did not return every selected speaker in order."
            ),
            Self::Diagnostic => write!(
                f,
                "Offline diagnostic reanalysis. Original reports are preserved; capture a fresh alignment before applying corrections."
            ),
            Self::Capture { detail } | Self::Proposal { detail } | Self::Other { detail } => {
                write!(f, "{detail}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_text_classifies_to_the_same_codes_the_ui_used_to_guess() {
        use ExclusionReason as R;
        for (text, expected) in [
            (
                "invalid calibration data: three reliable timing references were not found; missing synchronized timing evidence; missing impulse response",
                R::MissingTimingReferences,
            ),
            (
                "invalid calibration data: invalid calibration data: clipped recording",
                R::Clipped { overdrive_db: None },
            ),
            (
                "Clipping in the target speaker interval, about 4 dB over full scale: lower the input gain or test level by at least 7 dB and repeat.",
                R::Clipped {
                    overdrive_db: Some(4.0),
                },
            ),
            (
                "A reference interval clipped, more than 20 dB over full scale: lower the reference or input level by at least 23 dB and repeat.",
                R::Clipped {
                    overdrive_db: Some(MAX_OVERDRIVE_DB),
                },
            ),
            (
                "excited-band acquisition signal/noise below 15 dB; diagnostic result only",
                R::LowSnr {
                    band: SnrBand::Acquisition,
                    db: None,
                },
            ),
            (
                "integrated level-band signal/noise is 18.5 dB (minimum 20 dB for correction)",
                R::LowSnr {
                    band: SnrBand::Level,
                    db: Some(18.5),
                },
            ),
            (
                "arrival changed by 0.410 ms between repeats (limit 0.150 ms)",
                R::RepeatSpread { ms: 0.41, db: 0.0 },
            ),
            (
                "level changed by 0.62 dB between repeats (limit 0.50 dB)",
                R::RepeatSpread { ms: 0.0, db: 0.62 },
            ),
            (
                "reference level changed between repeats",
                R::UnstableReference,
            ),
            (
                "two independent complete target captures are required",
                R::NotMeasuredTwice,
            ),
            ("capture cancelled by user", R::Cancelled),
            (
                "No microphone signal received: the selected input contains only digital silence.",
                R::NoSignal,
            ),
            (
                "capture fault flags: 2",
                R::CaptureFailed {
                    detail: "capture fault flags: 2".into(),
                },
            ),
            (
                "something new",
                R::Other {
                    detail: "something new".into(),
                },
            ),
        ] {
            assert_eq!(R::from_message(text), expected, "{text}");
        }
        let echo = R::from_message(
            "speaker: early peaks disagree across frequency bands or detection thresholds",
        );
        assert_eq!(
            echo.human(),
            "No clear arrival. Echoes or an obstacle?",
            "{echo:?}"
        );
    }

    #[test]
    fn display_is_the_technical_text_and_reclassifies_to_itself() {
        use ExclusionReason as R;
        for code in [
            R::NoSignal,
            R::MissingTimingReferences,
            R::Clipped { overdrive_db: None },
            R::Clipped {
                overdrive_db: Some(4.2),
            },
            R::Clipped {
                overdrive_db: Some(MAX_OVERDRIVE_DB),
            },
            R::LowSnr {
                band: SnrBand::Acquisition,
                db: Some(12.3),
            },
            R::LowSnr {
                band: SnrBand::Level,
                db: Some(18.5),
            },
            R::UnstableReference,
            R::RepeatSpread { ms: 0.41, db: 0.0 },
            R::RepeatSpread { ms: 0.0, db: 0.62 },
            R::NotMeasuredTwice,
            R::Cancelled,
        ] {
            let text = code.to_string();
            let back = R::from_message(&text);
            match (&code, &back) {
                (R::LowSnr { band, db }, R::LowSnr { band: b, db: d }) => {
                    assert_eq!(band, b);
                    assert!((db.unwrap() - d.unwrap()).abs() < 0.05, "{text}");
                }
                _ => assert_eq!(back, code, "{text}"),
            }
        }
        let far = R::FarArrival { ms: 176.9 };
        assert_eq!(far.severity(), Severity::SetAside);
        assert_eq!(far.human(), "Set aside: 177 ms later than the others.");
        assert!(far.to_string().contains("+176.9 ms"));
        let clipped = R::Clipped {
            overdrive_db: Some(4.2),
        };
        assert_eq!(clipped.severity(), Severity::Severe);
        assert_eq!(
            clipped.human(),
            "Clipped by about 4 dB. Lower the mic gain or test level by 8 dB."
        );
        assert_eq!(
            R::Clipped {
                overdrive_db: Some(MAX_OVERDRIVE_DB)
            }
            .human(),
            "Clipped by more than 20 dB. Lower the mic gain or test level by at least 23 dB."
        );
        assert_eq!(
            R::Clipped { overdrive_db: None }.human(),
            "Clipped. Lower the mic gain or test level."
        );
    }

    #[test]
    fn codes_round_trip_through_json_with_a_stable_tag() {
        let codes = vec![
            ExclusionReason::FarArrival { ms: 176.9 },
            ExclusionReason::LowSnr {
                band: SnrBand::Level,
                db: None,
            },
            ExclusionReason::NoClearArrival {
                detail: "arrival unresolved".into(),
            },
            ExclusionReason::Clipped { overdrive_db: None },
            ExclusionReason::Clipped {
                overdrive_db: Some(4.2),
            },
        ];
        let json = serde_json::to_string(&codes).unwrap();
        // Unknown overdrive keeps the tag written before the value existed.
        assert!(json.contains(r#"{"code":"clipped"},"#), "{json}");
        assert!(
            json.contains(r#"{"code":"clipped","overdrive_db":4.2}"#),
            "{json}"
        );
        assert!(
            json.contains(r#"{"code":"far_arrival","ms":176.9}"#),
            "{json}"
        );
        assert!(
            json.contains(r#"{"code":"low_snr","band":"level"}"#),
            "{json}"
        );
        assert_eq!(
            serde_json::from_str::<Vec<ExclusionReason>>(&json).unwrap(),
            codes
        );
        let blockers = vec![
            Blocker::TooFewQualified {
                count: 1,
                set_aside: 2,
            },
            Blocker::Capture {
                detail: "input stream did not start cleanly".into(),
            },
        ];
        let json = serde_json::to_string(&blockers).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<Blocker>>(&json).unwrap(),
            blockers
        );
        assert!(blockers[0].is_derived() && !blockers[1].is_derived());
        assert_eq!(
            Blocker::TooFewQualified {
                count: 1,
                set_aside: 0
            }
            .to_string(),
            "Only 1 speaker(s) qualify; relative correction requires at least two."
        );
    }
}
