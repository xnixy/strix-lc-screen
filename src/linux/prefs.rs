//! The Settings page: how the app starts and runs.

use super::Ui;
use crate::settings;
use adw::prelude::*;
use std::rc::Rc;

pub fn page(ui: &Rc<Ui>) -> gtk::Widget {
    let page = adw::PreferencesPage::new();

    let startup = adw::PreferencesGroup::builder()
        .title("Startup and tray")
        .description("Click the tray icon to show this window. Right-click it to stop or quit.")
        .build();
    let tray = adw::SwitchRow::builder()
        .title("Keep running in the tray")
        .subtitle("Closing the window leaves the picture playing.")
        .build();
    let sign_in = adw::SwitchRow::builder()
        .title(crate::engine::sign_in_startup_label())
        .build();
    let hidden = adw::SwitchRow::builder()
        .title("Start in the tray")
        .subtitle("At sign-in, stay in the tray instead of opening this window.")
        .build();
    let resume = adw::SwitchRow::builder()
        .title("Play the last picture when the app opens")
        .subtitle("Sends the last photo or video again on its own.")
        .build();
    for row in [&tray, &sign_in, &hidden, &resume] {
        startup.add(row);
    }
    {
        let ui = Rc::clone(ui);
        tray.connect_active_notify(move |row| {
            let on = row.is_active();
            ui.act(|core| {
                core.settings.close_to_tray = on;
                core.persist();
            });
        });
    }
    {
        let ui = Rc::clone(ui);
        sign_in.connect_active_notify(move |row| {
            let on = row.is_active();
            ui.act(|core| core.set_start_at_sign_in(on));
        });
    }
    {
        let ui = Rc::clone(ui);
        hidden.connect_active_notify(move |row| {
            let on = row.is_active();
            ui.act(|core| core.set_start_in_tray(on));
        });
    }
    {
        let ui = Rc::clone(ui);
        resume.connect_active_notify(move |row| {
            let on = row.is_active();
            ui.act(|core| {
                core.settings.resume_last = on;
                core.persist();
            });
        });
    }
    page.add(&startup);

    let locking = adw::PreferencesGroup::builder().title("Locking").build();
    let follow = adw::SwitchRow::builder()
        .title("Sleep the screen while locked")
        .subtitle("Wakes it again when you unlock.")
        .build();
    {
        let ui = Rc::clone(ui);
        follow.connect_active_notify(move |row| {
            let on = row.is_active();
            ui.act(|core| core.set_sleep_when_locked(on));
        });
    }
    locking.add(&follow);
    page.add(&locking);

    let readings = adw::PreferencesGroup::builder()
        .title("Readings")
        .description("CPU temperature comes from the kernel. GPU readings need an NVIDIA card.")
        .build();
    let units = adw::ComboRow::builder()
        .title("Temperature")
        .model(&gtk::StringList::new(&["°C", "°F"]))
        .build();
    {
        let ui = Rc::clone(ui);
        units.connect_selected_notify(move |row| {
            let fahrenheit = row.selected() == 1;
            ui.act(|core| {
                core.settings.fahrenheit = fahrenheit;
                core.persist();
            });
        });
    }
    readings.add(&units);
    let file = adw::ActionRow::builder()
        .title("Settings file")
        .subtitle(settings::settings_path().display().to_string())
        .subtitle_selectable(true)
        .build();
    file.add_css_class("property");
    readings.add(&file);
    page.add(&readings);

    ui.on_refresh(move |ui| {
        let core = ui.core.borrow();
        let settings = &core.settings;
        tray.set_active(settings.close_to_tray);
        tray.set_sensitive(core.tray.available);
        if !core.tray.available {
            tray.set_subtitle("This desktop has no tray, so closing the window quits.");
        }
        sign_in.set_active(settings.start_with_windows);
        hidden.set_active(settings.start_in_tray);
        hidden.set_sensitive(core.tray.available);
        resume.set_active(settings.resume_last);
        follow.set_active(settings.sleep_when_locked);
        units.set_selected(u32::from(settings.fahrenheit));
    });
    page.upcast()
}
