//! The egui window, used on Windows: a sidebar with the pump's status and the
//! pages. What the buttons do lives in `core::Controller`.

use crate::engine::{demo, icon_rgba, Controller};
use crate::media::Crop;
use crate::settings::{self, Source};
use crate::theme;
use crate::tray::TrayAction;
use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod editor;
mod pages;

/// The pages in the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Show,
    Widgets,
    Screen,
    Settings,
}

/// The window's title. Windows finds the window by it.
fn window_title() -> &'static str {
    if demo() {
        "Pump screen demo"
    } else {
        "Pump screen"
    }
}

/// The page a demo window opens on, from STRIX_LC_SCREEN_DEMO.
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

pub fn run(start_in_tray: bool) -> eframe::Result<()> {
    if !demo() && !settings::claim_single_instance() {
        return Ok(());
    }
    // The tray and a second launch run on other threads. They repaint
    // whichever context the window has once it exists.
    let context: Arc<Mutex<Option<egui::Context>>> = Arc::default();
    let wake = {
        let context = Arc::clone(&context);
        Arc::new(move || {
            if let Some(ctx) = context.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                ctx.request_repaint();
            }
        })
    };
    settings::on_show_request({
        let wake = Arc::clone(&wake);
        move || wake()
    });
    let core = Controller::new(wake);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1200.0, 780.0])
        .with_min_inner_size([1000.0, 660.0])
        .with_resizable(true)
        .with_decorations(true)
        .with_title(window_title())
        .with_app_id("strix-lc-screen")
        .with_icon(app_icon());
    #[cfg(windows)]
    {
        viewport = viewport.with_transparent(true);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Pump screen",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            let _ = apply_window_material();
            *context.lock().unwrap_or_else(|e| e.into_inner()) = Some(cc.egui_ctx.clone());
            Ok(Box::new(App::new(core, start_in_tray)))
        }),
    )
}

struct App {
    core: Controller,
    preview: Option<egui::TextureHandle>,
    quit: bool,
    hide_once: bool,
    /// Window is parked off-screen. It stays visible to Windows so paints still arrive.
    in_tray: bool,
    material_applied: bool,
    /// The saved window from the tall layout is resized once.
    window_fitted: bool,
    /// The source whose controls the Show page has open. What the pump shows is `settings.source`.
    picture_tab: Source,
    page: Page,
    editor: editor::Editor,
    /// Widget layouts before each change, newest last.
    undo: Vec<(editor::Canvas, crate::layout::Layout)>,
    /// The layout as it was when the current drag began.
    gesture: Option<crate::layout::Layout>,
}

/// The pages read and change the controller's state as if it were the window's.
impl std::ops::Deref for App {
    type Target = Controller;

    fn deref(&self) -> &Controller {
        &self.core
    }
}

impl std::ops::DerefMut for App {
    fn deref_mut(&mut self) -> &mut Controller {
        &mut self.core
    }
}

impl App {
    fn new(core: Controller, start_in_tray: bool) -> Self {
        let picture_tab = core.settings.source;
        Self {
            core,
            preview: None,
            quit: false,
            hide_once: start_in_tray,
            in_tray: false,
            material_applied: false,
            window_fitted: false,
            picture_tab,
            page: demo_page().unwrap_or(Page::Show),
            editor: {
                let mut editor = editor::Editor::new();
                if demo() && std::env::var("STRIX_LC_SCREEN_DEMO").as_deref() == Ok("picture") {
                    editor.canvas = editor::Canvas::Picture;
                }
                editor
            },
            undo: Vec::new(),
            gesture: None,
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.in_tray = false;
        reveal_pump_window();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        while let Ok(action) = self.tray_rx.try_recv() {
            match action {
                TrayAction::Show => self.show_window(ctx),
                TrayAction::Stop => self.stop_video(),
                TrayAction::Quit => {
                    self.quit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn poll_preview(&mut self, ctx: &egui::Context) {
        let Some(preview) = self.core.take_preview() else {
            return;
        };
        match image::load_from_memory(&preview.jpeg) {
            Ok(image) => {
                let decoded = image.to_rgb8();
                let color = egui::ColorImage::from_rgb(
                    [decoded.width() as usize, decoded.height() as usize],
                    decoded.as_raw(),
                );
                self.preview =
                    Some(ctx.load_texture("preview", color, egui::TextureOptions::LINEAR));
            }
            Err(error) => {
                self.status = format!("could not show a preview: {error}");
            }
        }
        ctx.request_repaint();
    }

    fn choose_file(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "Photos and video",
                &[
                    "png", "jpg", "jpeg", "gif", "bmp", "mp4", "mov", "avi", "mkv", "webm",
                ],
            )
            .pick_file()
        else {
            return;
        };
        self.preview = None;
        self.core.choose(path);
        ctx.request_repaint();
    }

    fn choose_firmware(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("ASUS firmware package", &["zip"])
            .pick_file()
        else {
            return;
        };
        self.core.offer_firmware(&path);
    }

    fn fit_window(&mut self, ctx: &egui::Context) {
        if self.window_fitted || self.in_tray {
            return;
        }
        self.window_fitted = true;
        let narrow = ctx.input(|input| {
            input
                .viewport()
                .inner_rect
                .map(|rect| rect.width() < 1000.0 || rect.height() < 660.0)
                .unwrap_or(true)
        });
        if narrow {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1200.0, 780.0)));
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if settings::take_show_request() {
            self.show_window(ctx);
        }
        self.poll_tray(ctx);
        if self.hide_once && self.tray.available && park_pump_window(ctx) {
            self.hide_once = false;
            self.in_tray = true;
        } else if self.hide_once {
            if self.tray.available {
                ctx.request_repaint();
            } else {
                self.hide_once = false;
            }
        }
        if ctx.input(|input| input.viewport().close_requested())
            && !self.quit
            && self.closes_to_tray()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if park_pump_window(ctx) {
                self.in_tray = true;
            }
        }
        // Windows puts the window back on the taskbar unless the tool style is
        // applied again.
        #[cfg(windows)]
        if self.in_tray {
            if pump_window_is_on_screen() {
                self.in_tray = false;
            } else {
                let _ = park_pump_window(ctx);
            }
        }
        if !self.material_applied {
            self.material_applied = apply_window_material();
        }
        self.fit_window(ctx);
        self.core.window_open = !self.in_tray;
        if self.core.tick() {
            ctx.request_repaint();
        }
        self.poll_preview(ctx);
        if self.waiting() {
            ctx.request_repaint_after(Duration::from_millis(200));
        } else if !self.in_tray {
            ctx.request_repaint_after(Duration::from_secs(3));
        }
        self.draw(ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        #[cfg(windows)]
        {
            [0.0, 0.0, 0.0, 0.0]
        }
        #[cfg(not(windows))]
        {
            egui::Color32::from_rgb(16, 14, 20).to_normalized_gamma_f32()
        }
    }
}

fn apply_window_material() -> bool {
    #[cfg(windows)]
    {
        return apply_windows_material();
    }
    #[cfg(not(windows))]
    {
        true
    }
}

fn crop_view(
    ui: &mut egui::Ui,
    texture: &egui::TextureHandle,
    source_width: u32,
    source_height: u32,
    zoom: &mut f32,
    pan_x: &mut f32,
    pan_y: &mut f32,
) -> bool {
    let source_width = source_width.max(1) as f32;
    let source_height = source_height.max(1) as f32;
    let max_w = ui.available_width().min(280.0);
    let aspect = source_width / source_height;
    let (disp_w, disp_h) = if aspect >= 1.0 {
        (max_w, max_w / aspect)
    } else {
        (max_w * aspect, max_w)
    };
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(disp_w, disp_h), egui::Sense::click_and_drag());
    ui.painter().image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    let crop = Crop {
        zoom: *zoom,
        pan_x: *pan_x,
        pan_y: *pan_y,
    };
    let (side, x, y) = crop.square(source_width as u32, source_height as u32);
    let sx = disp_w / source_width;
    let sy = disp_h / source_height;
    let crop_rect = egui::Rect::from_min_size(
        rect.min + egui::vec2(x as f32 * sx, y as f32 * sy),
        egui::vec2(side as f32 * sx, side as f32 * sy),
    );
    let shade = egui::Color32::from_black_alpha(140);
    let painter = ui.painter();
    painter.rect_filled(
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, crop_rect.min.y)),
        0.0,
        shade,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(rect.min.x, crop_rect.max.y), rect.max),
        0.0,
        shade,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(rect.min.x, crop_rect.min.y),
            crop_rect.left_bottom(),
        ),
        0.0,
        shade,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            crop_rect.right_top(),
            egui::pos2(rect.max.x, crop_rect.max.y),
        ),
        0.0,
        shade,
    );
    painter.rect_stroke(
        crop_rect,
        2.0,
        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(255, 228, 232)),
        egui::StrokeKind::Inside,
    );
    if response.dragged() {
        let delta = response.drag_delta();
        let max_x = (source_width - side as f32).max(1.0);
        let max_y = (source_height - side as f32).max(1.0);
        *pan_x = (*pan_x + delta.x / (max_x * sx) * 2.0).clamp(-1.0, 1.0);
        *pan_y = (*pan_y + delta.y / (max_y * sy) * 2.0).clamp(-1.0, 1.0);
    }
    response.drag_stopped()
}

/// The window icon. X11 and Windows show it; Wayland uses the installed icon.
fn app_icon() -> egui::IconData {
    let image = icon_rgba();
    let (width, height) = image.dimensions();
    egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

/// Puts the window back on the primary monitor and on the taskbar.
pub fn reveal_pump_window() {
    #[cfg(windows)]
    unsafe {
        let Some(hwnd) = pump_hwnd() else {
            return;
        };
        set_taskbar_button(hwnd, true);
        if !pump_window_is_on_screen() {
            place_on_primary(hwnd);
        }
        let cmd = if IsIconic(hwnd) != 0 { 9 } else { 5 };
        ShowWindow(hwnd, cmd);
        let foreground = GetForegroundWindow();
        let foreground_thread = GetWindowThreadProcessId(foreground, std::ptr::null_mut());
        let this_thread = GetCurrentThreadId();
        if foreground_thread != 0 && foreground_thread != this_thread {
            AttachThreadInput(foreground_thread, this_thread, 1);
            SetForegroundWindow(hwnd);
            BringWindowToTop(hwnd);
            AttachThreadInput(foreground_thread, this_thread, 0);
        } else {
            SetForegroundWindow(hwnd);
            BringWindowToTop(hwnd);
        }
    }
    let _ = apply_window_material();
}

/// Moves the window off the virtual screen without hiding it.
///
/// `ShowWindow(SW_HIDE)` makes Windows drop `WM_PAINT`. eframe then leaves its
/// event loop in `Poll` after a missed redraw, which burns one core and never
/// reaches the code that starts playback.
fn park_pump_window(ctx: &egui::Context) -> bool {
    #[cfg(windows)]
    {
        let _ = ctx;
        unsafe {
            let Some(hwnd) = pump_hwnd() else {
                return false;
            };
            if !pump_window_is_on_screen() && is_tool_window(hwnd) {
                return true;
            }
            set_taskbar_button(hwnd, false);
            let rect = window_rect(hwnd);
            let width = (rect.right - rect.left).max(1);
            let height = (rect.bottom - rect.top).max(1);
            let x = GetSystemMetrics(76)
                .saturating_sub(width)
                .saturating_sub(80);
            let y = GetSystemMetrics(77)
                .saturating_sub(height)
                .saturating_sub(80);
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                x,
                y,
                0,
                0,
                0x0010 | 0x0001 | 0x0004 | 0x0020,
            );
            // Refresh the taskbar button, then leave the window visible so paints still arrive.
            ShowWindow(hwnd, 0);
            ShowWindow(hwnd, 8);
            return true;
        }
    }
    #[cfg(not(windows))]
    {
        // Hiding is safe here. The one-core spin came from Win32 dropping WM_PAINT
        // after SW_HIDE. Playback itself runs on the worker thread.
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        true
    }
}

#[cfg(windows)]
fn pump_window_is_on_screen() -> bool {
    unsafe {
        let Some(hwnd) = pump_hwnd() else {
            return false;
        };
        let rect = window_rect(hwnd);
        let cx = rect.left.saturating_add(rect.right) / 2;
        let cy = rect.top.saturating_add(rect.bottom) / 2;
        let vx = GetSystemMetrics(76);
        let vy = GetSystemMetrics(77);
        let vw = GetSystemMetrics(78);
        let vh = GetSystemMetrics(79);
        cx >= vx && cy >= vy && cx < vx.saturating_add(vw) && cy < vy.saturating_add(vh)
    }
}

#[cfg(windows)]
fn pump_hwnd() -> Option<*mut std::ffi::c_void> {
    let title: Vec<u16> = window_title()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if hwnd.is_null() {
        None
    } else {
        Some(hwnd)
    }
}

#[cfg(windows)]
fn window_rect(hwnd: *mut std::ffi::c_void) -> Rect {
    let mut rect = Rect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe {
        GetWindowRect(hwnd, &mut rect);
    }
    rect
}

#[cfg(windows)]
fn place_on_primary(hwnd: *mut std::ffi::c_void) {
    unsafe {
        let rect = window_rect(hwnd);
        let width = (rect.right - rect.left).max(200);
        let height = (rect.bottom - rect.top).max(200);
        let screen_w = GetSystemMetrics(0).max(width);
        let screen_h = GetSystemMetrics(1).max(height);
        let x = (screen_w - width) / 2;
        let y = (screen_h - height) / 2;
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            x,
            y,
            0,
            0,
            0x0010 | 0x0001 | 0x0004 | 0x0020,
        );
    }
}

#[cfg(windows)]
fn is_tool_window(hwnd: *mut std::ffi::c_void) -> bool {
    unsafe {
        const GWL_EXSTYLE: i32 = -20;
        const WS_EX_TOOLWINDOW: isize = 0x0000_0080;
        const WS_EX_APPWINDOW: isize = 0x0004_0000;
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        style & WS_EX_TOOLWINDOW != 0 && style & WS_EX_APPWINDOW == 0
    }
}

#[cfg(windows)]
fn set_taskbar_button(hwnd: *mut std::ffi::c_void, show: bool) {
    unsafe {
        const GWL_EXSTYLE: i32 = -20;
        const WS_EX_TOOLWINDOW: isize = 0x0000_0080;
        const WS_EX_APPWINDOW: isize = 0x0004_0000;
        let mut style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if show {
            style &= !WS_EX_TOOLWINDOW;
            style |= WS_EX_APPWINDOW;
        } else {
            style |= WS_EX_TOOLWINDOW;
            style &= !WS_EX_APPWINDOW;
        }
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
    }
}

#[cfg(windows)]
#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(windows)]
#[repr(C)]
struct DwmMargins {
    cx_left_width: i32,
    cx_right_width: i32,
    cy_top_height: i32,
    cy_bottom_height: i32,
}

#[cfg(windows)]
fn apply_windows_material() -> bool {
    let Some(hwnd) = pump_hwnd() else {
        return false;
    };
    unsafe {
        let dark: u32 = 1;
        // DWMWA_USE_IMMERSIVE_DARK_MODE
        let _ = DwmSetWindowAttribute(hwnd, 20, &dark as *const u32 as _, 4);
        if is_windows_11() {
            // No system backdrop: Windows turns those solid while the window is
            // inactive. The acrylic accent below keeps blurring when unfocused.
            let backdrop: i32 = 1;
            let _ = DwmSetWindowAttribute(hwnd, 38, &backdrop as *const i32 as _, 4);
            // DWMWCP_ROUND
            let corners: i32 = 2;
            let _ = DwmSetWindowAttribute(hwnd, 33, &corners as *const i32 as _, 4);
            // COLORREF is 0x00BBGGRR.
            let border: u32 = 0x00302832;
            let caption: u32 = 0x00181016;
            let text: u32 = 0x00F2EEF0;
            let _ = DwmSetWindowAttribute(hwnd, 34, &border as *const u32 as _, 4);
            let _ = DwmSetWindowAttribute(hwnd, 35, &caption as *const u32 as _, 4);
            let _ = DwmSetWindowAttribute(hwnd, 36, &text as *const u32 as _, 4);
        }
        // The whole client area is glass, so egui's transparent pixels show the blur.
        let margins = DwmMargins {
            cx_left_width: -1,
            cx_right_width: -1,
            cy_top_height: -1,
            cy_bottom_height: -1,
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        enable_acrylic(hwnd);
    }
    true
}

#[cfg(windows)]
fn is_windows_11() -> bool {
    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
    }
    unsafe {
        let mut info = std::mem::zeroed::<OsVersionInfoW>();
        info.size = std::mem::size_of::<OsVersionInfoW>() as u32;
        if RtlGetVersion(&mut info) != 0 {
            return false;
        }
        info.build >= 22000
    }
}

#[cfg(windows)]
fn enable_acrylic(hwnd: *mut std::ffi::c_void) {
    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        color: u32,
        animation: u32,
    }
    #[repr(C)]
    struct CompositionAttribute {
        attribute: u32,
        data: *mut std::ffi::c_void,
        size: usize,
    }
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut std::ffi::c_void;
        fn GetProcAddress(module: *mut std::ffi::c_void, name: *const u8) -> *mut std::ffi::c_void;
    }
    type SetComposition =
        unsafe extern "system" fn(*mut std::ffi::c_void, *mut CompositionAttribute) -> i32;
    unsafe {
        let module_name: Vec<u16> = "user32.dll"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let module = GetModuleHandleW(module_name.as_ptr());
        if module.is_null() {
            return;
        }
        let proc = GetProcAddress(module, b"SetWindowCompositionAttribute\0".as_ptr());
        if proc.is_null() {
            return;
        }
        let set: SetComposition = std::mem::transmute(proc);
        let mut policy = AccentPolicy {
            state: 4,
            flags: 2,
            color: 0x401A1218,
            animation: 0,
        };
        let mut data = CompositionAttribute {
            attribute: 19,
            data: &mut policy as *mut AccentPolicy as *mut std::ffi::c_void,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        let _ = set(hwnd, &mut data);
    }
}

#[cfg(windows)]
#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(
        hwnd: *mut std::ffi::c_void,
        attribute: u32,
        data: *const std::ffi::c_void,
        size: u32,
    ) -> i32;
    fn DwmExtendFrameIntoClientArea(hwnd: *mut std::ffi::c_void, margins: *const DwmMargins)
        -> i32;
}

#[cfg(windows)]
unsafe extern "system" {
    fn FindWindowW(class: *const u16, window: *const u16) -> *mut std::ffi::c_void;
    fn ShowWindow(hwnd: *mut std::ffi::c_void, cmd: i32) -> i32;
    fn SetForegroundWindow(hwnd: *mut std::ffi::c_void) -> i32;
    fn BringWindowToTop(hwnd: *mut std::ffi::c_void) -> i32;
    fn IsIconic(hwnd: *mut std::ffi::c_void) -> i32;
    fn GetForegroundWindow() -> *mut std::ffi::c_void;
    fn GetWindowThreadProcessId(hwnd: *mut std::ffi::c_void, process: *mut u32) -> u32;
    fn GetCurrentThreadId() -> u32;
    fn AttachThreadInput(from: u32, to: u32, attach: i32) -> i32;
    fn GetWindowRect(hwnd: *mut std::ffi::c_void, rect: *mut Rect) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn SetWindowPos(
        hwnd: *mut std::ffi::c_void,
        insert_after: *mut std::ffi::c_void,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
    fn GetWindowLongPtrW(hwnd: *mut std::ffi::c_void, index: i32) -> isize;
    fn SetWindowLongPtrW(hwnd: *mut std::ffi::c_void, index: i32, value: isize) -> isize;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_icon_decodes() {
        let icon = app_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }
}
