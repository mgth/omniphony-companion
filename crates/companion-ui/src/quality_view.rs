//! Font-independent quality symbols shared by speaker rows and individual results.
use companion_service::quality::Grade;
use eframe::egui::{self, Color32, RichText, Stroke, Vec2};

fn color(grade: Option<Grade>) -> Color32 {
    match grade {
        Some(Grade::Good) => Color32::from_rgb(100, 220, 145),
        Some(Grade::Limited) => super::WARM,
        Some(Grade::Poor) => super::RED,
        Some(Grade::Unavailable) | None => super::MUTED,
    }
}

pub fn badge(ui: &mut egui::Ui, grade: Option<Grade>, text: &str, details: &str) {
    let ink = color(grade);
    let description = match grade {
        Some(Grade::Good) => "Good · strong measurement evidence",
        Some(Grade::Limited) => "Limited · reduced confidence",
        Some(Grade::Poor) => "Poor · weak or unreliable evidence",
        Some(Grade::Unavailable) => "Unavailable · no usable estimate",
        None => "Not rated · no quality report",
    };
    egui::Frame::new()
        .fill(ink.gamma_multiply(0.10))
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                icon(ui, grade, ink);
                ui.label(RichText::new(text).color(ink));
            });
        })
        .response
        .on_hover_text(if details.is_empty() {
            description.to_owned()
        } else {
            format!("{description}\n{details}")
        });
}

fn icon(ui: &mut egui::Ui, grade: Option<Grade>, ink: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let p = ui.painter();
    let c = rect.center();
    let stroke = Stroke::new(1.5_f32, ink);
    match grade {
        Some(Grade::Limited) => {
            p.add(egui::Shape::closed_line(
                vec![
                    c + Vec2::new(0.0, -6.5),
                    c + Vec2::new(7.0, 5.5),
                    c + Vec2::new(-7.0, 5.5),
                ],
                stroke,
            ));
            p.line_segment([c + Vec2::new(0.0, -2.5), c + Vec2::new(0.0, 0.5)], stroke);
            p.circle_filled(c + Vec2::new(0.0, 3.0), 0.9, ink);
        }
        _ => {
            p.circle_stroke(c, 6.5, stroke);
            match grade {
                Some(Grade::Good) => {
                    p.add(egui::Shape::line(
                        vec![
                            c + Vec2::new(-3.5, 0.0),
                            c + Vec2::new(-0.8, 2.5),
                            c + Vec2::new(3.7, -2.7),
                        ],
                        stroke,
                    ));
                }
                Some(Grade::Poor) => {
                    for sign in [-1.0, 1.0] {
                        p.line_segment(
                            [
                                c + Vec2::new(-2.5, sign * -2.5),
                                c + Vec2::new(2.5, sign * 2.5),
                            ],
                            stroke,
                        );
                    }
                }
                _ => {
                    p.line_segment([c + Vec2::new(-3.0, 0.0), c + Vec2::new(3.0, 0.0)], stroke);
                }
            }
        }
    }
}
