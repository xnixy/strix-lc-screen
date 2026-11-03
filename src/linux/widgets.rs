//! The Widgets page: a live preview of the pump screen where readings are
//! dragged into place, resized from a corner, and styled from the side panel.

use super::paint::{self, Canvas};
use super::{label, linked, mark_swatch, slider, swatches, Ui};
use crate::engine::Controller;
use crate::layout::{self, snap_axis, stops, Backdrop, Layout, Style, Widget, SCREEN};
use crate::settings::Source;
use crate::stats::{self, Metric};
use adw::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

const UNDO_LIMIT: usize = 60;

/// A change to the layout for the widget at an index.
type LayoutEdit = fn(&mut Layout, &mut State, usize);
/// The handle in a selected widget's corner, in window pixels from its center.
const HANDLE: f64 = 9.0;
/// The selection color, GNOME's default accent.
const ACCENT: (f64, f64, f64) = (0.21, 0.52, 0.89);

#[derive(Clone, Copy)]
enum Drag {
    /// Moving a widget. `grab` is where the pointer holds it, from its corner.
    Move {
        grab: (f32, f32),
    },
    Resize,
}

struct State {
    selected: Option<usize>,
    drag: Option<Drag>,
    /// Where the drag started, in window pixels.
    origin: (f64, f64),
    guides: Vec<(bool, f32)>,
    hover: Option<(f64, f64)>,
    snap: bool,
    /// Widget layouts before each change, newest last.
    undo: Vec<(Canvas, Layout)>,
    /// The layout as it was when the current drag or slide began.
    gesture: Option<Layout>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            selected: None,
            drag: None,
            origin: (0.0, 0.0),
            guides: Vec::new(),
            hover: None,
            snap: true,
            undo: Vec::new(),
            gesture: None,
        }
    }
}

impl State {
    fn push_undo(&mut self, canvas: Canvas, before: Layout) {
        self.undo.push((canvas, before));
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }
}

#[derive(Default)]
pub struct Editor {
    canvas: Cell<Canvas>,
    state: RefCell<State>,
    area: OnceCell<gtk::DrawingArea>,
    settle: Rc<Cell<Option<glib::SourceId>>>,
}

fn layout_of(core: &mut Controller, canvas: Canvas) -> &mut Layout {
    match canvas {
        Canvas::Dashboard => core
            .settings
            .dashboard_layout
            .get_or_insert_with(Layout::default),
        Canvas::Picture => core
            .settings
            .picture_layout
            .get_or_insert_with(Layout::default),
    }
}

impl Editor {
    pub fn set_canvas(&self, ui: &Rc<Ui>, canvas: Canvas) {
        if self.canvas.replace(canvas) != canvas {
            self.state.borrow_mut().selected = None;
        }
        ui.refresh();
        self.redraw();
    }

    fn redraw(&self) {
        if let Some(area) = self.area.get() {
            area.queue_draw();
        }
    }

    fn selected(&self) -> Option<usize> {
        self.state.borrow().selected
    }

    /// A finished change: one step to undo, saved right away.
    fn change(&self, ui: &Rc<Ui>, edit: impl FnOnce(&mut Layout, &mut State)) {
        if ui.updating() {
            return;
        }
        let canvas = self.canvas.get();
        {
            let mut core = ui.core.borrow_mut();
            let mut state = self.state.borrow_mut();
            let layout = layout_of(&mut core, canvas);
            let before = layout.clone();
            edit(layout, &mut state);
            if state
                .selected
                .is_some_and(|index| index >= layout.widgets.len())
            {
                state.selected = None;
            }
            if *layout != before {
                if state.gesture.is_none() {
                    state.push_undo(canvas, before);
                }
                core.persist();
            }
        }
        ui.refresh();
        self.redraw();
    }

    /// A change from a slider. It becomes one undo step once the slider rests.
    fn slide(&self, ui: &Rc<Ui>, edit: impl FnOnce(&mut Widget)) {
        if ui.updating() {
            return;
        }
        let canvas = self.canvas.get();
        {
            let mut core = ui.core.borrow_mut();
            let mut state = self.state.borrow_mut();
            let Some(index) = state.selected else {
                return;
            };
            let layout = layout_of(&mut core, canvas);
            let Some(widget) = layout.widgets.get(index) else {
                return;
            };
            let before = widget.clone();
            let snapshot = layout.clone();
            edit(&mut layout.widgets[index]);
            if layout.widgets[index] == before {
                return;
            }
            if state.gesture.is_none() {
                state.gesture = Some(snapshot);
            }
        }
        self.redraw();
        let canvas_now = canvas;
        ui.debounce(&self.settle, move |ui| {
            let editor = &ui.widgets;
            let mut state = editor.state.borrow_mut();
            if let Some(before) = state.gesture.take() {
                state.push_undo(canvas_now, before);
            }
            drop(state);
            ui.act(|core| core.persist());
        });
        ui.refresh();
    }

    fn undo(&self, ui: &Rc<Ui>) {
        let canvas = self.canvas.get();
        {
            let mut state = self.state.borrow_mut();
            if state.gesture.is_some() {
                return;
            }
            let Some(index) = state.undo.iter().rposition(|(on, _)| *on == canvas) else {
                return;
            };
            let (_, previous) = state.undo.remove(index);
            state.selected = None;
            let mut core = ui.core.borrow_mut();
            *layout_of(&mut core, canvas) = previous;
            core.persist();
        }
        ui.refresh();
        self.redraw();
    }
}

fn duplicate(layout: &mut Layout, state: &mut State, index: usize) {
    if layout.widgets.len() >= layout::MAX_WIDGETS {
        return;
    }
    let mut copy = layout.widgets[index].clone();
    copy.x += 24.0;
    copy.y += 24.0;
    copy.keep_on_screen();
    layout.widgets.push(copy);
    state.selected = Some(layout.widgets.len() - 1);
}

/// Readings in the order they first appear, for laying a screen out again.
fn metrics_of(widgets: &[Widget]) -> Vec<Metric> {
    let mut metrics = Vec::new();
    for widget in widgets {
        if !metrics.contains(&widget.metric) {
            metrics.push(widget.metric);
        }
    }
    metrics
}

/// Converts between window pixels and pump screen pixels for one frame.
#[derive(Clone, Copy)]
struct Geometry {
    x: f64,
    y: f64,
    scale: f64,
}

impl Geometry {
    fn of(area: &gtk::DrawingArea) -> Self {
        let (x, y, side) = paint::square_in(area.width(), area.height());
        Self {
            x,
            y,
            scale: paint::scale_of(side),
        }
    }

    fn to_screen(self, (x, y): (f64, f64)) -> (f32, f32) {
        (
            ((x - self.x) / self.scale) as f32,
            ((y - self.y) / self.scale) as f32,
        )
    }

    fn to_window(self, x: f32, y: f32) -> (f64, f64) {
        (
            self.x + x as f64 * self.scale,
            self.y + y as f64 * self.scale,
        )
    }

    fn box_of(self, widget: &Widget) -> (f64, f64, f64, f64) {
        let (x, y) = self.to_window(widget.x, widget.y);
        (
            x,
            y,
            widget.w as f64 * self.scale,
            widget.h as f64 * self.scale,
        )
    }

    fn on_handle(self, widget: &Widget, (px, py): (f64, f64)) -> bool {
        let (x, y, w, h) = self.box_of(widget);
        (px - (x + w)).abs() <= HANDLE && (py - (y + h)).abs() <= HANDLE
    }
}

fn hit(layout: &Layout, (x, y): (f32, f32)) -> Option<usize> {
    layout
        .widgets
        .iter()
        .rposition(|widget| widget.contains(x, y))
}

pub fn page(ui: &Rc<Ui>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);

    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let intro = label(
        "Drag live readings anywhere on the screen. The pump follows along.",
        &["dim-label"],
    );
    intro.set_hexpand(true);
    let (canvases, canvas_buttons) = linked(&[
        (Canvas::Picture, "On your picture"),
        (Canvas::Dashboard, "Dashboard"),
    ]);
    top.append(&intro);
    top.append(&canvases);
    root.append(&top);
    for (canvas, button) in &canvas_buttons {
        let ui = Rc::clone(ui);
        let canvas = *canvas;
        button.connect_toggled(move |button| {
            if button.is_active() && !ui.updating() {
                ui.widgets.set_canvas(&ui, canvas);
            }
        });
    }

    let banner = adw::Banner::new("");
    let banner_action = Rc::new(Cell::new(0u8));
    {
        let ui = Rc::clone(ui);
        let action = Rc::clone(&banner_action);
        banner.connect_button_clicked(move |_| match action.get() {
            1 => ui.act(|core| {
                core.settings.stats_on_picture = true;
                core.persist();
            }),
            2 => ui.act(|core| core.show_dashboard()),
            _ => {}
        });
    }
    root.append(&banner);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    body.set_vexpand(true);
    body.append(&editor_column(ui));
    let side = gtk::Box::new(gtk::Orientation::Vertical, 18);
    side.append(&inspector(ui));
    side.append(&background_group(ui));
    side.append(&library(ui));
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&side)
        .width_request(360)
        .build();
    scroll.set_vexpand(true);
    body.append(&scroll);
    root.append(&body);

    ui.on_refresh(move |ui| {
        let canvas = ui.widgets.canvas.get();
        for (value, button) in &canvas_buttons {
            button.set_active(*value == canvas);
        }
        let core = ui.core.borrow();
        let (text, button, action) = match canvas {
            Canvas::Picture if !core.settings.stats_on_picture => {
                ("Widgets on your picture are off.", Some("Turn them on"), 1)
            }
            Canvas::Picture if core.settings.source != Source::Picture => {
                ("The pump is not showing your picture right now.", None, 0)
            }
            Canvas::Dashboard if core.settings.source != Source::Dashboard => (
                "The pump is not showing the dashboard right now.",
                Some("Show it"),
                2,
            ),
            _ => ("", None, 0),
        };
        banner.set_title(text);
        banner.set_button_label(button);
        banner.set_revealed(!text.is_empty());
        banner_action.set(action);
    });
    root.upcast()
}

fn editor_column(ui: &Rc<Ui>) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_hexpand(true);
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_size_request(340, 340);
    area.set_focusable(true);
    area.set_can_focus(true);
    let _ = ui.widgets.area.set(area.clone());
    ui.live(&area);
    {
        let ui = Rc::clone(ui);
        area.set_draw_func(move |area, cr, _, _| draw(&ui, area, cr));
    }
    add_gestures(ui, &area);
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("card");
    frame.add_css_class("screen-frame");
    frame.append(&area);
    // Keep the card close to square, the shape of the pump screen, with the
    // tools along its bottom.
    let square = gtk::AspectFrame::new(0.5, 0.0, 0.92, false);
    square.set_child(Some(&frame));
    square.set_vexpand(true);
    column.append(&square);

    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    tools.set_margin_top(12);
    let snap = gtk::Switch::new();
    snap.set_valign(gtk::Align::Center);
    snap.set_tooltip_text(Some(
        "Snap to the middle, the edges, and other widgets. Hold Alt to place freely.",
    ));
    let snap_label = gtk::Label::new(Some("Snap"));
    snap_label.set_margin_end(8);
    let undo = gtk::Button::from_icon_name("edit-undo-symbolic");
    undo.set_tooltip_text(Some("Undo (Ctrl+Z)"));
    undo.update_property(&[gtk::accessible::Property::Label("Undo")]);
    let arrange = gtk::Button::with_label("Arrange for me");
    arrange.set_tooltip_text(Some("Lay the same readings out in tidy rows."));
    let clear = gtk::Button::with_label("Remove all");
    clear.add_css_class("destructive-action");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    tools.append(&snap);
    tools.append(&snap_label);
    tools.append(&undo);
    tools.append(&spacer);
    tools.append(&arrange);
    tools.append(&clear);
    frame.append(&tools);
    {
        let ui = Rc::clone(ui);
        snap.connect_active_notify(move |switch| {
            if !ui.updating() {
                ui.widgets.state.borrow_mut().snap = switch.is_active();
            }
        });
    }
    {
        let ui = Rc::clone(ui);
        undo.connect_clicked(move |_| ui.widgets.undo(&ui));
    }
    {
        let ui = Rc::clone(ui);
        arrange.connect_clicked(move |_| {
            let canvas = ui.widgets.canvas.get();
            ui.widgets.change(&ui, |layout, state| {
                let metrics = metrics_of(&layout.widgets);
                *layout = match canvas {
                    Canvas::Dashboard => layout::arrange_dashboard(&metrics, layout.background),
                    Canvas::Picture => layout::arrange_band(&metrics, false),
                };
                state.selected = None;
            });
        });
    }
    {
        let ui = Rc::clone(ui);
        clear.connect_clicked(move |_| {
            ui.widgets.change(&ui, |layout, state| {
                layout.widgets.clear();
                state.selected = None;
            });
        });
    }
    ui.on_refresh(move |ui| {
        let canvas = ui.widgets.canvas.get();
        let state = ui.widgets.state.borrow();
        snap.set_active(state.snap);
        undo.set_sensitive(state.undo.iter().any(|(on, _)| *on == canvas));
        drop(state);
        let mut core = ui.core.borrow_mut();
        clear.set_sensitive(!layout_of(&mut core, canvas).widgets.is_empty());
    });
    column
}

fn draw(ui: &Ui, area: &gtk::DrawingArea, cr: &gtk::cairo::Context) {
    let canvas = ui.widgets.canvas.get();
    let geometry = Geometry::of(area);
    let side = SCREEN as f64 * geometry.scale;
    let (layout, readings, crop, source) = {
        let mut core = ui.core.borrow_mut();
        let layout = layout_of(&mut core, canvas).clone();
        (
            layout,
            core.stats.readings(),
            core.crop(),
            (core.source_width, core.source_height),
        )
    };
    let preview = ui.preview.borrow();
    let picture = preview
        .as_ref()
        .filter(|_| source.0 > 0)
        .map(|picture| (picture, picture.crop_rect(source, &crop)));
    ui.painter.borrow_mut().screen(
        cr,
        (geometry.x, geometry.y, side),
        canvas,
        &layout,
        readings.as_ref(),
        picture,
    );

    let state = ui.widgets.state.borrow();
    cr.set_line_width(1.0);
    cr.set_dash(&[5.0, 4.0], 0.0);
    cr.set_source_rgb(1.0, 0.59, 0.67);
    for (vertical, at) in &state.guides {
        let (a, b) = if *vertical {
            (
                geometry.to_window(*at, 0.0),
                geometry.to_window(*at, SCREEN),
            )
        } else {
            (
                geometry.to_window(0.0, *at),
                geometry.to_window(SCREEN, *at),
            )
        };
        cr.move_to(a.0, a.1);
        cr.line_to(b.0, b.1);
        let _ = cr.stroke();
    }
    cr.set_dash(&[], 0.0);
    let hovered = state
        .hover
        .and_then(|pos| hit(&layout, geometry.to_screen(pos)))
        .filter(|index| Some(*index) != state.selected);
    if let Some(index) = hovered {
        let (x, y, w, h) = geometry.box_of(&layout.widgets[index]);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
        paint::rounded(cr, x - 1.0, y - 1.0, w + 2.0, h + 2.0, 6.0);
        let _ = cr.stroke();
    }
    let Some(widget) = state.selected.and_then(|index| layout.widgets.get(index)) else {
        return;
    };
    let (x, y, w, h) = geometry.box_of(widget);
    let (r, g, b) = ACCENT;
    cr.set_source_rgb(r, g, b);
    cr.set_line_width(2.0);
    paint::rounded(cr, x - 1.0, y - 1.0, w + 2.0, h + 2.0, 6.0);
    let _ = cr.stroke();
    cr.arc(x + w, y + h, 6.5, 0.0, std::f64::consts::TAU);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    let _ = cr.fill_preserve();
    cr.set_source_rgb(r, g, b);
    let _ = cr.stroke();
    if state.drag.is_some() {
        let tag = format!(
            "{:.0}, {:.0}   {:.0} × {:.0}",
            widget.x, widget.y, widget.w, widget.h
        );
        cr.select_font_face(
            "Sans",
            gtk::cairo::FontSlant::Normal,
            gtk::cairo::FontWeight::Normal,
        );
        cr.set_font_size(11.5);
        if let Ok(extents) = cr.text_extents(&tag) {
            let ty = (y - 8.0).max(geometry.y + 18.0);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.78);
            paint::rounded(
                cr,
                x,
                ty - extents.height() - 6.0,
                extents.width() + 12.0,
                extents.height() + 8.0,
                5.0,
            );
            let _ = cr.fill();
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.move_to(x + 6.0, ty - 2.0);
            let _ = cr.show_text(&tag);
        }
    }
}

fn add_gestures(ui: &Rc<Ui>, area: &gtk::DrawingArea) {
    let drag = gtk::GestureDrag::new();
    drag.set_button(gdk::BUTTON_PRIMARY);
    {
        let ui = Rc::clone(ui);
        let area = area.clone();
        drag.connect_drag_begin(move |_, x, y| {
            area.grab_focus();
            let canvas = ui.widgets.canvas.get();
            let geometry = Geometry::of(&area);
            let mut core = ui.core.borrow_mut();
            let layout = layout_of(&mut core, canvas);
            let mut state = ui.widgets.state.borrow_mut();
            state.origin = (x, y);
            let selected = state.selected.and_then(|index| layout.widgets.get(index));
            state.drag = if selected.is_some_and(|widget| geometry.on_handle(widget, (x, y))) {
                Some(Drag::Resize)
            } else if let Some(index) = hit(layout, geometry.to_screen((x, y))) {
                state.selected = Some(index);
                let widget = &layout.widgets[index];
                let (px, py) = geometry.to_screen((x, y));
                Some(Drag::Move {
                    grab: (px - widget.x, py - widget.y),
                })
            } else {
                state.selected = None;
                None
            };
            if state.drag.is_some() {
                state.gesture = Some(layout.clone());
            }
            drop(state);
            drop(core);
            ui.refresh();
            area.queue_draw();
        });
    }
    {
        let ui = Rc::clone(ui);
        let area = area.clone();
        drag.connect_drag_update(move |gesture, dx, dy| {
            let alt = gesture
                .current_event_state()
                .contains(gdk::ModifierType::ALT_MASK);
            let canvas = ui.widgets.canvas.get();
            let geometry = Geometry::of(&area);
            let mut core = ui.core.borrow_mut();
            let layout = layout_of(&mut core, canvas);
            let mut state = ui.widgets.state.borrow_mut();
            let (Some(drag), Some(index)) = (state.drag, state.selected) else {
                return;
            };
            if index >= layout.widgets.len() {
                return;
            }
            let snap = state.snap && !alt;
            let (px, py) = geometry.to_screen((state.origin.0 + dx, state.origin.1 + dy));
            let others: Vec<Widget> = layout
                .widgets
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, widget)| widget.clone())
                .collect();
            let widget = &mut layout.widgets[index];
            state.guides.clear();
            match drag {
                Drag::Move { grab } => {
                    widget.x = px - grab.0;
                    widget.y = py - grab.1;
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
                        state.guides.extend(gx.map(|x| (true, x)));
                        state.guides.extend(gy.map(|y| (false, y)));
                    }
                }
                Drag::Resize => {
                    let (mut right, mut bottom) = (px, py);
                    if snap {
                        let (dx, gx) = snap_axis([right], &stops(&others, true));
                        let (dy, gy) = snap_axis([bottom], &stops(&others, false));
                        right += dx;
                        bottom += dy;
                        state.guides.extend(gx.map(|x| (true, x)));
                        state.guides.extend(gy.map(|y| (false, y)));
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
            drop(state);
            drop(core);
            ui.refresh();
            area.queue_draw();
        });
    }
    {
        let ui = Rc::clone(ui);
        let area = area.clone();
        drag.connect_drag_end(move |_, _, _| {
            let canvas = ui.widgets.canvas.get();
            {
                let mut state = ui.widgets.state.borrow_mut();
                state.guides.clear();
                if state.drag.take().is_some() {
                    let mut core = ui.core.borrow_mut();
                    let now = layout_of(&mut core, canvas).clone();
                    if let Some(before) = state.gesture.take() {
                        if before != now {
                            state.push_undo(canvas, before);
                            core.persist();
                        }
                    }
                }
            }
            ui.refresh();
            area.queue_draw();
        });
    }
    area.add_controller(drag);

    let menu = gtk::GestureClick::new();
    menu.set_button(gdk::BUTTON_SECONDARY);
    {
        let ui = Rc::clone(ui);
        let area = area.clone();
        menu.connect_pressed(move |_, _, x, y| {
            let canvas = ui.widgets.canvas.get();
            let geometry = Geometry::of(&area);
            let index = {
                let mut core = ui.core.borrow_mut();
                hit(layout_of(&mut core, canvas), geometry.to_screen((x, y)))
            };
            ui.widgets.state.borrow_mut().selected = index;
            ui.refresh();
            area.queue_draw();
            if let Some(index) = index {
                context_menu(&ui, &area, index, x, y);
            }
        });
    }
    area.add_controller(menu);

    let motion = gtk::EventControllerMotion::new();
    {
        let ui = Rc::clone(ui);
        let area = area.clone();
        motion.connect_motion(move |_, x, y| {
            let canvas = ui.widgets.canvas.get();
            let geometry = Geometry::of(&area);
            let cursor = {
                let mut core = ui.core.borrow_mut();
                let layout = layout_of(&mut core, canvas);
                let mut state = ui.widgets.state.borrow_mut();
                state.hover = Some((x, y));
                let on_handle = state
                    .selected
                    .and_then(|index| layout.widgets.get(index))
                    .is_some_and(|widget| geometry.on_handle(widget, (x, y)));
                match state.drag {
                    Some(Drag::Resize) => Some("nwse-resize"),
                    Some(Drag::Move { .. }) => Some("grabbing"),
                    None if on_handle => Some("nwse-resize"),
                    None if hit(layout, geometry.to_screen((x, y))).is_some() => Some("grab"),
                    None => None,
                }
            };
            area.set_cursor_from_name(cursor);
            area.queue_draw();
        });
    }
    {
        let ui = Rc::clone(ui);
        let area = area.clone();
        motion.connect_leave(move |_| {
            ui.widgets.state.borrow_mut().hover = None;
            area.queue_draw();
        });
    }
    area.add_controller(motion);

    let keys = gtk::EventControllerKey::new();
    {
        let ui = Rc::clone(ui);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let control = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
            if control && matches!(key, gdk::Key::z | gdk::Key::Z) {
                ui.widgets.undo(&ui);
                return glib::Propagation::Stop;
            }
            let Some(index) = ui.widgets.selected() else {
                return glib::Propagation::Proceed;
            };
            let step = if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                10.0
            } else {
                1.0
            };
            let nudge = match key {
                gdk::Key::Left => Some((-step, 0.0)),
                gdk::Key::Right => Some((step, 0.0)),
                gdk::Key::Up => Some((0.0, -step)),
                gdk::Key::Down => Some((0.0, step)),
                _ => None,
            };
            if let Some((dx, dy)) = nudge {
                ui.widgets.change(&ui, |layout, _| {
                    let widget = &mut layout.widgets[index];
                    widget.x += dx;
                    widget.y += dy;
                    widget.keep_on_screen();
                });
                return glib::Propagation::Stop;
            }
            if matches!(key, gdk::Key::Delete | gdk::Key::BackSpace) {
                ui.widgets.change(&ui, |layout, state| {
                    layout.widgets.remove(index);
                    state.selected = None;
                });
                return glib::Propagation::Stop;
            }
            if control && matches!(key, gdk::Key::d | gdk::Key::D) {
                ui.widgets
                    .change(&ui, |layout, state| duplicate(layout, state, index));
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    area.add_controller(keys);
}

fn context_menu(ui: &Rc<Ui>, area: &gtk::DrawingArea, index: usize, x: f64, y: f64) {
    let popover = gtk::Popover::new();
    popover.set_parent(area);
    popover.set_has_arrow(false);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    let items = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let entries: [(&str, LayoutEdit); 4] = [
        ("Duplicate", |layout, state, index| {
            duplicate(layout, state, index)
        }),
        ("Bring to front", |layout, state, index| {
            let widget = layout.widgets.remove(index);
            layout.widgets.push(widget);
            state.selected = Some(layout.widgets.len() - 1);
        }),
        ("Send to back", |layout, state, index| {
            let widget = layout.widgets.remove(index);
            layout.widgets.insert(0, widget);
            state.selected = Some(0);
        }),
        ("Remove", |layout, state, index| {
            layout.widgets.remove(index);
            state.selected = None;
        }),
    ];
    for (text, edit) in entries {
        let button = gtk::Button::with_label(text);
        button.add_css_class("flat");
        if let Some(child) = button.child().and_downcast::<gtk::Label>() {
            child.set_xalign(0.0);
        }
        let ui = Rc::clone(ui);
        let popover_ref = popover.clone();
        button.connect_clicked(move |_| {
            popover_ref.popdown();
            ui.widgets
                .change(&ui, |layout, state| edit(layout, state, index));
        });
        items.append(&button);
    }
    popover.set_child(Some(&items));
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });
    popover.popup();
}

/// The selected widget's style, size, color, and order.
fn inspector(ui: &Rc<Ui>) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let help = adw::PreferencesGroup::builder()
        .title("Arrange the screen")
        .build();
    for line in [
        "Click a widget to select it, then drag it anywhere.",
        "Drag the white dot in its corner to resize it.",
        "Arrow keys nudge it, Shift moves it further, Delete removes it.",
        "It snaps to the middle, the edges, and other widgets. Hold Alt to place it freely.",
    ] {
        let text = label(line, &["dim-label"]);
        text.set_margin_bottom(6);
        help.add(&text);
    }
    column.append(&help);

    let group = adw::PreferencesGroup::new();
    let value = label("", &["title-3"]);
    group.set_header_suffix(Some(&value));
    column.append(&group);

    let style_row = adw::ActionRow::builder().title("Style").build();
    let options: Vec<(Style, &str)> = Style::ALL
        .iter()
        .map(|style| (*style, style.name()))
        .collect();
    let (styles, style_buttons) = linked(&options);
    style_row.add_suffix(&styles);
    group.add(&style_row);
    for (style, button) in &style_buttons {
        let ui = Rc::clone(ui);
        let style = *style;
        button.connect_toggled(move |button| {
            if !button.is_active() {
                return;
            }
            let Some(index) = ui.widgets.selected() else {
                return;
            };
            ui.widgets
                .change(&ui, |layout, _| layout.widgets[index].set_style(style));
        });
    }

    let names: Vec<String> = stats::ALL
        .iter()
        .map(|metric| format!("{} · {}", metric.group(), metric.name()))
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let metric_row = adw::ComboRow::builder()
        .title("Reading")
        .model(&gtk::StringList::new(&names))
        .build();
    group.add(&metric_row);
    {
        let ui = Rc::clone(ui);
        metric_row.connect_selected_notify(move |row| {
            let Some(metric) = stats::ALL.get(row.selected() as usize).copied() else {
                return;
            };
            let Some(index) = ui.widgets.selected() else {
                return;
            };
            ui.widgets
                .change(&ui, |layout, _| layout.widgets[index].metric = metric);
        });
    }

    let width_row = adw::ActionRow::builder().title("Width").build();
    let width = slider(layout::MIN_SIZE as f64, SCREEN as f64, 1.0, 0);
    width_row.add_suffix(&width);
    group.add(&width_row);
    let height_row = adw::ActionRow::builder().title("Height").build();
    let height = slider(layout::MIN_SIZE as f64, SCREEN as f64, 1.0, 0);
    height_row.add_suffix(&height);
    group.add(&height_row);
    {
        let ui = Rc::clone(ui);
        width.connect_value_changed(move |scale| {
            let w = scale.value() as f32;
            ui.widgets.slide(&ui, |widget| {
                let ring = widget.style == Style::Ring;
                let (cx, cy) = widget.center();
                widget.w = w;
                if ring {
                    widget.h = w;
                    widget.y = cy - widget.h / 2.0;
                }
                widget.x = cx - widget.w / 2.0;
                widget.keep_on_screen();
            });
        });
    }
    {
        let ui = Rc::clone(ui);
        height.connect_value_changed(move |scale| {
            let h = scale.value() as f32;
            ui.widgets.slide(&ui, |widget| {
                let (_, cy) = widget.center();
                widget.h = h;
                widget.y = cy - h / 2.0;
                widget.keep_on_screen();
            });
        });
    }

    let color_row = adw::ActionRow::builder().title("Color").build();
    let (colors, color_buttons) = swatches({
        let ui = Rc::clone(ui);
        move |color| {
            if let Some(index) = ui.widgets.selected() {
                ui.widgets
                    .change(&ui, |layout, _| layout.widgets[index].color = color);
            }
        }
    });
    color_row.add_suffix(&colors);
    group.add(&color_row);

    let caption = adw::SwitchRow::builder().title("Show the name").build();
    let backdrop = adw::SwitchRow::builder()
        .title("Dark backdrop")
        .subtitle("Keeps it readable over a busy picture.")
        .build();
    group.add(&caption);
    group.add(&backdrop);
    {
        let ui = Rc::clone(ui);
        caption.connect_active_notify(move |row| {
            let on = row.is_active();
            if let Some(index) = ui.widgets.selected() {
                ui.widgets
                    .change(&ui, |layout, _| layout.widgets[index].caption = on);
            }
        });
    }
    {
        let ui = Rc::clone(ui);
        backdrop.connect_active_notify(move |row| {
            let on = row.is_active();
            if let Some(index) = ui.widgets.selected() {
                ui.widgets
                    .change(&ui, |layout, _| layout.widgets[index].backdrop = on);
            }
        });
    }

    let actions = gtk::FlowBox::new();
    actions.set_selection_mode(gtk::SelectionMode::None);
    actions.set_column_spacing(6);
    actions.set_row_spacing(6);
    actions.set_margin_top(12);
    let center = gtk::Button::with_label("Center");
    center.set_tooltip_text(Some("Move it to the middle of the screen."));
    let across = gtk::Button::with_label("Center across");
    across.set_tooltip_text(Some("Keep its height, and center it left to right."));
    let copy = gtk::Button::with_label("Duplicate");
    let remove = gtk::Button::with_label("Remove");
    remove.add_css_class("destructive-action");
    for button in [&center, &across, &copy, &remove] {
        actions.append(button);
    }
    group.add(&actions);
    let button_edit = |button: &gtk::Button, edit: LayoutEdit| {
        let ui = Rc::clone(ui);
        button.connect_clicked(move |_| {
            if let Some(index) = ui.widgets.selected() {
                ui.widgets
                    .change(&ui, |layout, state| edit(layout, state, index));
            }
        });
    };
    button_edit(&center, |layout, _, index| {
        let widget = &mut layout.widgets[index];
        widget.x = (SCREEN - widget.w) / 2.0;
        widget.y = (SCREEN - widget.h) / 2.0;
    });
    button_edit(&across, |layout, _, index| {
        let widget = &mut layout.widgets[index];
        widget.x = (SCREEN - widget.w) / 2.0;
    });
    button_edit(&copy, duplicate);
    button_edit(&remove, |layout, state, index| {
        layout.widgets.remove(index);
        state.selected = None;
    });

    ui.on_refresh(move |ui| {
        let canvas = ui.widgets.canvas.get();
        let selected = ui.widgets.selected();
        let widget = {
            let mut core = ui.core.borrow_mut();
            selected.and_then(|index| layout_of(&mut core, canvas).widgets.get(index).cloned())
        };
        help.set_visible(widget.is_none());
        group.set_visible(widget.is_some());
        let Some(widget) = widget else {
            return;
        };
        group.set_title(widget.metric.name());
        group.set_description(Some(widget.metric.group()));
        let readings = ui.core.borrow().stats.readings();
        value.set_label(
            &readings
                .filter(|_| !layout::is_clock(widget.metric))
                .map(|readings| readings.text(widget.metric))
                .unwrap_or_default(),
        );
        for (style, button) in &style_buttons {
            button.set_active(*style == widget.style);
        }
        if let Some(position) = stats::ALL
            .iter()
            .position(|metric| *metric == widget.metric)
        {
            metric_row.set_selected(position as u32);
        }
        let ring = widget.style == Style::Ring;
        width_row.set_title(if ring { "Size" } else { "Width" });
        width.set_value(widget.w as f64);
        height.set_value(widget.h as f64);
        height_row.set_visible(!ring);
        mark_swatch(&color_buttons, widget.color);
        caption.set_active(widget.caption);
        backdrop.set_active(widget.backdrop);
    });
    column
}

/// The dashboard's background style and color.
fn background_group(ui: &Rc<Ui>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Background").build();
    let style_row = adw::ActionRow::builder().title("Style").build();
    let options: Vec<(Backdrop, &str)> = Backdrop::ALL
        .iter()
        .map(|style| (*style, style.name()))
        .collect();
    let (styles, style_buttons) = linked(&options);
    style_row.add_suffix(&styles);
    group.add(&style_row);
    for (style, button) in &style_buttons {
        let ui = Rc::clone(ui);
        let style = *style;
        button.connect_toggled(move |button| {
            if button.is_active() {
                ui.widgets
                    .change(&ui, |layout, _| layout.background.style = style);
            }
        });
    }
    let color_row = adw::ActionRow::builder().title("Color").build();
    let (colors, color_buttons) = swatches({
        let ui = Rc::clone(ui);
        move |color| {
            ui.widgets
                .change(&ui, |layout, _| layout.background.color = color);
        }
    });
    color_row.add_suffix(&colors);
    group.add(&color_row);
    let shown = group.clone();
    ui.on_refresh(move |ui| {
        let canvas = ui.widgets.canvas.get();
        shown.set_visible(canvas == Canvas::Dashboard);
        if canvas != Canvas::Dashboard {
            return;
        }
        let background = ui.core.borrow_mut().settings.dashboard().background;
        for (style, button) in &style_buttons {
            button.set_active(*style == background.style);
        }
        color_row.set_visible(background.style != Backdrop::Plain);
        mark_swatch(&color_buttons, background.color);
    });
    group
}

/// Every reading, grouped, with its live value. Activating one adds it.
fn library(ui: &Rc<Ui>) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let title = label("Add a reading", &["heading"]);
    title.set_hexpand(true);
    let count = label("", &["dim-label"]);
    header.append(&title);
    header.append(&count);
    column.append(&header);
    let mut rows: Vec<(Metric, adw::ActionRow, gtk::Label, gtk::Label)> = Vec::new();
    let mut group: Option<(String, adw::PreferencesGroup)> = None;
    for metric in stats::ALL {
        if group
            .as_ref()
            .is_none_or(|(name, _)| name != metric.group())
        {
            let next = adw::PreferencesGroup::builder()
                .title(metric.group())
                .build();
            column.append(&next);
            group = Some((metric.group().to_string(), next));
        }
        let row = adw::ActionRow::builder()
            .title(metric.name())
            .activatable(true)
            .build();
        let value = label("", &["dim-label", "numeric"]);
        value.set_wrap(false);
        let placed = gtk::Label::new(Some("●"));
        placed.add_css_class("accent");
        placed.add_css_class("status-dot");
        placed.set_tooltip_text(Some("On the screen"));
        let add = gtk::Button::from_icon_name("list-add-symbolic");
        add.add_css_class("flat");
        add.set_valign(gtk::Align::Center);
        add.update_property(&[gtk::accessible::Property::Label(&format!(
            "Add {}",
            metric.name()
        ))]);
        row.add_suffix(&value);
        row.add_suffix(&placed);
        row.add_suffix(&add);
        row.set_activatable_widget(Some(&add));
        {
            let ui = Rc::clone(ui);
            add.connect_clicked(move |_| {
                let canvas = ui.widgets.canvas.get();
                ui.widgets.change(&ui, |layout, state| {
                    if layout.widgets.len() >= layout::MAX_WIDGETS {
                        return;
                    }
                    let (cx, cy) = layout.next_spot();
                    let style = match (canvas, layout::is_clock(metric)) {
                        (_, true) => Style::Text,
                        (Canvas::Dashboard, false) => Style::Tile,
                        (Canvas::Picture, false) => Style::Text,
                    };
                    let mut widget = Widget::new(metric, style, cx, cy);
                    widget.backdrop = canvas == Canvas::Picture;
                    layout.widgets.push(widget);
                    state.selected = Some(layout.widgets.len() - 1);
                });
            });
        }
        if let Some((_, group)) = &group {
            group.add(&row);
        }
        rows.push((metric, row, value, placed));
    }
    ui.on_refresh(move |ui| {
        let canvas = ui.widgets.canvas.get();
        let readings = ui.core.borrow().stats.readings();
        let widgets = {
            let mut core = ui.core.borrow_mut();
            layout_of(&mut core, canvas).widgets.clone()
        };
        let full = widgets.len() >= layout::MAX_WIDGETS;
        count.set_label(&format!("{} of {}", widgets.len(), layout::MAX_WIDGETS));
        for (metric, row, value, placed) in &rows {
            let clock = layout::is_clock(*metric);
            let available = clock
                || readings
                    .as_ref()
                    .is_none_or(|readings| readings.has(*metric));
            value.set_label(
                &readings
                    .as_ref()
                    .filter(|_| available && !clock)
                    .map(|readings| readings.text(*metric))
                    .unwrap_or_default(),
            );
            placed.set_visible(widgets.iter().any(|widget| widget.metric == *metric));
            row.set_sensitive(available && !full);
            row.set_tooltip_text(Some(if !available {
                match metric.source() {
                    stats::Source::Nvidia => "Needs an NVIDIA card.",
                    _ => "This computer does not report it.",
                }
            } else if full {
                "The screen is full. Remove a widget first."
            } else {
                "Add it to the screen."
            }));
        }
    });
    column
}
