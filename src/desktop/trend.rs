use super::{c_text, sheet::{cairo_fill, cairo_move_to, cairo_rectangle, cairo_set_source_rgb}, date_label,
    gtk::Object, history::Snapshot};
use crate::i18n::{message, tr};
use std::ffi::{c_char, c_int};

const LEFT: f64 = 102.0;
const RIGHT: f64 = 18.0;
const MINT: (f64, f64, f64) = (0.42, 0.89, 0.74);
const AMBER: (f64, f64, f64) = (1.0, 0.71, 0.36);
const MUTED: (f64, f64, f64) = (0.72, 0.79, 0.86);

fn point_x(sample: &Snapshot, first: &Snapshot, last: &Snapshot, width: c_int) -> f64 {
    let range = last.measured.saturating_sub(first.measured).max(1);
    let offset = sample.measured.saturating_sub(first.measured);
    LEFT + (f64::from(width) - LEFT - RIGHT).max(0.0) * (offset as f64 / range as f64)
}

pub fn tooltip(samples: &[Snapshot], width: c_int, x: Option<c_int>) -> Option<String> {
    let first = samples.last()?;
    let last = samples.first()?;
    let index = match x {
        Some(x) => samples.iter().enumerate().min_by(|(_, a), (_, b)| {
            let distance = |sample| (point_x(sample, first, last, width) - f64::from(x)).abs();
            distance(a).total_cmp(&distance(b))
        })?.0,
        None => 0,
    };
    let sample = &samples[index];
    let mut result = format!("{}\n{}", date_label(sample.measured), tr(sample.condition.label()));
    if let Some(remaining) = sample.remaining {
        result.push_str(&message(" · Állapot: {}%", &[&remaining.to_string()]));
    }
    if samples.get(index + 1).is_some_and(|previous| sample.worsened_since(previous)) {
        result.push('\n');
        result.push_str(tr("Romlás az előző mérés óta."));
    }
    Some(result)
}

pub unsafe fn draw(canvas: Object, width: c_int, height: c_int, samples: &[Snapshot]) {
    let (Some(first), Some(last)) = (samples.last(), samples.first()) else { return };
    let (w, h) = (f64::from(width), f64::from(height));
    if w < LEFT + RIGHT + 20.0 || h < 100.0 { return }

    let color = |(r, g, b)| cairo_set_source_rgb(canvas, r, g, b);
    let label = |text: &str, x, y, size, ink| {
        color(ink);
        cairo_set_font_size(canvas, size);
        cairo_move_to(canvas, x, y);
        cairo_show_text(canvas, c_text(text).as_ptr());
    };
    color((0.08, 0.12, 0.17));
    cairo_rectangle(canvas, 0.0, 0.0, w, h);
    cairo_fill(canvas);
    let life_mode = samples.iter().filter(|sample| sample.remaining.is_some()).take(2).count() == 2;
    label(tr(if life_mode { "Állapot" } else { "SMART-kondíció" }), 14.0, 22.0, 13.0, MUTED);
    if w >= 530.0 {
        color(AMBER);
        cairo_arc(canvas, w - 155.0, 17.5, 3.5, 0.0, std::f64::consts::TAU);
        cairo_fill(canvas);
        label(tr("Jelzett romlás"), w - 145.0, 22.0, 12.0, AMBER);
    }
    let top = 40.0;
    let bottom = h - 28.0;
    let (lower, upper) = samples.iter().filter_map(|sample| sample.remaining)
        .map(|value| value.min(100)).fold((100_u8, 0_u8), |(low, high), value| {
            (low.min(value), high.max(value))
        });
    let lower = lower.saturating_sub(3) / 10 * 10;
    let upper = ((upper + 12) / 10 * 10).min(100);
    let ticks = if life_mode {
        [upper, (upper + lower) / 2, lower].map(|tick| format!("{tick}%"))
    } else {
        ["Jó", "Figyelmeztetés", "Kritikus"].map(|text| tr(text).to_string())
    };
    for (index, text) in ticks.iter().enumerate() {
        let y = top + (bottom - top) * index as f64 / 2.0;
        label(text, 14.0, y + 4.0, 10.5, MUTED);
        color((0.25, 0.31, 0.38));
        cairo_set_line_width(canvas, 1.0);
        cairo_move_to(canvas, LEFT, y);
        cairo_line_to(canvas, w - RIGHT, y);
        cairo_stroke(canvas);
    }
    let position = |sample: &Snapshot| {
        let fraction = if life_mode {
            f64::from(sample.remaining?.min(100) - lower) / f64::from(upper - lower)
        }
            else { 1.0 - f64::from(sample.severity()?) / 2.0 };
        Some((point_x(sample, first, last, width), bottom - fraction * (bottom - top)))
    };
    let mut previous = None;
    for sample in samples.iter().rev() {
        if let (Some((x, y)), Some((previous_x, previous_y))) = (position(sample), previous) {
            color(MINT);
            cairo_set_line_width(canvas, 2.0);
            cairo_move_to(canvas, previous_x, previous_y);
            cairo_line_to(canvas, x, y);
            cairo_stroke(canvas);
        }
        previous = position(sample);
    }
    for (index, sample) in samples.iter().enumerate().rev() {
        let worsened = samples.get(index + 1).is_some_and(|previous| sample.worsened_since(previous));
        if let Some((x, y)) = position(sample) {
            color(if worsened { AMBER } else { MINT });
            cairo_arc(canvas, x, y, if worsened { 5.0 } else { 3.0 }, 0.0, std::f64::consts::TAU);
            cairo_fill(canvas);
        } else if worsened {
            let x = point_x(sample, first, last, width);
            color(AMBER);
            cairo_rectangle(canvas, x - 2.0, bottom + 3.0, 4.0, 5.0);
            cairo_fill(canvas);
        }
    }
    let start = date_label(first.measured);
    let end = date_label(last.measured);
    if w >= 450.0 {
        label(&start, LEFT, h - 9.0, 10.5, MUTED);
        label(&end, w - 133.0, h - 9.0, 10.5, MUTED);
    }
}

#[link(name = "libcairo.so.2", kind = "dylib", modifiers = "+verbatim")]
extern "C" {
    fn cairo_set_font_size(canvas: Object, size: f64);
    fn cairo_show_text(canvas: Object, text: *const c_char);
    fn cairo_set_line_width(canvas: Object, width: f64);
    fn cairo_line_to(canvas: Object, x: f64, y: f64);
    fn cairo_stroke(canvas: Object);
    fn cairo_arc(canvas: Object, x: f64, y: f64, radius: f64, a: f64, b: f64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{Condition, Health};
    use crate::desktop::history::DriveId;

    #[test]
    fn hover_uses_measurement_time_and_reports_deterioration_at_the_newer_point() {
        let drive = DriveId::new("TEST-42", "SSD", 512).unwrap();
        let health = Health { condition: Condition::Passed, remaining: Some(98) };
        let first = Snapshot::new(drive, 1_700_000_000_000_000_000, &health, &[]);
        let mut last = first.clone();
        last.measured += 60_000_000_000;
        last.remaining = Some(97);
        let samples = [last, first];

        let oldest = tooltip(&samples, 600, Some(102)).unwrap();
        assert!(oldest.contains("98%"));
        assert!(!oldest.contains("Romlás"));
        let newest = tooltip(&samples, 600, Some(582)).unwrap();
        assert!(newest.contains("97%"));
        assert!(newest.contains("Romlás"));
        assert_eq!(tooltip(&samples, 600, None).unwrap(), newest);
    }
}
