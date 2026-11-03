//! The GTK window, used on Linux: a libadwaita sidebar with the pump's status
//! and four pages. What the buttons do lives in `engine::Controller`.

mod paint;
mod prefs;
mod screen;
mod show;
mod widgets;

use crate::engine::{demo, Controller};
use crate::settings::{self, Source};
use crate::tray::TrayAction;
use adw::prelude::*;
use gtk::{gdk, glib};
use paint::{Canvas, Painter, Picture};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// How often finished jobs, tray clicks, and a second launch are picked up.
const TICK: Duration = Duration::from_millis(200);
/// Every this many ticks the readings and previews are redrawn.
const REDRAW_TICKS: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Show,
    Widgets,
    Screen,
    Settings,
}

impl Page {
    const ALL: [Page; 4] = [Page::Show, Page::Widgets, Page::Screen, Page::Settings];

    fn name(self) -> &'static str {
        match self {
            Page::Show => "show",
            Page::Widgets => "widgets",
            Page::Screen => "screen",
            Page::Settings => "settings",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Page::Show => "Show",
            Page::Widgets => "Widgets",
            Page::Screen => "Screen",
            Page::Settings => "Settings",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::Show => "image-x-generic-symbolic",
            Page::Widgets => "view-grid-symbolic",
            Page::Screen => "video-display-symbolic",
            Page::Settings => "emblem-system-symbolic",
        }
    }
}

/// Sets a page's widgets from the controller.
type Refresher = Box<dyn Fn(&Rc<Ui>)>;

/// Everything the pages share. Handlers change the controller through `act`,
/// which then brings every page up to date with `refresh`.
pub struct Ui {
    pub core: RefCell<Controller>,
    /// Set while the pages are brought up to date, so the signals that fires do nothing.
    updating: Cell<bool>,
    refreshers: RefCell<Vec<Refresher>>,
    /// Drawing areas that show live readings, redrawn once a second.
    live: RefCell<Vec<gtk::DrawingArea>>,
    pub preview: RefCell<Option<Picture>>,
    pub painter: RefCell<Painter>,
    /// The source whose controls the Show page has open. What the pump shows is `settings.source`.
    pub picture_tab: Cell<Source>,
    pub window: adw::ApplicationWindow,
    stack: gtk::Stack,
    nav: gtk::ListBox,
    pub widgets: widgets::Editor,
}

impl Ui {
    /// Run a change from a widget's signal, then bring the pages up to date.
    /// Signals fired by `refresh` itself are ignored.
    pub fn act(self: &Rc<Self>, change: impl FnOnce(&mut Controller)) {
        if self.updating.get() {
            return;
        }
        change(&mut self.core.borrow_mut());
        self.refresh();
    }

    /// True while `refresh` is setting widgets, when signals must be ignored.
    pub fn updating(&self) -> bool {
        self.updating.get()
    }

    pub fn refresh(self: &Rc<Self>) {
        if self.updating.replace(true) {
            return;
        }
        for refresher in self.refreshers.borrow().iter() {
            refresher(self);
        }
        self.updating.set(false);
    }

    /// `refresh` calls this to set the widgets from the controller.
    pub fn on_refresh(&self, refresher: impl Fn(&Rc<Ui>) + 'static) {
        self.refreshers.borrow_mut().push(Box::new(refresher));
    }

    /// A drawing area that shows live readings.
    pub fn live(&self, area: &gtk::DrawingArea) {
        self.live.borrow_mut().push(area.clone());
    }

    fn go_to(&self, page: Page) {
        self.stack.set_visible_child_name(page.name());
        if let Some(row) = self.nav.row_at_index(page as i32) {
            self.nav.select_row(Some(&row));
        }
    }

    pub fn show_widgets(self: &Rc<Self>, canvas: Canvas) {
        self.widgets.set_canvas(self, canvas);
        self.go_to(Page::Widgets);
    }

    /// Wait until the user stops moving a slider, then run `settle` once.
    pub fn debounce(
        self: &Rc<Self>,
        slot: &Rc<Cell<Option<glib::SourceId>>>,
        settle: impl Fn(&Rc<Ui>) + 'static,
    ) {
        if let Some(pending) = slot.take() {
            pending.remove();
        }
        let ui = Rc::clone(self);
        let done = Rc::clone(slot);
        slot.set(Some(glib::timeout_add_local_once(
            Duration::from_millis(450),
            move || {
                done.set(None);
                settle(&ui);
            },
        )));
    }
}

/// A row title and a muted explanation, as the window shows them.
pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .build();
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// A row of toggle buttons that act like one control, the way GNOME groups them.
pub fn linked<T: Copy + 'static>(options: &[(T, &str)]) -> (gtk::Box, Vec<(T, gtk::ToggleButton)>) {
    let group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    group.add_css_class("linked");
    group.set_valign(gtk::Align::Center);
    let mut buttons: Vec<(T, gtk::ToggleButton)> = Vec::new();
    for (value, text) in options {
        let button = gtk::ToggleButton::with_label(text);
        if let Some((_, first)) = buttons.first() {
            button.set_group(Some(first));
        }
        group.append(&button);
        buttons.push((*value, button));
    }
    (group, buttons)
}

/// A slider for a row. The value shows next to it.
pub fn slider(min: f64, max: f64, step: f64, digits: i32) -> gtk::Scale {
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
    scale.set_digits(digits);
    scale.set_draw_value(true);
    scale.set_value_pos(gtk::PositionType::Right);
    scale.set_hexpand(true);
    scale.set_width_request(220);
    scale.set_valign(gtk::Align::Center);
    scale
}

/// Little color buttons. The one matching the current color looks pressed.
pub fn swatches(on_pick: impl Fn([u8; 3]) + 'static) -> (gtk::Box, Vec<([u8; 3], gtk::Button)>) {
    let on_pick = Rc::new(on_pick);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    row.set_valign(gtk::Align::Center);
    let mut buttons = Vec::new();
    for (index, color) in crate::layout::PALETTE.iter().enumerate() {
        let button = gtk::Button::new();
        button.add_css_class("swatch");
        button.add_css_class(&format!("swatch-{index}"));
        button.set_tooltip_text(Some("Use this color"));
        let pick = Rc::clone(&on_pick);
        let color = *color;
        button.connect_clicked(move |_| pick(color));
        row.append(&button);
        buttons.push((color, button));
    }
    (row, buttons)
}

pub fn mark_swatch(buttons: &[([u8; 3], gtk::Button)], current: [u8; 3]) {
    for (color, button) in buttons {
        if *color == current {
            button.add_css_class("picked");
        } else {
            button.remove_css_class("picked");
        }
    }
}

fn css() -> String {
    let mut css = String::from(
        "
        button.swatch { min-width: 20px; min-height: 20px; padding: 0; margin: 0;
                  border-radius: 99px; border: none; background-image: none;
                  box-shadow: inset 0 0 0 1px alpha(black, 0.3); }
        button.swatch.picked { outline: 2px solid @accent_color; outline-offset: 2px; }
        .status-card { padding: 12px; }
        .status-dot { font-size: 10px; }
        .status-dot.ok { color: @success_color; }
        .status-dot.warn { color: @warning_color; }
        .status-bar { padding: 6px 12px; }
        .screen-frame { padding: 12px; }
        ",
    );
    for (index, [r, g, b]) in crate::layout::PALETTE.iter().enumerate() {
        css.push_str(&format!(
            "button.swatch-{index}, button.swatch-{index}:hover {{ background-color: rgb({r},{g},{b}); }}\n"
        ));
    }
    css
}

pub fn run(start_in_tray: bool) {
    if !demo() && !settings::claim_single_instance() {
        return;
    }
    // Wayland matches the window to strix-lc-screen.desktop by this name.
    glib::set_prgname(Some("strix-lc-screen"));
    glib::set_application_name("Pump screen");
    let app = adw::Application::new(None::<&str>, gtk::gio::ApplicationFlags::NON_UNIQUE);
    app.connect_activate(move |app| build(app, start_in_tray));
    app.run_with_args::<&str>(&[]);
}

fn build(app: &adw::Application, start_in_tray: bool) {
    gtk::Window::set_default_icon_name("strix-lc-screen");
    let provider = gtk::CssProvider::new();
    provider.connect_parsing_error(|_, section, error| eprintln!("css: {section}: {error}"));
    provider.load_from_string(&css());
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            // Above a user theme in ~/.config/gtk-4.0/gtk.css. These rules only
            // touch this app's own classes, so the theme still styles the rest.
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
    }
    // The tray runs on its own thread. The timer below reads what it sends.
    let core = Controller::new(Arc::new(|| {}));
    let in_tray = start_in_tray && core.tray.available;
    let picture_tab = core.settings.source;
    let title = if demo() {
        "Pump screen demo"
    } else {
        "Pump screen"
    };
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(title)
        .default_width(1180)
        .default_height(800)
        .width_request(760)
        .height_request(560)
        .build();
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    let nav = gtk::ListBox::new();
    nav.add_css_class("navigation-sidebar");
    let ui = Rc::new(Ui {
        core: RefCell::new(core),
        updating: Cell::new(false),
        refreshers: RefCell::default(),
        live: RefCell::default(),
        preview: RefCell::default(),
        painter: RefCell::default(),
        picture_tab: Cell::new(picture_tab),
        window: window.clone(),
        stack: stack.clone(),
        nav: nav.clone(),
        widgets: widgets::Editor::default(),
    });

    stack.add_named(&show::page(&ui), Some(Page::Show.name()));
    stack.add_named(&widgets::page(&ui), Some(Page::Widgets.name()));
    stack.add_named(&screen::page(&ui), Some(Page::Screen.name()));
    stack.add_named(&prefs::page(&ui), Some(Page::Settings.name()));

    let content_title = adw::WindowTitle::new(Page::Show.title(), "");
    let content_header = adw::HeaderBar::new();
    content_header.set_title_widget(Some(&content_title));
    let content = adw::ToolbarView::new();
    content.add_top_bar(&content_header);
    content.set_content(Some(&stack));
    content.add_bottom_bar(&status_bar(&ui));
    let content_page = adw::NavigationPage::builder()
        .title(Page::Show.title())
        .child(&content)
        .build();

    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&sidebar(&ui, &nav)));
    split.set_content(Some(&content_page));
    split.set_min_sidebar_width(230.0);
    split.set_max_sidebar_width(280.0);

    {
        let stack = stack.clone();
        let split = split.clone();
        nav.connect_row_selected(move |_, row| {
            let Some(row) = row else {
                return;
            };
            let page = Page::ALL[row.index() as usize];
            stack.set_visible_child_name(page.name());
            content_title.set_title(page.title());
            content_page.set_title(page.title());
            split.set_show_content(true);
        });
    }

    // A narrow window shows the sidebar and the page one at a time.
    let breakpoint = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 820sp").expect("a valid condition"),
    );
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(breakpoint);
    window.set_content(Some(&split));

    match demo_page() {
        Some(page) => ui.go_to(page),
        None => ui.go_to(Page::Show),
    }
    match demo()
        .then(|| std::env::var("STRIX_LC_SCREEN_DEMO").ok())
        .flatten()
        .as_deref()
    {
        Some("picture") => ui.widgets.set_canvas(&ui, Canvas::Picture),
        Some("photo") => ui.picture_tab.set(Source::Picture),
        _ => {}
    }

    // Closing hides the window and leaves the picture playing, like the tray
    // promises. The app keeps running without a window until Quit.
    let hold = app.hold();
    {
        let ui = Rc::clone(&ui);
        let app = app.clone();
        window.connect_close_request(move |window| {
            if ui.core.borrow().closes_to_tray() {
                window.set_visible(false);
                ui.core.borrow_mut().window_open = false;
            } else {
                app.quit();
            }
            glib::Propagation::Stop
        });
    }

    {
        let ui = Rc::clone(&ui);
        let app = app.clone();
        let mut ticks = 0u32;
        glib::timeout_add_local(TICK, move || {
            ticks = ticks.wrapping_add(1);
            let mut changed = tick(&ui, &app);
            if let Some(preview) = ui.core.borrow_mut().take_preview() {
                *ui.preview.borrow_mut() = Picture::from_jpeg(&preview.jpeg);
                changed = true;
            }
            let redraw = ticks.is_multiple_of(REDRAW_TICKS);
            if changed || redraw {
                ui.refresh();
            }
            if redraw && ui.window.is_visible() {
                for area in ui.live.borrow().iter() {
                    area.queue_draw();
                }
            }
            let _ = &hold;
            glib::ControlFlow::Continue
        });
    }
    ui.refresh();
    if in_tray {
        ui.core.borrow_mut().window_open = false;
    } else {
        window.present();
    }
    if ui.core.borrow().firmware_offer.is_some() {
        screen::confirm_firmware(&ui);
    }
    if let Some(path) = demo()
        .then(|| std::env::var("STRIX_LC_SCREEN_SHOT").ok())
        .flatten()
    {
        let window = window.clone();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(2500), move || {
            if let Err(error) = screenshot(&window, &path) {
                eprintln!("screenshot: {error}");
            }
            app.quit();
        });
    }
}

/// Demo builds can save the window as a picture, to look at it without a screen grab.
fn screenshot(window: &adw::ApplicationWindow, path: &str) -> Result<(), String> {
    let (width, height) = (window.width() as f64, window.height() as f64);
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, width, height);
    let node = snapshot.to_node().ok_or("nothing was drawn")?;
    let renderer = window.renderer().ok_or("the window has no renderer")?;
    let texture = renderer.render_texture(node, None);
    texture.save_to_png(path).map_err(|error| error.to_string())
}

/// The controller's periodic work, tray clicks, and a second launch.
/// Returns true when something changed that the pages show.
fn tick(ui: &Rc<Ui>, app: &adw::Application) -> bool {
    let mut changed = ui.core.borrow_mut().tick();
    if settings::take_show_request() {
        show_window(ui);
    }
    loop {
        let action = ui.core.borrow().tray_rx.try_recv();
        match action {
            Ok(TrayAction::Show) => show_window(ui),
            Ok(TrayAction::Stop) => {
                ui.core.borrow_mut().stop_video();
                changed = true;
            }
            Ok(TrayAction::Quit) => app.quit(),
            Err(_) => break,
        }
    }
    changed
}

fn show_window(ui: &Rc<Ui>) {
    ui.core.borrow_mut().window_open = true;
    ui.window.present();
    ui.refresh();
}

fn demo_page() -> Option<Page> {
    if !demo() {
        return None;
    }
    match std::env::var("STRIX_LC_SCREEN_DEMO").ok()?.as_str() {
        "widgets" | "picture" => Some(Page::Widgets),
        "screen" | "firmware" => Some(Page::Screen),
        "settings" => Some(Page::Settings),
        _ => None,
    }
}

fn sidebar(ui: &Rc<Ui>, nav: &gtk::ListBox) -> adw::NavigationPage {
    for page in Page::ALL {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.set_margin_start(6);
        let icon = gtk::Image::from_icon_name(page.icon());
        icon.set_valign(gtk::Align::Center);
        row.append(&icon);
        row.append(&gtk::Label::new(Some(page.title())));
        nav.append(&row);
    }
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.append(&status_card(ui));
    body.append(nav);
    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    body.append(&spacer);
    let note = label("", &["dim-label", "caption"]);
    note.set_margin_start(12);
    note.set_margin_end(12);
    body.append(&note);
    let version = label(
        &format!("Version {}", env!("CARGO_PKG_VERSION")),
        &["dim-label", "caption"],
    );
    version.set_margin_start(12);
    version.set_margin_bottom(12);
    body.append(&version);
    ui.on_refresh(move |ui| {
        note.set_label(if ui.core.borrow().closes_to_tray() {
            "Closing the window keeps it running in the tray."
        } else {
            "Closing the window hands the screen back to the pump."
        });
    });

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Pump screen", "")));
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&body));
    adw::NavigationPage::builder()
        .title("Pump screen")
        .child(&view)
        .build()
}

/// Whether the pump is reachable, what it shows, and its firmware.
fn status_card(ui: &Rc<Ui>) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("card");
    card.add_css_class("status-card");
    card.set_margin_start(12);
    card.set_margin_end(12);
    card.set_margin_top(6);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let dot = gtk::Label::new(Some("●"));
    dot.add_css_class("status-dot");
    let title = label("", &["heading"]);
    top.append(&dot);
    top.append(&title);
    let showing = label("", &["dim-label", "caption"]);
    showing.set_wrap(false);
    showing.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let firmware = label("", &["dim-label", "caption"]);
    let turn_on = gtk::Button::with_label("Turn the screen on");
    turn_on.add_css_class("suggested-action");
    turn_on.set_margin_top(6);
    {
        let ui = Rc::clone(ui);
        turn_on.connect_clicked(move |_| {
            ui.act(|core| core.apply_setting(crate::session::Setting::ScreenOn(true)));
        });
    }
    card.append(&top);
    card.append(&showing);
    card.append(&firmware);
    card.append(&turn_on);
    ui.on_refresh(move |ui| {
        let core = ui.core.borrow();
        let screen_off = core.snapshot.as_ref().is_some_and(|shot| !shot.screen_on);
        let (text, class) = if core.asus_running {
            ("ASUS app is open", "warn")
        } else if screen_off {
            ("Screen is off", "warn")
        } else if core.snapshot.is_some() {
            ("Connected", "ok")
        } else if core.busy {
            ("Connecting…", "warn")
        } else {
            ("Not connected", "dim-label")
        };
        title.set_label(text);
        for old in ["ok", "warn", "dim-label"] {
            dot.remove_css_class(old);
        }
        dot.add_css_class(class);
        let what = match core.settings.source {
            Source::Picture => core
                .chosen_name()
                .unwrap_or_else(|| "nothing chosen yet".into()),
            Source::Dashboard => "the stats dashboard".into(),
            Source::Animation => "the pump's own animation".into(),
        };
        showing.set_label(&format!("Showing {what}"));
        showing.set_tooltip_text(Some(&what));
        match &core.snapshot {
            Some(shot) => {
                firmware.set_label(&format!("Firmware {}", shot.firmware));
                firmware.set_visible(true);
            }
            None => firmware.set_visible(false),
        }
        turn_on.set_visible(screen_off);
        turn_on.set_sensitive(!core.busy);
    });
    card
}

/// The last thing the app did, along the bottom of the page.
fn status_bar(ui: &Rc<Ui>) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.add_css_class("status-bar");
    let spinner = gtk::Spinner::new();
    let text = label("", &["dim-label"]);
    text.set_wrap(false);
    text.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.set_hexpand(true);
    bar.append(&spinner);
    bar.append(&text);
    ui.on_refresh(move |ui| {
        let core = ui.core.borrow();
        text.set_label(&core.status);
        text.set_tooltip_text(Some(&core.status));
        spinner.set_spinning(core.busy);
        spinner.set_visible(core.busy);
    });
    bar
}
