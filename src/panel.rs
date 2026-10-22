//! The stats widgets: drawn on this computer and sent to the pump as part of
//! the picture, the way the ASUS app does it. Either over the user's photo or
//! video, or as a full dashboard on its own background.

use crate::layout::{Backdrop, Background, Layout, Style, Widget};
use crate::stats::{self, Metric, Readings, Sampler};
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use image::{Rgb, RgbImage, Rgba, RgbaImage};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

pub const SIZE: u32 = 720;

const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);
const MUTED: Rgba<u8> = Rgba([178, 172, 184, 255]);
const TRACK: Rgba<u8> = Rgba([255, 255, 255, 38]);
/// Behind a widget with a backdrop.
const SHADE: Rgba<u8> = Rgba([8, 6, 12, 190]);
/// A tile without a backdrop.
const FROST: Rgba<u8> = Rgba([255, 255, 255, 16]);
/// Gauges open at the bottom: they start at the lower left and sweep 270°.
const GAUGE_START: f32 = 135.0;
const GAUGE_SWEEP: f32 = 270.0;

fn fonts() -> &'static (FontRef<'static>, FontRef<'static>) {
    static FONTS: OnceLock<(FontRef<'static>, FontRef<'static>)> = OnceLock::new();
    FONTS.get_or_init(|| {
        let bold = FontRef::try_from_slice(include_bytes!("../assets/fonts/Rajdhani-Bold.ttf"))
            .expect("bundled bold font");
        let semi = FontRef::try_from_slice(include_bytes!("../assets/fonts/Rajdhani-SemiBold.ttf"))
            .expect("bundled semibold font");
        (bold, semi)
    })
}

/// Widgets on a transparent canvas, to lay over a photo or video.
pub fn overlay(readings: &Readings, widgets: &[Widget]) -> RgbaImage {
    let mut canvas = RgbaImage::new(SIZE, SIZE);
    draw_widgets(&mut canvas, readings, widgets);
    canvas
}

/// A full-screen dashboard: the background with the widgets on it.
pub fn dashboard(readings: &Readings, layout: &Layout, background: &RgbaImage) -> RgbImage {
    let mut canvas = background.clone();
    draw_widgets(&mut canvas, readings, &layout.widgets);
    flatten(&canvas)
}

/// Later widgets are drawn over earlier ones.
pub fn draw_widgets(canvas: &mut RgbaImage, readings: &Readings, widgets: &[Widget]) {
    for widget in widgets {
        match widget.style {
            Style::Text => text_widget(canvas, readings, widget),
            Style::Tile => tile_widget(canvas, readings, widget),
            Style::Ring => ring_widget(canvas, readings, widget),
            Style::Bar => bar_widget(canvas, readings, widget),
        }
    }
}

fn color_of(widget: &Widget) -> Rgba<u8> {
    let [r, g, b] = widget.color;
    Rgba([r, g, b, 255])
}

/// The value's color. The clock reads best in plain white and grey.
fn value_color(metric: Metric) -> Rgba<u8> {
    if metric == Metric::Date {
        MUTED
    } else {
        WHITE
    }
}

/// The name over a large value, both centered.
fn text_widget(canvas: &mut RgbaImage, readings: &Readings, widget: &Widget) {
    let (bold, semi) = fonts();
    let (x0, y0, w, h) = (widget.x, widget.y, widget.w, widget.h);
    if widget.backdrop {
        let radius = (h * 0.22).min(24.0);
        fill_rounded(canvas, x0, y0, x0 + w, y0 + h, radius, SHADE);
    }
    let pad = (w * 0.06).min(16.0);
    let inner = w - pad * 2.0;
    let center = x0 + w / 2.0;
    let value = readings.text(widget.metric);
    if widget.caption {
        let caption_size = h * 0.2;
        let value_size = h * 0.44;
        let gap = h * 0.08;
        let block = caption_size * 0.7 + gap + value_size * 0.7;
        let caption_baseline = y0 + (h - block) / 2.0 + caption_size * 0.7;
        text(
            canvas,
            semi,
            caption_size,
            center,
            caption_baseline,
            widget.metric.label(),
            color_of(widget),
            Align::Center,
            inner,
        );
        text(
            canvas,
            bold,
            value_size,
            center,
            caption_baseline + gap + value_size * 0.7,
            &value,
            value_color(widget.metric),
            Align::Center,
            inner,
        );
    } else {
        let size = h * 0.72;
        let font = if widget.metric == Metric::Date {
            semi
        } else {
            bold
        };
        text(
            canvas,
            font,
            size,
            center,
            y0 + h / 2.0 + size * 0.35,
            &value,
            value_color(widget.metric),
            Align::Center,
            inner,
        );
    }
}

/// A card with a colored edge, the name and value on the left, and a bar along the bottom.
fn tile_widget(canvas: &mut RgbaImage, readings: &Readings, widget: &Widget) {
    let (bold, semi) = fonts();
    let (x0, y0, w, h) = (widget.x, widget.y, widget.w, widget.h);
    let fill = if widget.backdrop { SHADE } else { FROST };
    fill_rounded(canvas, x0, y0, x0 + w, y0 + h, (h * 0.12).min(18.0), fill);
    let color = color_of(widget);
    fill_rounded(
        canvas,
        x0,
        y0 + h * 0.14,
        x0 + 5.0,
        y0 + h * 0.32,
        2.5,
        color,
    );
    let scale = (h / 170.0).clamp(0.3, 3.0);
    let pad = 22.0 * scale.min(1.0);
    let inner = w - pad * 2.0;
    let fraction = readings.fraction(widget.metric);
    if widget.caption {
        text(
            canvas,
            semi,
            28.0 * scale,
            x0 + pad,
            y0 + 44.0 * scale,
            widget.metric.label(),
            MUTED,
            Align::Left,
            inner,
        );
    }
    let value_size = 74.0 * scale;
    let baseline = if widget.caption {
        y0 + h * 0.5 + value_size * 0.42
    } else if fraction.is_some() {
        y0 + h * 0.45 + value_size * 0.35
    } else {
        y0 + h * 0.5 + value_size * 0.35
    };
    text(
        canvas,
        bold,
        value_size,
        x0 + pad,
        baseline,
        &readings.text(widget.metric),
        value_color(widget.metric),
        Align::Left,
        inner,
    );
    if let Some(fraction) = fraction {
        let thickness = (6.0 * scale).max(3.0);
        bar(
            canvas,
            x0 + pad,
            y0 + h - 26.0 * scale,
            inner,
            thickness,
            fraction,
            color,
        );
    }
}

/// A round gauge with the value in the middle and the name in the opening at the bottom.
fn ring_widget(canvas: &mut RgbaImage, readings: &Readings, widget: &Widget) {
    let (bold, semi) = fonts();
    let side = widget.w.min(widget.h);
    let (cx, cy) = widget.center();
    if widget.backdrop {
        fill_rounded(
            canvas,
            cx - side / 2.0,
            cy - side / 2.0,
            cx + side / 2.0,
            cy + side / 2.0,
            side / 2.0,
            SHADE,
        );
    }
    let thickness = side * 0.075;
    let radius = side / 2.0 - thickness / 2.0 - side * 0.05;
    arc(
        canvas,
        cx,
        cy,
        radius,
        thickness,
        GAUGE_START,
        GAUGE_SWEEP,
        TRACK,
    );
    if let Some(fraction) = readings.fraction(widget.metric) {
        let sweep = GAUGE_SWEEP * fraction as f32;
        if sweep > 0.5 {
            arc(
                canvas,
                cx,
                cy,
                radius,
                thickness,
                GAUGE_START,
                sweep,
                color_of(widget),
            );
        }
    }
    let inner = radius * 1.5;
    let value_size = side * 0.21;
    text(
        canvas,
        bold,
        value_size,
        cx,
        cy + value_size * 0.3,
        &readings.text(widget.metric),
        value_color(widget.metric),
        Align::Center,
        inner,
    );
    if widget.caption {
        text(
            canvas,
            semi,
            side * 0.1,
            cx,
            cy + radius * 0.82,
            widget.metric.label(),
            MUTED,
            Align::Center,
            radius,
        );
    }
}

/// The name on the left and the value on the right, over a bar the width of the widget.
fn bar_widget(canvas: &mut RgbaImage, readings: &Readings, widget: &Widget) {
    let (bold, semi) = fonts();
    let (x0, y0, w, h) = (widget.x, widget.y, widget.w, widget.h);
    if widget.backdrop {
        fill_rounded(canvas, x0, y0, x0 + w, y0 + h, (h * 0.2).min(20.0), SHADE);
    }
    let pad = if widget.backdrop {
        (h * 0.16).min(18.0)
    } else {
        0.0
    };
    let inner = w - pad * 2.0;
    let thickness = (h * 0.13).max(4.0);
    let value_size = h * 0.42;
    let baseline = y0 + pad + value_size * 0.75;
    let value = readings.text(widget.metric);
    let value_width = if widget.caption { inner * 0.55 } else { inner };
    text(
        canvas,
        bold,
        value_size,
        x0 + w - pad,
        baseline,
        &value,
        value_color(widget.metric),
        Align::Right,
        value_width,
    );
    if widget.caption {
        let used = measure(bold, value_size, &value).min(value_width);
        text(
            canvas,
            semi,
            h * 0.3,
            x0 + pad,
            baseline,
            widget.metric.label(),
            MUTED,
            Align::Left,
            (inner - used - 12.0).max(10.0),
        );
    }
    bar(
        canvas,
        x0 + pad,
        y0 + h - pad - thickness,
        inner,
        thickness,
        readings.fraction(widget.metric).unwrap_or(0.0),
        color_of(widget),
    );
}

/// The dashboard's background, before any widgets.
pub fn backdrop(background: Background) -> RgbaImage {
    let [r, g, b] = background.color.map(f32::from);
    RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        let (x, y) = (x as f32, y as f32);
        let glow = match background.style {
            Backdrop::Glow => falloff(x - 560.0, y - 600.0, 620.0, 1.6),
            Backdrop::Spotlight => falloff(x - 360.0, y - 360.0, 520.0, 1.8) * 0.8,
            Backdrop::Plain => 0.0,
        };
        let channel = |base: f32, color: f32| (base + color * 0.3 * glow).round().min(255.0) as u8;
        Rgba([channel(10.0, r), channel(8.0, g), channel(13.0, b), 255])
    })
}

fn falloff(dx: f32, dy: f32, reach: f32, power: f32) -> f32 {
    (1.0 - (dx * dx + dy * dy).sqrt() / reach)
        .clamp(0.0, 1.0)
        .powf(power)
}

fn bar(
    canvas: &mut RgbaImage,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    fraction: f64,
    color: Rgba<u8>,
) {
    let radius = height / 2.0;
    fill_rounded(canvas, x, y, x + width, y + height, radius, TRACK);
    let filled = width * fraction as f32;
    if filled >= 1.0 {
        fill_rounded(
            canvas,
            x,
            y,
            x + filled.max(height),
            y + height,
            radius,
            color,
        );
    }
}

/// An anti-aliased arc with round ends. Angles are in degrees, clockwise from
/// three o'clock, as the screen's y axis points down.
#[allow(clippy::too_many_arguments)]
fn arc(
    canvas: &mut RgbaImage,
    cx: f32,
    cy: f32,
    radius: f32,
    thickness: f32,
    start: f32,
    sweep: f32,
    color: Rgba<u8>,
) {
    let half = thickness / 2.0;
    let point = |degrees: f32| {
        let angle = degrees.to_radians();
        (cx + radius * angle.cos(), cy + radius * angle.sin())
    };
    let ends = [point(start), point(start + sweep)];
    let reach = radius + half + 1.0;
    let (left, right) = (
        (cx - reach).floor().max(0.0),
        (cx + reach).ceil().min(SIZE as f32),
    );
    let (top, bottom) = (
        (cy - reach).floor().max(0.0),
        (cy + reach).ceil().min(SIZE as f32),
    );
    for py in top as i32..bottom as i32 {
        for px in left as i32..right as i32 {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let (dx, dy) = (x - cx, y - cy);
            let along = (dy.atan2(dx).to_degrees() - start).rem_euclid(360.0);
            let distance = if along <= sweep {
                ((dx * dx + dy * dy).sqrt() - radius).abs()
            } else {
                ends.iter()
                    .map(|(ex, ey)| ((x - ex).powi(2) + (y - ey).powi(2)).sqrt())
                    .fold(f32::MAX, f32::min)
            };
            let coverage = (half - distance + 0.5).clamp(0.0, 1.0);
            if coverage > 0.0 {
                blend(canvas, px, py, color, coverage);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

/// Draw one line of text with its baseline at `baseline`. Text wider than
/// `max_width` is shrunk to fit.
#[allow(clippy::too_many_arguments)]
fn text(
    canvas: &mut RgbaImage,
    font: &FontRef<'static>,
    size: f32,
    x: f32,
    baseline: f32,
    line: &str,
    color: Rgba<u8>,
    align: Align,
    max_width: f32,
) {
    if size < 1.0 || max_width <= 0.0 {
        return;
    }
    let mut size = size;
    let mut width = measure(font, size, line);
    if width > max_width && width > 0.0 {
        size *= max_width / width;
        width = max_width;
    }
    let scaled = font.as_scaled(PxScale::from(size));
    let mut pen = match align {
        Align::Left => x,
        Align::Center => x - width / 2.0,
        Align::Right => x - width,
    };
    let mut previous = None;
    for character in line.chars() {
        let id = scaled.glyph_id(character);
        if let Some(previous) = previous {
            pen += scaled.kern(previous, id);
        }
        let glyph = id.with_scale_and_position(size, ab_glyph::point(pen, baseline));
        pen += scaled.h_advance(id);
        previous = Some(id);
        let Some(outline) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let px = bounds.min.x as i32 + gx as i32;
            let py = bounds.min.y as i32 + gy as i32;
            blend(canvas, px, py, color, coverage);
        });
    }
}

fn measure(font: &FontRef<'static>, size: f32, line: &str) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut width = 0.0;
    let mut previous = None;
    for character in line.chars() {
        let id = scaled.glyph_id(character);
        if let Some(previous) = previous {
            width += scaled.kern(previous, id);
        }
        width += scaled.h_advance(id);
        previous = Some(id);
    }
    width
}

/// Anti-aliased rounded rectangle.
fn fill_rounded(
    canvas: &mut RgbaImage,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    radius: f32,
    color: Rgba<u8>,
) {
    let radius = radius.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (half_w, half_h) = ((x1 - x0) / 2.0 - radius, (y1 - y0) / 2.0 - radius);
    for py in (y0.floor().max(0.0) as i32)..(y1.ceil().min(SIZE as f32) as i32) {
        for px in (x0.floor().max(0.0) as i32)..(x1.ceil().min(SIZE as f32) as i32) {
            let qx = ((px as f32 + 0.5 - cx).abs() - half_w).max(0.0);
            let qy = ((py as f32 + 0.5 - cy).abs() - half_h).max(0.0);
            let distance = (qx * qx + qy * qy).sqrt() - radius;
            let coverage = (0.5 - distance).clamp(0.0, 1.0);
            if coverage > 0.0 {
                blend(canvas, px, py, color, coverage);
            }
        }
    }
}

/// Source-over blend in straight alpha, so a transparent canvas stays usable as an overlay.
fn blend(canvas: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>, coverage: f32) {
    if x < 0 || y < 0 || x >= SIZE as i32 || y >= SIZE as i32 {
        return;
    }
    let pixel = canvas.get_pixel_mut(x as u32, y as u32);
    let alpha = color[3] as f32 / 255.0 * coverage.clamp(0.0, 1.0);
    let below = pixel[3] as f32 / 255.0;
    let out = alpha + below * (1.0 - alpha);
    if out <= 0.0 {
        return;
    }
    for channel in 0..3 {
        let mixed =
            (color[channel] as f32 * alpha + pixel[channel] as f32 * below * (1.0 - alpha)) / out;
        pixel[channel] = mixed.round() as u8;
    }
    pixel[3] = (out * 255.0).round() as u8;
}

fn flatten(canvas: &RgbaImage) -> RgbImage {
    RgbImage::from_fn(SIZE, SIZE, |x, y| {
        let pixel = canvas.get_pixel(x, y);
        Rgb([pixel[0], pixel[1], pixel[2]])
    })
}

/// Lay `overlay` over a decoded picture.
pub fn compose(picture: &mut RgbImage, overlay: &RgbaImage) {
    for (pixel, top) in picture.pixels_mut().zip(overlay.pixels()) {
        let alpha = u32::from(top[3]);
        if alpha == 0 {
            continue;
        }
        for channel in 0..3 {
            let mixed =
                (u32::from(top[channel]) * alpha + u32::from(pixel[channel]) * (255 - alpha) + 127)
                    / 255;
            pixel[channel] = mixed as u8;
        }
    }
}

pub fn encode(picture: &RgbImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
        .encode_image(picture)
        .ok()?;
    Some(out)
}

/// What the panel should show. Set by the window, read by the panel thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    /// Widgets over the user's photo or video, while that is on the pump.
    pub picture: Option<Vec<Widget>>,
    /// The dashboard, while it is on the pump.
    pub dashboard: Option<Layout>,
    /// The window is open and shows live readings.
    pub window_open: bool,
    pub fahrenheit: bool,
}

#[derive(Clone, Default)]
pub struct Rendered {
    /// Changes whenever a new panel is drawn.
    pub version: u64,
    pub overlay: Option<Arc<RgbaImage>>,
    pub dashboard: Option<Arc<Vec<u8>>>,
}

/// Shared between the window, the panel thread, and the screen worker.
#[derive(Default)]
pub struct Shared {
    config: Mutex<Config>,
    rendered: Mutex<Rendered>,
    readings: Mutex<Option<Readings>>,
}

impl Shared {
    pub fn configure(&self, config: Config) {
        *self
            .config
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = config;
    }

    pub fn config(&self) -> Config {
        self.config
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn draws_on_picture(&self) -> bool {
        self.config
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .picture
            .is_some()
    }

    pub fn rendered(&self) -> Rendered {
        self.rendered
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn readings(&self) -> Option<Readings> {
        self.readings
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

/// Sample and draw once a second while anything needs it.
pub fn spawn(shared: Arc<Shared>) {
    thread::spawn(move || {
        let mut sampler = Sampler::default();
        let mut version = 0u64;
        let mut background: Option<(Background, RgbaImage)> = None;
        loop {
            let config = shared.config();
            let on_picture = config
                .picture
                .as_ref()
                .is_some_and(|widgets| !widgets.is_empty());
            let drawing = on_picture || config.dashboard.is_some();
            if drawing || config.window_open {
                let mut readings = sampler.sample();
                readings.fahrenheit = config.fahrenheit;
                if drawing {
                    version += 1;
                    let overlay = config
                        .picture
                        .as_ref()
                        .filter(|widgets| !widgets.is_empty())
                        .map(|widgets| Arc::new(overlay(&readings, widgets)));
                    let dashboard = config.dashboard.as_ref().and_then(|layout| {
                        if background
                            .as_ref()
                            .is_none_or(|(drawn, _)| *drawn != layout.background)
                        {
                            background = Some((layout.background, backdrop(layout.background)));
                        }
                        let (_, image) = background.as_ref()?;
                        encode(&dashboard(&readings, layout, image)).map(Arc::new)
                    });
                    *shared
                        .rendered
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner()) = Rendered {
                        version,
                        overlay,
                        dashboard,
                    };
                }
                *shared
                    .readings
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = Some(readings);
            }
            thread::sleep(stats::INTERVAL);
        }
    });
}

/// Turns the frames the worker is about to send into frames with the panel on
/// them. Keeps the last result, so a held still is only redrawn once a second.
#[derive(Default)]
pub struct Composer {
    last: Option<(FrameKey, Arc<Vec<u8>>)>,
}

/// The frame's address and length, and the panel version drawn on it.
type FrameKey = (usize, usize, u64);

impl Composer {
    /// `None` means send `base` unchanged.
    pub fn frame(&mut self, shared: &Shared, base: &[u8], dashboard: bool) -> Option<Arc<Vec<u8>>> {
        let rendered = shared.rendered();
        if dashboard {
            return rendered.dashboard;
        }
        let overlay = rendered.overlay?;
        if !shared.draws_on_picture() {
            return None;
        }
        let key = (base.as_ptr() as usize, base.len(), rendered.version);
        if let Some((last_key, jpeg)) = &self.last {
            if *last_key == key {
                return Some(Arc::clone(jpeg));
            }
        }
        let mut picture = image::load_from_memory(base).ok()?.to_rgb8();
        if picture.dimensions() != (SIZE, SIZE) {
            return None;
        }
        compose(&mut picture, &overlay);
        let jpeg = Arc::new(encode(&picture)?);
        self.last = Some((key, Arc::clone(&jpeg)));
        Some(jpeg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{self, Widget};

    fn sample() -> Readings {
        let mut readings = Readings::default();
        readings.set(Metric::CpuUsage, 37.0, Some(100.0));
        readings.set(Metric::GpuTemp, 58.0, None);
        readings.set(
            Metric::MemoryUsed,
            21.0 * 1_073_741_824.0,
            Some(64.0 * 1_073_741_824.0),
        );
        readings
    }

    #[test]
    fn widgets_only_paint_inside_their_box() {
        for style in Style::ALL {
            for backdrop in [false, true] {
                let mut widget = Widget::new(Metric::CpuUsage, style, 360.0, 360.0);
                widget.backdrop = backdrop;
                let image = overlay(&sample(), &[widget.clone()]);
                let mut painted = 0;
                for (x, y, pixel) in image.enumerate_pixels() {
                    if pixel[3] == 0 {
                        continue;
                    }
                    painted += 1;
                    let (x, y) = (x as f32, y as f32);
                    assert!(
                        x >= widget.x - 2.0
                            && x <= widget.x + widget.w + 2.0
                            && y >= widget.y - 2.0
                            && y <= widget.y + widget.h + 2.0,
                        "{style:?} painted outside its box at {x},{y}"
                    );
                }
                assert!(painted > 0, "{style:?} drew nothing");
            }
        }
    }

    #[test]
    fn a_gauge_fills_with_the_reading() {
        let mut widget = Widget::new(Metric::CpuUsage, Style::Ring, 360.0, 360.0);
        widget.caption = false;
        let image = overlay(&sample(), &[widget.clone()]);
        let color = Rgba([widget.color[0], widget.color[1], widget.color[2], 255]);
        let thickness = widget.w * 0.075;
        let radius = widget.w / 2.0 - thickness / 2.0 - widget.w * 0.05;
        // 37% of 270° from the lower left reaches past the left edge, not the top.
        let at = |degrees: f32| {
            let angle = degrees.to_radians();
            let (cx, cy) = widget.center();
            *image.get_pixel(
                (cx + radius * angle.cos()) as u32,
                (cy + radius * angle.sin()) as u32,
            )
        };
        assert_eq!(at(180.0), color, "filled part");
        assert_ne!(at(300.0), color, "empty part");
    }

    #[test]
    fn overlay_leaves_the_rest_of_the_picture_clear() {
        let layout = layout::arrange_band(&[Metric::CpuUsage, Metric::GpuTemp], false);
        let overlay = overlay(&sample(), &layout.widgets);
        assert_eq!(overlay.get_pixel(360, 100)[3], 0, "top is untouched");
        assert!(overlay.get_pixel(250, 690)[3] > 0, "band is at the bottom");
    }

    #[test]
    fn dashboard_draws_every_arrangement() {
        for count in 0..=stats::ALL.len() {
            let layout = layout::arrange_dashboard(&stats::ALL[..count], Background::default());
            let image = dashboard(&sample(), &layout, &backdrop(layout.background));
            assert_eq!(image.dimensions(), (SIZE, SIZE));
        }
    }

    #[test]
    fn plain_backdrop_has_no_glow() {
        let plain = backdrop(Background {
            style: Backdrop::Plain,
            ..Background::default()
        });
        assert_eq!(plain.get_pixel(560, 600), plain.get_pixel(0, 0));
        let glow = backdrop(Background::default());
        assert!(glow.get_pixel(560, 600)[0] > glow.get_pixel(0, 0)[0]);
    }

    #[test]
    fn compose_keeps_pixels_under_clear_overlay() {
        let mut picture = RgbImage::from_pixel(SIZE, SIZE, Rgb([10, 200, 30]));
        let mut overlay = RgbaImage::new(SIZE, SIZE);
        overlay.put_pixel(5, 5, Rgba([255, 255, 255, 255]));
        compose(&mut picture, &overlay);
        assert_eq!(picture.get_pixel(0, 0), &Rgb([10, 200, 30]));
        assert_eq!(picture.get_pixel(5, 5), &Rgb([255, 255, 255]));
    }
}
