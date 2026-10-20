//! Top view of the room: where each speaker is around the listener, which one is
//! playing, which are set aside and which is the timing reference. Azimuths always come
//! from Omniphony's layout. Radii are the layout's until an alignment measured them:
//! then they are acoustic distances from arrival times, which include each output's
//! electronic latency. Display only: nothing here is sent to the renderer.
use companion_service::{quality::Grade, room::LAYOUT_MISMATCH_M};
use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2,
};

/// Largest side of the square map.
pub const SIDE: f32 = 220.0;
/// Smallest side, below which twelve chips no longer fit.
pub const MIN_SIDE: f32 = 150.0;
/// The card frame's margins on both sides of the map.
pub const CARD_MARGINS: f32 = 28.0;
/// The card's margins, title, legend and spacing around the map.
const CARD_EXTRA_HEIGHT: f32 = 88.0;
const CHIP_HEIGHT: f32 = 20.0;
const CHAR_WIDTH: f32 = 6.8;
const GAP: f32 = 3.0;
const STRIP_HEIGHT: f32 = 26.0;
/// Speakers higher or lower than this are drawn on the height layer.
const ELEVATION_MARK_DEG: f32 = 10.0;
/// Speakers this close to straight above or below have no direction on a top view.
const OFF_PLAN_DEG: f32 = 80.0;
/// No spot is drawn nearer the listener than this, so the head stays visible.
const MIN_SPOT_PX: f32 = 18.0;
/// Chips move off their spot in steps of this many pixels, at most `RINGS` steps away.
const RING_STEP: f32 = 5.0;
const RINGS: usize = 18;
/// One pulse of the speaker being measured.
const PULSE_SECONDS: f64 = 1.2;

/// Where the renderer's layout places one output, seen from the listening position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// 0° front, negative to the left.
    pub azimuth_deg: f32,
    /// 0° at ear level, positive up.
    pub elevation_deg: f32,
    /// `None` when the layout does not state it: drawn on the fallback ring.
    pub distance_m: Option<f32>,
}

impl Position {
    /// The contract's optional fields; no azimuth means the speaker cannot be placed.
    pub fn from_output(output: &omniphony_adapter::protocol::Output) -> Option<Self> {
        Some(Self {
            azimuth_deg: output.azimuth_deg.filter(|v| v.is_finite())?,
            elevation_deg: output
                .elevation_deg
                .filter(|v| v.is_finite())
                .unwrap_or(0.0),
            distance_m: output.distance_m.filter(|v| v.is_finite() && *v > 0.0),
        })
    }
    /// Whether a top view can show its direction.
    fn on_plan(self) -> bool {
        self.elevation_deg.abs() < OFF_PLAN_DEG
    }
    /// A distance seen from above: its projection on the floor.
    fn horizontal(self, distance_m: f32) -> f32 {
        distance_m * self.elevation_deg.to_radians().cos()
    }
    fn layer(self) -> Layer {
        if self.elevation_deg >= ELEVATION_MARK_DEG {
            Layer::Above
        } else if self.elevation_deg <= -ELEVATION_MARK_DEG {
            Layer::Below
        } else {
            Layer::Ear
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Ear,
    Above,
    Below,
}

/// What an alignment measured for one speaker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Measured {
    /// Acoustic distance in metres, anchored on the reference.
    Distance(f32),
    /// Its arrival is far from the group: another output device, distance unknown.
    OtherDevice,
}

/// One speaker as the map shows it.
pub struct Chip<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub position: Option<Position>,
    pub measured: Option<Measured>,
    /// Weakest quality grade, as on the speaker card.
    pub grade: Option<Grade>,
    pub selected: bool,
    /// Playing or being captured now: the chip pulses.
    pub active: bool,
    pub reference: bool,
    /// Why the speaker is dimmed: set aside before capture or excluded from correction.
    pub dimmed: Option<String>,
}

impl Chip<'_> {
    fn plan(&self) -> Option<Position> {
        self.position.filter(|p| p.on_plan())
    }
    fn layout_m(&self) -> Option<f32> {
        let p = self.plan()?;
        Some(p.horizontal(p.distance_m?))
    }
    fn measured_m(&self) -> Option<f32> {
        match self.measured {
            Some(Measured::Distance(d)) => Some(self.plan()?.horizontal(d)),
            _ => None,
        }
    }
    fn other_device(&self) -> bool {
        self.plan().is_some() && self.measured == Some(Measured::OtherDevice)
    }
    /// The layout's spot, drawn as a hollow ring when the measured distance is more
    /// than [`LAYOUT_MISMATCH_M`] away from it.
    fn ghost_m(&self) -> Option<f32> {
        let (Some(Measured::Distance(d)), Some(layout)) =
            (self.measured, self.plan().and_then(|p| p.distance_m))
        else {
            return None;
        };
        ((d - layout).abs() > LAYOUT_MISMATCH_M as f32)
            .then(|| self.layout_m())
            .flatten()
    }
}

/// The measured-distance scale and what its control shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Scale {
    /// The timing reference's short name.
    pub reference: String,
    /// The reference distance in use, in metres.
    pub anchor_m: f32,
    /// The shortest one that keeps every measured speaker in front of the listener.
    pub min_m: f32,
    /// Whether the user entered `anchor_m`; otherwise it is fitted to the layout.
    pub entered: bool,
    /// False when the reference plays through another device: only the fit applies.
    pub editable: bool,
}

/// What the user did on the map.
#[derive(Debug, Default, PartialEq)]
pub struct MapResponse {
    /// The speaker whose chip was clicked.
    pub clicked: Option<String>,
    /// A new reference distance, or `Some(None)` to fit to the layout again.
    pub reference_m: Option<Option<f32>>,
}

/// A short label that fits a chip: layout names (FL, TFL, LFE) as they are, the
/// initials of longer names ("Front left" → FL) keeping numbers whole ("spk-12" →
/// S12), or the start of one long word.
pub fn short_name(name: &str) -> String {
    let name = name.trim();
    if name.chars().count() <= 4 {
        return name.to_owned();
    }
    let words: Vec<_> = name
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter(|w| !w.is_empty())
        .collect();
    if words.len() > 1 {
        let mut short = String::new();
        for w in words {
            if w.chars().all(|c| c.is_ascii_digit()) {
                short.push_str(w);
            } else {
                short.extend(w.chars().next().into_iter().flat_map(char::to_uppercase));
            }
        }
        short.chars().take(4).collect()
    } else {
        name.chars().take(3).collect()
    }
}

fn chip_size(label: &str) -> Vec2 {
    Vec2::new(
        (label.chars().count() as f32 * CHAR_WIDTH + 24.0).max(30.0),
        CHIP_HEIGHT,
    )
}

/// Map scale: listener centre, outer radius and pixels per metre when distances are known.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub center: Pos2,
    pub radius: f32,
    pub pixels_per_m: Option<f32>,
    /// Where speakers without a stated distance sit.
    pub fallback_radius: f32,
    /// Some radius was measured: the rings are labelled in metres.
    pub measured: bool,
}

impl Geometry {
    /// Scaled so the farthest floor distance, measured or from the layout, fits.
    pub fn new(area: Rect, chips: &[Chip]) -> Self {
        let radius = (area.width().min(area.height()) / 2.0 - CHIP_HEIGHT).max(20.0);
        let mut layout: Vec<f32> = chips.iter().filter_map(Chip::layout_m).collect();
        layout.sort_by(f32::total_cmp);
        let farthest = chips
            .iter()
            .filter_map(Chip::measured_m)
            .chain(layout.last().copied())
            .fold(0.0_f32, f32::max);
        let pixels_per_m = (farthest > 0.0).then(|| radius * 0.9 / farthest);
        let fallback_radius = match (pixels_per_m, layout.get(layout.len() / 2)) {
            (Some(scale), Some(median)) => (median * scale).clamp(radius * 0.35, radius * 0.9),
            _ => radius * 0.75,
        };
        Self {
            center: area.center(),
            radius,
            pixels_per_m,
            fallback_radius,
            measured: chips.iter().any(|c| c.measured_m().is_some()),
        }
    }

    /// Front is up and the right is to the right.
    fn point(&self, azimuth_deg: f32, r: f32) -> Pos2 {
        let az = azimuth_deg.to_radians();
        self.center + Vec2::new(r * az.sin(), -r * az.cos())
    }

    fn at_distance(&self, azimuth_deg: f32, horizontal_m: Option<f32>) -> Pos2 {
        let r = match (horizontal_m, self.pixels_per_m) {
            (Some(d), Some(scale)) => (d * scale).clamp(MIN_SPOT_PX, self.radius),
            _ => self.fallback_radius,
        };
        self.point(azimuth_deg, r)
    }

    /// Where the speaker really is on the floor plan: the measured distance, else the
    /// layout's, both projected on the floor; another device sits on the rim.
    fn spot(&self, chip: &Chip) -> Option<Pos2> {
        let p = chip.plan()?;
        Some(if chip.other_device() {
            self.point(p.azimuth_deg, self.radius)
        } else {
            self.at_distance(p.azimuth_deg, chip.measured_m().or(chip.layout_m()))
        })
    }

    fn ghost(&self, chip: &Chip) -> Option<Pos2> {
        Some(self.at_distance(chip.plan()?.azimuth_deg, Some(chip.ghost_m()?)))
    }
}

/// Speakers drawn as one chip: those at the same spot with the same layout position.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Indices into the chips, in layout order.
    pub members: Vec<usize>,
    pub spot: Pos2,
    /// The layout's spot when the measured distance differs from it.
    pub ghost: Option<Pos2>,
    pub label: String,
    layer: Layer,
}

/// The map's drawing plan. Depends only on positions, measured distances, names and
/// size, so the progress of a capture can never move a chip.
#[derive(Debug, Clone)]
pub struct Plan {
    pub geometry: Geometry,
    pub groups: Vec<Group>,
    /// One chip rectangle per group, never overlapping one another or the listener.
    pub rects: Vec<Rect>,
    /// Chips with no direction on a top view, in layout order: no reported position, or
    /// straight above or below the listener (LFE).
    pub off_plan: Vec<usize>,
}

pub fn layout(chips: &[Chip], area: Rect) -> Plan {
    let geometry = Geometry::new(area, chips);
    let mut groups: Vec<Group> = Vec::new();
    let mut off_plan = Vec::new();
    for (i, chip) in chips.iter().enumerate() {
        let Some(spot) = geometry.spot(chip) else {
            off_plan.push(i);
            continue;
        };
        let same = |g: &Group| {
            let first = &chips[g.members[0]];
            first.plan() == chip.plan()
                && first.other_device() == chip.other_device()
                && (g.spot - spot).length() < 1.5
        };
        match groups.iter_mut().find(|g| same(g)) {
            Some(g) => {
                g.members.push(i);
                g.label = format!("{}·{}", g.label, short_name(chip.name));
            }
            None => groups.push(Group {
                members: vec![i],
                spot,
                ghost: geometry.ghost(chip),
                label: short_name(chip.name),
                layer: chip.plan().map_or(Layer::Ear, Position::layer),
            }),
        }
    }
    let rects = place(&groups, area, geometry.center);
    Plan {
        geometry,
        groups,
        rects,
        off_plan,
    }
}

/// The listener's head and nose, which no chip may cover.
fn listener(center: Pos2) -> Rect {
    Rect::from_min_max(
        center + Vec2::new(-10.0, -18.0),
        center + Vec2::new(10.0, 9.0),
    )
}

/// Greedy anchored placement: each chip takes the nearest free place to its spot,
/// trying its own spot first, then rings of 16 directions starting outward from the
/// listener. Ear-level speakers go first, so they keep their spot and heights move.
/// A place is free when it overlaps no placed chip and not the listener, and on a
/// first pass when it hides no other spot of its layer or of the ear level.
/// Deterministic.
fn place(groups: &[Group], area: Rect, center: Pos2) -> Vec<Rect> {
    let inside = |r: Rect| {
        let dx = (area.left() - r.left()).max(0.0) + (area.right() - r.right()).min(0.0);
        let dy = (area.top() - r.top()).max(0.0) + (area.bottom() - r.bottom()).min(0.0);
        r.translate(Vec2::new(dx, dy))
    };
    let head = listener(center);
    let rank = |g: &Group| u8::from(g.layer != Layer::Ear);
    let mut order: Vec<_> = (0..groups.len()).collect();
    order.sort_by_key(|&i| (rank(&groups[i]), i));
    let mut rects = vec![Rect::NOTHING; groups.len()];
    let mut placed: Vec<Rect> = Vec::with_capacity(groups.len());
    for i in order {
        let g = &groups[i];
        let size = chip_size(&g.label);
        let out = g.spot - center;
        let base = if out.length() > 0.5 {
            out.y.atan2(out.x)
        } else {
            -std::f32::consts::FRAC_PI_2
        };
        let candidates = std::iter::once(Vec2::ZERO).chain((1..=RINGS).flat_map(move |k| {
            (0..16).map(move |n| {
                let turn = ((n + 1) / 2) as f32 * std::f32::consts::FRAC_PI_8;
                let sign = if n % 2 == 1 { 1.0 } else { -1.0 };
                Vec2::angled(base + sign * turn) * (k as f32 * RING_STEP)
            })
        }));
        let free = |r: Rect, strict: bool| {
            let grown = r.expand(GAP / 2.0);
            !r.intersects(head)
                && placed
                    .iter()
                    .all(|p| !p.expand(GAP / 2.0).intersects(grown))
                && (!strict
                    || groups.iter().enumerate().all(|(j, o)| {
                        j == i
                            || (o.spot - g.spot).length() < 3.0
                            || rank(o) > rank(g)
                            || !r.expand(2.0).contains(o.spot)
                    }))
        };
        let at = |offset: Vec2| inside(Rect::from_center_size(g.spot + offset, size));
        // A crowded small map: the free place anywhere on the map nearest the spot.
        let anywhere = || {
            let steps = |len: f32, size: f32| 0..=((len - size) / 4.0).max(0.0) as usize;
            steps(area.height(), size.y)
                .flat_map(|y| {
                    steps(area.width(), size.x).map(move |x| {
                        Rect::from_min_size(area.min + Vec2::new(x as f32, y as f32) * 4.0, size)
                    })
                })
                .filter(|r| free(*r, false))
                .min_by(|a, b| {
                    let d = |r: &Rect| (r.center() - g.spot).length_sq();
                    d(a).total_cmp(&d(b))
                })
        };
        let rect = [true, false]
            .into_iter()
            .find_map(|strict| candidates.clone().map(at).find(|r| free(*r, strict)))
            .or_else(anywhere)
            .unwrap_or_else(|| at(Vec2::ZERO));
        rects[i] = rect;
        placed.push(rect);
    }
    separate(&mut rects, area, head);
    rects
}

/// The last resort of a map too small for its chips: each overlapping pair moves apart
/// along the axis that needs the shorter move, and a chip on the listener moves off it.
/// Leaves already separate chips where they are.
fn separate(rects: &mut [Rect], area: Rect, head: Rect) {
    let keep_inside = |r: Rect| {
        let dx = (area.left() - r.left()).max(0.0) + (area.right() - r.right()).min(0.0);
        let dy = (area.top() - r.top()).max(0.0) + (area.bottom() - r.bottom()).min(0.0);
        r.translate(Vec2::new(dx, dy))
    };
    let push = |a: Rect, b: Rect| {
        let (a2, b2) = (a.expand(GAP / 2.0), b.expand(GAP / 2.0));
        let overlap = Vec2::new(
            a2.right().min(b2.right()) - a2.left().max(b2.left()),
            a2.bottom().min(b2.bottom()) - a2.top().max(b2.top()),
        );
        let d = b.center() - a.center();
        // Coincident chips: the later one goes right or down.
        if overlap.x < overlap.y {
            Vec2::new(if d.x < 0.0 { -1.0 } else { 1.0 } * overlap.x, 0.0)
        } else {
            Vec2::new(0.0, if d.y < 0.0 { -1.0 } else { 1.0 } * overlap.y)
        }
    };
    for _ in 0..64 {
        let mut moved = false;
        for i in 0..rects.len() {
            if rects[i].intersects(head) {
                rects[i] = keep_inside(rects[i].translate(push(head, rects[i])));
                moved = true;
            }
            for j in i + 1..rects.len() {
                let (a, b) = (rects[i], rects[j]);
                if !a.expand(GAP / 2.0).intersects(b.expand(GAP / 2.0)) {
                    continue;
                }
                let step = push(a, b) / 2.0;
                rects[i] = keep_inside(a.translate(-step));
                rects[j] = keep_inside(b.translate(step));
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
}

/// A plain-language layout position for the tooltip.
pub fn describe(p: Position) -> String {
    let az = p.azimuth_deg;
    let side = if az.abs() < 2.5 {
        "front".to_owned()
    } else if az.abs() > 177.5 {
        "behind".to_owned()
    } else {
        format!(
            "{:.0}° {}",
            az.abs(),
            if az < 0.0 { "left" } else { "right" }
        )
    };
    let height = if p.elevation_deg.abs() < 2.5 {
        "ear level".to_owned()
    } else {
        format!(
            "{:.0}° {}",
            p.elevation_deg.abs(),
            if p.elevation_deg > 0.0 { "up" } else { "down" }
        )
    };
    let distance = p
        .distance_m
        .map_or("layout distance not stated".to_owned(), |d| {
            format!("layout {d:.1} m")
        });
    format!("{side} · {height} · {distance}")
}

/// What the map knows about one speaker's distance, in words.
fn distance_lines(chip: &Chip) -> Vec<String> {
    let layout = chip.position.and_then(|p| p.distance_m);
    match chip.measured {
        Some(Measured::Distance(d)) => {
            let mut lines = vec![format!(
                "{d:.2} m: measured acoustic distance (includes electronic latency)"
            )];
            if chip.ghost_m().is_some() {
                lines.push(format!(
                    "The layout says {:.2} m: hollow ring",
                    layout.unwrap_or_default()
                ));
            }
            lines
        }
        Some(Measured::OtherDevice) => vec![
            "Another device — distance unknown: it arrives far from the other speakers, so its latency hides the distance.".into(),
        ],
        None if chip.position.is_some() => vec!["Distance from Omniphony's layout, not measured".into()],
        None => vec![],
    }
}

fn tooltip(chips: &[&Chip]) -> String {
    let mut lines = Vec::new();
    if let [a, rest @ ..] = chips
        && !rest.is_empty()
    {
        let names: Vec<_> = chips.iter().map(|c| c.name).collect();
        lines.push(names.join(" · "));
        lines.push("These speakers share one layout position: drawn as one chip.".into());
        if let Some(p) = a.position {
            lines.push(describe(p));
        }
    }
    let merged = chips.len() > 1;
    for chip in chips {
        let prefix = |line: String| {
            if merged {
                format!("{}: {line}", chip.name)
            } else {
                line
            }
        };
        if !merged {
            lines.push(chip.name.to_owned());
            lines.push(match chip.position {
                Some(p) => describe(p),
                None => "Omniphony did not report a position".into(),
            });
            if let Some(p) = chip.position.filter(|p| !p.on_plan()) {
                lines.push(format!(
                    "Straight {} the listener: no direction on a top view",
                    if p.elevation_deg > 0.0 {
                        "above"
                    } else {
                        "below"
                    }
                ));
            }
        }
        for line in distance_lines(chip) {
            lines.push(prefix(line));
        }
        if chip.active {
            lines.push(prefix("Playing now".into()));
        }
        if chip.reference {
            lines.push(prefix(
                "Timing reference: arrivals are measured against this speaker.".into(),
            ));
        }
        if let Some(reason) = &chip.dimmed {
            lines.push(prefix(reason.clone()));
        }
        lines.push(prefix(match chip.grade {
            Some(grade) => format!("Weakest quality: {}", grade.label()),
            None => "Not rated".into(),
        }));
    }
    lines.join("\n")
}

/// Distance ring step: about three rings out to the farthest speaker.
fn ring_step(max_m: f32) -> f32 {
    let raw = max_m / 3.0;
    [0.25, 0.5, 1.0, 2.0, 2.5, 5.0, 10.0, 20.0, 50.0]
        .into_iter()
        .find(|s| *s >= raw)
        .unwrap_or(100.0)
}

fn dashed_circle(painter: &egui::Painter, center: Pos2, radius: f32, stroke: Stroke) {
    let points: Vec<_> = (0..=96)
        .map(|i| {
            let a = i as f32 / 96.0 * std::f32::consts::TAU;
            center + Vec2::new(radius * a.cos(), radius * a.sin())
        })
        .collect();
    painter.extend(Shape::dashed_line(&points, stroke, 3.0, 4.0));
}

fn triangle(painter: &egui::Painter, tip: Pos2, up: bool, color: Color32) {
    let s = if up { 1.0 } else { -1.0 };
    painter.add(Shape::convex_polygon(
        vec![
            tip,
            tip + Vec2::new(3.5, 5.0 * s),
            tip + Vec2::new(-3.5, 5.0 * s),
        ],
        color,
        Stroke::NONE,
    ));
}

/// The true spot: a dot at ear level, a triangle pointing up or down on the height layer.
fn spot_marker(painter: &egui::Painter, at: Pos2, layer: Layer, color: Color32) {
    match layer {
        Layer::Ear => {
            painter.circle_filled(at, 2.5, color);
        }
        Layer::Above => triangle(painter, at + Vec2::new(0.0, -3.0), true, color),
        Layer::Below => triangle(painter, at + Vec2::new(0.0, 3.0), false, color),
    }
}

/// What one chip shows for its speakers: the weakest grade, any selected, playing or
/// reference, dimmed only when all of them are.
struct Face<'a> {
    label: &'a str,
    grade: Option<Grade>,
    selected: bool,
    active: bool,
    reference: bool,
    dimmed: bool,
    layer: Layer,
}

impl<'a> Face<'a> {
    fn new(label: &'a str, members: &[&Chip], layer: Layer) -> Self {
        Self {
            label,
            grade: members.iter().filter_map(|c| c.grade).min(),
            selected: members.iter().any(|c| c.selected),
            active: members.iter().any(|c| c.active),
            reference: members.iter().any(|c| c.reference),
            dimmed: members.iter().all(|c| c.dimmed.is_some()),
            layer,
        }
    }
}

/// One chip: quality dot, short name, selection outline, elevation marker,
/// reference label, and a pulse while it plays.
fn paint_chip(painter: &egui::Painter, rect: Rect, chip: &Face, pulse: f32) {
    let opacity = if chip.dimmed { 0.35 } else { 1.0 };
    if chip.active {
        let grow = 2.0 + 7.0 * pulse;
        painter.rect_stroke(
            rect.expand(grow),
            CHIP_HEIGHT / 2.0 + grow,
            Stroke::new(2.0_f32, super::ACCENT.gamma_multiply(1.0 - pulse)),
            egui::StrokeKind::Outside,
        );
    }
    let fill = if chip.active {
        super::ACCENT.gamma_multiply(0.22 + 0.18 * (1.0 - pulse))
    } else if chip.selected {
        super::SELECTED_CARD
    } else {
        super::PANEL
    };
    let stroke = if chip.selected {
        Stroke::new(1.5_f32, super::ACCENT)
    } else {
        Stroke::new(1.0_f32, super::MUTED.gamma_multiply(0.45 * opacity))
    };
    painter.rect(
        rect,
        CHIP_HEIGHT / 2.0,
        fill.gamma_multiply(opacity),
        stroke,
        egui::StrokeKind::Inside,
    );
    super::quality_view::paint_dot(
        painter,
        Pos2::new(rect.left() + 9.0, rect.center().y),
        3.5,
        chip.grade,
        opacity,
    );
    painter.text(
        Pos2::new(rect.left() + 16.0, rect.center().y),
        Align2::LEFT_CENTER,
        chip.label,
        FontId::monospace(11.0),
        super::TEXT.gamma_multiply(opacity),
    );
    let ink = super::TEXT.gamma_multiply(0.75 * opacity);
    match chip.layer {
        Layer::Above => triangle(painter, rect.right_top() + Vec2::new(-6.0, -6.0), true, ink),
        Layer::Below => triangle(
            painter,
            rect.right_bottom() + Vec2::new(-6.0, 6.0),
            false,
            ink,
        ),
        Layer::Ear => {}
    }
}

/// The reference label under its chip, drawn over the other chips so it stays legible.
fn paint_reference(painter: &egui::Painter, rect: Rect) {
    let galley = painter.layout_no_wrap("REF".into(), FontId::proportional(9.5), super::ACCENT);
    let at = rect.center_bottom() + Vec2::new(-galley.size().x / 2.0, 1.0);
    painter.rect_filled(
        Rect::from_min_size(at, galley.size()).expand2(Vec2::new(2.0, 0.0)),
        3.0,
        super::CARD,
    );
    painter.galley(at, galley, super::ACCENT);
}

/// The nearest point of `rect` to `p`: where a leader line meets its chip.
fn nearest(rect: Rect, p: Pos2) -> Pos2 {
    Pos2::new(
        p.x.clamp(rect.left(), rect.right()),
        p.y.clamp(rect.top(), rect.bottom()),
    )
}

/// The text of the measured-scale control in the map's corner.
fn scale_caption(scale: &Scale) -> String {
    if scale.entered {
        format!("{} at {:.2} m", scale.reference, scale.anchor_m)
    } else {
        "fitted to layout".to_owned()
    }
}

/// The reference distance editor, opened from the map's corner.
fn scale_editor(ui: &mut egui::Ui, scale: &Scale) -> Option<Option<f32>> {
    let mut change = None;
    ui.set_max_width(250.0);
    if scale.editable {
        ui.label(format!(
            "Distance from the listener to {}, the timing reference",
            scale.reference
        ));
        let mut value = scale.anchor_m;
        if ui
            .add(
                egui::DragValue::new(&mut value)
                    .range(scale.min_m..=30.0)
                    .speed(0.01)
                    .max_decimals(2)
                    .suffix(" m"),
            )
            .changed()
        {
            change = Some(Some(value));
        }
    } else {
        ui.label(format!(
            "{} plays through another device: distances are fitted to the layout.",
            scale.reference
        ));
    }
    if scale.entered && ui.button("Fit to the layout again").clicked() {
        change = Some(None);
    }
    ui.label(
        RichText::new(
            "Measured distances are arrival times minus each output's delay, at 343 m/s: they include each output's electronic latency. A hollow ring marks the layout's distance when they differ by more than 0.3 m. Display only: never sent to Omniphony.",
        )
        .small()
        .color(super::MUTED),
    );
    change
}

/// Draw the map into `area` and the off-plan strip below it when needed.
fn paint(ui: &mut egui::Ui, chips: &[Chip], side: f32, scale: Option<&Scale>) -> MapResponse {
    // The strip depends on reported positions only, never on measurements.
    let strip = chips.iter().any(|c| c.plan().is_none());
    let height = side + if strip { STRIP_HEIGHT } else { 0.0 };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(side, height), Sense::click());
    let area = Rect::from_min_size(rect.min, Vec2::splat(side));
    let plan = layout(chips, area);
    let g = plan.geometry;
    // The strip holds speakers with no direction on a top view, in layout order.
    let mut strip_rects = Vec::new();
    let mut x = area.left() + 56.0;
    for &i in &plan.off_plan {
        let size = chip_size(&short_name(chips[i].name));
        if x + size.x <= rect.right() {
            strip_rects.push((
                i,
                Rect::from_min_size(
                    Pos2::new(x, area.bottom() + (STRIP_HEIGHT - size.y) / 2.0 + 2.0),
                    size,
                ),
            ));
            x += size.x + 4.0;
        }
    }
    let members = |g: &Group| -> Vec<&Chip> { g.members.iter().map(|&i| &chips[i]).collect() };
    // Hits: a plan chip, a strip chip or a layout ring, as the chips they stand for.
    let hit = |p: Pos2| -> Option<Vec<usize>> {
        plan.rects
            .iter()
            .zip(&plan.groups)
            .rev()
            .find(|(r, _)| r.contains(p))
            .map(|(_, g)| g.members.clone())
            .or_else(|| {
                strip_rects
                    .iter()
                    .find(|(_, r)| r.contains(p))
                    .map(|(i, _)| vec![*i])
            })
            .or_else(|| {
                plan.groups
                    .iter()
                    .find(|g| g.ghost.is_some_and(|q| (q - p).length() < 6.0))
                    .map(|g| g.members.clone())
            })
    };
    let hovered = response.hover_pos().and_then(hit);
    // A merged chip selects its next speaker on each click.
    let clicked = response
        .clicked()
        .then(|| response.interact_pointer_pos())
        .flatten()
        .and_then(hit)
        .map(|m| {
            let next = m
                .iter()
                .position(|&i| chips[i].selected)
                .map_or(0, |k| (k + 1) % m.len());
            chips[m[next]].id.to_owned()
        });
    let caption = scale.map(|s| {
        let galley =
            ui.painter()
                .layout_no_wrap(scale_caption(s), FontId::proportional(9.5), super::ACCENT);
        let r = Rect::from_min_size(
            area.left_bottom() - Vec2::new(0.0, galley.size().y + 1.0),
            galley.size(),
        );
        (
            galley,
            ui.interact(r, ui.id().with("room_scale"), Sense::click()),
        )
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect.expand(2.0));
        let faint = super::MUTED.gamma_multiply(0.16);
        let label_ink = super::MUTED.gamma_multiply(0.55);
        if let Some(scale) = g.pixels_per_m {
            let max_m = g.radius / scale;
            let step = ring_step(max_m);
            let mut k = 1;
            while k as f32 * step <= max_m * 1.02 {
                let r = k as f32 * step * scale;
                painter.circle_stroke(g.center, r, Stroke::new(1.0_f32, faint));
                // Metres only for measured radii: the layout's are normalised geometry.
                // Labelled behind the listener, where layouts rarely put a speaker.
                if g.measured {
                    painter.text(
                        g.center + Vec2::new(3.0, r),
                        Align2::LEFT_BOTTOM,
                        format!("{} m", k as f32 * step),
                        FontId::proportional(9.0),
                        label_ink,
                    );
                }
                k += 1;
            }
        } else {
            painter.circle_stroke(g.center, g.radius, Stroke::new(1.0_f32, faint));
        }
        if chips
            .iter()
            .any(|c| c.plan().is_some_and(|p| p.distance_m.is_none()) && c.measured.is_none())
        {
            dashed_circle(
                &painter,
                g.center,
                g.fallback_radius,
                Stroke::new(1.0_f32, super::MUTED.gamma_multiply(0.4)),
            );
        }
        if chips.iter().any(Chip::other_device) {
            dashed_circle(
                &painter,
                g.center,
                g.radius,
                Stroke::new(1.0_f32, super::WARM.gamma_multiply(0.45)),
            );
            let rim = painter.layout_no_wrap(
                "rim: another device".into(),
                FontId::proportional(9.0),
                super::WARM.gamma_multiply(0.75),
            );
            // Only beside the scale control, never over it.
            let taken = caption.as_ref().map_or(0.0, |(c, _)| c.size().x + 8.0);
            if taken + rim.size().x <= area.width() {
                painter.galley(area.right_bottom() - rim.size(), rim, super::WARM);
            }
        }
        painter.text(
            Pos2::new(g.center.x, area.top()),
            Align2::CENTER_TOP,
            "FRONT",
            FontId::proportional(9.5),
            super::MUTED,
        );
        // The listener: a head facing the front.
        painter.circle_stroke(
            g.center,
            6.0,
            Stroke::new(1.5_f32, super::TEXT.gamma_multiply(0.7)),
        );
        triangle(
            &painter,
            g.center + Vec2::new(0.0, -11.0),
            true,
            super::TEXT.gamma_multiply(0.7),
        );
        // Layout rings, leader lines and true spots under the chips.
        for (group, chip) in plan.groups.iter().zip(&plan.rects) {
            let dim = if members(group).iter().all(|c| c.dimmed.is_some()) {
                0.4
            } else {
                1.0
            };
            if let Some(ghost) = group.ghost {
                painter.extend(Shape::dashed_line(
                    &[ghost, group.spot],
                    Stroke::new(1.0_f32, super::MUTED.gamma_multiply(0.4 * dim)),
                    2.0,
                    3.0,
                ));
                painter.circle_stroke(
                    ghost,
                    4.0,
                    Stroke::new(1.2_f32, super::MUTED.gamma_multiply(0.8 * dim)),
                );
            }
            if !chip.expand(1.0).contains(group.spot) {
                painter.line_segment(
                    [group.spot, nearest(*chip, group.spot)],
                    Stroke::new(1.0_f32, super::MUTED.gamma_multiply(0.5 * dim)),
                );
            }
            spot_marker(
                &painter,
                group.spot,
                group.layer,
                super::TEXT.gamma_multiply(0.8 * dim),
            );
        }
        if let Some(reference) = plan
            .groups
            .iter()
            .find(|g| members(g).iter().any(|c| c.reference))
        {
            painter.extend(Shape::dashed_line(
                &[g.center, reference.spot],
                Stroke::new(1.2_f32, super::ACCENT.gamma_multiply(0.55)),
                3.0,
                3.0,
            ));
        }
        if strip {
            painter.text(
                Pos2::new(area.left(), area.bottom() + STRIP_HEIGHT / 2.0 + 2.0),
                Align2::LEFT_CENTER,
                "Off plan",
                FontId::proportional(10.5),
                super::MUTED,
            );
        }
        let phase = (ui.input(|i| i.time) / PULSE_SECONDS).fract() as f32;
        let mut faces: Vec<(Rect, Face)> = plan
            .groups
            .iter()
            .zip(&plan.rects)
            .map(|(group, r)| (*r, Face::new(&group.label, &members(group), group.layer)))
            .collect();
        let strip_labels: Vec<_> = strip_rects
            .iter()
            .map(|(i, _)| short_name(chips[*i].name))
            .collect();
        for ((i, r), label) in strip_rects.iter().zip(&strip_labels) {
            let layer = chips[*i].position.map_or(Layer::Ear, Position::layer);
            faces.push((*r, Face::new(label, &[&chips[*i]], layer)));
        }
        // Dimmed chips first, so a speaker in use is never hidden under one set aside.
        faces.sort_by_key(|(_, f)| (!f.dimmed, f.selected, f.active));
        for (r, face) in &faces {
            paint_chip(&painter, *r, face, phase);
        }
        for (r, _) in faces.iter().filter(|(_, f)| f.reference) {
            paint_reference(&painter, *r);
        }
        if let Some((galley, _)) = &caption {
            let r = area.left_bottom() - Vec2::new(0.0, galley.size().y + 1.0);
            painter.galley(r, galley.clone(), super::ACCENT);
        }
    }
    let mut out = MapResponse {
        clicked,
        reference_m: None,
    };
    if let (Some((_, caption)), Some(scale)) = (caption, scale) {
        let caption = caption.on_hover_text("Measured distances: change how they are anchored");
        egui::Popup::menu(&caption)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| out.reference_m = scale_editor(ui, scale));
    }
    if let Some(members) = hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        let chips: Vec<&Chip> = members.iter().map(|&i| &chips[i]).collect();
        response.on_hover_text(tooltip(&chips));
    }
    out
}

/// One legend entry, allocated as a single widget so wrapping keeps icon and text together.
fn legend_item(ui: &mut egui::Ui, kind: u8, text: &str) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::TextStyle::Small.resolve(ui.style()),
        super::MUTED,
    );
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(20.0 + galley.size().x, galley.size().y.max(14.0)),
        Sense::hover(),
    );
    if !ui.is_rect_visible(rect) {
        return;
    }
    let p = ui.painter();
    let c = Pos2::new(rect.left() + 8.0, rect.center().y);
    match kind {
        0 => {
            p.circle_stroke(c, 5.0, Stroke::new(2.0_f32, super::ACCENT));
        }
        1 => {
            p.text(
                c,
                Align2::CENTER_CENTER,
                "REF",
                FontId::proportional(8.5),
                super::ACCENT,
            );
        }
        2 => {
            p.circle_filled(c, 5.0, super::MUTED.gamma_multiply(0.35));
        }
        3 => {
            let ink = super::TEXT.gamma_multiply(0.75);
            triangle(p, c + Vec2::new(-3.0, -5.0), true, ink);
            triangle(p, c + Vec2::new(4.0, 5.0), false, ink);
        }
        _ => dashed_circle(p, c, 5.0, Stroke::new(1.0_f32, super::MUTED)),
    }
    p.galley(
        Pos2::new(rect.left() + 20.0, rect.center().y - galley.size().y / 2.0),
        galley,
        super::MUTED,
    );
}

/// Legend entries depend on reported positions only, so the card's height never
/// follows a measurement.
fn legend(ui: &mut egui::Ui, chips: &[Chip]) {
    legend_item(ui, 0, "playing");
    legend_item(ui, 1, "reference");
    legend_item(ui, 2, "set aside");
    legend_item(ui, 3, "height layer");
    if chips
        .iter()
        .any(|c| c.plan().is_some_and(|p| p.distance_m.is_none()))
    {
        legend_item(ui, 4, "distance not stated");
    }
}

/// Whether the renderer placed any speaker; otherwise the map is not drawn.
pub fn available(chips: &[Chip]) -> bool {
    chips.iter().any(|c| c.position.is_some())
}

fn title(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Room").strong());
        ui.label(
            RichText::new("top view · front is up")
                .small()
                .color(super::MUTED),
        );
    })
    .response
    .on_hover_text(
        "Directions come from Omniphony's speaker layout. Distances are the layout's until an alignment measures them: then they are acoustic distances, which include each output's electronic latency.",
    );
}

/// Where the map card goes in the speaker workspace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// A right-hand column beside the speaker cards (and the chart when it is wide).
    Beside { side: f32 },
    /// Below the cards, when two card columns would not fit beside the map.
    Below { side: f32 },
}

/// Choose the placement from the workspace width and the card grid alone. Beside the
/// cards, the map is no taller than their grid unless it is at its smallest, so the
/// panels below the cards keep their place.
pub fn placement(width: f32, cards: usize, card_min: f32, card_height: f32, gap: f32) -> Placement {
    let column = |side: f32| width - side - CARD_MARGINS - gap;
    let columns = |w: f32| (((w + gap) / (card_min + gap)).floor() as usize).max(1);
    if columns(column(MIN_SIDE)) < 2 {
        return Placement::Below {
            side: (width * 0.4).clamp(MIN_SIDE, SIDE),
        };
    }
    let rows = cards.max(1).div_ceil(columns(column(MIN_SIDE)));
    let grid = rows as f32 * (card_height + gap) - gap;
    Placement::Beside {
        side: (grid - CARD_EXTRA_HEIGHT).clamp(MIN_SIDE, SIDE),
    }
}

/// The map card's contents: title, a `side`-wide square map, the off-plan strip and a
/// wrapped legend. Its height depends only on the outputs, never on capture state.
pub fn compact(ui: &mut egui::Ui, chips: &[Chip], side: f32, scale: Option<&Scale>) -> MapResponse {
    ui.set_width(side);
    title(ui);
    let response = paint(ui, chips, side, scale);
    ui.add_space(4.0);
    // Wrapped rows take their height from the interaction size, set before they start.
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 2.0);
        ui.spacing_mut().interact_size.y = 14.0;
        ui.horizontal_wrapped(|ui| legend(ui, chips));
    });
    response
}

/// The map with its legend to the right, below the cards in a narrow window.
pub fn wide(ui: &mut egui::Ui, chips: &[Chip], side: f32, scale: Option<&Scale>) -> MapResponse {
    let mut response = MapResponse::default();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(side);
            title(ui);
            response = paint(ui, chips, side, scale);
        });
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.add_space(24.0);
            legend(ui, chips);
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Directions from Omniphony's layout; distances measured once an alignment has run. The speaker being measured pulses; click one to see its result.",
                    )
                    .small()
                    .color(super::MUTED),
                )
                .wrap(),
            );
        });
    });
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chip<'a>(name: &'a str, position: Option<Position>) -> Chip<'a> {
        Chip {
            id: name,
            name,
            position,
            measured: None,
            grade: None,
            selected: false,
            active: false,
            reference: false,
            dimmed: None,
        }
    }
    fn at(azimuth_deg: f32, elevation_deg: f32, distance_m: Option<f32>) -> Option<Position> {
        Some(Position {
            azimuth_deg,
            elevation_deg,
            distance_m,
        })
    }
    fn area() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::splat(SIDE))
    }
    fn spot(chips: &[Chip], name: &str) -> Pos2 {
        let plan = layout(chips, area());
        let i = chips.iter().position(|c| c.name == name).unwrap();
        plan.groups
            .iter()
            .find(|g| g.members.contains(&i))
            .unwrap()
            .spot
    }
    /// The user's 14-output bench: Omniphony's normalised cube, two stacked pairs.
    fn bench() -> Vec<Chip<'static>> {
        [
            ("FL", -26.565, 0.0, 2.236),
            ("FR", 26.565, 0.0, 2.236),
            ("C", 0.0, 0.0, 2.0),
            ("BC", 180.0, -4.229, 1.003),
            ("BR", -135.0, 0.0, 1.414),
            ("BL", 135.0, 0.0, 1.414),
            ("TBL", -135.0, 35.264, 1.732),
            ("TBR", 135.0, 35.264, 1.732),
            ("TFL", -26.565, 24.095, 2.449),
            ("TFR", 26.565, 24.095, 2.449),
            ("SR", 135.0, 0.0, 1.414),
            ("LFE", 0.0, -90.0, 0.5),
            ("spk-12", -90.0, 64.179, 1.111),
            ("spk-13", 90.0, 61.26, 1.140),
        ]
        .into_iter()
        .map(|(name, az, el, d)| chip(name, at(az, el, Some(d))))
        .collect()
    }

    #[test]
    fn short_names_keep_layout_names_and_abbreviate_long_ones() {
        for (name, short) in [
            ("FL", "FL"),
            ("TFL", "TFL"),
            ("LFE", "LFE"),
            ("Front left", "FL"),
            ("Surround right", "SR"),
            ("top-front-left", "TFL"),
            ("Center", "Cen"),
            ("Subwoofer", "Sub"),
            ("spk-12", "S12"),
            ("spk-13", "S13"),
        ] {
            assert_eq!(short_name(name), short, "{name}");
        }
    }

    #[test]
    fn front_is_up_right_is_right_and_farther_is_outward() {
        let chips = [chip("X", at(0.0, 0.0, Some(3.0)))];
        let g = Geometry::new(area(), &chips);
        let c = g.center;
        let front = g.at_distance(0.0, Some(3.0));
        assert!((front.x - c.x).abs() < 1e-3 && front.y < c.y);
        let right = g.at_distance(90.0, Some(3.0));
        assert!(right.x > c.x && (right.y - c.y).abs() < 1e-3);
        let behind_left = g.at_distance(-135.0, Some(3.0));
        assert!(behind_left.x < c.x && behind_left.y > c.y);
        let near = g.at_distance(0.0, Some(1.5));
        assert!(near.y > front.y && near.y < c.y);
        // The farthest speaker sits inside the map, never on the listener.
        assert!((c - front).length() <= g.radius);
        let close = g.at_distance(0.0, Some(0.01));
        assert!((c - close).length() >= MIN_SPOT_PX - 1e-3);
    }

    #[test]
    fn the_top_view_projects_heights_onto_the_floor() {
        let chips = bench();
        // TFL is 2.45 m away but 24° up: on the floor it stands right above FL.
        let (fl, tfl) = (spot(&chips, "FL"), spot(&chips, "TFL"));
        assert!((fl - tfl).length() < 1.0, "{fl:?} {tfl:?}");
        // Heights are no longer pushed outward: the cube's top corner TBL stands
        // above the bed corner BR, not beyond it.
        let (br, tbl) = (spot(&chips, "BR"), spot(&chips, "TBL"));
        assert!((br - tbl).length() < 1.0, "{br:?} {tbl:?}");
    }

    #[test]
    fn the_lfe_and_speakers_without_a_position_leave_the_plan() {
        let mut chips = bench();
        chips.push(chip("Not reported", None));
        let plan = layout(&chips, area());
        let names: Vec<_> = plan.off_plan.iter().map(|&i| chips[i].name).collect();
        assert_eq!(names, ["LFE", "Not reported"]);
        assert!(plan.groups.iter().all(|g| !g.members.contains(&11)));
    }

    #[test]
    fn identical_layout_positions_share_one_chip_others_get_leader_lines() {
        let chips = bench();
        let plan = layout(&chips, area());
        let labels: Vec<_> = plan.groups.iter().map(|g| g.label.as_str()).collect();
        assert!(labels.contains(&"BL·SR"), "{labels:?}");
        assert!(!labels.contains(&"BL") && !labels.contains(&"SR"));
        assert_eq!(
            plan.groups.len(),
            12,
            "14 outputs, LFE off plan, BL·SR merged"
        );
        // Every chip is near its own spot, so a leader line stays short.
        for (g, r) in plan.groups.iter().zip(&plan.rects) {
            let reach = (nearest(*r, g.spot) - g.spot).length();
            assert!(reach <= RINGS as f32 * RING_STEP, "{} {reach}", g.label);
        }
        // Ear-level speakers keep their spot; the stacked height chip moves.
        let fl = plan.groups.iter().position(|g| g.label == "FL").unwrap();
        let tfl = plan.groups.iter().position(|g| g.label == "TFL").unwrap();
        assert!(plan.rects[fl].contains(plan.groups[fl].spot));
        assert!(!plan.rects[tfl].contains(plan.groups[tfl].spot));
        let text = tooltip(&[&chips[5], &chips[10]]);
        assert!(text.contains("BL · SR") && text.contains("share one layout position"));
    }

    #[test]
    fn unknown_distances_share_a_ring_at_the_typical_distance() {
        let chips = [
            chip("A", at(-30.0, 0.0, Some(2.0))),
            chip("B", at(30.0, 0.0, Some(3.0))),
            chip("C", at(0.0, 0.0, Some(2.5))),
            chip("Sub", at(110.0, 0.0, None)),
        ];
        let g = Geometry::new(area(), &chips);
        let sub = g.spot(&chips[3]).unwrap();
        assert!(((g.center - sub).length() - 2.5 * g.pixels_per_m.unwrap()).abs() < 1e-3);
        // Without any distance, every speaker sits on one ring.
        let none = [
            chip("A", at(0.0, 0.0, None)),
            chip("B", at(90.0, 0.0, None)),
        ];
        let g = Geometry::new(area(), &none);
        assert!(g.pixels_per_m.is_none());
        for c in &none {
            assert!(((g.center - g.spot(c).unwrap()).length() - g.fallback_radius).abs() < 1e-3);
        }
    }

    #[test]
    fn measured_radii_replace_the_layout_and_other_devices_sit_on_the_rim() {
        let mut chips = bench();
        for (name, measured) in [
            ("FL", Measured::Distance(2.4)),
            ("FR", Measured::Distance(2.85)),
            ("BC", Measured::Distance(0.51)),
            ("spk-12", Measured::OtherDevice),
        ] {
            chips.iter_mut().find(|c| c.name == name).unwrap().measured = Some(measured);
        }
        let plan = layout(&chips, area());
        let g = plan.geometry;
        assert!(g.measured);
        let r = |name| (spot(&chips, name) - g.center).length();
        // FR is measured 0.45 m farther than FL, though the layout puts them level.
        assert!(r("FR") > r("FL") + 0.4 * g.pixels_per_m.unwrap());
        let group = |name: &str| {
            let i = chips.iter().position(|c| c.name == name).unwrap();
            plan.groups.iter().find(|g| g.members.contains(&i)).unwrap()
        };
        // BC is half a metre away, the layout says one: a hollow ring at the layout spot.
        let bc = group("BC");
        let ghost = bc.ghost.expect("layout ghost");
        assert!((ghost - g.center).length() > (bc.spot - g.center).length());
        // FL is within 0.3 m of its 2.24 m layout distance: no ghost.
        assert!(group("FL").ghost.is_none());
        assert!((r("spk-12") - g.radius).abs() < 1e-3);
        let text = tooltip(&[group("spk-12")
            .members
            .iter()
            .map(|&i| &chips[i])
            .next()
            .unwrap()]);
        assert!(text.contains("Another device — distance unknown"), "{text}");
        let text = tooltip(&[&chips[1]]);
        assert!(
            text.contains("2.85 m: measured acoustic distance (includes electronic latency)"),
            "{text}"
        );
    }

    #[test]
    fn crowded_chips_never_overlap_cover_the_listener_or_leave_the_map() {
        // Omniphony's default 7.1.4 corners: heights share their bed speaker's azimuth.
        let h = 35.26;
        let corner = 2f32.sqrt();
        let top = 3f32.sqrt();
        let default = vec![
            chip("FL", at(-45.0, 0.0, Some(corner))),
            chip("FR", at(45.0, 0.0, Some(corner))),
            chip("C", at(0.0, 0.0, Some(1.0))),
            chip("LFE", at(45.0, -h, Some(top))),
            chip("BL", at(-135.0, 0.0, Some(corner))),
            chip("BR", at(135.0, 0.0, Some(corner))),
            chip("SL", at(-90.0, 0.0, Some(1.0))),
            chip("SR", at(90.0, 0.0, Some(1.0))),
            chip("TFL", at(-45.0, h, Some(top))),
            chip("TFR", at(45.0, h, Some(top))),
            chip("TBL", at(-135.0, h, Some(top))),
            chip("TBR", at(135.0, h, Some(top))),
            chip("Not reported", None),
        ];
        for chips in [default, bench()] {
            for side in [MIN_SIDE, SIDE] {
                let area = Rect::from_min_size(Pos2::ZERO, Vec2::splat(side));
                let plan = layout(&chips, area);
                let head = listener(plan.geometry.center);
                for (i, a) in plan.rects.iter().enumerate() {
                    assert!(area.expand(0.5).contains_rect(*a), "{a:?} leaves the map");
                    assert!(!a.intersects(head), "{a:?} covers the listener");
                    for b in &plan.rects[i + 1..] {
                        assert!(!a.intersects(*b), "{a:?} overlaps {b:?} at {side}");
                    }
                }
                // Layout depends only on its inputs: the same input, the same map.
                assert_eq!(plan.rects, layout(&chips, area).rects);
            }
        }
    }

    #[test]
    fn placement_keeps_two_card_columns_and_follows_the_grid_height() {
        let place = |width, cards| placement(width, cards, 210.0, 112.0, 8.0);
        // Two card columns no longer fit beside the smallest map: below the cards.
        assert!(matches!(place(492.0, 6), Placement::Below { .. }));
        // Beside the cards, the map grows with the grid it stands next to.
        let Placement::Beside { side: few } = place(800.0, 2) else {
            panic!("beside")
        };
        let Placement::Beside { side: many } = place(800.0, 12) else {
            panic!("beside")
        };
        assert_eq!(few, MIN_SIDE);
        assert_eq!(many, SIDE);
        // One row of cards keeps the smallest map; three rows leave room for the largest.
        assert_eq!(place(800.0, 6), Placement::Beside { side: SIDE });
        assert_eq!(place(1100.0, 4), Placement::Beside { side: MIN_SIDE });
    }

    #[test]
    fn tooltips_describe_position_role_and_quality_in_words() {
        let mut c = chip("Front left", at(-30.0, 15.0, Some(2.6)));
        c.reference = true;
        c.active = true;
        c.dimmed = Some("Set aside: unchecked".into());
        let text = tooltip(&[&c]);
        for needle in [
            "Front left",
            "30° left · 15° up · layout 2.6 m",
            "Distance from Omniphony's layout, not measured",
            "Playing now",
            "Timing reference",
            "Set aside: unchecked",
            "Not rated",
        ] {
            assert!(text.contains(needle), "{needle} missing from {text}");
        }
        assert_eq!(
            describe(at(180.0, -20.0, None).unwrap()),
            "behind · 20° down · layout distance not stated"
        );
        assert_eq!(
            tooltip(&[&chip("X", None)]).lines().nth(1),
            Some("Omniphony did not report a position")
        );
        let lfe = chip("LFE", at(0.0, -90.0, Some(0.5)));
        assert!(tooltip(&[&lfe]).contains("Straight below the listener"));
    }
}
