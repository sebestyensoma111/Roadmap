use super::{c_text, gtk::{g_object_unref, Object}};
use crate::devices::{Condition, Result};
use std::{ffi::{c_char, c_int, c_uint}, slice};

const WIDTH: f64 = 440.0;
const MARGIN: f64 = 24.0;
const CONTENT_WIDTH: f64 = WIDTH - 2.0 * MARGIN;
const SCALE: f64 = 2.0;
const COLOR_ALPHA: c_int = 0x3000;
const RGB24: c_int = 1;

pub struct Sheet<'a> {
    pub model: &'a str,
    pub condition: Condition,
    pub status: &'a str,
    pub reason: Option<&'a str>,
    pub metrics: &'a [(&'a str, &'a str)],
    pub measured: &'a str,
    pub serial: Option<&'a str>,
}

pub fn render(sheet: &Sheet<'_>) -> Result<Vec<u8>> {
    let mut bytes: Vec<u8> = Vec::new();
    unsafe {
        let drawing = cairo_recording_surface_create(COLOR_ALPHA, std::ptr::null());
        if cairo_surface_status(drawing) != 0 {
            cairo_surface_destroy(drawing);
            return Err("A mérési lap nem készíthető el.".into());
        }
        let canvas = cairo_create(drawing);
        let layout = pango_cairo_create_layout(canvas);
        let ink = (0.10, 0.14, 0.19);
        let muted = (0.30, 0.36, 0.41);
        let accent = match sheet.condition {
            Condition::Passed => (0.05, 0.36, 0.27),
            Condition::MediaWarnings | Condition::LowEndurance => (0.51, 0.28, 0.07),
            Condition::Critical => (0.58, 0.13, 0.14),
            Condition::Unavailable => muted,
        };
        let rect = |x, y, w, h, (r, g, b)| {
            cairo_set_source_rgb(canvas, r, g, b);
            cairo_rectangle(canvas, x, y, w, h);
            cairo_fill(canvas);
        };
        let text = |value: &str, style: &str, x, y, width: f64, lines: c_int, (r, g, b)| {
            let font = pango_font_description_from_string(c_text(style).as_ptr());
            pango_layout_set_font_description(layout, font);
            pango_font_description_free(font);
            pango_layout_set_width(layout, (width * 1024.0) as c_int);
            pango_layout_set_wrap(layout, 2);
            pango_layout_set_ellipsize(layout, 3);
            pango_layout_set_height(layout, -lines);
            pango_layout_set_text(layout, c_text(value).as_ptr(), -1);
            cairo_set_source_rgb(canvas, r, g, b);
            cairo_move_to(canvas, x, y);
            pango_cairo_show_layout(canvas, layout);
            let mut height = 0;
            pango_layout_get_size(layout, std::ptr::null_mut(), &mut height);
            f64::from(height) / 1024.0
        };

        text("Mérési lap", "Sans Bold 20", MARGIN, 18.0, CONTENT_WIDTH, 1, ink);
        text(&format!("Mérés ideje: {}", sheet.measured), "Sans 9", MARGIN, 52.0, CONTENT_WIDTH, 1, muted);
        rect(MARGIN, 76.0, CONTENT_WIDTH, 1.0, (0.85, 0.88, 0.90));

        let model_height = text(sheet.model, "Sans Bold 15", MARGIN, 90.0, CONTENT_WIDTH, 2, ink);
        let mut top = 90.0 + model_height + 9.0;
        rect(MARGIN, top, 3.0, 18.0, accent);
        top += text(sheet.status, "Sans Bold 11", MARGIN + 12.0, top, CONTENT_WIDTH - 12.0, 2, accent) + 3.0;
        if let Some(reason) = sheet.reason {
            top += text(reason, "Sans 9", MARGIN + 12.0, top, CONTENT_WIDTH - 12.0, 3, ink) + 3.0;
        }

        if !sheet.metrics.is_empty() { top += 12.0; }
        for &(name, value) in sheet.metrics {
            rect(MARGIN, top, CONTENT_WIDTH, 1.0, (0.88, 0.90, 0.92));
            text(name, "Sans 9", MARGIN, top + 8.0, 158.0, 1, muted);
            text(value, "Sans Bold 10", MARGIN + 170.0, top + 7.0, CONTENT_WIDTH - 170.0, 1, ink);
            top += 30.0;
        }
        if let Some(serial) = sheet.serial {
            top += 8.0;
            top += text(&format!("Sorozatszám: {serial}"), "Sans 9", MARGIN, top, CONTENT_WIDTH, 2, muted);
        }

        g_object_unref(layout);
        cairo_destroy(canvas);
        let surface = cairo_image_surface_create(RGB24, (WIDTH * SCALE) as c_int,
            ((top + 19.0) * SCALE).ceil() as c_int);
        let canvas = cairo_create(surface);
        cairo_set_source_rgb(canvas, 1.0, 1.0, 1.0);
        cairo_paint(canvas);
        cairo_scale(canvas, SCALE, SCALE);
        cairo_set_source_surface(canvas, drawing, 0.0, 0.0);
        cairo_paint(canvas);
        cairo_destroy(canvas);
        cairo_surface_destroy(drawing);
        let status = cairo_surface_write_to_png_stream(surface, append, (&mut bytes as *mut Vec<u8>).cast());
        cairo_surface_destroy(surface);
        if status != 0 {
            return Err("A mérési lap nem készíthető el.".into());
        }
    }
    Ok(bytes)
}

unsafe extern "C" fn append(data: Object, bytes: *const u8, length: c_uint) -> c_int {
    if length != 0 {
        (&mut *data.cast::<Vec<u8>>()).extend_from_slice(slice::from_raw_parts(bytes, length as usize));
    }
    0
}

#[link(name = "libcairo.so.2", kind = "dylib", modifiers = "+verbatim")]
extern "C" {
    fn cairo_recording_surface_create(content: c_int, extents: *const std::ffi::c_void) -> Object;
    fn cairo_image_surface_create(format: c_int, width: c_int, height: c_int) -> Object;
    fn cairo_surface_status(surface: Object) -> c_int;
    fn cairo_surface_write_to_png_stream(surface: Object,
        write: unsafe extern "C" fn(Object, *const u8, c_uint) -> c_int, data: Object) -> c_int;
    fn cairo_create(surface: Object) -> Object;
    fn cairo_paint(canvas: Object);
    fn cairo_scale(canvas: Object, x: f64, y: f64);
    fn cairo_set_source_surface(canvas: Object, surface: Object, x: f64, y: f64);
    pub(super) fn cairo_set_source_rgb(canvas: Object, red: f64, green: f64, blue: f64);
    pub(super) fn cairo_rectangle(canvas: Object, x: f64, y: f64, width: f64, height: f64);
    pub(super) fn cairo_fill(canvas: Object);
    pub(super) fn cairo_move_to(canvas: Object, x: f64, y: f64);
    fn cairo_destroy(canvas: Object);
    fn cairo_surface_destroy(surface: Object);
}

#[link(name = "libpangocairo-1.0.so.0", kind = "dylib", modifiers = "+verbatim")]
extern "C" {
    fn pango_cairo_create_layout(canvas: Object) -> Object;
    fn pango_cairo_show_layout(canvas: Object, layout: Object);
}

#[link(name = "libpango-1.0.so.0", kind = "dylib", modifiers = "+verbatim")]
extern "C" {
    fn pango_font_description_from_string(text: *const c_char) -> Object;
    fn pango_font_description_free(font: Object);
    fn pango_layout_set_font_description(layout: Object, font: Object);
    fn pango_layout_set_width(layout: Object, width: c_int);
    fn pango_layout_set_ellipsize(layout: Object, mode: c_int);
    fn pango_layout_set_wrap(layout: Object, mode: c_int);
    fn pango_layout_set_height(layout: Object, height: c_int);
    fn pango_layout_get_size(layout: Object, width: *mut c_int, height: *mut c_int);
    fn pango_layout_set_text(layout: Object, text: *const c_char, length: c_int);
}
