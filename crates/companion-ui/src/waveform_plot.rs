use crate::{ACCENT, MUTED, ResultPlot, WARM};
use companion_service::review::{SignedTrace, WaveformReport};
use eframe::egui::{self, FontId, RichText, Stroke, Vec2};

pub(super) struct State {
    frequency: f64,
    overview: bool,
    overlay: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            frequency: 1000.0,
            overview: false,
            overlay: true,
        }
    }
}

pub(super) fn draw(
    ui: &mut egui::Ui,
    report: &WaveformReport,
    mode: ResultPlot,
    state: &mut State,
) {
    let burst = mode == ResultPlot::ToneBurst;
    ui.label(
        RichText::new(if burst {
            "Virtual tone burst · predicted from measured IR"
        } else {
            "Signed impulse response"
        })
        .strong(),
    );
    if burst && report.bursts.is_empty() {
        ui.label(
            RichText::new("No burst frequency fits inside this measurement's usable band.")
                .color(WARM),
        );
        return;
    }
    let index = report
        .bursts
        .iter()
        .position(|b| b.frequency_hz == state.frequency)
        .unwrap_or(0);
    ui.horizontal_wrapped(|ui| {
        if burst {
            state.frequency = report.bursts[index].frequency_hz;
            egui::ComboBox::from_id_salt("burst_frequency")
                .selected_text(format!("{:.0} Hz", state.frequency))
                .show_ui(ui, |ui| {
                    for b in &report.bursts {
                        ui.selectable_value(
                            &mut state.frequency,
                            b.frequency_hz,
                            format!("{:.0} Hz", b.frequency_hz),
                        );
                    }
                });
            ui.checkbox(&mut state.overlay, "Source overlay");
        }
        ui.selectable_value(&mut state.overview, false, "Arrival region");
        ui.selectable_value(&mut state.overview, true, "Overview");
    });
    let selected = report
        .bursts
        .iter()
        .find(|b| b.frequency_hz == state.frequency);
    let traces = if burst {
        &selected.unwrap().predicted
    } else {
        &report.impulse
    };
    let trace = if state.overview {
        &traces.overview
    } else {
        &traces.detail
    };
    let input = if burst && state.overlay {
        selected.map(|b| {
            if state.overview {
                &b.input.overview
            } else {
                &b.input.detail
            }
        })
    } else {
        None
    };
    if burst {
        let b = selected.unwrap();
        ui.label(
            RichText::new(format!(
                "{} cycles · {:.2} ms · Phase: {}",
                b.cycles,
                b.duration_ms,
                b.phase_deg.map_or_else(
                    || "withheld (weak evidence)".into(),
                    |v| format!("{v:+.1}° modulo 360°")
                )
            ))
            .small()
            .color(MUTED),
        );
    }
    ui.label(
        RichText::new(if burst {
            "Prediction: teal · Source: gray · Each trace normalized to its own peak"
        } else {
            "Signed amplitude, normalized to peak · Positive and negative oscillations retained"
        })
        .small()
        .color(MUTED),
    );
    plot(ui, trace, input);
    ui.label(RichText::new("Time and phase use this segment's correlation zero; frequencies are not individually realigned.").small().color(MUTED));
    if burst {
        ui.label(RichText::new("Derived view · No burst played · Includes room and crossover effects. Packet peak is not a direct-arrival estimate.").small().color(WARM));
    } else {
        ui.label(RichText::new("Band-limited measured response. Early oscillations can include processing pre-ringing; the first wiggle is not proof of the direct path.").small().color(WARM));
    }
}

fn plot(ui: &mut egui::Ui, trace: &SignedTrace, input: Option<&SignedTrace>) {
    if trace.extrema.is_empty() || trace.bucket_ms <= 0.0 || !trace.bucket_ms.is_finite() {
        return;
    }
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(100.0), 180.0),
        egui::Sense::hover(),
    );
    let plot = rect.shrink2(Vec2::new(36.0, 20.0));
    let painter = ui.painter_at(rect);
    let ink = ui.painter_at(plot);
    let left = trace.start_ms;
    let right = left + trace.bucket_ms * trace.extrema.len() as f64;
    let map = |ms: f64, amplitude: f64| {
        egui::pos2(
            plot.left() + ((ms - left) / (right - left)) as f32 * plot.width(),
            plot.center().y - amplitude.clamp(-1.0, 1.0) as f32 * plot.height() * 0.5,
        )
    };
    for v in [-1.0, 0.0, 1.0] {
        let y = map(left, v).y;
        painter.line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            Stroke::new(1.0_f32, MUTED.gamma_multiply(0.3)),
        );
        painter.text(
            egui::pos2(plot.left() - 5.0, y),
            egui::Align2::RIGHT_CENTER,
            format!("{v:+.0}"),
            FontId::proportional(10.0),
            MUTED,
        );
    }
    for i in 0..=4 {
        let ms = left + (right - left) * i as f64 / 4.0;
        let x = map(ms, 0.0).x;
        painter.text(
            egui::pos2(x, plot.bottom() + 5.0),
            egui::Align2::CENTER_TOP,
            format!("{ms:+.2}"),
            FontId::proportional(10.0),
            MUTED,
        );
    }
    if left < 0.0 && right > 0.0 {
        let x = map(0.0, 0.0).x;
        painter.line_segment(
            [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
            Stroke::new(1.0_f32, WARM.gamma_multiply(0.4)),
        );
    }
    for (trace, color) in input
        .into_iter()
        .map(|t| (t, MUTED.gamma_multiply(0.65)))
        .chain(std::iter::once((trace, ACCENT)))
    {
        let mut last = None;
        for (i, &[lo, hi]) in trace.extrema.iter().take(1600).enumerate() {
            if !lo.is_finite() || !hi.is_finite() {
                last = None;
                continue;
            }
            let ms = trace.start_ms + i as f64 * trace.bucket_ms;
            let scale = trace.peak.max(1e-15);
            let a = map(ms, f64::from(lo) / scale);
            let b = map(ms, f64::from(hi) / scale);
            ink.line_segment([a, b], Stroke::new(1.0_f32, color));
            let middle = a.lerp(b, 0.5);
            if let Some(previous) = last {
                ink.line_segment([previous, middle], Stroke::new(1.0_f32, color));
            }
            last = Some(middle);
        }
    }
    if let Some(pos) = response.hover_pos().filter(|p| plot.contains(*p)) {
        let ms = left + f64::from((pos.x - plot.left()) / plot.width()) * (right - left);
        response.on_hover_text(format!("{ms:+.3} ms relative to correlation zero\nSigned min/max samples preserved in each display bucket"));
    }
}
