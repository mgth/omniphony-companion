//! Alignment presentation: a timing/level chart, one action bar and Lab-only evidence.
//! Everything here reads `Alignment` as produced by companion-service; nothing writes it.
use companion_service::{
    alignment::{Alignment, Row},
    quality::Grade,
};
use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2,
    text::{LayoutJob, TextFormat, TextWrapping},
};

/// Arrivals further than this from the reference are drawn in a separate bracket.
const FAR_MS: f64 = 20.0;
/// The knob hint appears when the quietest reliable speaker trails all others by more.
const KNOB_HINT_DB: f64 = 3.0;
const ROW_HEIGHT: f32 = 22.0;
const AXIS_TOP: f32 = 18.0;
const AXIS_BOTTOM: f32 = 18.0;
const FAR: Color32 = Color32::from_rgb(157, 171, 255);

/// A short, human explanation of a raw exclusion reason. The raw text stays in tooltips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reason<'a> {
    pub text: &'static str,
    /// Nothing usable was captured (red) rather than captured but not trusted (amber).
    pub severe: bool,
    pub raw: &'a str,
}
impl Reason<'_> {
    pub fn color(&self) -> Color32 {
        if self.severe { super::RED } else { super::WARM }
    }
}

/// Map the service and core reason strings to one short sentence. The first match wins,
/// so causes that make the others meaningless (no sound, clipping) are checked first.
pub fn human_reason(raw: &str) -> Reason<'_> {
    let raw = readable_reason(raw);
    let lower = raw.to_ascii_lowercase();
    let has = |needle: &str| lower.contains(needle);
    let (text, severe) = if has("timing references were not found")
        || has("no target sweep")
        || has("no microphone signal")
        || has("digital silence")
    {
        ("No sound picked up. Wired and unmuted?", true)
    } else if has("clip") {
        ("Clipped. Lower the mic gain or test level.", true)
    } else if has("signal/noise") || has("weak target") {
        ("Too noisy to trust.", false)
    } else if has("reference level") {
        (
            "Reference speaker level drifted. Keep the mic gain fixed.",
            false,
        )
    } else if has("arrival changed by") {
        (
            "Timing moved between runs. Keep still and measure again.",
            false,
        )
    } else if has("level changed by") {
        (
            "Level moved between runs. Keep still and measure again.",
            false,
        )
    } else if has("cancel") {
        ("Measurement was stopped before the end.", false)
    } else if has("two independent") || has("two qualified") || has("did not complete") {
        ("Not measured twice. Measure again.", false)
    } else if has("arrival")
        || has("peak")
        || has("impulse shapes")
        || has("waveform alignment")
        || has("clock ratio")
        || has("ambiguous")
    {
        ("No clear arrival. Echoes or an obstacle?", false)
    } else if has("different renderer") || has("identity differs") {
        ("Saved take does not match this session.", false)
    } else {
        ("Not reliable enough to correct.", false)
    };
    Reason { text, severe, raw }
}

pub fn readable_reason(mut reason: &str) -> &str {
    while let Some(trimmed) = reason.strip_prefix("invalid calibration data: ") {
        reason = trimmed;
    }
    reason
}

/// Before/target absolute settings of one corrected speaker.
#[derive(Debug, Clone, Copy)]
pub struct Change {
    pub before_gain_db: f64,
    pub before_delay_ms: f64,
    pub target_gain_db: f64,
    pub target_delay_ms: f64,
}
impl Change {
    pub fn gain_db(&self) -> f64 {
        self.target_gain_db - self.before_gain_db
    }
    pub fn delay_ms(&self) -> f64 {
        self.target_delay_ms - self.before_delay_ms
    }
}

pub struct Speaker<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub row: &'a Row,
    /// Weakest grade across this speaker's passes.
    pub grade: Option<Grade>,
    /// Repeatable arrival (ms vs the reference) and spread between runs.
    pub arrival: Option<(f64, f64)>,
    /// Repeatable level relative to the quietest reliable speaker, and spread between runs.
    pub level: Option<(f64, f64)>,
    /// Diagnostic arrival of an excluded speaker; never used for correction.
    pub diagnostic_arrival_ms: Option<f64>,
    pub change: Option<Change>,
    pub reason: Option<Reason<'a>>,
}

/// One frame's view of an alignment, shared by the chart, the cards and Lab.
pub struct Summary<'a> {
    pub alignment: &'a Alignment,
    pub speakers: Vec<Speaker<'a>>,
    /// Index of the quietest reliable speaker and how far all others are above it.
    pub quietest: Option<(usize, f64)>,
}

impl<'a> Summary<'a> {
    pub fn new(a: &'a Alignment) -> Self {
        let profile = a.profile.as_ref();
        let quietest_db = a
            .rows
            .iter()
            .filter_map(|r| r.qualified.as_ref().map(|q| q.level_db))
            .fold(f64::INFINITY, f64::min);
        let speakers: Vec<_> = a
            .rows
            .iter()
            .map(|row| {
                let id = row.output_id.as_str();
                let name = a
                    .snapshot
                    .outputs
                    .iter()
                    .find(|o| o.id == id)
                    .map_or(id, |o| o.name.as_str());
                let change = profile.and_then(|p| {
                    let target = p.target_snapshot.speakers.iter().find(|s| s.id == id)?;
                    let (gain, delay) = p
                        .base_snapshot
                        .speakers
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| (s.settings.gain_db, s.settings.delay_ms))
                        .or_else(|| {
                            a.snapshot
                                .outputs
                                .iter()
                                .find(|o| o.id == id)
                                .map(|o| (f64::from(o.gain_db), f64::from(o.delay_ms)))
                        })?;
                    Some(Change {
                        before_gain_db: gain,
                        before_delay_ms: delay,
                        target_gain_db: target.settings.gain_db,
                        target_delay_ms: target.settings.delay_ms,
                    })
                });
                Speaker {
                    id,
                    name,
                    row,
                    grade: row.quality.iter().map(|q| q.grade()).min(),
                    arrival: row
                        .qualified
                        .as_ref()
                        .map(|q| (q.arrival_ms, q.arrival_repeat_spread_ms)),
                    level: row
                        .qualified
                        .as_ref()
                        .map(|q| (q.level_db - quietest_db, q.level_repeat_spread_db)),
                    diagnostic_arrival_ms: row
                        .qualified
                        .is_none()
                        .then(|| row.repeats.iter().rev().find_map(|r| r.arrival_ms))
                        .flatten()
                        .filter(|v| v.is_finite()),
                    change,
                    reason: row.qualified.is_none().then(|| {
                        human_reason(
                            row.reason
                                .as_deref()
                                .unwrap_or("Two qualified independent captures are required."),
                        )
                    }),
                }
            })
            .collect();
        let mut levels: Vec<_> = speakers
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.level.map(|(db, _)| (i, db)))
            .collect();
        levels.sort_by(|a, b| a.1.total_cmp(&b.1));
        let quietest = match levels.as_slice() {
            [(i, low), (_, next), ..] => Some((*i, next - low)),
            [(i, _)] => Some((*i, 0.0)),
            [] => None,
        };
        Self {
            alignment: a,
            speakers,
            quietest,
        }
    }
    pub fn speaker(&self, id: &str) -> Option<&Speaker<'a>> {
        self.speakers.iter().find(|s| s.id == id)
    }
    pub fn reliable(&self) -> usize {
        self.speakers.iter().filter(|s| s.arrival.is_some()).count()
    }
    fn reference_name(&self) -> &str {
        let id = self.alignment.settings.reference_output.as_deref();
        id.and_then(|id| self.speaker(id).map(|s| s.name))
            .or(id)
            .unwrap_or("the reference")
    }
}

pub fn signed(value: f64, decimals: usize, unit: &str) -> String {
    let value = if value.abs() < 0.5 * 10f64.powi(-(decimals as i32)) {
        0.0
    } else {
        value
    };
    format!("{value:+.decimals$} {unit}")
}
pub fn delay_change(change: &Change) -> String {
    if change.delay_ms().abs() < 0.005 {
        "delay unchanged".into()
    } else {
        format!("delay {}", signed(change.delay_ms(), 2, "ms"))
    }
}
pub fn gain_change(change: &Change) -> String {
    if change.gain_db().abs() < 0.05 {
        "gain unchanged".into()
    } else {
        format!("gain {}", signed(change.gain_db(), 1, "dB"))
    }
}

/// The alignment lines of one speaker card.
pub struct Card {
    pub grade: Option<Grade>,
    pub measured: (String, Color32),
    /// None when `measured` is an exclusion reason, which may then use both lines.
    pub planned: Option<(String, Color32)>,
    pub details: String,
}
pub fn card(sp: &Speaker) -> Card {
    let (measured, planned) = match (sp.arrival, &sp.reason) {
        (Some((ms, _)), _) => (
            (
                format!(
                    "{}{}",
                    signed(ms, 2, "ms"),
                    sp.level
                        .map(|(db, _)| format!(" · {}", signed(db, 1, "dB")))
                        .unwrap_or_default()
                ),
                super::TEXT,
            ),
            Some(sp.change.map_or_else(
                || ("No proposal yet".to_owned(), super::MUTED),
                |c| {
                    (
                        format!("{} · {}", delay_change(&c), gain_change(&c)),
                        super::ACCENT,
                    )
                },
            )),
        ),
        (None, Some(reason)) => ((reason.text.to_owned(), reason.color()), None),
        (None, None) => (("Not measured".to_owned(), super::MUTED), None),
    };
    Card {
        grade: sp.grade,
        measured,
        planned,
        details: tooltip(sp),
    }
}

/// What a click in the alignment stage asked for.
#[derive(Default)]
pub struct StageResponse {
    pub select: Option<String>,
    pub match_knobs: Option<String>,
}

/// Session heading, state banners, the chart and the casual hints.
pub fn stage(ui: &mut egui::Ui, s: &Summary, selected: Option<&str>) -> StageResponse {
    let a = s.alignment;
    let mut response = StageResponse::default();
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Alignment session").size(18.0).strong());
        let excluded = s.speakers.len() - s.reliable();
        ui.label(
            RichText::new(format!(
                "{} · Reference: {} · two passes · {} reliable, {} excluded",
                a.settings.position_id,
                s.reference_name(),
                s.reliable(),
                excluded
            ))
            .small()
            .color(super::MUTED),
        );
    });
    banners(ui, s);
    ui.add_space(6.0);
    lane_title(
        ui,
        "Timing",
        &format!(
            "arrival at the microphone vs {} · ms · negative = earlier",
            s.reference_name()
        ),
    );
    if let Some(id) = timing_lane(ui, s, selected) {
        response.select = Some(id);
    }
    ui.add_space(4.0);
    let quietest = s.quietest.map(|(i, _)| s.speakers[i].name);
    lane_title(
        ui,
        "Level",
        &format!(
            "500–2000 Hz · dB above the quietest reliable speaker{}",
            quietest.map(|n| format!(" ({n})")).unwrap_or_default()
        ),
    );
    if let Some(id) = level_lane(ui, s, selected) {
        response.select = Some(id);
    }
    legend(ui);
    if let Some((index, gap)) = s.quietest.filter(|(_, gap)| *gap > KNOB_HINT_DB) {
        let name = s.speakers[index].name;
        let clicked = hint(
            ui,
            |ui| {
                ui.label(
                    RichText::new(format!("{name} is {gap:.1} dB quieter than the others."))
                        .strong()
                        .color(super::WARM),
                );
                ui.label(format!(
                    "Apply would turn the louder speakers down to match it. If {name} has a volume knob, raising it keeps more headroom."
                ));
            },
            Some("Match knobs"),
        );
        if clicked {
            response.match_knobs = Some(s.speakers[index].id.to_owned());
        }
    }
    if let Some(far) = s
        .speakers
        .iter()
        .find(|sp| sp.arrival.is_some_and(|(ms, _)| ms.abs() > FAR_MS))
    {
        let ms = far.arrival.map_or(0.0, |(ms, _)| ms);
        hint(
            ui,
            |ui| {
                ui.label(format!(
                    "{} arrives {:.0} ms {}. Matching it shifts the other speakers by as much, which may affect lip sync.",
                    far.name,
                    ms.abs(),
                    if ms > 0.0 { "late" } else { "early" }
                ))
                .on_hover_text(calibration_core::long_delay_warning(ms).unwrap_or_default());
            },
            None,
        );
    }
    response
}

/// Holds the previous chart's place while a new alignment is captured.
pub fn placeholder(ui: &mut egui::Ui, height: f32) {
    ui.set_min_height(height.max(0.0));
    ui.label(RichText::new("Alignment session").size(18.0).strong());
    ui.label(
        RichText::new("Measuring · the new chart appears when both passes finish.")
            .small()
            .color(super::MUTED),
    );
}

fn banners(ui: &mut egui::Ui, s: &Summary) {
    let a = s.alignment;
    if a.application_uncertain {
        ui.colored_label(super::RED, "Application outcome is uncertain. Inspect Omniphony and the saved original settings before further changes.");
    }
    if a.physical_levels_changed {
        ui.colored_label(super::WARM, "Physical levels may have changed · measure a new alignment. Original digital settings can still be restored.");
    }
    if a.application_uncertain || a.physical_levels_changed {
        return;
    }
    let excluded = a.rows.iter().filter(|r| r.qualified.is_none()).count();
    if let Some(profile) = &a.profile {
        ui.colored_label(
            super::ACCENT,
            format!(
                "{} for {} / {} speakers · {} excluded",
                if a.applied_snapshot.is_some() {
                    "Corrections applied"
                } else {
                    "Correction proposal ready"
                },
                profile.target_snapshot.speakers.len(),
                a.rows.len(),
                excluded
            ),
        );
    } else if !a.blockers.is_empty() {
        ui.colored_label(
            super::WARM,
            "Correction proposal blocked by session issues:",
        );
        for reason in &a.blockers {
            ui.colored_label(super::WARM, readable_reason(reason));
        }
    } else if a.qualified_count() >= 2 {
        ui.colored_label(
            super::WARM,
            format!(
                "{} / {} speakers qualify · update the proposal from saved passes",
                a.qualified_count(),
                a.rows.len()
            ),
        );
    } else {
        ui.colored_label(
            super::WARM,
            format!(
                "{} / {} speakers qualify · at least two are needed for relative correction",
                a.qualified_count(),
                a.rows.len()
            ),
        );
    }
}

fn lane_title(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        ui.add(egui::Label::new(RichText::new(detail).small().color(super::MUTED)).truncate());
    });
}

pub fn lane_height(rows: usize) -> f32 {
    AXIS_TOP + rows.max(1) as f32 * ROW_HEIGHT + AXIS_BOTTOM
}

/// Name, plot, optional far bracket and note columns of one lane.
struct Columns {
    rect: Rect,
    name: f32,
    plot: (f32, f32),
    bracket: Option<Rect>,
    note: f32,
}
impl Columns {
    fn new(rect: Rect, far: bool) -> Self {
        let width = rect.width();
        let name = (width * 0.16).clamp(64.0, 110.0);
        let note = (width * 0.3).clamp(120.0, 250.0);
        let bracket_width = if far {
            (width * 0.12).clamp(60.0, 88.0)
        } else {
            0.0
        };
        let right = rect.right() - note - bracket_width - if far { 10.0 } else { 0.0 };
        let left = rect.left() + name;
        Self {
            rect,
            name,
            plot: (left, right.max(left + 40.0)),
            bracket: far.then(|| {
                Rect::from_min_max(
                    Pos2::new(right + 10.0, rect.top() + 2.0),
                    Pos2::new(right + 10.0 + bracket_width, rect.bottom() - 2.0),
                )
            }),
            note: rect.right() - note + 8.0,
        }
    }
    fn row_y(&self, index: usize) -> f32 {
        self.rect.top() + AXIS_TOP + (index as f32 + 0.5) * ROW_HEIGHT
    }
    fn row_at(&self, pos: Pos2, rows: usize) -> Option<usize> {
        let offset = (pos.y - self.rect.top() - AXIS_TOP) / ROW_HEIGHT;
        (offset >= 0.0 && (offset as usize) < rows).then_some(offset as usize)
    }
}

/// Rounded axis bounds and step for roughly four intervals.
fn axis(lo: f64, hi: f64) -> (f64, f64, f64) {
    let raw = ((hi - lo) / 4.0).max(1e-6);
    let magnitude = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * magnitude)
        .find(|step| *step >= raw)
        .unwrap_or(10.0 * magnitude);
    ((lo / step).floor() * step, (hi / step).ceil() * step, step)
}

fn tick_label(value: f64, step: f64) -> String {
    let decimals = if step >= 1.0 {
        0
    } else if (step * 10.0 - (step * 10.0).round()).abs() < 1e-6 {
        1
    } else {
        2
    };
    if value.abs() < step * 1e-3 {
        "0".into()
    } else {
        format!("{value:+.decimals$}")
    }
}

fn text_fit(
    painter: &egui::Painter,
    pos: Pos2,
    anchor: Align2,
    text: impl Into<String>,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> Rect {
    let mut job = LayoutJob::single_section(text.into(), TextFormat::simple(font, color));
    job.wrap = TextWrapping::truncate_at_width(max_width.max(12.0));
    let galley = painter.layout_job(job);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, color);
    rect
}

fn arrow_head(painter: &egui::Painter, tip: Pos2, rightwards: bool, stroke: Stroke) {
    let back = if rightwards { -6.0 } else { 6.0 };
    painter.add(Shape::line(
        vec![tip + Vec2::new(back, -4.0), tip, tip + Vec2::new(back, 4.0)],
        stroke,
    ));
}

fn grid(
    painter: &egui::Painter,
    c: &Columns,
    (lo, hi, step): (f64, f64, f64),
    x: &dyn Fn(f64) -> f32,
) {
    let mut value = lo;
    while value <= hi + step * 1e-6 {
        let gx = x(value);
        painter.line_segment(
            [
                Pos2::new(gx, c.rect.top() + AXIS_TOP - 6.0),
                Pos2::new(gx, c.rect.bottom() - AXIS_BOTTOM + 4.0),
            ],
            Stroke::new(1.0_f32, super::MUTED.gamma_multiply(0.18)),
        );
        painter.text(
            Pos2::new(gx, c.rect.top() + 2.0),
            Align2::CENTER_TOP,
            tick_label(value, step),
            FontId::monospace(10.0),
            super::MUTED,
        );
        value += step;
    }
}

fn row_frame(painter: &egui::Painter, c: &Columns, sp: &Speaker, index: usize, selected: bool) {
    let y = c.row_y(index);
    if selected {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(c.rect.left(), y - ROW_HEIGHT / 2.0),
                Pos2::new(c.rect.right(), y + ROW_HEIGHT / 2.0),
            ),
            4.0,
            super::ACCENT.gamma_multiply(0.08),
        );
    }
    text_fit(
        painter,
        Pos2::new(c.rect.left() + c.name - 10.0, y),
        Align2::RIGHT_CENTER,
        sp.name,
        FontId::proportional(12.0),
        super::TEXT,
        c.name - 14.0,
    );
}

fn hollow(painter: &egui::Painter, center: Pos2, color: Color32) {
    painter.circle_stroke(center, 4.5, Stroke::new(1.8_f32, color));
}

/// The right-hand column: what Apply changes, or why this speaker is left alone.
fn note(painter: &egui::Painter, c: &Columns, y: f32, sp: &Speaker, text: Option<String>) {
    let width = c.rect.right() - c.note;
    let at = Pos2::new(c.note, y);
    match (&sp.reason, text) {
        (Some(reason), _) => {
            text_fit(
                painter,
                at,
                Align2::LEFT_CENTER,
                reason.text,
                FontId::proportional(11.0),
                reason.color(),
                width,
            );
        }
        (None, text) => {
            let muted = sp.change.is_none();
            text_fit(
                painter,
                at,
                Align2::LEFT_CENTER,
                text.unwrap_or_else(|| "no proposal".into()),
                FontId::monospace(10.5),
                if muted { super::MUTED } else { super::TEXT },
                width,
            );
        }
    }
}

fn tooltip(sp: &Speaker) -> String {
    let mut lines = vec![sp.name.to_owned()];
    if let Some((ms, spread)) = sp.arrival {
        lines.push(format!(
            "Arrival {} · runs differ by {spread:.3} ms",
            signed(ms, 3, "ms")
        ));
    }
    if let Some((db, spread)) = sp.level {
        lines.push(format!(
            "Level {} above the quietest · runs differ by {spread:.2} dB",
            signed(db, 2, "dB")
        ));
    }
    if let Some(change) = &sp.change {
        lines.push(format!(
            "Delay {:.3} ms to {:.3} ms · Gain {:+.2} dB to {:+.2} dB",
            change.before_delay_ms,
            change.target_delay_ms,
            change.before_gain_db,
            change.target_gain_db
        ));
    }
    if let Some(reason) = &sp.reason {
        lines.push(format!("{} Gain and delay stay unchanged.", reason.text));
        lines.push(format!("Detail: {}", reason.raw));
        if let Some(ms) = sp.diagnostic_arrival_ms {
            lines.push(format!("Diagnostic arrival {}", signed(ms, 3, "ms")));
        }
    }
    lines.join("\n")
}

fn lane_interaction(
    ui: &egui::Ui,
    response: egui::Response,
    c: &Columns,
    s: &Summary,
) -> Option<String> {
    let hovered = response
        .hover_pos()
        .and_then(|p| c.row_at(p, s.speakers.len()));
    let clicked = response
        .clicked()
        .then(|| response.interact_pointer_pos())
        .flatten()
        .and_then(|p| c.row_at(p, s.speakers.len()))
        .map(|i| s.speakers[i].id.to_owned());
    if let Some(index) = hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        response.on_hover_text(tooltip(&s.speakers[index]));
    }
    clicked
}

fn timing_lane(ui: &mut egui::Ui, s: &Summary, selected: Option<&str>) -> Option<String> {
    let far = s.speakers.iter().any(|sp| {
        sp.arrival
            .map(|(ms, _)| ms)
            .or(sp.diagnostic_arrival_ms)
            .is_some_and(|ms| ms.abs() > FAR_MS)
    });
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), lane_height(s.speakers.len())),
        Sense::click(),
    );
    let c = Columns::new(rect, far);
    let (mut lo, mut hi) = (0.0_f64, 0.0_f64);
    for sp in &s.speakers {
        if let Some((ms, spread)) = sp.arrival.filter(|(ms, _)| ms.abs() <= FAR_MS) {
            lo = lo.min(ms - spread / 2.0);
            hi = hi.max(ms + spread / 2.0);
            if let Some(target) = sp
                .change
                .map(|c| ms + c.delay_ms())
                .filter(|t| t.abs() <= FAR_MS)
            {
                lo = lo.min(target);
                hi = hi.max(target);
            }
        }
    }
    let pad = ((hi - lo) * 0.08).max(0.1);
    let center = (lo + hi) / 2.0;
    let half = ((hi - lo) / 2.0 + pad).max(0.5);
    let range = axis(center - half, center + half);
    let (left, right) = c.plot;
    let x = |ms: f64| left + ((ms - range.0) / (range.1 - range.0)) as f32 * (right - left);
    if !ui.is_rect_visible(rect) {
        return None;
    }
    let painter = ui.painter_at(rect);
    grid(&painter, &c, range, &x);
    let zero = x(0.0);
    painter.extend(Shape::dashed_line(
        &[
            Pos2::new(zero, rect.top() + AXIS_TOP - 6.0),
            Pos2::new(zero, rect.bottom() - AXIS_BOTTOM + 4.0),
        ],
        Stroke::new(1.3_f32, super::ACCENT),
        3.0,
        3.0,
    ));
    let bottom = rect.bottom() - 2.0;
    painter.text(
        Pos2::new(left, bottom),
        Align2::LEFT_BOTTOM,
        "earlier",
        FontId::proportional(10.0),
        super::MUTED,
    );
    painter.text(
        Pos2::new(right, bottom),
        Align2::RIGHT_BOTTOM,
        "later",
        FontId::proportional(10.0),
        super::MUTED,
    );
    text_fit(
        &painter,
        Pos2::new(zero, bottom),
        Align2::CENTER_BOTTOM,
        format!("{} = 0", s.reference_name()),
        FontId::proportional(10.0),
        super::ACCENT,
        (right - left) * 0.5,
    );
    if let Some(bracket) = c.bracket {
        painter.rect(
            bracket,
            6.0,
            FAR.gamma_multiply(0.06),
            Stroke::new(1.0_f32, FAR.gamma_multiply(0.45)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            Pos2::new(bracket.center().x, rect.top() + 2.0),
            Align2::CENTER_TOP,
            format!("beyond ±{FAR_MS:.0} ms"),
            FontId::proportional(10.0),
            FAR,
        );
    }
    for (i, sp) in s.speakers.iter().enumerate() {
        let y = c.row_y(i);
        row_frame(&painter, &c, sp, i, selected == Some(sp.id));
        let ms = sp.arrival.map(|(ms, _)| ms).or(sp.diagnostic_arrival_ms);
        if let (Some(ms), Some(bracket)) = (ms.filter(|ms| ms.abs() > FAR_MS), c.bracket) {
            let dot = Pos2::new(bracket.left() + 12.0, y);
            match &sp.reason {
                Some(reason) => hollow(&painter, dot, reason.color()),
                None => {
                    painter.circle_filled(dot, 4.5, super::TEXT);
                }
            }
            text_fit(
                &painter,
                Pos2::new(dot.x + 9.0, y),
                Align2::LEFT_CENTER,
                signed(ms, 1, "ms"),
                FontId::monospace(10.5),
                FAR,
                bracket.right() - dot.x - 11.0,
            );
        } else if let Some((ms, spread)) = sp.arrival {
            let mx = x(ms);
            if let Some(change) = sp.change {
                let target = ms + change.delay_ms();
                let tx = x(target.clamp(range.0, range.1));
                let stroke = Stroke::new(1.5_f32, super::ACCENT.gamma_multiply(0.85));
                if (tx - mx).abs() > 6.0 {
                    let toward = if tx > mx { -5.0 } else { 5.0 };
                    painter.line_segment([Pos2::new(mx, y), Pos2::new(tx + toward, y)], stroke);
                    arrow_head(&painter, Pos2::new(tx + toward, y), tx > mx, stroke);
                }
                if (range.0..=range.1).contains(&target) {
                    painter.circle_stroke(
                        Pos2::new(tx, y),
                        5.0,
                        Stroke::new(2.0_f32, super::ACCENT),
                    );
                }
            }
            let width = (x(ms + spread / 2.0) - x(ms - spread / 2.0)).max(2.0);
            painter.rect_filled(
                Rect::from_center_size(Pos2::new(mx, y), Vec2::new(width, 4.0)),
                1.0,
                super::TEXT.gamma_multiply(0.35),
            );
            painter.circle_filled(Pos2::new(mx, y), 4.5, super::TEXT);
        } else if let Some(reason) = &sp.reason {
            let at = Pos2::new(ms.map_or(zero, |ms| x(ms.clamp(range.0, range.1))), y);
            hollow(&painter, at, reason.color());
        }
        note(&painter, &c, y, sp, sp.change.as_ref().map(delay_change));
    }
    lane_interaction(ui, response, &c, s)
}

fn level_lane(ui: &mut egui::Ui, s: &Summary, selected: Option<&str>) -> Option<String> {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), lane_height(s.speakers.len())),
        Sense::click(),
    );
    let c = Columns::new(rect, false);
    let (mut lo, mut hi) = (0.0_f64, 0.0_f64);
    for sp in &s.speakers {
        if let Some((db, spread)) = sp.level {
            hi = hi.max(db + spread / 2.0);
            if let Some(change) = &sp.change {
                lo = lo.min(db + change.gain_db());
                hi = hi.max(db + change.gain_db());
            }
        }
    }
    let lo = lo - ((hi - lo) * 0.25).max(1.0);
    let hi = hi + ((hi - lo) * 0.05).max(0.25);
    let range = axis(lo, hi);
    let (left, right) = c.plot;
    let x = |db: f64| left + ((db - range.0) / (range.1 - range.0)) as f32 * (right - left);
    if !ui.is_rect_visible(rect) {
        return None;
    }
    let painter = ui.painter_at(rect);
    grid(&painter, &c, range, &x);
    let zero = x(0.0);
    if let Some((index, _)) = s.quietest {
        painter.extend(Shape::dashed_line(
            &[
                Pos2::new(zero, rect.top() + AXIS_TOP - 6.0),
                Pos2::new(zero, rect.bottom() - AXIS_BOTTOM + 4.0),
            ],
            Stroke::new(1.3_f32, super::ACCENT),
            3.0,
            3.0,
        ));
        text_fit(
            &painter,
            Pos2::new(zero, rect.bottom() - 2.0),
            Align2::CENTER_BOTTOM,
            format!("target = quietest ({})", s.speakers[index].name),
            FontId::proportional(10.0),
            super::ACCENT,
            (right - left) * 0.9,
        );
    }
    for (i, sp) in s.speakers.iter().enumerate() {
        let y = c.row_y(i);
        row_frame(&painter, &c, sp, i, selected == Some(sp.id));
        let note_text = if let Some((db, spread)) = sp.level {
            let end = x(db);
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(left, y - 6.0), Pos2::new(end, y + 6.0)),
                3.0,
                super::TEXT.gamma_multiply(0.18),
            );
            if let Some(change) = &sp.change {
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(left, y - 3.0),
                        Pos2::new(x(db + change.gain_db()), y + 3.0),
                    ),
                    2.0,
                    super::ACCENT.gamma_multiply(0.6),
                );
            }
            let width = (x(db + spread / 2.0) - x(db - spread / 2.0)).max(2.0);
            painter.rect_filled(
                Rect::from_center_size(Pos2::new(end, y), Vec2::new(width, 16.0)),
                1.0,
                super::TEXT.gamma_multiply(0.55),
            );
            Some(match &sp.change {
                Some(change) if change.gain_db().abs() >= 0.05 => format!(
                    "{} → gain {}",
                    signed(db, 1, "dB"),
                    signed(change.gain_db(), 1, "")
                )
                .trim_end()
                .to_owned(),
                Some(_) => format!("{} · unchanged", signed(db, 1, "dB")),
                None => signed(db, 1, "dB"),
            })
        } else {
            if let Some(reason) = &sp.reason {
                hollow(&painter, Pos2::new(left + 6.0, y), reason.color());
            }
            None
        };
        note(&painter, &c, y, sp, note_text);
    }
    lane_interaction(ui, response, &c, s)
}

fn legend(ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        for (kind, text) in [
            (0, "measured · bar = spread between runs"),
            (1, "after correction"),
            (2, "not reliable · unchanged"),
        ] {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
            let p = ui.painter();
            match kind {
                0 => p.circle_filled(rect.center(), 4.0, super::TEXT),
                1 => p.circle_stroke(rect.center(), 4.0, Stroke::new(2.0_f32, super::ACCENT)),
                _ => p.circle_stroke(rect.center(), 4.0, Stroke::new(1.8_f32, super::WARM)),
            };
            ui.label(RichText::new(text).small().color(super::MUTED));
            ui.add_space(6.0);
        }
    });
}

/// A highlighted advice box; returns whether its optional right-hand link was clicked.
fn hint(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui), link: Option<&str>) -> bool {
    let mut clicked = false;
    egui::Frame::new()
        .fill(super::WARM.gamma_multiply(0.07))
        .stroke(Stroke::new(1.0_f32, super::WARM.gamma_multiply(0.3)))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let link_width = if link.is_some() { 100.0 } else { 0.0 };
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width() - link_width, 0.0),
                    egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true),
                    contents,
                );
                if let Some(link) = link {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        clicked = ui.link(link).clicked();
                    });
                }
            });
        });
    clicked
}

/// Whether the live renderer revision still matches the state this alignment expects.
pub fn live(a: &Alignment, revision: Option<&str>) -> bool {
    let expected = a
        .applied_snapshot
        .as_ref()
        .or(a.restored_snapshot.as_ref())
        .unwrap_or(&a.snapshot);
    revision == Some(expected.revision.as_str())
}

/// Apply, Original/Corrected, Confirm and Undo with one status line.
pub fn action_bar(
    ui: &mut egui::Ui,
    a: &Alignment,
    live: bool,
    busy: bool,
    updating: bool,
) -> Option<&'static str> {
    let mut action = None;
    let applied = a.applied_snapshot.is_some();
    let blocked = if busy {
        Some("Wait for the current operation to finish.")
    } else if a.application_uncertain {
        Some(
            "The last application is uncertain. Inspect Omniphony and the saved original settings first.",
        )
    } else if a.profile.is_none() {
        Some("There is no correction proposal to apply.")
    } else if !live {
        Some(
            "Omniphony is disconnected or its settings changed since this alignment. Refresh, then measure again if needed.",
        )
    } else {
        None
    };
    let stale = a
        .physical_levels_changed
        .then_some("Physical levels may have changed. Measure a new alignment first.");
    let apply_block = blocked.or(stale).or(applied.then_some("Already applied."));
    let verify_block = blocked
        .or(stale)
        .or((!applied).then_some("Apply the corrections first."));
    let restore_block = blocked.or((!applied).then_some("Nothing is applied."));
    ui.horizontal_wrapped(|ui| {
        let enabled = apply_block.is_none();
        let button = ui
            .add_enabled(
                enabled,
                egui::Button::new(
                    RichText::new("Apply to Omniphony")
                        .strong()
                        .color(if enabled { super::BG } else { super::MUTED }),
                )
                .fill(if enabled { super::ACCENT } else { super::PANEL })
                .min_size(Vec2::new(150.0, 30.0)),
            )
            .on_hover_text("Send the corrected gains and delays to the live renderer.")
            .on_disabled_hover_text(apply_block.unwrap_or_default());
        if button.clicked() {
            action = Some("apply");
        }
        egui::Frame::new()
            .stroke(Stroke::new(1.0_f32, super::MUTED.gamma_multiply(0.4)))
            .corner_radius(6)
            .inner_margin(egui::Margin::same(2))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.horizontal(|ui| {
                    let original = ui
                        .add_enabled(
                            !applied || restore_block.is_none(),
                            egui::Button::selectable(!applied, "Original"),
                        )
                        .on_hover_text("Listen with the settings from before this alignment.")
                        .on_disabled_hover_text(restore_block.unwrap_or_default());
                    if original.clicked() && applied {
                        action = Some("restore");
                    }
                    let corrected = ui
                        .add_enabled(
                            applied || apply_block.is_none(),
                            egui::Button::selectable(applied, "Corrected"),
                        )
                        .on_hover_text("Listen with the corrected gains and delays.")
                        .on_disabled_hover_text(apply_block.unwrap_or_default());
                    if corrected.clicked() && !applied {
                        action = Some("apply");
                    }
                });
            });
        if ui
            .add_enabled(
                verify_block.is_none(),
                egui::Button::new("Confirm by re-measuring"),
            )
            .on_hover_text("Measure the corrected speakers again, twice, and compare.")
            .on_disabled_hover_text(verify_block.unwrap_or_default())
            .clicked()
        {
            action = Some("verify");
        }
        if ui
            .add_enabled(restore_block.is_none(), egui::Button::new("Undo"))
            .on_hover_text("Restore the gains and delays from before this alignment.")
            .on_disabled_hover_text(restore_block.unwrap_or_default())
            .clicked()
        {
            action = Some("restore");
        }
    });
    let (status, color) = status_line(a, live, updating);
    ui.add(egui::Label::new(RichText::new(status).small().color(color)).wrap());
    action
}

fn status_line(a: &Alignment, live: bool, updating: bool) -> (String, Color32) {
    let save = "Live only: press Save in Studio to keep it.";
    if updating {
        return ("Updating the correction proposal…".into(), super::MUTED);
    }
    if a.application_uncertain {
        return (
            "Application outcome is uncertain; nothing else can be applied until it is checked."
                .into(),
            super::RED,
        );
    }
    if a.profile.is_none() {
        return (
            if a.qualified_count() >= 2 && a.blockers.is_empty() {
                "No proposal yet. Turn on Lab to update the proposal from the saved passes."
            } else {
                "Nothing to apply. At least two speakers must measure reliably."
            }
            .into(),
            super::MUTED,
        );
    }
    if !live {
        return (
            "Omniphony is disconnected or changed since this alignment. Refresh; measure again if its settings changed.".into(),
            super::WARM,
        );
    }
    match (&a.applied_snapshot, &a.verification, &a.verification_note) {
        (None, ..) => (
            "Not applied yet. Apply changes the live renderer only; press Save in Studio to keep it."
                .into(),
            super::MUTED,
        ),
        (Some(_), _, Some(note)) => (
            format!(
                "Applied. {} Undo restores the original settings.",
                note.split(". Review ").next().unwrap_or(note)
            ),
            super::WARM,
        ),
        (Some(_), Some(v), None) if v.passed => (
            format!(
                "Applied and confirmed at this seat: arrival spread {:.2} ms down to {:.2} ms, level spread {:.1} dB down to {:.1} dB. {save}",
                v.before.arrival_spread_ms,
                v.after.arrival_spread_ms,
                v.before.level_spread_db,
                v.after.level_spread_db
            ),
            super::ACCENT,
        ),
        (Some(_), Some(v), None) => (
            format!(
                "Applied, but the re-measurement is outside tolerance (arrival spread {:.2} ms, level spread {:.1} dB). Undo, or inspect in Lab.",
                v.after.arrival_spread_ms, v.after.level_spread_db
            ),
            super::WARM,
        ),
        (Some(_), None, None) => (
            format!("Applied. Confirm by re-measuring. {save}"),
            super::ACCENT,
        ),
    }
}

/// Everything the previous session card showed, for Lab mode.
pub fn lab(
    ui: &mut egui::Ui,
    s: &Summary,
    busy: bool,
    older: &dyn Fn(&Row) -> bool,
) -> Option<&'static str> {
    let a = s.alignment;
    let mut action = None;
    ui.label(RichText::new("Lab · alignment evidence").strong());
    let included = a
        .profile
        .as_ref()
        .map_or(0, |p| p.target_snapshot.speakers.len());
    let excluded = a.rows.iter().filter(|r| r.qualified.is_none()).count();
    if a.profile.is_some() {
        ui.label("Only included speakers receive targets. Excluded speaker gains and delays stay unchanged.");
    }
    if excluded > 0 {
        egui::CollapsingHeader::new(format!("Excluded speakers ({excluded}) · unchanged"))
            .default_open(true)
            .show(ui, |ui| {
                for row in a.rows.iter().filter(|r| r.qualified.is_none()) {
                    ui.colored_label(
                        super::WARM,
                        format!(
                            "{}: {}",
                            row.output_id,
                            row.reason
                                .as_deref()
                                .map(readable_reason)
                                .unwrap_or("Two qualified independent captures are required.")
                        ),
                    );
                }
            });
    }
    if !a.blockers.is_empty() {
        ui.colored_label(
            super::WARM,
            format!(
                "{} session issue(s) · see session details",
                a.blockers.len()
            ),
        )
        .on_hover_text(a.blockers.join("\n"));
    }
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(!busy && a.applied_snapshot.is_none() && !a.application_uncertain, egui::Button::new("Reanalyze saved passes"))
            .on_hover_text("Recompute diagnostic results and quality from the original audio. Creates a separate report; no playback or correction.").clicked() {
            action = Some("reanalyze");
        }
        if ui.add_enabled(!busy && a.profile.is_none() && a.blockers.is_empty() && !a.physical_levels_changed
            && a.applied_snapshot.is_none() && !a.application_uncertain && a.qualified_count() >= 2,
            egui::Button::new("Update correction proposal"))
            .on_hover_text("Requalify the original saved passes and propose targets for eligible speakers only. No playback or settings changes.").clicked() {
            action = Some("proposal");
        }
    });
    egui::CollapsingHeader::new("Alignment session details").show(ui, |ui| {
        ui.label("Two independent passes · fixed microphone · 500–2000 Hz relative levels");
        ui.label(RichText::new("Experimental arrival estimates: repeatability does not prove an unobstructed direct path. No EQ or subwoofer alignment.").small().color(super::WARM));
        ui.label(format!("Position: {} · Microphone: {} · Reference: {}", a.settings.position_id, a.settings.microphone_id, a.settings.reference_output.as_deref().unwrap_or("—")));
        pass_summary(ui, "Baseline", a.pass_reports.as_deref(), a.rows.len());
        if let Some(passes) = &a.verification_pass_reports {
            pass_summary(ui, "Verification", Some(passes), included);
        }
        for reason in &a.blockers { ui.colored_label(super::RED, reason); }
        ui.label("Targets include existing output settings. Earlier speakers receive delay; louder speakers are attenuated. Levels are relative transfer dB, not SPL.");
        ui.label("Application changes live settings only. Save in Studio after verification if you want them in Omniphony's persistent configuration.");
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Session: {}", a.directory.display()));
            if ui.small_button("Copy session folder").clicked() {
                ui.ctx().copy_text(a.directory.display().to_string());
            }
        });
    });
    if let Some(note) = &a.verification_note {
        ui.colored_label(super::WARM, note);
    }
    if let Some(v) = &a.verification {
        ui.colored_label(
            if v.passed { super::ACCENT } else { super::WARM },
            if v.passed {
                "Verification passed at this microphone position"
            } else {
                "Verification outside tolerance — review or restore"
            },
        );
        ui.label(format!(
            "Arrival spread: {:.3} → {:.3} ms · Level spread: {:.2} → {:.2} dB",
            v.before.arrival_spread_ms,
            v.after.arrival_spread_ms,
            v.before.level_spread_db,
            v.after.level_spread_db
        ));
        ui.label("Prototype tolerances: 0.10 ms / 0.50 dB. These are comparison thresholds, not a claim of hardware accuracy.");
    } else if a.applied_snapshot.is_some() {
        ui.colored_label(
            super::WARM,
            "Settings applied and read back; acoustic verification still required.",
        );
    }
    egui::CollapsingHeader::new("Per-speaker evidence")
        .default_open(true)
        .show(ui, |ui| {
            for sp in &s.speakers {
                ui.separator();
                ui.label(RichText::new(sp.name).strong());
                evidence(ui, a, sp.row, older(sp.row));
            }
        });
    action
}

/// The two-pass estimates, quality and targets of one speaker, as raw numbers.
fn evidence(ui: &mut egui::Ui, a: &Alignment, row: &Row, older: bool) {
    let arrival = row
        .qualified
        .as_ref()
        .map(|q| q.arrival_ms)
        .or_else(|| row.repeats.iter().rev().find_map(|r| r.arrival_ms));
    let level = row
        .qualified
        .as_ref()
        .map(|q| q.level_db)
        .or_else(|| row.repeats.last().and_then(|r| r.level_db));
    let spread = row
        .qualified
        .as_ref()
        .map(|q| (q.arrival_repeat_spread_ms, q.level_repeat_spread_db))
        .or_else(|| {
            row.repeats
                .first()
                .zip(row.repeats.get(1))
                .and_then(|(first, second)| {
                    Some((
                        (first.arrival_ms? - second.arrival_ms?).abs(),
                        (first.level_db? - second.level_db?).abs(),
                    ))
                })
        });
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!("Alignment · {} / 2 passes", row.take_folders.len()))
                .small()
                .strong(),
        );
        ui.label(
            RichText::new(if row.qualified.is_some() {
                "Repeatable"
            } else {
                "Excluded from correction"
            })
            .small()
            .color(if row.qualified.is_some() {
                super::ACCENT
            } else {
                super::WARM
            }),
        );
        if older {
            ui.colored_label(
                super::WARM,
                "Previous alignment · settings or measurement changed",
            );
        }
    });
    ui.horizontal_wrapped(|ui| {
        metric(
            ui,
            "Arrival candidate",
            arrival
                .map(|v| format!("{v:+.3} ms"))
                .unwrap_or("Unavailable".into()),
        );
        metric(
            ui,
            "Level",
            level
                .map(|v| format!("{v:+.2} dB"))
                .unwrap_or("Unavailable".into()),
        );
        metric(
            ui,
            "Repeat Δt / ΔL",
            spread
                .map(|(t, l)| format!("{t:.3} ms / {l:.2} dB"))
                .unwrap_or("Unavailable".into()),
        );
        let grade = row.quality.iter().map(|q| q.grade()).min();
        ui.vertical(|ui| {
            super::caption(ui, "Quality");
            let details = row
                .quality
                .iter()
                .flat_map(|q| {
                    q.acquisition
                        .details
                        .iter()
                        .chain(&q.timing.details)
                        .chain(&q.level.details)
                })
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            super::quality_view::badge(
                ui,
                grade,
                grade.map(|g| g.label()).unwrap_or("Not rated"),
                &details,
            );
        });
    });
    if let Some(target) = a.profile.as_ref().and_then(|p| {
        p.target_snapshot
            .speakers
            .iter()
            .find(|s| s.id == row.output_id)
    }) {
        let before = a.snapshot.outputs.iter().find(|o| o.id == row.output_id);
        ui.horizontal_wrapped(|ui| {
            metric(
                ui,
                "Gain: before → target",
                before
                    .map(|o| format!("{:+.2} → {:+.2} dB", o.gain_db, target.settings.gain_db))
                    .unwrap_or_else(|| format!("{:+.2} dB", target.settings.gain_db)),
            );
            metric(
                ui,
                "Delay: before → target",
                before
                    .map(|o| format!("{:.3} → {:.3} ms", o.delay_ms, target.settings.delay_ms))
                    .unwrap_or_else(|| format!("{:.3} ms", target.settings.delay_ms)),
            );
        });
    }
    if let Some(warning) = arrival.and_then(calibration_core::long_delay_warning) {
        ui.colored_label(super::WARM, warning);
    }
    if let Some(reason) = &row.reason {
        ui.colored_label(
            super::WARM,
            format!(
                "Excluded: {} · gain and delay unchanged",
                readable_reason(reason)
            ),
        );
    }
}

fn metric(ui: &mut egui::Ui, label: &str, value: String) {
    ui.vertical(|ui| {
        super::caption(ui, label);
        ui.monospace(value);
    });
}

fn pass_summary(
    ui: &mut egui::Ui,
    label: &str,
    passes: Option<&[companion_service::alignment::PassReport]>,
    speakers: usize,
) {
    let Some(passes) = passes else {
        ui.label(format!(
            "{label}: pass completion was not recorded by this older session."
        ));
        return;
    };
    for pass in 0..2 {
        let status = match passes.get(pass) {
            None => "not started".to_owned(),
            Some(p) => format!(
                "{} · {} / {} speakers analyzed",
                if p.error.is_some() {
                    "stopped"
                } else {
                    "complete"
                },
                p.analyzed_speakers,
                speakers
            ),
        };
        ui.label(format!("{label} pass {} / 2: {status}", pass + 1));
    }
}

#[cfg(test)]
mod tests {
    use super::human_reason;

    #[test]
    fn raw_reasons_map_to_short_human_sentences_and_keep_the_raw_text() {
        for (raw, text, severe) in [
            (
                "invalid calibration data: three reliable timing references were not found",
                "No sound picked up. Wired and unmuted?",
                true,
            ),
            (
                "invalid calibration data: invalid calibration data: clipped recording",
                "Clipped. Lower the mic gain or test level.",
                true,
            ),
            (
                "excited-band acquisition signal/noise below 15 dB; diagnostic result only",
                "Too noisy to trust.",
                false,
            ),
            (
                "integrated level-band signal/noise is 18.5 dB (minimum 20 dB for correction)",
                "Too noisy to trust.",
                false,
            ),
            (
                "arrival changed by 0.410 ms between repeats (limit 0.150 ms)",
                "Timing moved between runs. Keep still and measure again.",
                false,
            ),
            (
                "level changed by 0.62 dB between repeats (limit 0.50 dB)",
                "Level moved between runs. Keep still and measure again.",
                false,
            ),
            (
                "reference level changed between repeats",
                "Reference speaker level drifted. Keep the mic gain fixed.",
                false,
            ),
            (
                "speaker: early peaks disagree across frequency bands or detection thresholds",
                "No clear arrival. Echoes or an obstacle?",
                false,
            ),
            (
                "two independent complete target captures are required",
                "Not measured twice. Measure again.",
                false,
            ),
            ("something new", "Not reliable enough to correct.", false),
        ] {
            let reason = human_reason(raw);
            assert_eq!((reason.text, reason.severe), (text, severe), "{raw}");
            assert!(!reason.raw.starts_with("invalid calibration data"));
            assert!(raw.ends_with(reason.raw));
        }
    }
}
