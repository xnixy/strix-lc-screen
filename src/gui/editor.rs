//! The widget editor: a live preview of the pump screen where readings are
//! dragged into place, resized from a corner, and styled from the side panel.

use crate::layout::{self, Backdrop, Background, Layout, Style, Widget, SCREEN};
use crate::panel;
use crate::stats::{self, Metric, Readings};
use crate::theme::{self, ACCENT, MUTED, ROW_HOVER, TEXT};
use eframe::egui;
use egui::{
    Color32, CornerRadius, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    TextureHandle, Ui,
};
use std::time::{Duration, Instant};

/// Which layout is being edited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Canvas {
    #[default]
    Dashboard,
    Picture,
}

/// What an edit did, so the window knows when to save.
#[derive(Clone, Copy, Default)]
pub struct Edit {
    pub changed: bool,
    /// The change is finished, for example the drag was let go. Worth saving.
    pub settled: bool,
}

impl Edit {
    pub fn done() -> Self {
        Self {
            changed: true,
            settled: true,
        }
    }

    pub fn merge(&mut self, other: Edit) {
        self.changed |= other.changed;
        self.settled |= other.settled;
    }
}

enum Action {
    Duplicate,
    Remove,
}

#[derive(Clone, Copy)]
enum Drag {
    /// Moving a widget. `grab` is where the pointer holds it, from its corner.
    Move {
        grab: egui::Vec2,
    },
    Resize,
}

/// Snap distance, in screen pixels.
const SNAP: f32 = 9.0;
/// Lines worth lining up with: the edges, a margin inside them, and the middle.
const STOPS: [f32; 5] = [0.0, 24.0, SCREEN / 2.0, SCREEN - 24.0, SCREEN];
/// Readings are redrawn this often while nothing moves.
const REFRESH: Duration = Duration::from_secs(1);

struct Drawn {
    widgets: Vec<Widget>,
    at: Instant,
    texture: TextureHandle,
}

#[derive(Default)]
pub struct Editor {
    pub canvas: Canvas,
    pub selected: Option<usize>,
    pub snap: bool,
    drag: Option<Drag>,
    guides: Vec<(bool, f32)>,
    drawn: [Option<Drawn>; 2],
    background: Option<(Background, TextureHandle)>,
}

fn texture_options() -> egui::TextureOptions {
    egui::TextureOptions {
        mipmap_mode: Some(egui::TextureFilter::Linear),
        ..egui::TextureOptions::LINEAR
    }
}

impl Editor {
    pub fn new() -> Self {
        Self {
            snap: true,
            ..Self::default()
        }
    }

    /// The widgets drawn by the same code that draws them for the pump.
    fn overlay(
        &mut self,
        ctx: &egui::Context,
        canvas: Canvas,
        widgets: &[Widget],
        readings: Option<&Readings>,
    ) -> egui::TextureId {
        let slot = &mut self.drawn[canvas as usize];
        let stale = slot
            .as_ref()
            .is_none_or(|drawn| drawn.widgets != widgets || drawn.at.elapsed() >= REFRESH);
        if stale {
            let fallback = Readings::default();
            let image = panel::overlay(readings.unwrap_or(&fallback), widgets);
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [image.width() as usize, image.height() as usize],
                image.as_raw(),
            );
            match slot {
                Some(drawn) => {
                    drawn.texture.set(color, texture_options());
                    drawn.widgets = widgets.to_vec();
                    drawn.at = Instant::now();
                }
                None => {
                    *slot = Some(Drawn {
                        widgets: widgets.to_vec(),
                        at: Instant::now(),
                        texture: ctx.load_texture(
                            format!("widgets-{}", canvas as usize),
                            color,
                            texture_options(),
                        ),
                    });
                }
            }
        }
        ctx.request_repaint_after(REFRESH);
        slot.as_ref().expect("drawn above").texture.id()
    }

    fn background(&mut self, ctx: &egui::Context, background: Background) -> egui::TextureId {
        if self
            .background
            .as_ref()
            .is_none_or(|(drawn, _)| *drawn != background)
        {
            let image = panel::backdrop(background);
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [image.width() as usize, image.height() as usize],
                image.as_raw(),
            );
            let texture = ctx.load_texture("dashboard-background", color, texture_options());
            self.background = Some((background, texture));
        }
        self.background.as_ref().expect("drawn above").1.id()
    }

    /// Paint what the pump would show for this layout, without any editing.
    pub fn preview(
        &mut self,
        ui: &mut Ui,
        side: f32,
        canvas: Canvas,
        layout: &Layout,
        readings: Option<&Readings>,
        picture: Option<(&TextureHandle, Rect)>,
    ) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), Sense::hover());
        self.paint_screen(ui, rect, canvas, layout, readings, picture);
    }

    fn paint_screen(
        &mut self,
        ui: &Ui,
        rect: Rect,
        canvas: Canvas,
        layout: &Layout,
        readings: Option<&Readings>,
        picture: Option<(&TextureHandle, Rect)>,
    ) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter_at(rect);
        let full = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
        match (canvas, picture) {
            (Canvas::Dashboard, _) => {
                let background = self.background(&ctx, layout.background);
                painter.image(background, rect, full, Color32::WHITE);
            }
            (Canvas::Picture, Some((texture, uv))) => {
                painter.image(texture.id(), rect, uv, Color32::WHITE);
            }
            (Canvas::Picture, None) => {
                painter.rect_filled(rect, 0.0, Color32::from_rgb(22, 18, 26));
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Your photo or video goes here",
                    egui::FontId::proportional(14.0),
                    MUTED.gamma_multiply(0.6),
                );
            }
        }
        let overlay = self.overlay(&ctx, canvas, &layout.widgets, readings);
        painter.image(overlay, rect, full, Color32::WHITE);
        // The bezel, so the preview reads as a screen.
        ui.painter().rect_stroke(
            rect.expand(3.0),
            CornerRadius::same(14),
            Stroke::new(4.0_f32, Color32::from_rgb(8, 6, 10)),
            StrokeKind::Outside,
        );
        ui.painter().rect_stroke(
            rect.expand(7.0),
            CornerRadius::same(18),
            Stroke::new(1.0_f32, Color32::from_white_alpha(40)),
            StrokeKind::Outside,
        );
    }

    /// The editable screen. Click to select, drag to move, drag the corner dot
    /// to resize. Arrow keys nudge, Delete removes, and Alt turns snapping off.
    pub fn edit(
        &mut self,
        ui: &mut Ui,
        side: f32,
        layout: &mut Layout,
        readings: Option<&Readings>,
        picture: Option<(&TextureHandle, Rect)>,
    ) -> Edit {
        let mut edit = Edit::default();
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(side, side), Sense::click_and_drag());
        let scale = side / SCREEN;
        let to_screen = |x: f32, y: f32| rect.min + egui::vec2(x, y) * scale;
        let from_screen = |pos: Pos2| {
            let local = (pos - rect.min) / scale;
            (local.x, local.y)
        };
        let box_of = |widget: &Widget| {
            Rect::from_min_max(
                to_screen(widget.x, widget.y),
                to_screen(widget.x + widget.w, widget.y + widget.h),
            )
        };
        if self
            .selected
            .is_some_and(|index| index >= layout.widgets.len())
        {
            self.selected = None;
        }
        let hit = |layout: &Layout, pos: Pos2| {
            let (x, y) = from_screen(pos);
            layout
                .widgets
                .iter()
                .rposition(|widget| widget.contains(x, y))
        };
        let handle_of = |rect: Rect| Rect::from_center_size(rect.max, egui::vec2(16.0, 16.0));
        let on_handle = |layout: &Layout, selected: Option<usize>, pos: Pos2| {
            selected
                .and_then(|index| layout.widgets.get(index))
                .is_some_and(|widget| handle_of(box_of(widget)).contains(pos))
        };

        // Start a drag from where the button went down, not where the drag was noticed.
        if response.drag_started() {
            let origin = ui
                .input(|input| input.pointer.press_origin())
                .or(response.interact_pointer_pos());
            if let Some(origin) = origin {
                if on_handle(layout, self.selected, origin) {
                    self.drag = Some(Drag::Resize);
                } else if let Some(index) = hit(layout, origin) {
                    self.selected = Some(index);
                    let widget = &layout.widgets[index];
                    let (x, y) = from_screen(origin);
                    self.drag = Some(Drag::Move {
                        grab: egui::vec2(x - widget.x, y - widget.y),
                    });
                } else {
                    self.selected = None;
                }
            }
        }
        if response.dragged() {
            if let (Some(drag), Some(index), Some(pointer)) =
                (self.drag, self.selected, response.interact_pointer_pos())
            {
                let snap = self.snap && !ui.input(|input| input.modifiers.alt);
                let (px, py) = from_screen(pointer);
                let others: Vec<Widget> = layout
                    .widgets
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(_, widget)| widget.clone())
                    .collect();
                let widget = &mut layout.widgets[index];
                let before = widget.clone();
                self.guides.clear();
                match drag {
                    Drag::Move { grab } => {
                        widget.x = px - grab.x;
                        widget.y = py - grab.y;
                        widget.keep_on_screen();
                        if snap {
                            let (dx, gx) = snap_axis(
                                [widget.x, widget.x + widget.w / 2.0, widget.x + widget.w],
                                &stops(&others, true),
                            );
                            let (dy, gy) = snap_axis(
                                [widget.y, widget.y + widget.h / 2.0, widget.y + widget.h],
                                &stops(&others, false),
                            );
                            widget.x += dx;
                            widget.y += dy;
                            widget.keep_on_screen();
                            self.guides.extend(gx.map(|x| (true, x)));
                            self.guides.extend(gy.map(|y| (false, y)));
                        }
                    }
                    Drag::Resize => {
                        let (mut right, mut bottom) = (px, py);
                        if snap {
                            let (dx, gx) = snap_axis([right], &stops(&others, true));
                            let (dy, gy) = snap_axis([bottom], &stops(&others, false));
                            right += dx;
                            bottom += dy;
                            self.guides.extend(gx.map(|x| (true, x)));
                            self.guides.extend(gy.map(|y| (false, y)));
                        }
                        let right = right.min(SCREEN);
                        let bottom = bottom.min(SCREEN);
                        widget.w = (right - widget.x).max(layout::MIN_SIZE);
                        widget.h = (bottom - widget.y).max(layout::MIN_SIZE);
                        if widget.style == Style::Ring {
                            let side = widget
                                .w
                                .max(widget.h)
                                .min(SCREEN - widget.x)
                                .min(SCREEN - widget.y);
                            widget.w = side;
                            widget.h = side;
                        }
                        widget.keep_on_screen();
                    }
                }
                if *widget != before {
                    edit.changed = true;
                }
            }
        }
        if response.drag_stopped() {
            if self.drag.take().is_some() {
                edit.settled = true;
            }
            self.guides.clear();
        }
        if response.clicked() || response.secondary_clicked() {
            self.selected = response
                .interact_pointer_pos()
                .and_then(|pos| hit(layout, pos));
        }
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        edit.merge(self.keyboard(ui, layout));
        let mut menu_edit = Edit::default();
        response.context_menu(|ui| {
            let Some(index) = self.selected else {
                ui.label(theme::muted("Right-click a widget to change it."));
                return;
            };
            if ui.button("Duplicate").clicked() {
                self.duplicate(layout, index);
                menu_edit = Edit::done();
                ui.close_menu();
            }
            if ui.button("Bring to front").clicked() {
                let widget = layout.widgets.remove(index);
                layout.widgets.push(widget);
                self.selected = Some(layout.widgets.len() - 1);
                menu_edit = Edit::done();
                ui.close_menu();
            }
            if ui.button("Send to back").clicked() {
                let widget = layout.widgets.remove(index);
                layout.widgets.insert(0, widget);
                self.selected = Some(0);
                menu_edit = Edit::done();
                ui.close_menu();
            }
            if ui.button("Remove").clicked() {
                layout.widgets.remove(index);
                self.selected = None;
                menu_edit = Edit::done();
                ui.close_menu();
            }
        });
        edit.merge(menu_edit);

        let canvas = self.canvas;
        self.paint_screen(ui, rect, canvas, layout, readings, picture);
        let painter = ui.painter_at(rect.expand(12.0));
        let hover = response.hover_pos();
        let hovered = hover.and_then(|pos| hit(layout, pos));
        for (vertical, at) in &self.guides {
            let (a, b) = if *vertical {
                (to_screen(*at, 0.0), to_screen(*at, SCREEN))
            } else {
                (to_screen(0.0, *at), to_screen(SCREEN, *at))
            };
            painter.extend(egui::Shape::dashed_line(
                &[a, b],
                Stroke::new(1.0_f32, Color32::from_rgb(255, 150, 170)),
                5.0,
                4.0,
            ));
        }
        if let Some(index) = hovered.filter(|index| Some(*index) != self.selected) {
            painter.rect_stroke(
                box_of(&layout.widgets[index]),
                CornerRadius::same(6),
                Stroke::new(1.0_f32, Color32::from_white_alpha(110)),
                StrokeKind::Outside,
            );
        }
        if let Some(widget) = self.selected.and_then(|index| layout.widgets.get(index)) {
            let outline = box_of(widget);
            painter.rect_stroke(
                outline,
                CornerRadius::same(6),
                Stroke::new(2.0_f32, ACCENT),
                StrokeKind::Outside,
            );
            painter.circle_filled(outline.max, 6.5, Color32::WHITE);
            painter.circle_stroke(outline.max, 6.5, Stroke::new(2.0_f32, ACCENT));
            if self.drag.is_some() {
                let tag = format!(
                    "{:.0}, {:.0}   {:.0} × {:.0}",
                    widget.x, widget.y, widget.w, widget.h
                );
                let at = egui::pos2(outline.left(), (outline.top() - 6.0).max(rect.top() + 14.0));
                let galley =
                    painter.layout_no_wrap(tag, egui::FontId::proportional(11.5), Color32::WHITE);
                let back = Rect::from_min_size(
                    at - egui::vec2(0.0, galley.size().y + 4.0),
                    galley.size() + egui::vec2(10.0, 4.0),
                );
                painter.rect_filled(back, CornerRadius::same(5), Color32::from_black_alpha(200));
                painter.galley(back.min + egui::vec2(5.0, 2.0), galley, Color32::WHITE);
            }
        }
        let cursor = match (self.drag, hover) {
            (Some(Drag::Resize), _) => Some(CursorIcon::ResizeNwSe),
            (Some(Drag::Move { .. }), _) => Some(CursorIcon::Grabbing),
            (None, Some(pos)) if on_handle(layout, self.selected, pos) => {
                Some(CursorIcon::ResizeNwSe)
            }
            (None, Some(_)) if hovered.is_some() => Some(CursorIcon::Grab),
            _ => None,
        };
        if let Some(cursor) = cursor {
            ui.ctx().set_cursor_icon(cursor);
        }
        edit
    }

    fn keyboard(&mut self, ui: &Ui, layout: &mut Layout) -> Edit {
        let Some(index) = self.selected else {
            return Edit::default();
        };
        if ui.ctx().wants_keyboard_input() || index >= layout.widgets.len() {
            return Edit::default();
        }
        let (step, nudge, remove, duplicate) = ui.input(|input| {
            let step = if input.modifiers.shift { 10.0 } else { 1.0 };
            let mut nudge = egui::Vec2::ZERO;
            if input.key_pressed(egui::Key::ArrowLeft) {
                nudge.x -= 1.0;
            }
            if input.key_pressed(egui::Key::ArrowRight) {
                nudge.x += 1.0;
            }
            if input.key_pressed(egui::Key::ArrowUp) {
                nudge.y -= 1.0;
            }
            if input.key_pressed(egui::Key::ArrowDown) {
                nudge.y += 1.0;
            }
            let remove =
                input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace);
            let duplicate = input.modifiers.command && input.key_pressed(egui::Key::D);
            (step, nudge, remove, duplicate)
        });
        if remove {
            layout.widgets.remove(index);
            self.selected = None;
            return Edit::done();
        }
        if duplicate {
            self.duplicate(layout, index);
            return Edit::done();
        }
        if nudge != egui::Vec2::ZERO {
            let widget = &mut layout.widgets[index];
            widget.x += nudge.x * step;
            widget.y += nudge.y * step;
            widget.keep_on_screen();
            return Edit::done();
        }
        Edit::default()
    }

    fn duplicate(&mut self, layout: &mut Layout, index: usize) {
        if layout.widgets.len() >= layout::MAX_WIDGETS {
            return;
        }
        let mut copy = layout.widgets[index].clone();
        copy.x += 24.0;
        copy.y += 24.0;
        copy.keep_on_screen();
        layout.widgets.push(copy);
        self.selected = Some(layout.widgets.len() - 1);
    }

    /// The selected widget's style, size, color, and order.
    pub fn inspector(
        &mut self,
        ui: &mut Ui,
        layout: &mut Layout,
        readings: Option<&Readings>,
    ) -> Edit {
        let mut edit = Edit::default();
        let mut action = None;
        let Some(index) = self.selected.filter(|index| *index < layout.widgets.len()) else {
            theme::card(ui, |ui| {
                ui.label(RichText::new("Arrange the screen").size(16.0).color(TEXT));
                ui.add_space(4.0);
                for line in [
                    "Click a widget to select it, then drag it anywhere.",
                    "Drag the white dot in its corner to resize it.",
                    "Arrow keys nudge it, Shift moves it further, Delete removes it.",
                    "It snaps to the middle, the edges, and other widgets. Hold Alt to place it freely.",
                ] {
                    ui.add(egui::Label::new(theme::muted(line)).wrap());
                }
            });
            return edit;
        };
        theme::card(ui, |ui| {
            let widget = &mut layout.widgets[index];
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(theme::muted(widget.metric.group()).small());
                    ui.label(RichText::new(widget.metric.name()).size(17.0).color(TEXT));
                });
                if let Some(readings) = readings {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(readings.text(widget.metric))
                                .size(17.0)
                                .color(theme::color32(widget.color)),
                        );
                    });
                }
            });
            ui.add_space(4.0);
            theme::section_label(ui, "Style");
            let mut style = widget.style;
            let options: Vec<(Style, &str)> = Style::ALL
                .iter()
                .map(|style| (*style, style.name()))
                .collect();
            if theme::segmented(ui, &mut style, &options) {
                widget.set_style(style);
                edit = Edit::done();
            }
            theme::section_label(ui, "Reading");
            let mut metric = widget.metric;
            egui::ComboBox::from_id_salt("widget-metric")
                .width(ui.available_width() - 8.0)
                .selected_text(format!("{} · {}", metric.group(), metric.name()))
                .show_ui(ui, |ui| {
                    for option in stats::ALL {
                        ui.selectable_value(
                            &mut metric,
                            option,
                            format!("{} · {}", option.group(), option.name()),
                        );
                    }
                });
            if metric != widget.metric {
                widget.metric = metric;
                edit = Edit::done();
            }
            theme::section_label(ui, "Size");
            let ring = widget.style == Style::Ring;
            let mut w = widget.w;
            let slider = theme::slider_row(ui, if ring { "Size" } else { "Width" }, |ui| {
                ui.add(egui::Slider::new(&mut w, layout::MIN_SIZE..=SCREEN).integer())
            });
            if slider.changed() {
                let (cx, cy) = widget.center();
                widget.w = w;
                if ring {
                    widget.h = w;
                }
                widget.x = cx - widget.w / 2.0;
                if ring {
                    widget.y = cy - widget.h / 2.0;
                }
                widget.keep_on_screen();
                edit.changed = true;
            }
            edit.settled |= slider.drag_stopped() || slider.lost_focus();
            if !ring {
                let mut h = widget.h;
                let slider = theme::slider_row(ui, "Height", |ui| {
                    ui.add(egui::Slider::new(&mut h, layout::MIN_SIZE..=SCREEN).integer())
                });
                if slider.changed() {
                    let (_, cy) = widget.center();
                    widget.h = h;
                    widget.y = cy - h / 2.0;
                    widget.keep_on_screen();
                    edit.changed = true;
                }
                edit.settled |= slider.drag_stopped() || slider.lost_focus();
            }
            theme::section_label(ui, "Color");
            if let Some(color) = theme::swatches(ui, widget.color, &layout::PALETTE) {
                widget.color = color;
                edit = Edit::done();
            }
            ui.add_space(6.0);
            if theme::switch_row(ui, &mut widget.caption, "Show the name", "") {
                edit = Edit::done();
            }
            if theme::switch_row(
                ui,
                &mut widget.backdrop,
                "Dark backdrop",
                "Keeps it readable over a busy picture.",
            ) {
                edit = Edit::done();
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("Center")
                    .on_hover_text("Move it to the middle of the screen.")
                    .clicked()
                {
                    widget.x = (SCREEN - widget.w) / 2.0;
                    widget.y = (SCREEN - widget.h) / 2.0;
                    edit = Edit::done();
                }
                if ui
                    .button("Center across")
                    .on_hover_text("Keep its height, and center it left to right.")
                    .clicked()
                {
                    widget.x = (SCREEN - widget.w) / 2.0;
                    edit = Edit::done();
                }
                if ui.button("Duplicate").clicked() {
                    action = Some(Action::Duplicate);
                }
                if ui.add(theme::danger_button("Remove")).clicked() {
                    action = Some(Action::Remove);
                }
            });
        });
        match action {
            Some(Action::Duplicate) => {
                self.duplicate(layout, index);
                edit = Edit::done();
            }
            Some(Action::Remove) => {
                layout.widgets.remove(index);
                self.selected = None;
                edit = Edit::done();
            }
            None => {}
        }
        edit
    }

    /// Every reading, grouped, with its live value. Clicking one adds it.
    pub fn library(
        &mut self,
        ui: &mut Ui,
        layout: &mut Layout,
        readings: Option<&Readings>,
    ) -> Edit {
        let mut edit = Edit::default();
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Add a reading").size(16.0).color(TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(theme::muted(format!(
                        "{} of {}",
                        layout.widgets.len(),
                        layout::MAX_WIDGETS
                    )));
                });
            });
            let full = layout.widgets.len() >= layout::MAX_WIDGETS;
            let mut group = "";
            for metric in stats::ALL {
                if metric.group() != group {
                    group = metric.group();
                    theme::section_label(ui, group);
                }
                let available = layout::is_clock(metric)
                    || readings.is_none_or(|readings| readings.has(metric));
                let value = readings
                    .filter(|_| available && !layout::is_clock(metric))
                    .map(|readings| readings.text(metric));
                let placed = layout
                    .widgets
                    .iter()
                    .filter(|widget| widget.metric == metric)
                    .count();
                let response =
                    library_row(ui, metric, value.as_deref(), placed, available && !full);
                let response = if !available {
                    response.on_hover_text(match metric.source() {
                        stats::Source::Nvidia => "Needs an NVIDIA card.",
                        _ if cfg!(windows) => {
                            "Needs HWiNFO running with Shared Memory Support turned on."
                        }
                        _ => "This computer does not report it.",
                    })
                } else if full {
                    response.on_hover_text("The screen is full. Remove a widget first.")
                } else {
                    response.on_hover_text("Add it to the screen.")
                };
                if response.clicked() && available && !full {
                    let (cx, cy) = layout.next_spot();
                    let style = match (self.canvas, layout::is_clock(metric)) {
                        (_, true) => Style::Text,
                        (Canvas::Dashboard, false) => Style::Tile,
                        (Canvas::Picture, false) => Style::Text,
                    };
                    let mut widget = Widget::new(metric, style, cx, cy);
                    widget.backdrop = self.canvas == Canvas::Picture;
                    layout.widgets.push(widget);
                    self.selected = Some(layout.widgets.len() - 1);
                    edit = Edit::done();
                }
            }
        });
        edit
    }

    /// The dashboard's background style and color.
    pub fn background_card(&mut self, ui: &mut Ui, layout: &mut Layout) -> Edit {
        let mut edit = Edit::default();
        theme::card(ui, |ui| {
            ui.label(RichText::new("Background").size(16.0).color(TEXT));
            ui.add_space(4.0);
            let mut style = layout.background.style;
            let options: Vec<(Backdrop, &str)> = Backdrop::ALL
                .iter()
                .map(|style| (*style, style.name()))
                .collect();
            if theme::segmented(ui, &mut style, &options) {
                layout.background.style = style;
                edit = Edit::done();
            }
            if style != Backdrop::Plain {
                ui.add_space(4.0);
                if let Some(color) = theme::swatches(ui, layout.background.color, &layout::PALETTE)
                {
                    layout.background.color = color;
                    edit = Edit::done();
                }
            }
        });
        edit
    }
}

fn library_row(
    ui: &mut Ui,
    metric: Metric,
    value: Option<&str>,
    placed: usize,
    enabled: bool,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 28.0), Sense::click());
    let response = if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    };
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    if response.hovered() && enabled {
        painter.rect_filled(rect, CornerRadius::same(8), ROW_HOVER);
    }
    let name_color = if enabled {
        TEXT
    } else {
        MUTED.gamma_multiply(0.6)
    };
    painter.text(
        egui::pos2(rect.left() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        metric.name(),
        egui::FontId::proportional(14.0),
        name_color,
    );
    let mut right = rect.right() - 8.0;
    let plus = if enabled && response.hovered() {
        ACCENT
    } else {
        MUTED.gamma_multiply(if enabled { 0.9 } else { 0.4 })
    };
    let center = egui::pos2(right - 8.0, rect.center().y);
    painter.line_segment(
        [center - egui::vec2(5.0, 0.0), center + egui::vec2(5.0, 0.0)],
        Stroke::new(1.8_f32, plus),
    );
    painter.line_segment(
        [center - egui::vec2(0.0, 5.0), center + egui::vec2(0.0, 5.0)],
        Stroke::new(1.8_f32, plus),
    );
    right -= 26.0;
    if placed > 0 {
        let dot = egui::pos2(right - 3.0, rect.center().y);
        painter.circle_filled(dot, 3.0, ACCENT);
        right -= 14.0;
    }
    if let Some(value) = value {
        painter.text(
            egui::pos2(right, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            value,
            egui::FontId::proportional(13.0),
            MUTED,
        );
    }
    response
}

/// Lines other widgets offer to line up with, on one axis.
fn stops(others: &[Widget], vertical: bool) -> Vec<f32> {
    let mut lines = STOPS.to_vec();
    for widget in others {
        let (start, length) = if vertical {
            (widget.x, widget.w)
        } else {
            (widget.y, widget.h)
        };
        lines.extend([start, start + length / 2.0, start + length]);
    }
    lines
}

/// The shift that lines one of `edges` up with the nearest stop, and that stop.
fn snap_axis<const N: usize>(edges: [f32; N], stops: &[f32]) -> (f32, Option<f32>) {
    let mut best: Option<(f32, f32)> = None;
    for edge in edges {
        for stop in stops {
            let shift = stop - edge;
            if shift.abs() <= SNAP && best.is_none_or(|(current, _)| shift.abs() < current.abs()) {
                best = Some((shift, *stop));
            }
        }
    }
    match best {
        Some((shift, stop)) => (shift, Some(stop)),
        None => (0.0, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the editor on a 720-point screen at the window's top left, one frame per event list.
    fn drive(editor: &mut Editor, layout: &mut Layout, frames: &[Vec<egui::Event>]) {
        let ctx = egui::Context::default();
        for (index, events) in frames.iter().enumerate() {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 800.0))),
                time: Some(index as f64 * 0.05),
                events: events.clone(),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        editor.edit(ui, SCREEN, layout, None, None);
                    });
            });
        }
    }

    fn moved(x: f32, y: f32) -> Vec<egui::Event> {
        vec![egui::Event::PointerMoved(egui::pos2(x, y))]
    }

    fn button(x: f32, y: f32, pressed: bool) -> Vec<egui::Event> {
        vec![egui::Event::PointerButton {
            pos: egui::pos2(x, y),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }]
    }

    fn drag(from: (f32, f32), to: (f32, f32)) -> Vec<Vec<egui::Event>> {
        let mut frames = vec![moved(from.0, from.1), button(from.0, from.1, true)];
        for step in 1..=5 {
            let t = step as f32 / 5.0;
            frames.push(moved(
                from.0 + (to.0 - from.0) * t,
                from.1 + (to.1 - from.1) * t,
            ));
        }
        frames.push(button(to.0, to.1, false));
        frames.push(moved(to.0, to.1));
        frames
    }

    fn one_widget() -> Layout {
        let mut widget = Widget::new(Metric::CpuUsage, Style::Text, 0.0, 0.0);
        (widget.x, widget.y, widget.w, widget.h) = (100.0, 100.0, 200.0, 100.0);
        Layout {
            widgets: vec![widget],
            ..Layout::default()
        }
    }

    #[test]
    fn dragging_a_widget_moves_it_with_the_pointer() {
        let mut editor = Editor::new();
        editor.snap = false;
        let mut layout = one_widget();
        drive(
            &mut editor,
            &mut layout,
            &drag((150.0, 130.0), (263.0, 207.0)),
        );
        let widget = &layout.widgets[0];
        assert_eq!((widget.x, widget.y), (213.0, 177.0));
        assert_eq!(editor.selected, Some(0));
    }

    #[test]
    fn dragging_the_corner_resizes() {
        let mut editor = Editor::new();
        editor.snap = false;
        let mut layout = one_widget();
        // Select it first, then pull the dot at its bottom right.
        let mut frames = vec![moved(150.0, 130.0), button(150.0, 130.0, true)];
        frames.push(button(150.0, 130.0, false));
        frames.extend(drag((300.0, 200.0), (401.0, 251.0)));
        drive(&mut editor, &mut layout, &frames);
        let widget = &layout.widgets[0];
        assert_eq!((widget.x, widget.y), (100.0, 100.0));
        assert_eq!((widget.w, widget.h), (301.0, 151.0));
    }

    #[test]
    fn a_drag_snaps_to_the_middle_of_the_screen() {
        let mut editor = Editor::new();
        let mut layout = one_widget();
        // The widget's center lands 4 points left of the middle line.
        drive(
            &mut editor,
            &mut layout,
            &drag((200.0, 150.0), (356.0, 150.0)),
        );
        let widget = &layout.widgets[0];
        assert_eq!(widget.x + widget.w / 2.0, SCREEN / 2.0);
    }

    #[test]
    fn clicking_empty_space_clears_the_selection() {
        let mut editor = Editor::new();
        let mut layout = one_widget();
        editor.selected = Some(0);
        let frames = vec![
            moved(600.0, 600.0),
            button(600.0, 600.0, true),
            button(600.0, 600.0, false),
        ];
        drive(&mut editor, &mut layout, &frames);
        assert_eq!(editor.selected, None);
    }

    #[test]
    fn snapping_picks_the_closest_line() {
        let (shift, line) = snap_axis([352.0, 400.0], &[0.0, 360.0, 404.0]);
        assert_eq!((shift, line), (4.0, Some(404.0)));
        let (shift, line) = snap_axis([100.0], &[0.0, 360.0]);
        assert_eq!((shift, line), (0.0, None));
    }

    #[test]
    fn other_widgets_offer_their_edges_and_middle() {
        let widget = Widget::new(Metric::CpuUsage, Style::Text, 360.0, 360.0);
        let lines = stops(std::slice::from_ref(&widget), true);
        assert!(lines.contains(&widget.x));
        assert!(lines.contains(&(widget.x + widget.w / 2.0)));
        assert!(lines.contains(&(widget.x + widget.w)));
    }
}
