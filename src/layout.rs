//! Where each reading sits on the pump screen and how it is drawn. Positions
//! and sizes are in screen pixels, 0 to 720 on both axes.

use crate::stats::Metric;
use serde::{Deserialize, Serialize};

pub const SCREEN: f32 = 720.0;
/// Smallest box a widget can be resized to.
pub const MIN_SIZE: f32 = 48.0;
pub const MAX_WIDGETS: usize = 24;

/// Colors offered in the window. The first is the app's accent.
pub const PALETTE: [[u8; 3]; 10] = [
    [232, 44, 72],
    [255, 106, 61],
    [255, 184, 48],
    [120, 220, 110],
    [46, 214, 190],
    [64, 170, 255],
    [124, 108, 255],
    [214, 92, 255],
    [255, 120, 180],
    [236, 236, 240],
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    /// The name over a large value.
    #[default]
    Text,
    /// A card with a colored edge, and a bar when the reading has a scale.
    Tile,
    /// A round gauge with the value in the middle.
    Ring,
    /// The name and value on one line over a long bar.
    Bar,
}

impl Style {
    pub const ALL: [Style; 4] = [Style::Text, Style::Tile, Style::Ring, Style::Bar];

    pub fn name(self) -> &'static str {
        match self {
            Style::Text => "Text",
            Style::Tile => "Tile",
            Style::Ring => "Gauge",
            Style::Bar => "Bar",
        }
    }

    /// A good starting size for a new widget in this style.
    pub fn default_size(self, metric: Metric) -> (f32, f32) {
        match (self, metric) {
            (Style::Text, Metric::Time) => (420.0, 170.0),
            (Style::Text, Metric::Date) => (360.0, 64.0),
            (Style::Text, _) => (220.0, 120.0),
            (Style::Tile, _) => (310.0, 150.0),
            (Style::Ring, _) => (220.0, 220.0),
            (Style::Bar, _) => (420.0, 96.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Widget {
    pub metric: Metric,
    #[serde(default)]
    pub style: Style,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    #[serde(default = "accent")]
    pub color: [u8; 3],
    /// Draw the reading's name, such as "CPU TEMP".
    #[serde(default = "yes")]
    pub caption: bool,
    /// A dark panel behind the widget, so it reads over a busy picture.
    #[serde(default)]
    pub backdrop: bool,
}

fn accent() -> [u8; 3] {
    PALETTE[0]
}

fn yes() -> bool {
    true
}

impl Widget {
    /// A new widget centered on `(cx, cy)`, in the style that suits the reading.
    pub fn new(metric: Metric, style: Style, cx: f32, cy: f32) -> Self {
        let (w, h) = style.default_size(metric);
        let mut widget = Self {
            metric,
            style,
            x: cx - w / 2.0,
            y: cy - h / 2.0,
            w,
            h,
            color: accent(),
            caption: !is_clock(metric),
            backdrop: false,
        };
        widget.keep_on_screen();
        widget
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }

    /// Change the style, and give a gauge a square box.
    pub fn set_style(&mut self, style: Style) {
        self.style = style;
        if style == Style::Ring {
            let side = self.w.min(self.h).max(MIN_SIZE * 2.0);
            let (cx, cy) = self.center();
            self.w = side;
            self.h = side;
            self.x = cx - side / 2.0;
            self.y = cy - side / 2.0;
        }
        self.keep_on_screen();
    }

    /// Clamp the size, then slide the box back inside the screen.
    pub fn keep_on_screen(&mut self) {
        self.w = self.w.clamp(MIN_SIZE, SCREEN);
        self.h = self.h.clamp(MIN_SIZE, SCREEN);
        if self.style == Style::Ring {
            let side = self.w.min(self.h);
            self.w = side;
            self.h = side;
        }
        self.x = self.x.clamp(0.0, SCREEN - self.w);
        self.y = self.y.clamp(0.0, SCREEN - self.h);
    }
}

pub fn is_clock(metric: Metric) -> bool {
    matches!(metric, Metric::Time | Metric::Date)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backdrop {
    /// Near-black with a glow in one corner, like the ASUS backgrounds.
    #[default]
    Glow,
    /// A glow in the middle of the screen.
    Spotlight,
    /// Plain near-black.
    Plain,
}

impl Backdrop {
    pub const ALL: [Backdrop; 3] = [Backdrop::Glow, Backdrop::Spotlight, Backdrop::Plain];

    pub fn name(self) -> &'static str {
        match self {
            Backdrop::Glow => "Corner glow",
            Backdrop::Spotlight => "Spotlight",
            Backdrop::Plain => "Plain",
        }
    }
}

/// The dashboard's background. Unused for widgets over a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Background {
    #[serde(default)]
    pub style: Backdrop,
    #[serde(default = "accent")]
    pub color: [u8; 3],
}

impl Default for Background {
    fn default() -> Self {
        Self {
            style: Backdrop::Glow,
            color: accent(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    #[serde(default)]
    pub widgets: Vec<Widget>,
    #[serde(default)]
    pub background: Background,
}

impl Layout {
    /// A free spot near the middle for one more widget, stepped so new ones do not stack exactly.
    pub fn next_spot(&self) -> (f32, f32) {
        let step = (self.widgets.len() % 6) as f32 * 28.0 - 70.0;
        (SCREEN / 2.0 + step, SCREEN / 2.0 + step)
    }
}

/// The dashboard the way earlier versions drew it: the time and date on top,
/// then tiles two to a row, an odd last tile taking the whole row.
pub fn arrange_dashboard(items: &[Metric], background: Background) -> Layout {
    let show_time = items.contains(&Metric::Time);
    let show_date = items.contains(&Metric::Date);
    let tiles: Vec<Metric> = items
        .iter()
        .copied()
        .filter(|metric| !is_clock(*metric))
        .take(8)
        .collect();
    let mut widgets = Vec::new();
    let (time_h, date_h) = if tiles.is_empty() {
        (220.0, 70.0)
    } else {
        (130.0, 50.0)
    };
    let clock_h = if show_time { time_h } else { 0.0 } + if show_date { date_h } else { 0.0 };
    let mut y = if tiles.is_empty() {
        (SCREEN - clock_h) / 2.0
    } else {
        40.0
    };
    if show_time {
        widgets.push(clock_widget(Metric::Time, y, time_h));
        y += time_h;
    }
    if show_date {
        widgets.push(clock_widget(Metric::Date, y, date_h));
        y += date_h;
    }
    y = if clock_h > 0.0 { y + 16.0 } else { 40.0 };
    if !tiles.is_empty() {
        let (left, right, bottom, gap) = (40.0, SCREEN - 40.0, SCREEN - 40.0, 16.0);
        let rows = tiles.len().div_ceil(2);
        let tile_h = ((bottom - y - gap * (rows as f32 - 1.0)) / rows as f32).min(170.0);
        let used = tile_h * rows as f32 + gap * (rows as f32 - 1.0);
        let top = y + ((bottom - y) - used) / 2.0;
        for (index, metric) in tiles.iter().enumerate() {
            let (row, column) = (index / 2, index % 2);
            let wide = tiles.len() % 2 == 1 && index == tiles.len() - 1;
            let w = if wide {
                right - left
            } else {
                (right - left - gap) / 2.0
            };
            let mut widget = Widget::new(*metric, Style::Tile, 0.0, 0.0);
            widget.x = left + column as f32 * (w + gap);
            widget.y = top + row as f32 * (tile_h + gap);
            widget.w = w;
            widget.h = tile_h.max(MIN_SIZE);
            widget.keep_on_screen();
            widgets.push(widget);
        }
    }
    Layout {
        widgets,
        background,
    }
}

fn clock_widget(metric: Metric, y: f32, h: f32) -> Widget {
    let mut widget = Widget::new(metric, Style::Text, SCREEN / 2.0, 0.0);
    widget.x = 60.0;
    widget.w = SCREEN - 120.0;
    widget.y = y;
    widget.h = h;
    widget.caption = false;
    widget.keep_on_screen();
    widget
}

/// Readings over a picture, the way earlier versions drew the band: three to
/// a row along the top or bottom edge, with the time and date on their own line.
pub fn arrange_band(items: &[Metric], at_top: bool) -> Layout {
    let clock: Vec<Metric> = items
        .iter()
        .copied()
        .filter(|metric| is_clock(*metric))
        .collect();
    let readings: Vec<Metric> = items
        .iter()
        .copied()
        .filter(|metric| !is_clock(*metric))
        .take(6)
        .collect();
    let (margin, gap, row_h, clock_h) = (22.0, 10.0, 118.0, 64.0);
    let rows = readings.len().div_ceil(3);
    let height = if clock.is_empty() { 0.0 } else { clock_h + gap }
        + rows as f32 * row_h
        + rows.saturating_sub(1) as f32 * gap;
    let mut y = if at_top {
        margin
    } else {
        SCREEN - margin - height
    };
    let mut widgets = Vec::new();
    if !clock.is_empty() {
        let w = if clock.len() == 2 { 300.0 } else { 220.0 };
        let total = w * clock.len() as f32 + gap * (clock.len() as f32 - 1.0);
        let mut x = (SCREEN - total) / 2.0;
        for metric in clock {
            let mut widget = Widget::new(metric, Style::Text, 0.0, 0.0);
            (widget.x, widget.y, widget.w, widget.h) = (x, y, w, clock_h);
            widget.backdrop = true;
            widget.keep_on_screen();
            widgets.push(widget);
            x += w + gap;
        }
        y += clock_h + gap;
    }
    let cell_w = (SCREEN - margin * 2.0 - gap * 2.0) / 3.0;
    for (index, metric) in readings.iter().enumerate() {
        let (row, column) = (index / 3, index % 3);
        let in_row = (readings.len() - row * 3).min(3);
        let offset = (3 - in_row) as f32 * (cell_w + gap) / 2.0;
        let mut widget = Widget::new(*metric, Style::Text, 0.0, 0.0);
        widget.x = margin + offset + column as f32 * (cell_w + gap);
        widget.y = y + row as f32 * (row_h + gap);
        widget.w = cell_w;
        widget.h = row_h;
        widget.backdrop = true;
        widget.keep_on_screen();
        widgets.push(widget);
    }
    Layout {
        widgets,
        background: Background::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arranged_widgets_stay_on_screen_and_do_not_overlap() {
        let items = crate::stats::ALL;
        for count in 0..=items.len() {
            for layout in [
                arrange_dashboard(&items[..count], Background::default()),
                arrange_band(&items[..count], false),
                arrange_band(&items[..count], true),
            ] {
                for (index, a) in layout.widgets.iter().enumerate() {
                    assert!(a.x >= 0.0 && a.y >= 0.0, "{a:?}");
                    assert!(a.x + a.w <= SCREEN && a.y + a.h <= SCREEN, "{a:?}");
                    for b in &layout.widgets[index + 1..] {
                        let apart = a.x + a.w <= b.x + 0.5
                            || b.x + b.w <= a.x + 0.5
                            || a.y + a.h <= b.y + 0.5
                            || b.y + b.h <= a.y + 0.5;
                        assert!(apart, "{a:?} overlaps {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_gauge_keeps_a_square_box() {
        let mut widget = Widget::new(Metric::CpuUsage, Style::Tile, 360.0, 360.0);
        widget.set_style(Style::Ring);
        assert_eq!(widget.w, widget.h);
        widget.w = 500.0;
        widget.h = 200.0;
        widget.keep_on_screen();
        assert_eq!(widget.w, widget.h);
    }

    #[test]
    fn widgets_dragged_off_the_edge_come_back() {
        let mut widget = Widget::new(Metric::NetDown, Style::Text, 360.0, 360.0);
        widget.x = -80.0;
        widget.y = 900.0;
        widget.keep_on_screen();
        assert_eq!(widget.x, 0.0);
        assert_eq!(widget.y, SCREEN - widget.h);
    }

    #[test]
    fn older_widget_entries_fill_in_defaults() {
        let saved = r#"{"metric":"cpu_temp","x":10,"y":20,"w":200,"h":100}"#;
        let widget: Widget = serde_json::from_str(saved).unwrap();
        assert_eq!(widget.style, Style::Text);
        assert!(widget.caption);
        assert_eq!(widget.color, PALETTE[0]);
    }
}
