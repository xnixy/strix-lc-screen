//! The Screen page: brightness, rotation, power, and firmware.

use super::{linked, slider, Ui};
use crate::session::Setting;
use adw::prelude::*;
use gtk::gio;
use std::cell::Cell;
use std::rc::Rc;

pub fn page(ui: &Rc<Ui>) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    page.add(&display_group(ui));
    page.add(&pump_group(ui));
    page.add(&firmware_group(ui));
    page.upcast()
}

fn display_group(ui: &Rc<Ui>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Display")
        .description("The pump remembers these.")
        .build();
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.add_css_class("flat");
    refresh.set_tooltip_text(Some("Read the pump again"));
    refresh.update_property(&[gtk::accessible::Property::Label("Read the pump again")]);
    {
        let ui = Rc::clone(ui);
        refresh.connect_clicked(move |_| ui.act(|core| core.refresh()));
    }
    group.set_header_suffix(Some(&refresh));

    let brightness_row = adw::ActionRow::builder().title("Brightness").build();
    let brightness = slider(0.0, 100.0, 1.0, 0);
    brightness.set_format_value_func(|_, value| format!("{value:.0}%"));
    brightness_row.add_suffix(&brightness);
    group.add(&brightness_row);
    let settle = Rc::new(Cell::new(None));
    {
        let ui = Rc::clone(ui);
        brightness.connect_value_changed(move |scale| {
            if ui.updating() {
                return;
            }
            ui.core.borrow_mut().brightness = scale.value().round() as u8;
            ui.debounce(&settle, |ui| {
                if !ui.core.borrow().busy {
                    ui.act(|core| core.apply_brightness());
                }
            });
        });
    }

    let rotation_row = adw::ActionRow::builder().title("Rotation").build();
    let (rotation, angles) = linked(&[(0u16, "0°"), (90, "90°"), (180, "180°"), (270, "270°")]);
    rotation_row.add_suffix(&rotation);
    group.add(&rotation_row);
    for (degrees, button) in &angles {
        let ui = Rc::clone(ui);
        let degrees = *degrees;
        button.connect_toggled(move |button| {
            if button.is_active() {
                ui.act(|core| core.apply_rotation(degrees));
            }
        });
    }

    let screen_on = adw::SwitchRow::builder()
        .title("Screen on")
        .subtitle("Off keeps the panel dark until you turn it back on.")
        .build();
    {
        let ui = Rc::clone(ui);
        screen_on.connect_active_notify(move |row| {
            let on = row.is_active();
            ui.act(|core| core.apply_setting(Setting::ScreenOn(on)));
        });
    }
    group.add(&screen_on);
    let standby = adw::SwitchRow::builder()
        .title("Standby clip while the computer sleeps")
        .subtitle("Off turns the panel off while the computer sleeps.")
        .build();
    {
        let ui = Rc::clone(ui);
        standby.connect_active_notify(move |row| {
            let on = row.is_active();
            ui.act(|core| core.apply_setting(Setting::DisplayInSleep(on)));
        });
    }
    group.add(&standby);

    let power = adw::ActionRow::builder()
        .title("Wake or sleep the screen")
        .subtitle("Like the computer waking or going to sleep.")
        .build();
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_valign(gtk::Align::Center);
    let wake = gtk::Button::with_label("Wake");
    let sleep = gtk::Button::with_label("Sleep");
    buttons.append(&wake);
    buttons.append(&sleep);
    power.add_suffix(&buttons);
    {
        let ui = Rc::clone(ui);
        wake.connect_clicked(move |_| ui.act(|core| core.apply_power("resume")));
    }
    {
        let ui = Rc::clone(ui);
        sleep.connect_clicked(move |_| ui.act(|core| core.apply_power("suspend")));
    }
    group.add(&power);

    ui.on_refresh(move |ui| {
        let core = ui.core.borrow();
        let shot = core.snapshot.as_ref();
        let connected = shot.is_some();
        refresh.set_sensitive(!core.busy);
        brightness.set_value(core.brightness as f64);
        brightness_row.set_sensitive(shot.is_none_or(|shot| shot.screen_on));
        match shot {
            Some(shot) => {
                for (degrees, button) in &angles {
                    button.set_active(*degrees == shot.rotation);
                }
                screen_on.set_active(shot.screen_on);
                standby.set_active(shot.display_in_sleep);
            }
            None => {
                for (_, button) in &angles {
                    button.set_active(false);
                }
            }
        }
        rotation_row.set_sensitive(connected && !core.busy);
        screen_on.set_sensitive(connected && !core.busy);
        standby.set_sensitive(connected && !core.busy);
        power.set_sensitive(!core.busy);
    });
    group
}

fn pump_group(ui: &Rc<Ui>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("This pump")
        .description("The pump and fans run from the motherboard. This app only drives the screen.")
        .build();
    let row = |title: &str, value: &str| {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(value)
            .subtitle_selectable(true)
            .build();
        row.add_css_class("property");
        group.add(&row);
        row
    };
    row("Model", "ROG Strix LC IV");
    let serial = row("Serial", "—");
    let firmware = row("Firmware", "—");
    let free = row("Free space", "—");
    row("USB", "0B05:1DE7");
    ui.on_refresh(move |ui| {
        let core = ui.core.borrow();
        let shot = core.snapshot.as_ref();
        serial.set_subtitle(shot.map_or("—", |shot| shot.serial.as_str()));
        firmware.set_subtitle(shot.map_or("—", |shot| shot.firmware.as_str()));
        free.set_subtitle(shot.map_or("—", |shot| shot.free_space.as_str()));
    });
    group
}

fn firmware_group(ui: &Rc<Ui>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Firmware")
        .description(
            "Install a firmware package downloaded from ASUS. It is checked first, then sent \
             the same way the ASUS app sends it.",
        )
        .build();
    let row = adw::ActionRow::builder()
        .title("Update firmware")
        .subtitle("Connect the pump first.")
        .build();
    let button = gtk::Button::with_label("Choose package…");
    button.set_valign(gtk::Align::Center);
    row.add_suffix(&button);
    {
        let ui = Rc::clone(ui);
        button.connect_clicked(move |_| choose_firmware(&ui));
    }
    group.add(&row);
    ui.on_refresh(move |ui| {
        let core = ui.core.borrow();
        let ready = core.snapshot.is_some() && !core.busy && !core.asus_running;
        button.set_sensitive(ready);
        row.set_subtitle(if core.snapshot.is_some() {
            "A .zip package for the ROG Strix LC IV."
        } else {
            "Connect the pump first."
        });
    });
    group
}

fn choose_firmware(ui: &Rc<Ui>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("ASUS firmware package"));
    filter.add_suffix("zip");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title("Choose a firmware package")
        .filters(&filters)
        .default_filter(&filter)
        .modal(true)
        .build();
    let ui = Rc::clone(ui);
    dialog.open(
        Some(&ui.window.clone()),
        None::<&gio::Cancellable>,
        move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else {
                return;
            };
            ui.act(|core| core.offer_firmware(&path));
            if ui.core.borrow().firmware_offer.is_some() {
                confirm_firmware(&ui);
            }
        },
    );
}

/// Ask before sending a checked package.
pub fn confirm_firmware(ui: &Rc<Ui>) {
    let core = ui.core.borrow();
    let Some(package) = &core.firmware_offer else {
        return;
    };
    let installed = core
        .snapshot
        .as_ref()
        .map_or("unknown".to_string(), |shot| shot.firmware.clone());
    let same = crate::firmware::parse_version(&installed)
        == crate::firmware::parse_version(&package.version);
    let first = if same {
        format!("The pump already has {installed}. This installs it again.")
    } else {
        format!("The pump's firmware is {installed} now.")
    };
    let body = format!(
        "{first}\n\nThe screen goes dark while the pump installs it and restarts. That takes \
         about half a minute, and this app reconnects on its own.\n\nKeep the computer awake \
         and the cooler plugged in until the screen is back. Cooling is not affected: the \
         pump runs from the motherboard."
    );
    let dialog = adw::AlertDialog::new(
        Some(&format!("Install firmware {}?", package.version)),
        Some(&body),
    );
    drop(core);
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("install", "Install");
    dialog.set_response_appearance("install", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let ui = Rc::clone(ui);
    let parent = ui.window.clone();
    dialog.choose(Some(&parent), None::<&gio::Cancellable>, move |response| {
        if response == "install" {
            ui.act(|core| core.install_firmware());
        } else {
            ui.act(|core| core.cancel_firmware());
        }
    });
}
