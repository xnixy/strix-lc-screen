//! The stats panel: drawn on this computer and sent to the pump as part of the
//! picture, the way the ASUS app does it. Either a band over the user's photo
//! or video, or a full dashboard on its own background.

use crate::stats::{self, Metric, Readings, Sampler};
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use image::{Rgb, RgbImage, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

pub const SIZE: u32 = 720;
/// The band over a picture holds up to this many readings.
pub const MAX_ON_PICTURE: usize = 6;
/// The dashboard holds this many tiles, plus the time and date header.
pub const MAX_TILES: usize = 6;

const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);
const MUTED: Rgba<u8> = Rgba([178, 172, 184, 255]);
const ACCENT: Rgba<u8> = Rgba([232, 44, 72, 255]);
const TRACK: Rgba<u8> = Rgba([255, 255, 255, 38]);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Position {
    Top,
    #[default]
    Bottom,
}

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

/// A band of readings over a transparent canvas, for a photo or video. The
/// time and date get their own line on top and do not use up reading slots.
pub fn overlay(readings: &Readings, items: &[Metric], position: Position) -> RgbaImage {
    let mut canvas = RgbaImage::new(SIZE, SIZE);
    let clock: Vec<Metric> = items
        .iter()
        .copied()
        .filter(|metric| matches!(metric, Metric::Time | Metric::Date))
        .collect();
    let items: Vec<Metric> = items
        .iter()
        .copied()
        .filter(|metric| !clock.contains(metric))
        .take(MAX_ON_PICTURE)
        .collect();
    if items.is_empty() && clock.is_empty() {
        return canvas;
    }
    let columns = items.len().clamp(1, 3);
    let rows = items.len().div_ceil(3);
    let margin = 22.0;
    let row_height = 118.0;
    let clock_height = if clock.is_empty() { 0.0 } else { 62.0 };
    let height = clock_height
        + if rows > 0 {
            row_height * rows as f32 + 20.0
        } else {
            10.0
        };
    let top = match position {
        Position::Top => margin,
        Position::Bottom => SIZE as f32 - margin - height,
    };
    let (left, right) = (margin, SIZE as f32 - margin);
    fill_rounded(
        &mut canvas,
        left,
        top,
        right,
        top + height,
        22.0,
        Rgba([8, 6, 12, 190]),
    );
    if !clock.is_empty() {
        clock_line(&mut canvas, readings, &clock, top + 50.0);
    }
    let top = top + clock_height;
    let cell_width = (right - left - 20.0) / columns as f32;
    for (index, metric) in items.iter().enumerate() {
        let (row, column) = (index / 3, index % 3);
        // A short last row is centered.
        let in_row = if row == rows - 1 {
            items.len() - row * 3
        } else {
            columns
        };
        let offset = (columns - in_row) as f32 * cell_width / 2.0;
        let x0 = left + 10.0 + offset + column as f32 * cell_width;
        let y0 = top + 10.0 + row as f32 * row_height;
        reading_cell(
            &mut canvas,
            readings,
            *metric,
            x0,
            y0,
            cell_width,
            row_height,
        );
    }
    canvas
}

/// "19:45  MON 19 OCT", centered, with the time larger than the date.
fn clock_line(canvas: &mut RgbaImage, readings: &Readings, clock: &[Metric], baseline: f32) {
    let (bold, semi) = fonts();
    let time = clock
        .contains(&Metric::Time)
        .then(|| readings.text(Metric::Time));
    let date = clock
        .contains(&Metric::Date)
        .then(|| readings.text(Metric::Date));
    let (time_size, date_size, gap) = (44.0, 30.0, 18.0);
    let time_width = time
        .as_deref()
        .map_or(0.0, |text| measure(bold, time_size, text));
    let date_width = date
        .as_deref()
        .map_or(0.0, |text| measure(semi, date_size, text));
    let gap = if time.is_some() && date.is_some() {
        gap
    } else {
        0.0
    };
    let mut x = (SIZE as f32 - time_width - gap - date_width) / 2.0;
    if let Some(time) = &time {
        text(
            canvas,
            bold,
            time_size,
            x,
            baseline,
            time,
            WHITE,
            Align::Left,
            400.0,
        );
        x += time_width + gap;
    }
    if let Some(date) = &date {
        text(
            canvas,
            semi,
            date_size,
            x,
            baseline,
            date,
            MUTED,
            Align::Left,
            400.0,
        );
    }
}

fn reading_cell(
    canvas: &mut RgbaImage,
    readings: &Readings,
    metric: Metric,
    x0: f32,
    y0: f32,
    width: f32,
    height: f32,
) {
    let (bold, semi) = fonts();
    let center = x0 + width / 2.0;
    let inner = width - 24.0;
    text(
        canvas,
        semi,
        24.0,
        center,
        y0 + 32.0,
        metric.label(),
        MUTED,
        Align::Center,
        inner,
    );
    let value = readings.text(metric);
    text(
        canvas,
        bold,
        50.0,
        center,
        y0 + 80.0,
        &value,
        WHITE,
        Align::Center,
        inner,
    );
    if let Some(fraction) = readings.fraction(metric) {
        let bar_width = (inner * 0.7).min(140.0);
        let bx = center - bar_width / 2.0;
        let by = y0 + height - 26.0;
        bar(canvas, bx, by, bar_width, 5.0, fraction);
    }
}

/// A full-screen dashboard with the time on top and tiles below.
pub fn dashboard(readings: &Readings, items: &[Metric]) -> RgbImage {
    let mut canvas = RgbaImage::from_fn(SIZE, SIZE, background);
    corner_marks(&mut canvas);
    let (bold, semi) = fonts();
    let show_time = items.contains(&Metric::Time);
    let show_date = items.contains(&Metric::Date);
    let tiles: Vec<Metric> = items
        .iter()
        .copied()
        .filter(|metric| !matches!(metric, Metric::Time | Metric::Date))
        .take(MAX_TILES)
        .collect();

    let (time_size, date_size) = if tiles.is_empty() {
        (190.0, 52.0)
    } else {
        (120.0, 36.0)
    };
    // With nothing below it, the clock sits in the middle of the screen.
    let mut y = if tiles.is_empty() {
        let height = if show_time { time_size * 0.86 } else { 0.0 }
            + if show_date { date_size + 18.0 } else { 0.0 };
        (SIZE as f32 - height) / 2.0
    } else {
        56.0
    };
    if show_time {
        let size = time_size;
        text(
            &mut canvas,
            bold,
            size,
            360.0,
            y + size * 0.78,
            &readings.text(Metric::Time),
            WHITE,
            Align::Center,
            640.0,
        );
        y += size * 0.86;
    }
    if show_date {
        let size = date_size;
        text(
            &mut canvas,
            semi,
            size,
            360.0,
            y + size,
            &readings.text(Metric::Date),
            MUTED,
            Align::Center,
            640.0,
        );
        y += size + 18.0;
    }
    if show_time || show_date {
        y += 14.0;
    } else {
        y = 48.0;
    }
    if tiles.is_empty() {
        return flatten(&canvas);
    }

    let (left, right, bottom) = (40.0, SIZE as f32 - 40.0, SIZE as f32 - 40.0);
    let gap = 16.0;
    let rows = tiles.len().div_ceil(2);
    let tile_height = ((bottom - y - gap * (rows as f32 - 1.0)) / rows as f32).min(170.0);
    let used = tile_height * rows as f32 + gap * (rows as f32 - 1.0);
    let y = y + ((bottom - y) - used) / 2.0;
    for (index, metric) in tiles.iter().enumerate() {
        let (row, column) = (index / 2, index % 2);
        // An odd last tile takes the whole row.
        let wide = tiles.len() % 2 == 1 && index == tiles.len() - 1;
        let width = if wide {
            right - left
        } else {
            (right - left - gap) / 2.0
        };
        let x0 = left + column as f32 * (width + gap);
        let y0 = y + row as f32 * (tile_height + gap);
        tile(&mut canvas, readings, *metric, x0, y0, width, tile_height);
    }
    flatten(&canvas)
}

fn tile(
    canvas: &mut RgbaImage,
    readings: &Readings,
    metric: Metric,
    x0: f32,
    y0: f32,
    width: f32,
    height: f32,
) {
    let (bold, semi) = fonts();
    fill_rounded(
        canvas,
        x0,
        y0,
        x0 + width,
        y0 + height,
        18.0,
        Rgba([255, 255, 255, 16]),
    );
    fill_rounded(canvas, x0, y0 + 18.0, x0 + 5.0, y0 + 50.0, 2.5, ACCENT);
    let scale = (height / 170.0).clamp(0.6, 1.0);
    let pad = 22.0;
    text(
        canvas,
        semi,
        28.0 * scale,
        x0 + pad,
        y0 + 44.0 * scale,
        metric.label(),
        MUTED,
        Align::Left,
        width - pad * 2.0,
    );
    let value_size = 74.0 * scale;
    let baseline = y0 + height * 0.5 + value_size * 0.42;
    text(
        canvas,
        bold,
        value_size,
        x0 + pad,
        baseline,
        &readings.text(metric),
        WHITE,
        Align::Left,
        width - pad * 2.0,
    );
    if let Some(fraction) = readings.fraction(metric) {
        bar(
            canvas,
            x0 + pad,
            y0 + height - 26.0 * scale,
            width - pad * 2.0,
            6.0,
            fraction,
        );
    }
}

/// Near-black with a red glow in the lower right, like the ASUS backgrounds.
fn background(x: u32, y: u32) -> Rgba<u8> {
    let (dx, dy) = (x as f32 - 560.0, y as f32 - 600.0);
    let glow = (1.0 - (dx * dx + dy * dy).sqrt() / 620.0)
        .clamp(0.0, 1.0)
        .powf(1.6);
    let channel = |base: f32, lift: f32| (base + lift * glow).round() as u8;
    Rgba([
        channel(10.0, 70.0),
        channel(8.0, 6.0),
        channel(13.0, 20.0),
        255,
    ])
}

fn corner_marks(canvas: &mut RgbaImage) {
    let color = Rgba([255, 255, 255, 60]);
    let (near, far, length, thickness) = (18.0, SIZE as f32 - 18.0, 46.0, 3.0);
    for (x, y, dx, dy) in [
        (near, near, 1.0, 1.0),
        (far, near, -1.0, 1.0),
        (near, far, 1.0, -1.0),
        (far, far, -1.0, -1.0),
    ] {
        let (x1, x2) = order(x, x + dx * length);
        let (y1, y2) = order(y, y + dy * thickness);
        fill_rounded(canvas, x1, y1, x2, y2, 1.0, color);
        let (x1, x2) = order(x, x + dx * thickness);
        let (y1, y2) = order(y, y + dy * length);
        fill_rounded(canvas, x1, y1, x2, y2, 1.0, color);
    }
}

fn order(a: f32, b: f32) -> (f32, f32) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn bar(canvas: &mut RgbaImage, x: f32, y: f32, width: f32, height: f32, fraction: f64) {
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
            ACCENT,
        );
    }
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
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
    pub items: Vec<Metric>,
    /// Draw the band over the user's photo or video.
    pub on_picture: bool,
    pub position: Position,
    /// The dashboard is the picture.
    pub dashboard: bool,
    /// The window is open and shows live readings.
    pub window_open: bool,
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
        loop {
            let config = shared.config();
            let drawing = !config.items.is_empty() && (config.on_picture || config.dashboard);
            if drawing || config.window_open {
                let readings = sampler.sample();
                if drawing {
                    version += 1;
                    let overlay = config
                        .on_picture
                        .then(|| Arc::new(overlay(&readings, &config.items, config.position)));
                    let dashboard = config
                        .dashboard
                        .then(|| encode(&dashboard(&readings, &config.items)).map(Arc::new))
                        .flatten();
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
        if !shared.config().on_picture {
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
    fn time_and_date_do_not_take_a_reading_slot() {
        let six = [
            Metric::CpuUsage,
            Metric::CpuClock,
            Metric::GpuUsage,
            Metric::GpuTemp,
            Metric::MemoryUsed,
            Metric::NetDown,
        ];
        let mut with_clock = six.to_vec();
        with_clock.extend([Metric::Time, Metric::Date]);
        // The clock line makes the band taller, so it reaches higher up the picture.
        let plain = overlay(&sample(), &six, Position::Bottom);
        let clocked = overlay(&sample(), &with_clock, Position::Bottom);
        assert_eq!(plain.get_pixel(360, 400)[3], 0);
        assert!(clocked.get_pixel(360, 400)[3] > 0);
    }

    #[test]
    fn overlay_leaves_the_rest_of_the_picture_clear() {
        let overlay = overlay(
            &sample(),
            &[Metric::CpuUsage, Metric::GpuTemp],
            Position::Bottom,
        );
        assert_eq!(
            overlay.get_pixel(360, 100)[3],
            0,
            "top of the picture is untouched"
        );
        assert!(overlay.get_pixel(360, 690)[3] > 0, "band is at the bottom");
    }

    #[test]
    fn dashboard_draws_every_tile_count() {
        for count in 0..=MAX_TILES {
            let mut items = vec![Metric::Time, Metric::Date];
            items.extend(
                stats::ALL
                    .iter()
                    .copied()
                    .filter(|m| !matches!(m, Metric::Time | Metric::Date))
                    .take(count),
            );
            let image = dashboard(&sample(), &items);
            assert_eq!(image.dimensions(), (SIZE, SIZE));
        }
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
