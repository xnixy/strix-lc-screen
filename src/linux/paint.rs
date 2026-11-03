//! Drawing the pump screen in the window. The widgets and backgrounds come from
//! the same code that draws them for the pump, so the preview matches it.

use crate::layout::{Background, Layout, Widget, SCREEN};
use crate::media::Crop;
use crate::panel;
use crate::stats::Readings;
use gtk::cairo;
use std::f64::consts::{FRAC_PI_2, PI};
use std::time::{Duration, Instant};

/// Readings are redrawn this often while nothing moves.
const REFRESH: Duration = Duration::from_secs(1);

/// Which layout is being edited or previewed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Canvas {
    #[default]
    Dashboard,
    Picture,
}

/// An RGBA image as a cairo surface, which wants premultiplied BGRA.
pub fn surface(image: &image::RgbaImage) -> cairo::ImageSurface {
    let (width, height) = image.dimensions();
    let stride = cairo::Format::ARgb32
        .stride_for_width(width)
        .expect("a valid width") as usize;
    let mut data = vec![0u8; stride * height as usize];
    for (y, row) in image.rows().enumerate() {
        for (x, pixel) in row.enumerate() {
            let [r, g, b, a] = pixel.0;
            let scale = |c: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
            let at = y * stride + x * 4;
            data[at..at + 4].copy_from_slice(&[scale(b), scale(g), scale(r), a]);
        }
    }
    cairo::ImageSurface::create_for_data(
        data,
        cairo::Format::ARgb32,
        width as i32,
        height as i32,
        stride as i32,
    )
    .expect("an image surface")
}

/// The preview of the chosen photo or video.
pub struct Picture {
    pub surface: cairo::ImageSurface,
}

impl Picture {
    pub fn from_jpeg(jpeg: &[u8]) -> Option<Self> {
        let image = image::load_from_memory(jpeg).ok()?.into_rgba8();
        Some(Self {
            surface: surface(&image),
        })
    }

    /// The part the crop square covers, in this preview's pixels. The crop is
    /// worked out on the source size, which can be larger than the preview.
    pub fn crop_rect(&self, source: (u32, u32), crop: &Crop) -> (f64, f64, f64) {
        let (width, height) = (source.0.max(1), source.1.max(1));
        let (side, x, y) = crop.square(width, height);
        let sx = self.surface.width() as f64 / width as f64;
        let sy = self.surface.height() as f64 / height as f64;
        (x as f64 * sx, y as f64 * sy, side as f64 * sx.min(sy))
    }
}

struct Drawn {
    widgets: Vec<Widget>,
    at: Instant,
    surface: cairo::ImageSurface,
}

/// Keeps the drawn widgets and backgrounds between frames.
#[derive(Default)]
pub struct Painter {
    overlay: [Option<Drawn>; 2],
    background: Option<(Background, cairo::ImageSurface)>,
}

impl Painter {
    fn overlay(
        &mut self,
        canvas: Canvas,
        widgets: &[Widget],
        readings: Option<&Readings>,
    ) -> &cairo::ImageSurface {
        let slot = &mut self.overlay[canvas as usize];
        let stale = slot
            .as_ref()
            .is_none_or(|drawn| drawn.widgets != widgets || drawn.at.elapsed() >= REFRESH);
        if stale {
            let fallback = Readings::default();
            let image = panel::overlay(readings.unwrap_or(&fallback), widgets);
            *slot = Some(Drawn {
                widgets: widgets.to_vec(),
                at: Instant::now(),
                surface: surface(&image),
            });
        }
        &slot.as_ref().expect("drawn above").surface
    }

    fn background(&mut self, background: Background) -> &cairo::ImageSurface {
        if self
            .background
            .as_ref()
            .is_none_or(|(drawn, _)| *drawn != background)
        {
            self.background = Some((background, surface(&panel::backdrop(background))));
        }
        &self.background.as_ref().expect("drawn above").1
    }

    /// Paint what the pump shows for this layout into the square at `(x, y)`.
    #[allow(clippy::too_many_arguments)]
    pub fn screen(
        &mut self,
        cr: &cairo::Context,
        (x, y, side): (f64, f64, f64),
        canvas: Canvas,
        layout: &Layout,
        readings: Option<&Readings>,
        picture: Option<(&Picture, (f64, f64, f64))>,
    ) {
        let _ = cr.save();
        rounded(cr, x, y, side, side, 10.0);
        cr.clip();
        match (canvas, picture) {
            (Canvas::Dashboard, _) => {
                let background = self.background(layout.background);
                fill_with(
                    cr,
                    background,
                    (0.0, 0.0, background.width() as f64),
                    (x, y, side),
                );
            }
            (Canvas::Picture, Some((picture, crop))) => {
                fill_with(cr, &picture.surface, crop, (x, y, side));
            }
            (Canvas::Picture, None) => {
                cr.set_source_rgb(0.09, 0.07, 0.1);
                let _ = cr.paint();
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
                centered_text(
                    cr,
                    "Your photo or video goes here",
                    x + side / 2.0,
                    y + side / 2.0,
                    14.0,
                );
            }
        }
        let overlay = self.overlay(canvas, &layout.widgets, readings);
        fill_with(
            cr,
            overlay,
            (0.0, 0.0, overlay.width() as f64),
            (x, y, side),
        );
        let _ = cr.restore();
        // The bezel, so the preview reads as a screen.
        cr.set_source_rgb(0.03, 0.02, 0.04);
        cr.set_line_width(5.0);
        rounded(cr, x - 2.5, y - 2.5, side + 5.0, side + 5.0, 12.0);
        let _ = cr.stroke();
    }
}

/// Draw the square `(sx, sy, side)` of `source` into the square `(x, y, side)`.
fn fill_with(
    cr: &cairo::Context,
    source: &cairo::ImageSurface,
    (sx, sy, from): (f64, f64, f64),
    (x, y, to): (f64, f64, f64),
) {
    if from <= 0.0 {
        return;
    }
    let _ = cr.save();
    cr.translate(x, y);
    cr.scale(to / from, to / from);
    let _ = cr.set_source_surface(source, -sx, -sy);
    cr.source().set_filter(cairo::Filter::Good);
    cr.rectangle(0.0, 0.0, from, from);
    let _ = cr.fill();
    let _ = cr.restore();
}

pub fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

pub fn centered_text(cr: &cairo::Context, text: &str, cx: f64, cy: f64, size: f64) {
    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(size);
    if let Ok(extents) = cr.text_extents(text) {
        cr.move_to(
            cx - extents.width() / 2.0 - extents.x_bearing(),
            cy - extents.height() / 2.0 - extents.y_bearing(),
        );
        let _ = cr.show_text(text);
    }
}

/// Where the screen square sits in a drawing area of this size: centered, as
/// large as fits with room for the bezel.
pub fn square_in(width: i32, height: i32) -> (f64, f64, f64) {
    let side = (width.min(height) as f64 - 12.0).max(1.0);
    (
        (width as f64 - side) / 2.0,
        (height as f64 - side) / 2.0,
        side,
    )
}

/// Pixels of the pump screen per pixel of the window.
pub fn scale_of(side: f64) -> f64 {
    side / SCREEN as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surfaces_are_premultiplied_bgra() {
        let image = image::RgbaImage::from_pixel(2, 1, image::Rgba([200, 100, 50, 128]));
        let mut surface = surface(&image);
        let data = surface.data().expect("readable");
        assert_eq!(&data[..4], &[25, 50, 100, 128]);
    }

    #[test]
    fn the_square_is_centered() {
        assert_eq!(square_in(212, 112), (56.0, 6.0, 100.0));
    }
}
