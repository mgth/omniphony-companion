//! Top view of the room: where Omniphony's layout places each speaker, which one is
//! playing, which are set aside and which is the timing reference. Display only: the
//! positions are declared layout geometry, never measured evidence.
use companion_service::quality::Grade;
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
/// Speakers higher or lower than this get an elevation marker.
const ELEVATION_MARK_DEG: f32 = 10.0;
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
}

/// One speaker as the map shows it.
pub struct Chip<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub position: Option<Position>,
    /// Weakest quality grade, as on the speaker card.
    pub grade: Option<Grade>,
    pub selected: bool,
    /// Playing or being captured now: the chip pulses.
    pub active: bool,
    pub reference: bool,
    /// Why the speaker is dimmed: set aside before capture or excluded from correction.
    pub dimmed: Option<String>,
}

/// A short label that fits a chip: layout names (FL, TFL, LFE) as they are, the
/// initials of longer names ("Front left" → FL), or the start of one long word.
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
        words
            .iter()
            .filter_map(|w| w.chars().next())
            .flat_map(char::to_uppercase)
            .take(4)
            .collect()
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
}

impl Geometry {
    pub fn new(area: Rect, positions: &[Option<Position>]) -> Self {
        let radius = (area.width().min(area.height()) / 2.0 - CHIP_HEIGHT).max(20.0);
        let mut known: Vec<f32> = positions
            .iter()
            .flatten()
            .filter_map(|p| p.distance_m)
            .collect();
        known.sort_by(f32::total_cmp);
        let pixels_per_m = known.last().map(|max| radius * 0.9 / max);
        let fallback_radius = match (pixels_per_m, known.get(known.len() / 2)) {
            (Some(scale), Some(median)) => (median * scale).clamp(radius * 0.35, radius * 0.9),
            _ => radius * 0.75,
        };
        Self {
            center: area.center(),
            radius,
            pixels_per_m,
            fallback_radius,
        }
    }

    /// Front is up and the right is to the right; the radius follows the distance and
    /// keeps clear of the listener.
    pub fn spot(&self, p: Position) -> Pos2 {
        let r = match (p.distance_m, self.pixels_per_m) {
            (Some(d), Some(scale)) => (d * scale).clamp(self.radius * 0.32, self.radius),
            _ => self.fallback_radius,
        };
        let az = p.azimuth_deg.to_radians();
        self.center + Vec2::new(r * az.sin(), -r * az.cos())
    }
}

/// Chip rectangles for the placed speakers (`None` for unplaced ones), pushed apart
/// so no two overlap, inside `area`. Depends only on positions, names and size, so
/// capture state can never move a chip.
pub fn layout(chips: &[Chip], area: Rect) -> (Geometry, Vec<Option<Rect>>) {
    let positions: Vec<_> = chips.iter().map(|c| c.position).collect();
    let geometry = Geometry::new(area, &positions);
    let mut rects: Vec<Option<Rect>> = chips
        .iter()
        .map(|c| {
            c.position
                .map(|p| Rect::from_center_size(geometry.spot(p), chip_size(&short_name(c.name))))
        })
        .collect();
    separate(&mut rects, area);
    (geometry, rects)
}

/// Minimal-translation relaxation: each overlapping pair moves apart along the axis
/// that needs the shorter move. Deterministic, a few dozen passes for up to 32 chips.
fn separate(rects: &mut [Option<Rect>], area: Rect) {
    let keep_inside = |r: Rect| {
        let dx = (area.left() - r.left()).max(0.0) + (area.right() - r.right()).min(0.0);
        let dy = (area.top() - r.top()).max(0.0) + (area.bottom() - r.bottom()).min(0.0);
        r.translate(Vec2::new(dx, dy))
    };
    for r in rects.iter_mut().flatten() {
        *r = keep_inside(*r);
    }
    for _ in 0..48 {
        let mut moved = false;
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let (Some(a), Some(b)) = (rects[i], rects[j]) else {
                    continue;
                };
                let (a2, b2) = (a.expand(GAP / 2.0), b.expand(GAP / 2.0));
                if !a2.intersects(b2) {
                    continue;
                }
                let overlap = Vec2::new(
                    a2.right().min(b2.right()) - a2.left().max(b2.left()),
                    a2.bottom().min(b2.bottom()) - a2.top().max(b2.top()),
                );
                let d = b.center() - a.center();
                // Coincident chips: the later one goes right or down.
                let push = if overlap.x < overlap.y {
                    Vec2::new(if d.x < 0.0 { -1.0 } else { 1.0 } * overlap.x / 2.0, 0.0)
                } else {
                    Vec2::new(0.0, if d.y < 0.0 { -1.0 } else { 1.0 } * overlap.y / 2.0)
                };
                rects[i] = Some(keep_inside(a.translate(-push)));
                rects[j] = Some(keep_inside(b.translate(push)));
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
}

/// A plain-language position for the tooltip.
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
        .map_or("distance not stated".to_owned(), |d| format!("{d:.1} m"));
    format!("{side} · {height} · {distance}")
}

fn tooltip(chip: &Chip) -> String {
    let mut lines = vec![chip.name.to_owned()];
    lines.push(match chip.position {
        Some(p) => describe(p),
        None => "Omniphony did not report a position".into(),
    });
    if chip.active {
        lines.push("Playing now".into());
    }
    if chip.reference {
        lines.push("Timing reference: arrivals are measured against this speaker.".into());
    }
    if let Some(reason) = &chip.dimmed {
        lines.push(reason.clone());
    }
    lines.push(match chip.grade {
        Some(grade) => format!("Weakest quality: {}", grade.label()),
        None => "Not rated".into(),
    });
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

/// One chip: quality dot, short name, selection outline, elevation marker,
/// reference label, and a pulse while it plays.
fn paint_chip(painter: &egui::Painter, rect: Rect, chip: &Chip, pulse: f32) {
    let opacity = if chip.dimmed.is_some() { 0.35 } else { 1.0 };
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
        short_name(chip.name),
        FontId::monospace(11.0),
        super::TEXT.gamma_multiply(opacity),
    );
    if let Some(p) = chip.position {
        let ink = super::TEXT.gamma_multiply(0.75 * opacity);
        if p.elevation_deg >= ELEVATION_MARK_DEG {
            triangle(painter, rect.right_top() + Vec2::new(-6.0, -6.0), true, ink);
        } else if p.elevation_deg <= -ELEVATION_MARK_DEG {
            triangle(
                painter,
                rect.right_bottom() + Vec2::new(-6.0, 6.0),
                false,
                ink,
            );
        }
    }
    if chip.reference {
        painter.text(
            rect.center_bottom() + Vec2::new(0.0, 1.0),
            Align2::CENTER_TOP,
            "REF",
            FontId::proportional(9.5),
            super::ACCENT,
        );
    }
}

/// Draw the map into `area` and the not-placed strip below it when needed.
/// Returns the speaker whose chip was clicked.
fn paint(ui: &mut egui::Ui, chips: &[Chip], side: f32) -> Option<String> {
    let unplaced: Vec<_> = (0..chips.len())
        .filter(|&i| chips[i].position.is_none())
        .collect();
    let height = side
        + if unplaced.is_empty() {
            0.0
        } else {
            STRIP_HEIGHT
        };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(side, height), Sense::click());
    let area = Rect::from_min_size(rect.min, Vec2::splat(side));
    let (g, mut rects) = layout(chips, area);
    // The strip holds speakers without a position, in layout order.
    let mut x = area.left() + 72.0;
    for &i in &unplaced {
        let size = chip_size(&short_name(chips[i].name));
        if x + size.x <= rect.right() {
            rects[i] = Some(Rect::from_min_size(
                Pos2::new(x, area.bottom() + (STRIP_HEIGHT - size.y) / 2.0 + 2.0),
                size,
            ));
            x += size.x + 4.0;
        }
    }
    let hovered = response
        .hover_pos()
        .and_then(|p| (0..chips.len()).find(|&i| rects[i].is_some_and(|r| r.contains(p))));
    let clicked = response
        .clicked()
        .then(|| response.interact_pointer_pos())
        .flatten()
        .and_then(|p| (0..chips.len()).find(|&i| rects[i].is_some_and(|r| r.contains(p))))
        .map(|i| chips[i].id.to_owned());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect.expand(2.0));
        let faint = super::MUTED.gamma_multiply(0.16);
        if let Some(scale) = g.pixels_per_m {
            let max_m = g.radius / scale;
            let step = ring_step(max_m);
            let mut k = 1;
            while k as f32 * step <= max_m * 1.02 {
                let r = k as f32 * step * scale;
                painter.circle_stroke(g.center, r, Stroke::new(1.0_f32, faint));
                // Labelled behind the listener, where layouts rarely put a speaker.
                painter.text(
                    g.center + Vec2::new(3.0, r),
                    Align2::LEFT_BOTTOM,
                    format!("{} m", k as f32 * step),
                    FontId::proportional(9.0),
                    super::MUTED.gamma_multiply(0.55),
                );
                k += 1;
            }
        } else {
            painter.circle_stroke(g.center, g.radius, Stroke::new(1.0_f32, faint));
        }
        if chips
            .iter()
            .any(|c| c.position.is_some_and(|p| p.distance_m.is_none()))
        {
            dashed_circle(
                &painter,
                g.center,
                g.fallback_radius,
                Stroke::new(1.0_f32, super::MUTED.gamma_multiply(0.4)),
            );
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
        if let Some(reference) = chips.iter().zip(&rects).find_map(|(c, r)| {
            (c.reference && c.position.is_some())
                .then_some(*r)
                .flatten()
        }) {
            painter.extend(Shape::dashed_line(
                &[g.center, reference.center()],
                Stroke::new(1.2_f32, super::ACCENT.gamma_multiply(0.55)),
                3.0,
                3.0,
            ));
        }
        if !unplaced.is_empty() {
            painter.text(
                Pos2::new(area.left(), area.bottom() + STRIP_HEIGHT / 2.0 + 2.0),
                Align2::LEFT_CENTER,
                "Not placed",
                FontId::proportional(10.5),
                super::MUTED,
            );
        }
        let phase = (ui.input(|i| i.time) / PULSE_SECONDS).fract() as f32;
        // Dimmed chips first, so a speaker in use is never hidden under one set aside.
        let mut order: Vec<_> = (0..chips.len()).collect();
        order.sort_by_key(|&i| {
            (
                chips[i].dimmed.is_none(),
                chips[i].selected,
                chips[i].active,
            )
        });
        for i in order {
            if let Some(r) = rects[i] {
                paint_chip(&painter, r, &chips[i], phase);
            }
        }
    }
    if let Some(i) = hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        response.on_hover_text(tooltip(&chips[i]));
    }
    clicked
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

fn legend(ui: &mut egui::Ui, chips: &[Chip]) {
    legend_item(ui, 0, "playing");
    legend_item(ui, 1, "reference");
    legend_item(ui, 2, "set aside");
    legend_item(ui, 3, "above / below ear");
    if chips
        .iter()
        .any(|c| c.position.is_some_and(|p| p.distance_m.is_none()))
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
    .on_hover_text("Positions come from Omniphony's speaker layout. They are not measured.");
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

/// The map card's contents: title, a `side`-wide square map, the not-placed strip
/// and a wrapped legend. Its height depends only on the outputs, never on capture state.
pub fn compact(ui: &mut egui::Ui, chips: &[Chip], side: f32) -> Option<String> {
    ui.set_width(side);
    title(ui);
    let clicked = paint(ui, chips, side);
    ui.add_space(4.0);
    // Wrapped rows take their height from the interaction size, set before they start.
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 2.0);
        ui.spacing_mut().interact_size.y = 14.0;
        ui.horizontal_wrapped(|ui| legend(ui, chips));
    });
    clicked
}

/// The map with its legend to the right, below the cards in a narrow window.
pub fn wide(ui: &mut egui::Ui, chips: &[Chip], side: f32) -> Option<String> {
    let mut clicked = None;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(side);
            title(ui);
            clicked = paint(ui, chips, side);
        });
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.add_space(24.0);
            legend(ui, chips);
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Where Omniphony's layout places each speaker. The speaker being measured pulses; click one to see its result.",
                    )
                    .small()
                    .color(super::MUTED),
                )
                .wrap(),
            );
        });
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chip<'a>(name: &'a str, position: Option<Position>) -> Chip<'a> {
        Chip {
            id: name,
            name,
            position,
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
        ] {
            assert_eq!(short_name(name), short, "{name}");
        }
    }

    #[test]
    fn front_is_up_right_is_right_and_farther_is_outward() {
        let g = Geometry::new(area(), &[at(0.0, 0.0, Some(3.0))]);
        let c = g.center;
        let front = g.spot(at(0.0, 0.0, Some(3.0)).unwrap());
        assert!((front.x - c.x).abs() < 1e-3 && front.y < c.y);
        let right = g.spot(at(90.0, 0.0, Some(3.0)).unwrap());
        assert!(right.x > c.x && (right.y - c.y).abs() < 1e-3);
        let behind_left = g.spot(at(-135.0, 0.0, Some(3.0)).unwrap());
        assert!(behind_left.x < c.x && behind_left.y > c.y);
        let near = g.spot(at(0.0, 0.0, Some(1.5)).unwrap());
        assert!(near.y > front.y && near.y < c.y);
        // The farthest speaker sits inside the map, never on the listener.
        assert!((c - front).length() <= g.radius);
        let close = g.spot(at(0.0, 0.0, Some(0.01)).unwrap());
        assert!((c - close).length() >= g.radius * 0.3);
    }

    #[test]
    fn unknown_distances_share_a_ring_at_the_typical_distance() {
        let positions = [
            at(-30.0, 0.0, Some(2.0)),
            at(30.0, 0.0, Some(3.0)),
            at(0.0, 0.0, Some(2.5)),
            at(110.0, 0.0, None),
        ];
        let g = Geometry::new(area(), &positions);
        let sub = g.spot(positions[3].unwrap());
        assert!(((g.center - sub).length() - 2.5 * g.pixels_per_m.unwrap()).abs() < 1e-3);
        // Without any distance, every speaker sits on one ring.
        let none = [at(0.0, 0.0, None), at(90.0, 0.0, None)];
        let g = Geometry::new(area(), &none);
        assert!(g.pixels_per_m.is_none());
        for p in none {
            assert!(((g.center - g.spot(p.unwrap())).length() - g.fallback_radius).abs() < 1e-3);
        }
    }

    #[test]
    fn coincident_and_crowded_chips_never_overlap_and_stay_inside() {
        // Omniphony's default 7.1.4 corners: heights share their bed speaker's azimuth.
        let h = 35.26;
        let corner = 2f32.sqrt();
        let top = 3f32.sqrt();
        let chips = [
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
        let (_, rects) = layout(&chips, area());
        assert!(rects[12].is_none(), "unplaced speakers go to the strip");
        let placed: Vec<Rect> = rects.iter().flatten().copied().collect();
        assert_eq!(placed.len(), 12);
        for (i, a) in placed.iter().enumerate() {
            assert!(area().expand(0.5).contains_rect(*a), "{a:?} leaves the map");
            for b in &placed[i + 1..] {
                assert!(!a.intersects(*b), "{a:?} overlaps {b:?}");
            }
        }
        // Layout depends only on positions and names: the same input, the same map.
        let (_, again) = layout(&chips, area());
        assert_eq!(rects, again);
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
        let text = tooltip(&c);
        for needle in [
            "Front left",
            "30° left · 15° up · 2.6 m",
            "Playing now",
            "Timing reference",
            "Set aside: unchecked",
            "Not rated",
        ] {
            assert!(text.contains(needle), "{needle} missing from {text}");
        }
        assert_eq!(
            describe(at(180.0, -20.0, None).unwrap()),
            "behind · 20° down · distance not stated"
        );
        assert_eq!(
            tooltip(&chip("X", None)).lines().nth(1),
            Some("Omniphony did not report a position")
        );
    }
}
