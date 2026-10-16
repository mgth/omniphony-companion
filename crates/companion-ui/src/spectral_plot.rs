use crate::{ACCENT, MUTED, ResultPlot, WARM};
use companion_service::review::{SpectralPoint, SpectralReport};
use eframe::egui::{self, FontId, RichText, Stroke, Vec2};

pub(super) fn draw(ui: &mut egui::Ui, report: &SpectralReport, mode: ResultPlot) {
    let response = mode == ResultPlot::FrequencyResponse;
    let arrival = mode == ResultPlot::EnergyArrival;
    let peak_db = report
        .points
        .iter()
        .filter_map(|p| p.magnitude_db)
        .filter(|v| v.is_finite())
        .fold(-300.0, f64::max);
    let value = |p: &SpectralPoint| {
        (if response {
            p.magnitude_db.map(|v| v - peak_db)
        } else if arrival {
            p.peak_time_ms
        } else {
            p.group_delay_ms
        })
        .filter(|v| v.is_finite())
    };
    let title = if response {
        "Frequency response · relative dB"
    } else if arrival {
        "Peak-energy arrival vs frequency · ms"
    } else {
        "Group delay vs frequency · ms"
    };
    ui.label(RichText::new(title).strong());
    ui.label(
        RichText::new(if response {
            "Peak = 0 dB · Amber: estimated S/N below 20 dB · Not SPL or anechoic response"
        } else if arrival {
            "Strongest energy in overlapping 1/3-octave bands · Amber points have competing peaks"
        } else {
            "Phase-slope delay · Amber: weak evidence · Gaps: unstable or unavailable values"
        })
        .small()
        .color(MUTED),
    );
    let [low_hz, high_hz] = report.frequency_bounds_hz;
    if !low_hz.is_finite() || !high_hz.is_finite() || low_hz <= 0.0 || high_hz <= low_hz {
        return;
    }
    let values: Vec<_> = report.points.iter().filter_map(value).collect();
    if !response {
        ui.label(RichText::new("Zero = this segment's correlation peak; positive = later. Not absolute propagation time.").small().color(MUTED));
    }
    if values.is_empty() {
        ui.label(RichText::new("No reliable points in this view.").color(WARM));
    }
    let (low, high) = if response {
        (
            values
                .iter()
                .copied()
                .fold(-30.0, f64::min)
                .clamp(-80.0, -10.0),
            0.0,
        )
    } else {
        let lo = values.iter().copied().fold(0.0, f64::min);
        let hi = values.iter().copied().fold(0.0, f64::max);
        let margin = ((hi - lo) * 0.1).max(0.5);
        (lo - margin, hi + margin)
    };
    let (rect, interaction) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(120.0), 200.0),
        egui::Sense::hover(),
    );
    let plot = rect.shrink2(Vec2::new(38.0, 22.0));
    let painter = ui.painter_at(rect);
    let map = |hz: f64, v: f64| {
        egui::pos2(
            plot.left() + ((hz / low_hz).ln() / (high_hz / low_hz).ln()) as f32 * plot.width(),
            plot.bottom() - ((v.clamp(low, high) - low) / (high - low)) as f32 * plot.height(),
        )
    };
    for v in [low, (low + high) * 0.5, high] {
        let y = map(low_hz, v).y;
        painter.line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            Stroke::new(1.0_f32, MUTED.gamma_multiply(0.2)),
        );
        painter.text(
            egui::pos2(plot.left() - 5.0, y),
            egui::Align2::RIGHT_CENTER,
            format!("{v:.1}"),
            FontId::proportional(10.0),
            MUTED,
        );
    }
    if !response && low < 0.0 && high > 0.0 {
        let y = map(low_hz, 0.0).y;
        painter.line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            Stroke::new(1.0_f32, MUTED.gamma_multiply(0.5)),
        );
    }
    let mut ticks: Vec<f64> = [
        20., 50., 100., 200., 500., 1000., 2000., 5000., 10000., 20000., 40000.,
    ]
    .into_iter()
    .filter(|hz| *hz > low_hz * 1.1 && *hz < high_hz / 1.1)
    .collect();
    ticks.insert(0, low_hz);
    ticks.push(high_hz);
    let mut previous_x = f32::NEG_INFINITY;
    for hz in ticks {
        let x = map(hz, low).x;
        if x - previous_x < 36.0 {
            continue;
        }
        previous_x = x;
        painter.line_segment(
            [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
            Stroke::new(1.0_f32, MUTED.gamma_multiply(0.15)),
        );
        let label = if hz >= 1000.0 {
            format!("{:.1}k", hz / 1000.0)
        } else {
            format!("{hz:.0}")
        };
        painter.text(
            egui::pos2(x, plot.bottom() + 5.0),
            egui::Align2::CENTER_TOP,
            label,
            FontId::proportional(10.0),
            MUTED,
        );
    }
    let mut previous = None;
    for p in &report.points {
        if !p.frequency_hz.is_finite() || p.frequency_hz < low_hz || p.frequency_hz > high_hz {
            previous = None;
            continue;
        }
        if let Some(v) = value(p) {
            let point = map(p.frequency_hz, v);
            let uncertain = p.estimated_snr_db < 20.0 || (arrival && p.ambiguous_peak);
            if !uncertain && let Some(last) = previous {
                painter.line_segment([last, point], Stroke::new(1.6_f32, ACCENT));
            }
            painter.circle_filled(
                point,
                if uncertain { 2.5 } else { 1.5 },
                if uncertain { WARM } else { ACCENT },
            );
            previous = (!uncertain).then_some(point);
        } else {
            previous = None;
        }
    }
    if let Some(pos) = interaction.hover_pos().filter(|p| plot.contains(*p)) {
        let hz = low_hz * (high_hz / low_hz).powf(f64::from((pos.x - plot.left()) / plot.width()));
        if let Some(point) = report.points.iter().min_by(|a, b| {
            (a.frequency_hz / hz)
                .ln()
                .abs()
                .total_cmp(&(b.frequency_hz / hz).ln().abs())
        }) {
            let reading = value(point).map_or_else(
                || "Withheld".into(),
                |v| format!("{v:+.3} {}", if response { "dB relative" } else { "ms" }),
            );
            interaction.on_hover_text(format!("{:.0} Hz · {reading}\nEstimated spectral S/N: {:.1} dB\nBand time width: ~{:.2} ms (not an accuracy bound){}",
                point.frequency_hz,point.estimated_snr_db,point.time_resolution_ms,
                if arrival && point.ambiguous_peak {"\nCompeting energy peaks"} else {""}));
        }
    }
    ui.label(
        RichText::new("Frequency in Hz · Logarithmic axis")
            .small()
            .color(MUTED),
    );
    if !response {
        ui.label(RichText::new("Driver offsets may appear across frequency ranges, but crossovers and reflections also affect these curves. Drivers are not isolated.").small().color(WARM));
    }
    ui.label(RichText::new(format!("IR window: {:+.0} to {:+.0} ms · Includes room reflections · Microphone calibration not applied",
        report.response_window_ms[0], report.response_window_ms[1])).small().color(MUTED));
}
