//! The Show page: what the pump screen shows, and the controls for each.

use super::paint::{self, Canvas};
use super::{label, linked, slider, Ui};
use crate::settings::Source;
use adw::prelude::*;
use gtk::gio;
use std::cell::Cell;
use std::rc::Rc;

/// Longest side of the crop preview, in window pixels.
const CROP_SIDE: f64 = 300.0;

pub fn page(ui: &Rc<Ui>) -> gtk::Widget {
    let page = adw::PreferencesPage::new();

    let choose = adw::PreferencesGroup::builder()
        .title("What the pump shows")
        .description("It keeps playing while this app runs in the tray.")
        .build();
    let (tabs, buttons) = linked(&[
        (Source::Picture, "Photo or video"),
        (Source::Dashboard, "Stats dashboard"),
        (Source::Animation, "Pump animation"),
    ]);
    tabs.set_halign(gtk::Align::Start);
    tabs.set_margin_bottom(6);
    choose.add(&tabs);
    page.add(&choose);

    let picture = picture_group(ui);
    let dashboard = dashboard_group(ui);
    let animation = animation_group(ui);
    page.add(&picture);
    page.add(&dashboard);
    page.add(&animation);

    for (source, button) in &buttons {
        let ui = Rc::clone(ui);
        let source = *source;
        button.connect_toggled(move |button| {
            if button.is_active() && !ui.updating() {
                ui.picture_tab.set(source);
                ui.refresh();
            }
        });
    }
    ui.on_refresh(move |ui| {
        let tab = ui.picture_tab.get();
        let live = ui.core.borrow().settings.source;
        for (source, button) in &buttons {
            button.set_active(*source == tab);
            let text = match source {
                Source::Picture => "Photo or video",
                Source::Dashboard => "Stats dashboard",
                Source::Animation => "Pump animation",
            };
            // A check mark on the one the pump is showing.
            let text = if *source == live {
                format!("✓ {text}")
            } else {
                text.to_string()
            };
            button.set_label(&text);
        }
        picture.set_visible(tab == Source::Picture);
        dashboard.set_visible(tab == Source::Dashboard);
        animation.set_visible(tab == Source::Animation);
    });
    page.upcast()
}

/// "On the pump now" in green, or a button to put it there.
fn live_or_button(
    ui: &Rc<Ui>,
    source: Source,
    text: &str,
    action: impl Fn(&Rc<Ui>) + 'static,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let button = gtk::Button::with_label(text);
    button.add_css_class("suggested-action");
    button.add_css_class("pill");
    let live = label("On the pump now", &["success", "heading"]);
    live.set_wrap(false);
    row.append(&button);
    row.append(&live);
    {
        let ui = Rc::clone(ui);
        button.connect_clicked(move |_| action(&ui));
    }
    ui.on_refresh(move |ui| {
        let core = ui.core.borrow();
        let on = core.settings.source == source;
        button.set_visible(!on || source == Source::Picture);
        button.set_sensitive(!core.busy && (source != Source::Picture || core.chosen.is_some()));
        live.set_visible(on);
    });
    row
}

fn picture_group(ui: &Rc<Ui>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Photo or video")
        .description("Any picture, GIF, or short clip, cropped to the square screen.")
        .build();

    // The crop square over the picture. Dragging it moves the crop.
    let crop = gtk::DrawingArea::new();
    crop.set_content_width(CROP_SIDE as i32);
    crop.set_content_height((CROP_SIDE * 0.7) as i32);
    crop.set_halign(gtk::Align::Center);
    crop.set_margin_top(6);
    crop.set_margin_bottom(12);
    {
        let ui = Rc::clone(ui);
        crop.set_draw_func(move |_, cr, width, height| draw_crop(&ui, cr, width, height));
    }
    let start = Rc::new(Cell::new((0.0f32, 0.0f32)));
    let drag = gtk::GestureDrag::new();
    {
        let ui = Rc::clone(ui);
        let start = Rc::clone(&start);
        drag.connect_drag_begin(move |gesture, _, _| {
            if ui.preview.borrow().is_none() {
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            let core = ui.core.borrow();
            start.set((core.pan_x, core.pan_y));
        });
    }
    {
        let ui = Rc::clone(ui);
        let start = Rc::clone(&start);
        let area = crop.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            let Some((sx, sy)) = crop_scale(&ui, area.width(), area.height()) else {
                return;
            };
            let mut core = ui.core.borrow_mut();
            let (width, height) = (
                core.source_width.max(1) as f64,
                core.source_height.max(1) as f64,
            );
            let (side, _, _) = core.crop().square(width as u32, height as u32);
            let max_x = (width - side as f64).max(1.0);
            let max_y = (height - side as f64).max(1.0);
            let (x0, y0) = start.get();
            core.pan_x = (x0 + (dx / (max_x * sx) * 2.0) as f32).clamp(-1.0, 1.0);
            core.pan_y = (y0 + (dy / (max_y * sy) * 2.0) as f32).clamp(-1.0, 1.0);
            drop(core);
            area.queue_draw();
        });
    }
    {
        let ui = Rc::clone(ui);
        drag.connect_drag_end(move |_, _, _| ui.act(|core| core.persist()));
    }
    crop.add_controller(drag);
    let click = gtk::GestureClick::new();
    {
        let ui = Rc::clone(ui);
        click.connect_released(move |_, _, _, _| {
            if ui.preview.borrow().is_none() && !ui.core.borrow().busy {
                choose_file(&ui);
            }
        });
    }
    crop.add_controller(click);
    let crop_hint = label("Drag the square to crop.", &["dim-label", "caption"]);
    crop_hint.set_halign(gtk::Align::Center);
    crop_hint.set_xalign(0.5);
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.append(&crop);
    frame.append(&crop_hint);
    frame.set_margin_bottom(12);
    group.add(&frame);
    // Rows in their own list, so they come after the preview and not before it.
    let rows = gtk::ListBox::new();
    rows.add_css_class("boxed-list");
    rows.set_selection_mode(gtk::SelectionMode::None);
    group.add(&rows);

    let file = adw::ActionRow::builder().title("File").build();
    let choose = gtk::Button::with_label("Choose…");
    choose.set_valign(gtk::Align::Center);
    {
        let ui = Rc::clone(ui);
        choose.connect_clicked(move |_| choose_file(&ui));
    }
    file.add_suffix(&choose);
    rows.append(&file);

    let zoom_row = adw::ActionRow::builder().title("Zoom").build();
    let zoom = slider(1.0, 4.0, 0.05, 2);
    zoom_row.add_suffix(&zoom);
    rows.append(&zoom_row);
    let zoom_settle = Rc::new(Cell::new(None));
    {
        let ui = Rc::clone(ui);
        let area = crop.clone();
        zoom.connect_value_changed(move |scale| {
            if ui.updating() {
                return;
            }
            ui.core.borrow_mut().zoom = scale.value() as f32;
            area.queue_draw();
            ui.debounce(&zoom_settle, |ui| ui.act(|core| core.persist()));
        });
    }

    let speed_row = adw::ActionRow::builder()
        .title("Speed")
        .subtitle("1× matches the video. It changes while the video plays.")
        .build();
    let speed = slider(0.25, 2.0, 0.05, 2);
    speed_row.add_suffix(&speed);
    rows.append(&speed_row);
    let speed_settle = Rc::new(Cell::new(None));
    {
        let ui = Rc::clone(ui);
        speed.connect_value_changed(move |scale| {
            if ui.updating() {
                return;
            }
            ui.core.borrow_mut().set_speed(scale.value() as f32);
            ui.debounce(&speed_settle, |ui| ui.act(|core| core.persist()));
        });
    }

    let widgets = adw::SwitchRow::builder()
        .title("Show widgets on the picture")
        .subtitle("Live readings over it, placed on the Widgets page.")
        .build();
    let edit = gtk::Button::with_label("Edit widgets");
    edit.set_valign(gtk::Align::Center);
    edit.add_css_class("flat");
    widgets.add_suffix(&edit);
    {
        let ui = Rc::clone(ui);
        widgets.connect_active_notify(move |row| {
            let on = row.is_active();
            ui.act(|core| {
                core.settings.stats_on_picture = on;
                core.persist();
            });
        });
    }
    {
        let ui = Rc::clone(ui);
        edit.connect_clicked(move |_| ui.show_widgets(Canvas::Picture));
    }
    rows.append(&widgets);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    actions.set_margin_top(18);
    actions.append(&live_or_button(ui, Source::Picture, "Show on pump", |ui| {
        ui.act(|core| core.send_chosen());
    }));
    let stop = gtk::Button::with_label("Stop video");
    stop.add_css_class("pill");
    {
        let ui = Rc::clone(ui);
        stop.connect_clicked(move |_| ui.act(|core| core.stop_video()));
    }
    let reset = gtk::Button::with_label("Reset crop");
    reset.add_css_class("pill");
    {
        let ui = Rc::clone(ui);
        let area = crop.clone();
        reset.connect_clicked(move |_| {
            ui.act(|core| {
                core.reset_crop();
                core.persist();
            });
            area.queue_draw();
        });
    }
    actions.append(&stop);
    actions.append(&reset);
    group.add(&actions);
    let note = label(
        "Crop and zoom apply the next time you press Show on pump.",
        &["dim-label", "caption"],
    );
    note.set_margin_top(8);
    group.add(&note);

    ui.on_refresh(move |ui| {
        let core = ui.core.borrow();
        file.set_subtitle(
            &core
                .chosen_name()
                .unwrap_or_else(|| "No file chosen".into()),
        );
        file.set_tooltip_text(
            core.chosen
                .as_ref()
                .map(|path| path.display().to_string())
                .as_deref(),
        );
        choose.set_sensitive(!core.busy);
        zoom.set_value(core.zoom as f64);
        speed.set_value(core.speed as f64);
        let video = core.is_video();
        speed_row.set_visible(video);
        stop.set_visible(video);
        widgets.set_active(core.settings.stats_on_picture);
        edit.set_visible(core.settings.stats_on_picture);
        let has_preview = ui.preview.borrow().is_some();
        crop_hint.set_visible(has_preview);
        reset.set_sensitive(has_preview);
        zoom_row.set_sensitive(core.chosen.is_some());
        let height = match (
            ui.preview.borrow().as_ref(),
            core.source_width,
            core.source_height,
        ) {
            (Some(_), w, h) if w > 0 && h > 0 => {
                let aspect = w as f64 / h as f64;
                if aspect >= 1.0 {
                    CROP_SIDE / aspect
                } else {
                    CROP_SIDE
                }
            }
            _ => CROP_SIDE * 0.7,
        };
        crop.set_content_height(height.round() as i32);
        crop.queue_draw();
    });
    group
}

/// Window pixels per source pixel in the crop preview, when there is one.
fn crop_scale(ui: &Ui, width: i32, height: i32) -> Option<(f64, f64)> {
    let core = ui.core.borrow();
    if ui.preview.borrow().is_none() || core.source_width == 0 || core.source_height == 0 {
        return None;
    }
    let (w, h) = fitted(core.source_width, core.source_height, width, height);
    Some((w / core.source_width as f64, h / core.source_height as f64))
}

/// The picture's size in the crop preview: as large as fits, aspect kept.
fn fitted(source_width: u32, source_height: u32, width: i32, height: i32) -> (f64, f64) {
    let aspect = source_width as f64 / source_height.max(1) as f64;
    let (width, height) = (width as f64, height as f64);
    if width / height > aspect {
        (height * aspect, height)
    } else {
        (width, width / aspect)
    }
}

fn draw_crop(ui: &Ui, cr: &gtk::cairo::Context, width: i32, height: i32) {
    let preview = ui.preview.borrow();
    let core = ui.core.borrow();
    let Some(picture) = preview.as_ref().filter(|_| core.source_width > 0) else {
        paint::rounded(cr, 0.5, 0.5, width as f64 - 1.0, height as f64 - 1.0, 12.0);
        cr.set_source_rgba(0.5, 0.5, 0.5, 0.12);
        let _ = cr.fill_preserve();
        cr.set_source_rgba(0.5, 0.5, 0.5, 0.4);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        cr.set_source_rgba(0.5, 0.5, 0.5, 0.9);
        let text = if core.loading_preview() {
            "Loading…"
        } else {
            "Choose a photo or video"
        };
        paint::centered_text(cr, text, width as f64 / 2.0, height as f64 / 2.0, 15.0);
        return;
    };
    let (w, h) = fitted(core.source_width, core.source_height, width, height);
    let (left, top) = ((width as f64 - w) / 2.0, (height as f64 - h) / 2.0);
    let surface = &picture.surface;
    let _ = cr.save();
    cr.translate(left, top);
    cr.scale(w / surface.width() as f64, h / surface.height() as f64);
    let _ = cr.set_source_surface(surface, 0.0, 0.0);
    cr.source().set_filter(gtk::cairo::Filter::Good);
    let _ = cr.paint();
    let _ = cr.restore();
    let (side, x, y) = core.crop().square(core.source_width, core.source_height);
    let sx = w / core.source_width as f64;
    let sy = h / core.source_height as f64;
    let (cx, cy, cw, ch) = (
        left + x as f64 * sx,
        top + y as f64 * sy,
        side as f64 * sx,
        side as f64 * sy,
    );
    // Shade what the crop leaves out.
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
    cr.set_fill_rule(gtk::cairo::FillRule::EvenOdd);
    cr.rectangle(left, top, w, h);
    cr.rectangle(cx, cy, cw, ch);
    let _ = cr.fill();
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(2.0);
    cr.rectangle(cx + 1.0, cy + 1.0, cw - 2.0, ch - 2.0);
    let _ = cr.stroke();
}

pub fn choose_file(ui: &Rc<Ui>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Photos and video"));
    for kind in [
        "png", "jpg", "jpeg", "gif", "bmp", "mp4", "mov", "avi", "mkv", "webm",
    ] {
        filter.add_suffix(kind);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title("Choose a photo or video")
        .filters(&filters)
        .default_filter(&filter)
        .modal(true)
        .build();
    let ui = Rc::clone(ui);
    dialog.open(
        Some(&ui.window.clone()),
        None::<&gio::Cancellable>,
        move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            *ui.preview.borrow_mut() = None;
            ui.act(|core| core.choose(path));
        },
    );
}

fn dashboard_group(ui: &Rc<Ui>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Stats dashboard")
        .description(
            "Live readings on a dark background. This app redraws it once a second, so it \
             stops updating when the app quits.",
        )
        .build();
    let preview = gtk::DrawingArea::new();
    preview.set_content_width(260);
    preview.set_content_height(260);
    preview.set_halign(gtk::Align::Start);
    preview.set_margin_top(6);
    preview.set_margin_bottom(12);
    {
        let ui = Rc::clone(ui);
        preview.set_draw_func(move |_, cr, width, height| {
            let layout = ui.core.borrow_mut().settings.dashboard().clone();
            let readings = ui.core.borrow().stats.readings();
            ui.painter.borrow_mut().screen(
                cr,
                paint::square_in(width, height),
                Canvas::Dashboard,
                &layout,
                readings.as_ref(),
                None,
            );
        });
    }
    ui.live(&preview);
    group.add(&preview);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    actions.append(&live_or_button(
        ui,
        Source::Dashboard,
        "Show dashboard on pump",
        |ui| {
            ui.act(|core| core.show_dashboard());
        },
    ));
    let edit = gtk::Button::with_label("Edit widgets");
    edit.add_css_class("pill");
    {
        let ui = Rc::clone(ui);
        edit.connect_clicked(move |_| ui.show_widgets(Canvas::Dashboard));
    }
    actions.append(&edit);
    group.add(&actions);
    group
}

fn animation_group(ui: &Rc<Ui>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Pump animation")
        .description(
            "The animation stored in the pump. Nothing is sent from this computer, so it \
             keeps playing after this app quits.",
        )
        .build();
    let actions = live_or_button(ui, Source::Animation, "Show pump animation", |ui| {
        ui.act(|core| core.show_animation());
    });
    actions.set_margin_top(6);
    group.add(&actions);
    group
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_pictures_fill_the_width() {
        assert_eq!(fitted(1600, 900, 320, 320), (320.0, 180.0));
        assert_eq!(fitted(900, 1600, 320, 320), (180.0, 320.0));
    }
}
