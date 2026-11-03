//! The tray icon and its menu: Show, Stop video, and Quit. Menu clicks arrive
//! on `Controller::tray_rx`, and `Wake` nudges the window to read them.

use std::sync::Arc;

/// Called from the tray's thread so the window looks at `tray_rx` soon.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

pub enum TrayAction {
    Show,
    Stop,
    Quit,
}

#[cfg(windows)]
fn tray_click_shows_window(event: &tray_icon::TrayIconEvent) -> bool {
    match event {
        tray_icon::TrayIconEvent::Click {
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Up,
            ..
        }
        | tray_icon::TrayIconEvent::DoubleClick {
            button: tray_icon::MouseButton::Left,
            ..
        } => true,
        _ => false,
    }
}

pub struct TrayMenu {
    pub available: bool,
    #[cfg(windows)]
    _icon: tray_icon::TrayIcon,
    #[cfg(windows)]
    _show: tray_icon::menu::MenuItem,
    #[cfg(windows)]
    _stop: tray_icon::menu::MenuItem,
    #[cfg(windows)]
    _quit: tray_icon::menu::MenuItem,
    #[cfg(target_os = "linux")]
    _handle: Option<ksni::blocking::Handle<PumpTray>>,
}

impl TrayMenu {
    pub fn build(tx: std::sync::mpsc::SyncSender<TrayAction>, wake: Wake) -> Self {
        #[cfg(windows)]
        {
            build_windows_tray(tx, wake)
        }
        #[cfg(target_os = "linux")]
        {
            build_linux_tray(tx, wake)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (tx, wake);
            Self { available: false }
        }
    }
}

#[cfg(windows)]
fn build_windows_tray(tx: std::sync::mpsc::SyncSender<TrayAction>, wake: Wake) -> TrayMenu {
    use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
    let show = MenuItem::with_id("show", "Show", true, None);
    let stop = MenuItem::with_id("stop", "Stop video", true, None);
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    let menu = Menu::new();
    let _ = menu.append(&show);
    let _ = menu.append(&stop);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit);
    let show_id = show.id().clone();
    let stop_id = stop.id().clone();
    let quit_id = quit.id().clone();
    let click_tx = tx.clone();
    let click_wake = Arc::clone(&wake);
    tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
        if tray_click_shows_window(&event) {
            crate::gui::reveal_pump_window();
            let _ = click_tx.try_send(TrayAction::Show);
        }
        click_wake();
    }));
    tray_icon::menu::MenuEvent::set_event_handler(Some(
        move |event: tray_icon::menu::MenuEvent| {
            let action = if event.id == show_id {
                TrayAction::Show
            } else if event.id == stop_id {
                TrayAction::Stop
            } else if event.id == quit_id {
                TrayAction::Quit
            } else {
                return;
            };
            let _ = tx.try_send(action);
            wake();
        },
    ));
    let icon = tray_icon_image();
    let tray = tray_icon::TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Pump screen")
        .with_icon(icon)
        .build()
        .expect("the tray icon could not be created");
    TrayMenu {
        available: true,
        _icon: tray,
        _show: show,
        _stop: stop,
        _quit: quit,
    }
}

#[cfg(target_os = "linux")]
struct PumpTray {
    tx: std::sync::mpsc::SyncSender<TrayAction>,
    wake: Wake,
}

#[cfg(target_os = "linux")]
impl PumpTray {
    fn send(&mut self, action: TrayAction) {
        let _ = self.tx.try_send(action);
        (self.wake)();
    }
}

#[cfg(target_os = "linux")]
impl ksni::Tray for PumpTray {
    fn id(&self) -> String {
        "strix-lc-screen".into()
    }

    fn title(&self) -> String {
        "Pump screen".into()
    }

    fn icon_name(&self) -> String {
        "strix-lc-screen".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![pump_tray_pixmap()]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Pump screen".into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayAction::Show);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Show".into(),
                activate: Box::new(|tray: &mut Self| tray.send(TrayAction::Show)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Stop video".into(),
                activate: Box::new(|tray: &mut Self| tray.send(TrayAction::Stop)),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| tray.send(TrayAction::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(target_os = "linux")]
fn build_linux_tray(tx: std::sync::mpsc::SyncSender<TrayAction>, wake: Wake) -> TrayMenu {
    use ksni::blocking::TrayMethods;
    let tray = PumpTray { tx, wake };
    match tray.spawn() {
        Ok(handle) => TrayMenu {
            available: true,
            _handle: Some(handle),
        },
        Err(error) => {
            eprintln!("tray icon: {error}");
            TrayMenu {
                available: false,
                _handle: None,
            }
        }
    }
}

/// Used when the tray host cannot find the installed icon, as with the tarball.
#[cfg(target_os = "linux")]
fn pump_tray_pixmap() -> ksni::Icon {
    let size = 32u32;
    let image = image::imageops::resize(
        &crate::engine::icon_rgba(),
        size,
        size,
        image::imageops::FilterType::Lanczos3,
    );
    // StatusNotifierItem wants ARGB in network byte order.
    let data = image
        .pixels()
        .flat_map(|p| [p[3], p[0], p[1], p[2]])
        .collect();
    ksni::Icon {
        width: size as i32,
        height: size as i32,
        data,
    }
}

#[cfg(windows)]
fn tray_icon_image() -> tray_icon::Icon {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let dx = x as i32 - 15;
            let dy = y as i32 - 15;
            let index = ((y * size + x) * 4) as usize;
            if dx * dx + dy * dy <= 13 * 13 {
                rgba[index] = 214;
                rgba[index + 1] = 64;
                rgba[index + 2] = 38;
                rgba[index + 3] = 255;
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, size, size).expect("tray icon image")
}
