//! Window for brightness, rotation, and pictures on the pump screen.

use crate::lock;
use crate::media::{self, photo_jpeg, video_jpegs, Crop};
// `Panel` is the USB session with the screen; `stats_panel` draws the stats panel.
use crate::panel::{self as stats_panel, Composer, Position};
use crate::session::{Panel, Setting};
use crate::settings::{self, Settings, Source};
use crate::stats::{self, Metric};
use anyhow::{Context, Result};
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

enum Request {
    Refresh(Sender<Result<Snapshot>>),
    Brightness(u8, Sender<Result<Snapshot>>),
    Rotate(u16, Sender<Result<Snapshot>>),
    Power(&'static str, Sender<Result<Snapshot>>),
    Apply(Setting, Sender<Result<Snapshot>>),
    /// Stop sending, so the pump goes back to its own animation.
    Animation(Sender<Result<Snapshot>>),
    /// Show the stats dashboard as the picture.
    Dashboard,
    ShowStill(Vec<u8>, Sender<Result<()>>),
    Play(Vec<Vec<u8>>, u64),
    Stop,
    /// The computer was locked (`true`) or unlocked.
    Locked(bool),
}

#[derive(Clone)]
struct Snapshot {
    serial: String,
    firmware: String,
    brightness: u8,
    rotation: u16,
    free_space: String,
    screen_on: bool,
    display_in_sleep: bool,
}

#[derive(Clone)]
struct Worker {
    pub(crate) tx: Sender<Request>,
    cancel: Arc<AtomicBool>,
    /// 1000 means 1×. The playback loop reads this between frames.
    speed: Arc<AtomicU32>,
}

impl Worker {
    fn start(stats: Arc<stats_panel::Shared>) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let speed = Arc::new(AtomicU32::new(1000));
        let flag = Arc::clone(&cancel);
        let pace = Arc::clone(&speed);
        thread::spawn(move || worker_loop(rx, flag, pace, stats));
        Self { tx, cancel, speed }
    }

    fn play(&self, frames: Vec<Vec<u8>>, frame_ms: u64) -> Result<()> {
        self.cancel.store(false, Ordering::Relaxed);
        self.tx
            .send(Request::Play(frames, frame_ms))
            .context("screen worker stopped")?;
        Ok(())
    }

    fn set_speed(&self, speed: f32) {
        let thousandths = (speed.clamp(0.25, 2.0) * 1000.0).round() as u32;
        self.speed.store(thousandths.max(250), Ordering::Relaxed);
    }

    fn stop_playback(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Request::Stop);
    }
}

struct Playing {
    frames: Vec<Vec<u8>>,
    frame_ms: u64,
    index: usize,
    next_at: Instant,
    /// A frame has reached the pump. Later failures give up sooner than the login retry.
    delivered: bool,
    /// The live picture channel is open. The next frame can skip the wake and channel setup.
    prepared: bool,
    retries: u8,
    /// One picture, sent again so the panel does not fall back to its built-in clip.
    hold: bool,
    paused: bool,
    /// Send the stats dashboard in place of the frame.
    dashboard: bool,
}

/// The panel drops a live picture about a second after the frames stop.
const HOLD_MS: u64 = 500;

fn hold_of(jpeg: Vec<u8>) -> Playing {
    Playing {
        frames: vec![jpeg],
        frame_ms: HOLD_MS,
        index: 0,
        next_at: Instant::now() + Duration::from_millis(HOLD_MS),
        delivered: true,
        prepared: true,
        retries: 0,
        hold: true,
        paused: false,
        dashboard: false,
    }
}

/// The dashboard is held like a still. Its frame is replaced as it is sent,
/// and stays black until the first dashboard is drawn.
fn dashboard_hold() -> Playing {
    let black = image::RgbImage::new(stats_panel::SIZE, stats_panel::SIZE);
    Playing {
        frames: vec![stats_panel::encode(&black).unwrap_or_default()],
        frame_ms: HOLD_MS,
        index: 0,
        next_at: Instant::now(),
        delivered: false,
        prepared: false,
        retries: 0,
        hold: true,
        paused: false,
        dashboard: true,
    }
}

/// `next_index` is the frame the loop would send next.
fn last_sent_index(next_index: usize, len: usize, delivered: bool) -> Option<usize> {
    if !delivered || len == 0 || next_index >= len {
        return None;
    }
    Some(if next_index == 0 {
        len - 1
    } else {
        next_index - 1
    })
}

fn hold_last(play: Playing) -> Option<Playing> {
    // The dashboard is already a held picture.
    if play.dashboard {
        return Some(play);
    }
    let index = last_sent_index(play.index, play.frames.len(), play.delivered)?;
    let jpeg = play.frames.get(index)?.clone();
    Some(hold_of(jpeg))
}

fn open_panel() -> Option<Panel> {
    Panel::open()
        .and_then(|mut panel| {
            panel.connect()?;
            Ok(panel)
        })
        .ok()
}

enum AfterCommand {
    Pause,
    Wake,
    /// Brightness and rotation can kick the panel back to its built-in clip.
    Rewake,
    Keep,
}

fn worker_loop(
    rx: Receiver<Request>,
    cancel: Arc<AtomicBool>,
    speed: Arc<AtomicU32>,
    stats: Arc<stats_panel::Shared>,
) {
    let mut panel = open_panel();
    let mut playing: Option<Playing> = None;
    let mut composer = Composer::default();
    loop {
        let wait = match &playing {
            Some(play) if play.paused => Duration::from_secs(3600),
            Some(play) => play.next_at.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(3600),
        };
        let incoming = if playing.is_some() {
            rx.recv_timeout(wait).ok()
        } else {
            match rx.recv() {
                Ok(request) => Some(request),
                Err(_) => return,
            }
        };
        if let Some(request) = incoming {
            match request {
                Request::Stop => {
                    playing = playing.take().and_then(hold_last);
                    cancel.store(false, Ordering::Relaxed);
                }
                Request::Play(frames, frame_ms) => {
                    playing = Some(Playing {
                        frames,
                        frame_ms: frame_ms.max(1),
                        index: 0,
                        next_at: Instant::now(),
                        delivered: false,
                        prepared: false,
                        retries: 0,
                        hold: false,
                        paused: false,
                        dashboard: false,
                    });
                    cancel.store(false, Ordering::Relaxed);
                }
                Request::ShowStill(jpeg, reply) => {
                    cancel.store(false, Ordering::Relaxed);
                    playing = None;
                    if panel.is_none() {
                        panel = open_panel();
                    }
                    let result = (|| {
                        let panel = panel.as_mut().context("pump screen is not connected")?;
                        let composed = composer.frame(&stats, &jpeg, false);
                        let first = composed.as_deref().map_or(jpeg.as_slice(), Vec::as_slice);
                        panel.show_jpeg(first, &|| cancel.load(Ordering::Relaxed))
                    })();
                    if result.is_ok() && !cancel.load(Ordering::Relaxed) {
                        playing = Some(hold_of(jpeg));
                    }
                    let _ = reply.send(result);
                }
                Request::Dashboard => {
                    cancel.store(false, Ordering::Relaxed);
                    playing = Some(dashboard_hold());
                }
                Request::Locked(locked) => {
                    if panel.is_none() {
                        panel = open_panel();
                    }
                    if let Some(panel) = panel.as_mut() {
                        let effect = lock_change(panel, locked);
                        after_command(&mut playing, effect);
                    }
                }
                Request::Animation(reply) => {
                    // The pump goes back to its animation about a second after
                    // frames stop. Closing the channel by command left it blank.
                    playing = None;
                    cancel.store(false, Ordering::Relaxed);
                    if panel.is_none() {
                        panel = open_panel();
                    }
                    let result = (|| {
                        let panel = panel.as_mut().context("pump screen is not connected")?;
                        read_snapshot(panel)
                    })();
                    let _ = reply.send(result);
                }
                other => {
                    let effect = match &other {
                        Request::Power("suspend", _) => AfterCommand::Pause,
                        Request::Power("resume", _) => AfterCommand::Wake,
                        Request::Apply(Setting::ScreenOn(false), _) => AfterCommand::Pause,
                        Request::Apply(Setting::ScreenOn(true), _) => AfterCommand::Wake,
                        Request::Brightness(_, _) | Request::Rotate(_, _) => AfterCommand::Rewake,
                        _ => AfterCommand::Keep,
                    };
                    if panel.is_none() {
                        panel = open_panel();
                    }
                    dispatch(&mut panel, other);
                    after_command(&mut playing, effect);
                }
            }
        }
        // A failed USB read or write can leave a dead handle. Open a new one next time.
        if panel.as_ref().is_some_and(Panel::io_failed) {
            panel = None;
            if let Some(play) = playing.as_mut() {
                play.prepared = false;
            }
        }
        if cancel.load(Ordering::Relaxed) {
            playing = playing.take().and_then(hold_last);
            cancel.store(false, Ordering::Relaxed);
            continue;
        }
        if playing.as_ref().is_some_and(|play| play.frames.is_empty()) {
            playing = None;
            continue;
        }
        if playing.as_ref().is_some_and(|play| play.paused) {
            continue;
        }
        let ready = match playing.as_ref() {
            Some(play) if Instant::now() < play.next_at => continue,
            // Open the picture channel once. Repeating that for every frame
            // of a one-frame hold blanks the panel.
            Some(play) => Some((!play.prepared, play.index)),
            None => None,
        };
        let Some((prepare, index)) = ready else {
            continue;
        };
        if panel.is_none() {
            panel = open_panel();
        }
        if panel.is_none() {
            if let Some(play) = playing.as_mut() {
                play.next_at = Instant::now() + Duration::from_millis(500);
            }
            continue;
        }
        if playing
            .as_ref()
            .is_some_and(|play| play.frames.get(index).is_none())
        {
            playing = None;
            continue;
        }
        let began = Instant::now();
        let result = {
            let panel = panel.as_mut().expect("panel is open");
            let play = playing.as_ref().expect("clip is loaded");
            let base = &play.frames[index];
            let composed = composer.frame(&stats, base, play.dashboard);
            let jpeg = composed.as_deref().map_or(base.as_slice(), Vec::as_slice);
            if prepare {
                panel.show_jpeg(jpeg, &|| cancel.load(Ordering::Relaxed))
            } else {
                panel.push_jpeg(jpeg, &|| cancel.load(Ordering::Relaxed))
            }
        };
        if result.is_err() {
            if panel.as_ref().is_some_and(Panel::io_failed) {
                panel = None;
            }
            // At sign-in the pump USB is often still enumerating. Keep the clip
            // and try again. Once frames are flowing, a few failures mean it left.
            // The screen may also have closed the live channel, so open it again.
            let give_up = match playing.as_mut() {
                Some(play) => {
                    play.prepared = false;
                    let limit = if play.delivered { 3 } else { 40 };
                    play.retries = play.retries.saturating_add(1);
                    if play.retries > limit {
                        true
                    } else {
                        play.next_at = Instant::now() + Duration::from_millis(500);
                        false
                    }
                }
                None => true,
            };
            if give_up {
                playing = None;
            }
            continue;
        }
        if let Some(play) = playing.as_mut() {
            play.delivered = true;
            play.prepared = true;
            play.retries = 0;
            let thousandths = speed.load(Ordering::Relaxed).clamp(250, 2000) as f32 / 1000.0;
            let gap = Duration::from_millis(play.frame_ms);
            let gap = if play.hold {
                gap
            } else {
                gap.div_f32(thousandths)
            };
            let spent = began.elapsed();
            // If a frame already used up its slot, wait out another full slot.
            // Sending the next one immediately pegs a core trying to catch up.
            play.next_at = Instant::now() + next_frame_delay(gap, spent);
            let count = play.frames.len();
            play.index = (play.index + 1) % count.max(1);
        }
    }
}

fn after_command(playing: &mut Option<Playing>, effect: AfterCommand) {
    let Some(play) = playing.as_mut() else {
        return;
    };
    match effect {
        AfterCommand::Pause => play.paused = true,
        AfterCommand::Wake => {
            play.paused = false;
            play.delivered = false;
            play.prepared = false;
            play.retries = 0;
            play.next_at = Instant::now();
        }
        AfterCommand::Rewake => {
            if !play.paused {
                play.delivered = false;
                play.prepared = false;
                play.retries = 0;
                play.next_at = Instant::now();
            }
        }
        AfterCommand::Keep => {}
    }
}

/// Sleep the screen on lock and wake it on unlock. Like the ASUS app, an
/// unlock does not wake a screen that was switched off.
fn lock_change(panel: &mut Panel, locked: bool) -> AfterCommand {
    if locked {
        return match panel.power("suspend") {
            Ok(()) => AfterCommand::Pause,
            Err(_) => AfterCommand::Keep,
        };
    }
    let switched_off = panel
        .state()
        .is_ok_and(|state| state["waterBlockScreen"].as_u64() == Some(0));
    if switched_off {
        return AfterCommand::Keep;
    }
    match panel.power("resume") {
        Ok(()) => AfterCommand::Wake,
        Err(_) => AfterCommand::Keep,
    }
}

/// Sleep until the next frame. A send that already ran long still waits a full
/// slot, so playback cannot busy-loop to catch up.
fn next_frame_delay(gap: Duration, spent: Duration) -> Duration {
    if gap > spent {
        gap - spent
    } else {
        gap
    }
}

fn dispatch(panel: &mut Option<Panel>, request: Request) {
    match request {
        Request::Refresh(reply) => {
            let _ = reply.send(snapshot(panel));
        }
        Request::Brightness(value, reply) => {
            let result = (|| {
                let panel = panel.as_mut().context("pump screen is not connected")?;
                panel.brightness(value)?;
                read_snapshot(panel)
            })();
            let _ = reply.send(result);
        }
        Request::Rotate(degrees, reply) => {
            let result = (|| {
                let panel = panel.as_mut().context("pump screen is not connected")?;
                panel.rotate(degrees)?;
                read_snapshot(panel)
            })();
            let _ = reply.send(result);
        }
        Request::Power(event, reply) => {
            let result = (|| {
                let panel = panel.as_mut().context("pump screen is not connected")?;
                panel.power(event)?;
                read_snapshot(panel)
            })();
            let _ = reply.send(result);
        }
        Request::Apply(setting, reply) => {
            let result = (|| {
                let panel = panel.as_mut().context("pump screen is not connected")?;
                panel.apply(&setting)?;
                read_snapshot(panel)
            })();
            let _ = reply.send(result);
        }
        Request::ShowStill(_, reply) => {
            let _ = reply.send(Err(anyhow::anyhow!(
                "the picture was not handed to the pump"
            )));
        }
        Request::Animation(reply) => {
            let _ = reply.send(Err(anyhow::anyhow!(
                "the request was not handed to the pump"
            )));
        }
        Request::Play(_, _) | Request::Stop | Request::Dashboard | Request::Locked(_) => {}
    }
}

fn snapshot(panel: &mut Option<Panel>) -> Result<Snapshot> {
    if panel.is_none() {
        *panel = Some(Panel::open().and_then(|mut panel| {
            panel.connect()?;
            Ok(panel)
        })?);
    }
    read_snapshot(panel.as_mut().context("pump screen is not connected")?)
}

fn read_snapshot(panel: &mut Panel) -> Result<Snapshot> {
    // `conn` is the handshake. Sending it again drops the live picture.
    let identity = panel.identity()?;
    let state = panel.state()?;
    let brightness = state["brightness"].as_u64().unwrap_or(0).min(100) as u8;
    let rotation = state["degree"].as_u64().unwrap_or(0) as u16;
    Ok(Snapshot {
        serial: identity["sn"].as_str().unwrap_or("unknown").to_string(),
        firmware: identity["version"]["firmware"]
            .as_str()
            .unwrap_or("unknown")
            .to_string(),
        brightness,
        rotation,
        free_space: state["space"].to_string(),
        screen_on: state["waterBlockScreen"].as_u64() != Some(0),
        display_in_sleep: state["displayInSleep"].as_u64() == Some(1),
    })
}

const ACCENT: egui::Color32 = egui::Color32::from_rgb(196, 54, 78);

pub fn run(start_in_tray: bool) -> eframe::Result<()> {
    if !settings::claim_single_instance() {
        return Ok(());
    }
    let (tray_tx, tray_rx) = std::sync::mpsc::sync_channel(64);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1000.0, 640.0])
        .with_min_inner_size([900.0, 540.0])
        .with_resizable(true)
        .with_decorations(true)
        .with_title("Pump screen");
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
            install_theme(&cc.egui_ctx);
            let _ = apply_window_material();
            let ctx = cc.egui_ctx.clone();
            settings::on_show_request({
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            });
            Ok(Box::new(App::new(start_in_tray, tray_tx, tray_rx, ctx)))
        }),
    )
}

enum TrayAction {
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

struct App {
    worker: Worker,
    snapshot: Option<Snapshot>,
    brightness: u8,
    status: String,
    busy: bool,
    chosen: Option<PathBuf>,
    preview: Option<egui::TextureHandle>,
    source_width: u32,
    source_height: u32,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    speed: f32,
    pending: Option<Receiver<JobResult>>,
    preview_rx: Option<Receiver<Result<media::Preview>>>,
    asus_running: bool,
    asus_checked: std::time::Instant,
    settings: Settings,
    tray: TrayMenu,
    quit: bool,
    hide_once: bool,
    /// Window is parked off-screen. It stays visible to Windows so paints still arrive.
    in_tray: bool,
    resume_when_ready: bool,
    tray_rx: std::sync::mpsc::Receiver<TrayAction>,
    material_applied: bool,
    /// The saved window from the tall layout is resized once.
    window_fitted: bool,
    /// Read by the lock watcher.
    follow_lock: Arc<AtomicBool>,
    /// The tab shown in the Picture card. What the pump shows is `settings.source`.
    picture_tab: Source,
    /// Shared with the stats panel thread and the screen worker.
    stats: Arc<stats_panel::Shared>,
}

struct TrayMenu {
    available: bool,
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

enum JobResult {
    Snapshot(Result<Snapshot>),
    /// A setting was sent. The string replaces the "working" status on success.
    Applied(Result<Snapshot>, String),
    Done(Result<()>),
    /// The worker is already playing. The UI only needs the count for its status line.
    Playing(usize),
    Failed(String),
}

impl App {
    fn new(
        start_in_tray: bool,
        tray_tx: std::sync::mpsc::SyncSender<TrayAction>,
        tray_rx: std::sync::mpsc::Receiver<TrayAction>,
        ctx: egui::Context,
    ) -> Self {
        let saved = Settings::load();
        let stats = Arc::new(stats_panel::Shared::default());
        stats.configure(panel_config(&saved, true));
        stats_panel::spawn(Arc::clone(&stats));
        let worker = Worker::start(Arc::clone(&stats));
        worker.set_speed(saved.speed);
        let pending = Some(spawn_snapshot(&worker));
        let mut chosen = None;
        let mut preview_rx = None;
        let mut resume_when_ready = saved.source == Source::Dashboard;
        if let Some(path) = saved.last_path.clone() {
            let path = PathBuf::from(path);
            if path.exists() {
                if saved.source == Source::Picture {
                    resume_when_ready = saved.resume_last;
                }
                let (tx, rx) = mpsc::channel();
                let preview_path = path.clone();
                thread::spawn(move || {
                    let _ = tx.send(media::preview_jpeg(&preview_path));
                });
                chosen = Some(path);
                preview_rx = Some(rx);
            }
        }
        if saved.start_with_windows {
            let _ = settings::set_run_at_logon(true, saved.start_in_tray);
        }
        let picture_tab = saved.source;
        let follow_lock = Arc::new(AtomicBool::new(saved.sleep_when_locked));
        spawn_lock_watcher(worker.tx.clone(), Arc::clone(&follow_lock));
        Self {
            worker,
            snapshot: None,
            brightness: 100,
            status: "Reading the pump…".into(),
            busy: true,
            chosen,
            preview: None,
            source_width: 0,
            source_height: 0,
            zoom: saved.zoom.clamp(1.0, 4.0),
            pan_x: saved.pan_x.clamp(-1.0, 1.0),
            pan_y: saved.pan_y.clamp(-1.0, 1.0),
            speed: saved.speed.clamp(0.25, 2.0),
            pending,
            preview_rx,
            asus_running: false,
            asus_checked: std::time::Instant::now()
                .checked_sub(Duration::from_secs(10))
                .unwrap_or_else(std::time::Instant::now),
            settings: saved,
            tray: TrayMenu::build(tray_tx, ctx),
            quit: false,
            hide_once: start_in_tray,
            in_tray: false,
            resume_when_ready,
            tray_rx,
            material_applied: false,
            window_fitted: false,
            follow_lock,
            picture_tab,
            stats,
        }
    }

    /// Tell the panel thread what to draw. Cheap enough to call every frame.
    fn sync_stats_panel(&self) {
        let config = panel_config(&self.settings, !self.in_tray);
        if self.stats.config() != config {
            self.stats.configure(config);
        }
    }

    fn persist(&mut self) {
        self.settings.zoom = self.zoom;
        self.settings.pan_x = self.pan_x;
        self.settings.pan_y = self.pan_y;
        self.settings.speed = self.speed;
        self.settings.last_path = self.chosen.as_ref().map(|path| path.display().to_string());
        if let Err(error) = self.settings.save() {
            self.status = format!("{error:#}");
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
                TrayAction::Stop => {
                    self.worker.stop_playback();
                    self.status = "Stopped. The last frame stays until you quit.".into();
                }
                TrayAction::Quit => {
                    self.quit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn maybe_resume(&mut self) {
        if !self.resume_when_ready {
            return;
        }
        self.resume_when_ready = false;
        if self.asus_running {
            self.status = "Quit the ROG STRIX LC app first. The last file was not sent.".into();
            return;
        }
        if self.settings.source == Source::Dashboard {
            self.show_dashboard();
        } else if self.chosen.as_ref().is_some_and(|path| path.exists()) {
            self.send_chosen();
        }
    }

    fn refresh_asus_flag(&mut self) {
        if self.asus_checked.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.asus_running = asus_app_running();
        self.asus_checked = std::time::Instant::now();
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.pending.as_ref() else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        self.pending = None;
        self.busy = false;
        match result {
            JobResult::Snapshot(Ok(shot)) => {
                // A switched-off screen reports brightness 0. Keep the slider where it was.
                if shot.screen_on {
                    self.brightness = shot.brightness;
                }
                self.snapshot = Some(shot);
                self.busy = false;
                if self.resume_when_ready {
                    self.maybe_resume();
                } else if self.status.starts_with("Reading the pump") {
                    self.status = if self.tray.available {
                        "Ready.".into()
                    } else {
                        "Ready. This desktop has no tray, so closing the window quits.".into()
                    };
                }
            }
            JobResult::Snapshot(Err(error)) => {
                self.busy = false;
                self.status = format!("{error:#}");
                self.maybe_resume();
            }
            JobResult::Applied(Ok(shot), done) => {
                if shot.screen_on {
                    self.brightness = shot.brightness;
                }
                self.snapshot = Some(shot);
                self.status = done;
            }
            JobResult::Applied(Err(error), _) => {
                self.status = format!("{error:#}");
            }
            JobResult::Done(Ok(())) => {
                self.status = "On the pump. It stays while this app is open.".into();
                self.pending = Some(spawn_snapshot(&self.worker));
                self.busy = true;
            }
            JobResult::Done(Err(error)) => {
                self.status = format!("{error:#}");
            }
            JobResult::Playing(count) => {
                let stay = if self.settings.close_to_tray {
                    "Closing the window leaves it running in the tray."
                } else {
                    "The window has to stay open."
                };
                self.status = format!("Playing {count} frames at {:.2}×. {stay}", self.speed);
            }
            JobResult::Failed(error) => {
                self.status = error;
            }
        }
        ctx.request_repaint();
    }

    fn poll_preview(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.preview_rx.as_ref() else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        self.preview_rx = None;
        match result {
            Ok(preview) => match image::load_from_memory(&preview.jpeg) {
                Ok(image) => {
                    let decoded = image.to_rgb8();
                    let color = egui::ColorImage::from_rgb(
                        [decoded.width() as usize, decoded.height() as usize],
                        decoded.as_raw(),
                    );
                    self.preview =
                        Some(ctx.load_texture("preview", color, egui::TextureOptions::LINEAR));
                    self.source_width = preview.width;
                    self.source_height = preview.height;
                    self.status = "Drag the square to crop, then show it on the pump.".into();
                }
                Err(error) => {
                    self.status = format!("could not show a preview: {error}");
                }
            },
            Err(error) => {
                self.status = format!("{error:#}");
            }
        }
        ctx.request_repaint();
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
            && self.settings.close_to_tray
            && self.tray.available
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if park_pump_window(ctx) {
                self.in_tray = true;
            }
        }
        // Windows puts the window back on the taskbar unless the tool style is
        // applied again. Linux hides the window once; repeating that wakes the loop.
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
        self.poll(ctx);
        self.poll_preview(ctx);
        self.refresh_asus_flag();
        self.sync_stats_panel();
        let waiting = self.pending.is_some() || self.preview_rx.is_some();
        if waiting {
            ctx.request_repaint_after(Duration::from_millis(200));
        } else if !self.in_tray {
            ctx.request_repaint_after(Duration::from_secs(3));
        }
        egui::CentralPanel::default()
            .frame(shell_frame())
            .show(ctx, |ui| {
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.header(ui);
                        if self.asus_running {
                            ui.add_space(6.0);
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 186, 110),
                                "Quit ROG STRIX LC & SLC IV Series. It is holding the pump USB connection.",
                            );
                        }
                        ui.add_space(8.0);
                        ui.columns_const(|[left, right]| {
                            self.picture_card(left, ctx);
                            self.controls_card(right);
                        });
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(&self.status).size(13.5));
                    });
            });
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

fn install_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = pump_visuals();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size.y = 30.0;
    ctx.set_style(style);
}

fn pump_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = egui::Color32::TRANSPARENT;
    visuals.panel_fill = egui::Color32::TRANSPARENT;
    visuals.faint_bg_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14);
    visuals.extreme_bg_color = egui::Color32::from_rgba_unmultiplied(18, 14, 20, 170);
    visuals.widgets.noninteractive.fg_stroke.color = egui::Color32::from_rgb(176, 170, 176);
    visuals.widgets.noninteractive.bg_stroke.color =
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 28);
    let mut idle = visuals.widgets.inactive;
    idle.bg_fill = egui::Color32::from_rgb(58, 50, 58);
    idle.weak_bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18);
    idle.bg_stroke = egui::Stroke::new(
        1.0_f32,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 36),
    );
    idle.corner_radius = egui::CornerRadius::same(8);
    idle.fg_stroke.color = egui::Color32::from_rgb(240, 236, 238);
    let mut hovered = idle;
    hovered.weak_bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30);
    hovered.bg_fill = egui::Color32::from_rgb(78, 66, 74);
    let mut active = hovered;
    active.bg_fill = ACCENT;
    active.weak_bg_fill = ACCENT;
    active.fg_stroke.color = egui::Color32::WHITE;
    visuals.widgets.inactive = idle;
    visuals.widgets.hovered = hovered;
    visuals.widgets.active = active;
    visuals.widgets.open = hovered;
    visuals.selection.bg_fill = egui::Color32::from_rgb(190, 52, 74);
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(230, 140, 152));
    visuals.hyperlink_color = egui::Color32::from_rgb(255, 176, 186);
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.menu_corner_radius = egui::CornerRadius::same(10);
    visuals
}

fn shell_frame() -> egui::Frame {
    #[cfg(windows)]
    let fill = egui::Color32::from_rgba_unmultiplied(16, 12, 18, 150);
    #[cfg(not(windows))]
    let fill = egui::Color32::from_rgb(16, 14, 20);
    egui::Frame::new()
        .fill(fill)
        .inner_margin(egui::Margin::same(16))
}

fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(egui::Color32::from_rgba_unmultiplied(34, 28, 36, 214))
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 32),
        ))
        .corner_radius(egui::CornerRadius::same(16))
        .inner_margin(egui::Margin::same(14))
        .outer_margin(egui::Margin::symmetric(4, 0))
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 18,
            spread: 0,
            color: egui::Color32::from_black_alpha(80),
        })
}

fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    card_frame().show(ui, |ui| {
        ui.label(egui::RichText::new(title).strong().size(15.0));
        ui.add_space(8.0);
        add(ui);
    });
}

fn dim(text: &str) -> egui::RichText {
    egui::RichText::new(text).color(egui::Color32::from_rgb(186, 180, 186))
}

/// The main action of a card.
fn accent_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .strong()
            .color(egui::Color32::WHITE),
    )
    .fill(ACCENT)
    .corner_radius(egui::CornerRadius::same(10))
    .min_size(egui::vec2(160.0, 34.0))
}

fn status_pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.22))
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.5)))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(color).size(12.0));
        });
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

impl App {
    fn fit_window(&mut self, ctx: &egui::Context) {
        if self.window_fitted || self.in_tray {
            return;
        }
        self.window_fitted = true;
        let narrow = ctx.input(|input| {
            input
                .viewport()
                .inner_rect
                .map(|rect| rect.width() < 900.0)
                .unwrap_or(true)
        });
        if narrow {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1000.0, 640.0)));
        }
    }

    fn header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 22.0), egui::Sense::hover());
            let mark = if self.snapshot.is_some() {
                ACCENT
            } else {
                egui::Color32::from_rgb(120, 116, 124)
            };
            ui.painter().circle_filled(rect.center(), 5.0, mark);
            ui.heading("Pump screen");
            ui.label(dim("ROG Strix LC IV"));
            let (text, color) = if self.snapshot.is_some() {
                ("Connected", egui::Color32::from_rgb(126, 196, 154))
            } else if self.busy {
                ("Connecting", egui::Color32::from_rgb(230, 186, 120))
            } else {
                ("Offline", egui::Color32::from_rgb(176, 170, 176))
            };
            status_pill(ui, text, color);
            if let Some(shot) = &self.snapshot {
                ui.label(dim(&format!(
                    "{}   {}   {} free",
                    shot.serial, shot.firmware, shot.free_space
                )));
            }
        });
    }

    fn picture_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        section(ui, "Picture", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (tab, label) in [
                    (Source::Picture, "Your photo or video"),
                    (Source::Dashboard, "Stats dashboard"),
                    (Source::Animation, "Pump animation"),
                ] {
                    let selected = self.picture_tab == tab;
                    // A dot marks what the pump is showing.
                    let label = if self.settings.source == tab {
                        format!("● {label}")
                    } else {
                        label.to_string()
                    };
                    let mut text = egui::RichText::new(label);
                    if selected {
                        text = text.strong().color(egui::Color32::WHITE);
                    }
                    let mut button =
                        egui::Button::new(text).corner_radius(egui::CornerRadius::same(10));
                    if selected {
                        button = button.fill(ACCENT);
                    }
                    if ui.add(button).clicked() {
                        self.picture_tab = tab;
                    }
                }
            });
            ui.add_space(6.0);
            if self.snapshot.as_ref().is_some_and(|shot| !shot.screen_on) {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 186, 110),
                        "The screen is switched off, so nothing shows.",
                    );
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Turn on"))
                        .clicked()
                    {
                        self.apply_setting(Setting::ScreenOn(true));
                    }
                });
                ui.add_space(4.0);
            }
            match self.picture_tab {
                Source::Picture => self.my_picture(ui, ctx),
                Source::Dashboard => self.dashboard_tab(ui),
                Source::Animation => self.animation_tab(ui),
            }
        });
    }

    fn dashboard_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(dim(
            "The readings picked under Stats panel, on a dark background. \
             This app draws it once a second, so it stops updating when the app quits.",
        ));
        ui.add_space(6.0);
        let showing = self.settings.source == Source::Dashboard;
        let ready = !self.settings.stats_items.is_empty() && !self.busy;
        if showing {
            ui.label(egui::RichText::new("On the pump now.").strong());
        } else if ui
            .add_enabled(ready, accent_button("Show dashboard on pump"))
            .clicked()
        {
            self.show_dashboard();
        }
        if self.settings.stats_items.is_empty() {
            ui.label(dim("Pick some readings under Stats panel first."));
        }
    }

    fn animation_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(dim(
            "The animation stored in the pump. Nothing is sent from this computer, \
             so it keeps playing after this app quits.",
        ));
        ui.add_space(6.0);
        if self.settings.source == Source::Animation {
            ui.label(egui::RichText::new("On the pump now.").strong());
        } else if ui
            .add_enabled(!self.busy, accent_button("Show pump animation"))
            .clicked()
        {
            self.show_animation();
        }
    }

    fn my_picture(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if ui.button("Choose file…").clicked() && !self.busy {
                self.choose_file(ctx);
            }
            if let Some(path) = &self.chosen {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("chosen file");
                ui.label(egui::RichText::new(name).strong())
                    .on_hover_text(path.display().to_string());
            }
        });
        if let Some(preview) = &self.preview {
            ui.label(dim("Drag the square to crop."));
            let crop_settled = crop_view(
                ui,
                preview,
                self.source_width,
                self.source_height,
                &mut self.zoom,
                &mut self.pan_x,
                &mut self.pan_y,
            );
            if crop_settled {
                self.persist();
            }
        }
        ui.horizontal(|ui| {
            let zoom_slider = ui.add(egui::Slider::new(&mut self.zoom, 1.0..=4.0).text("Zoom"));
            if zoom_slider.drag_stopped() {
                self.persist();
            }
            if ui.button("Reset").clicked() {
                self.zoom = 1.0;
                self.pan_x = 0.0;
                self.pan_y = 0.0;
                self.persist();
            }
        });
        ui.horizontal(|ui| {
            let speed_slider = ui.add(
                egui::Slider::new(&mut self.speed, 0.25..=2.0)
                    .fixed_decimals(2)
                    .text("Speed")
                    .suffix("×"),
            );
            if speed_slider.drag_stopped() {
                self.worker.set_speed(self.speed);
                self.persist();
            } else if speed_slider.changed() {
                self.worker.set_speed(self.speed);
            }
        });
        ui.label(dim(
            "1× matches the video. Crop applies the next time you press Show.",
        ));
        ui.horizontal(|ui| {
            let can_send = self.chosen.is_some() && !self.busy;
            if ui
                .add_enabled(can_send, accent_button("Show on pump"))
                .clicked()
            {
                self.send_chosen();
            }
            if ui.button("Stop video").clicked() {
                self.worker.stop_playback();
                self.status = "Stopped. The last frame stays until you quit.".into();
            }
        });
    }

    fn controls_card(&mut self, ui: &mut egui::Ui) {
        section(ui, "Screen", |ui| {
            ui.horizontal(|ui| {
                ui.label(dim("Brightness"));
                let screen_on = self.snapshot.as_ref().is_none_or(|shot| shot.screen_on);
                let slider = ui.add_enabled(
                    screen_on,
                    egui::Slider::new(&mut self.brightness, 0..=100).suffix("%"),
                );
                if slider.drag_stopped() && !self.busy {
                    self.apply_brightness();
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(dim("Rotation"));
                let current = self.snapshot.as_ref().map(|shot| shot.rotation);
                for degrees in [0u16, 90, 180, 270] {
                    let selected = current == Some(degrees);
                    let mut button = egui::Button::new(format!("{degrees}°"));
                    if selected {
                        button = button.fill(ACCENT);
                    }
                    if ui.add_enabled(!self.busy, button).clicked() {
                        self.apply_rotation(degrees);
                    }
                }
                if ui.button("Wake").clicked() && !self.busy {
                    self.apply_power("resume");
                }
                if ui.button("Sleep").clicked() && !self.busy {
                    self.apply_power("suspend");
                }
                if ui.button("Refresh").clicked() && !self.busy {
                    self.busy = true;
                    self.status = "Reading the pump…".into();
                    self.pending = Some(spawn_snapshot(&self.worker));
                }
            });
            if let Some(shot) = self.snapshot.clone() {
                let mut screen_on = shot.screen_on;
                let toggle = ui
                    .add_enabled(!self.busy, egui::Checkbox::new(&mut screen_on, "Screen on"))
                    .on_hover_text("Off keeps the panel dark until you turn it back on.");
                if toggle.changed() {
                    self.apply_setting(Setting::ScreenOn(screen_on));
                }
                let mut standby = shot.display_in_sleep;
                let toggle = ui
                    .add_enabled(
                        !self.busy,
                        egui::Checkbox::new(
                            &mut standby,
                            "Play the standby clip while the computer sleeps",
                        ),
                    )
                    .on_hover_text("Off turns the panel off while the computer sleeps.");
                if toggle.changed() {
                    self.apply_setting(Setting::DisplayInSleep(standby));
                }
            }
        });
        ui.add_space(8.0);
        section(ui, "Startup and tray", |ui| {
            if ui
                .checkbox(
                    &mut self.settings.close_to_tray,
                    "Keep running in the tray when the window closes",
                )
                .changed()
            {
                self.persist();
            }
            if ui
                .checkbox(
                    &mut self.settings.start_with_windows,
                    sign_in_startup_label(),
                )
                .changed()
            {
                if let Err(error) = settings::set_run_at_logon(
                    self.settings.start_with_windows,
                    self.settings.start_in_tray,
                ) {
                    self.settings.start_with_windows = !self.settings.start_with_windows;
                    self.status = format!("{error:#}");
                } else {
                    self.persist();
                }
            }
            if ui
                .checkbox(
                    &mut self.settings.start_in_tray,
                    "At sign-in, stay in the tray instead of opening this window",
                )
                .changed()
            {
                if self.settings.start_with_windows {
                    if let Err(error) =
                        settings::set_run_at_logon(true, self.settings.start_in_tray)
                    {
                        self.status = format!("{error:#}");
                    }
                }
                self.persist();
            }
            if ui
                .checkbox(
                    &mut self.settings.resume_last,
                    "Play the last photo or video when the app opens",
                )
                .changed()
            {
                self.persist();
            }
            if ui
                .checkbox(
                    &mut self.settings.sleep_when_locked,
                    "Sleep the pump screen while this computer is locked",
                )
                .changed()
            {
                self.follow_lock
                    .store(self.settings.sleep_when_locked, Ordering::Relaxed);
                self.persist();
            }
            ui.label(dim(
                "Click the tray icon to show this window. Right-click it to stop or quit.",
            ));
        });
        ui.add_space(8.0);
        self.stats_card(ui);
    }

    fn stats_card(&mut self, ui: &mut egui::Ui) {
        section(ui, "Stats panel", |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .checkbox(
                        &mut self.settings.stats_on_picture,
                        "Show on your photo or video",
                    )
                    .changed()
                {
                    self.persist();
                }
                if self.settings.stats_on_picture {
                    for (position, label) in [(Position::Top, "Top"), (Position::Bottom, "Bottom")]
                    {
                        let mut button = egui::Button::new(label);
                        if self.settings.stats_position == position {
                            button = button.fill(ACCENT);
                        }
                        if ui.add(button).clicked() {
                            self.settings.stats_position = position;
                            self.persist();
                        }
                    }
                }
            });
            ui.label(dim(&format!(
                "Up to {} readings, plus the time and date on a line of their own.",
                stats_panel::MAX_TILES.min(stats_panel::MAX_ON_PICTURE)
            )));
            ui.add_space(4.0);
            let readings = self.stats.readings();
            let mut changed = false;
            for group in [
                "CPU", "GPU", "Memory", "Disk", "Network", "Cooling", "Clock",
            ] {
                ui.horizontal_wrapped(|ui| {
                    ui.add_sized([62.0, 20.0], egui::Label::new(dim(group)));
                    for metric in stats::ALL
                        .into_iter()
                        .filter(|metric| metric.group() == group)
                    {
                        changed |= self.metric_checkbox(ui, metric, readings.as_ref());
                    }
                });
            }
            if changed {
                self.persist();
            }
            if let Some(readings) = &readings {
                let missing = |source| {
                    stats::ALL
                        .iter()
                        .any(|metric| metric.source() == source && !readings.has(*metric))
                };
                if cfg!(windows) && missing(stats::Source::HwInfo) {
                    ui.label(dim(
                        "CPU temperature and power, pump, coolant, and fan speeds come from \
                         HWiNFO. Run it with Shared Memory Support turned on in its settings.",
                    ));
                }
                if missing(stats::Source::Nvidia) {
                    ui.label(dim("GPU readings need an NVIDIA card."));
                }
            }
        });
    }

    /// One reading's checkbox, with its live value. Returns true if it changed.
    fn metric_checkbox(
        &mut self,
        ui: &mut egui::Ui,
        metric: Metric,
        readings: Option<&stats::Readings>,
    ) -> bool {
        let chosen = self.settings.stats_items.contains(&metric);
        let clock = matches!(metric, Metric::Time | Metric::Date);
        let available = clock || readings.is_none_or(|readings| readings.has(metric));
        let readings_chosen = self
            .settings
            .stats_items
            .iter()
            .filter(|metric| !matches!(metric, Metric::Time | Metric::Date))
            .count();
        let full = !clock && readings_chosen >= stats_panel::MAX_TILES;
        let label = match readings.filter(|_| available && !clock) {
            Some(readings) => format!("{} {}", metric.name(), readings.text(metric)),
            None => metric.name().to_string(),
        };
        let mut on = chosen;
        let response = ui.add_enabled(
            available && (chosen || !full),
            egui::Checkbox::new(&mut on, label),
        );
        let response = if !available {
            response.on_disabled_hover_text(match metric.source() {
                stats::Source::Nvidia => "Needs an NVIDIA card.",
                _ if cfg!(windows) => "Needs HWiNFO running with Shared Memory Support on.",
                _ => "This computer does not report it.",
            })
        } else if full && !chosen {
            response.on_disabled_hover_text("Take another reading off first.")
        } else {
            response
        };
        if !response.changed() {
            return false;
        }
        if on {
            self.settings.stats_items.push(metric);
            // Keep the window's order, so the panel matches the list.
            self.settings
                .stats_items
                .sort_by_key(|item| stats::ALL.iter().position(|metric| metric == item));
        } else {
            self.settings.stats_items.retain(|item| *item != metric);
        }
        true
    }

    /// Hand one request to the screen worker. The reply refreshes the controls.
    /// `working` shows while the pump is busy and `done` replaces it on success.
    fn run_on_worker(
        &mut self,
        working: String,
        done: String,
        request: impl FnOnce(Sender<Result<Snapshot>>) -> Request + Send + 'static,
    ) {
        if self.asus_running {
            self.status = "Quit the ROG STRIX LC app first.".into();
            return;
        }
        self.busy = true;
        self.status = working;
        let (tx, rx) = mpsc::channel();
        let worker_tx = self.worker.tx.clone();
        thread::spawn(move || {
            let (reply_tx, reply_rx) = mpsc::channel();
            let result = if worker_tx.send(request(reply_tx)).is_err() {
                Err(anyhow_worker())
            } else {
                reply_rx.recv().unwrap_or_else(|_| Err(anyhow_worker()))
            };
            let _ = tx.send(JobResult::Applied(result, done));
        });
        self.pending = Some(rx);
    }

    fn apply_brightness(&mut self) {
        let value = self.brightness;
        self.run_on_worker(
            "Setting brightness…".into(),
            format!("Brightness is {value}%."),
            move |reply| Request::Brightness(value, reply),
        );
    }

    fn apply_rotation(&mut self, degrees: u16) {
        self.run_on_worker(
            format!("Rotating to {degrees}°…"),
            format!("Rotated to {degrees}°."),
            move |reply| Request::Rotate(degrees, reply),
        );
    }

    fn apply_power(&mut self, event: &'static str) {
        let (working, done) = match event {
            "resume" => ("Waking the screen…", "The screen is awake."),
            _ => ("Putting the screen to sleep…", "The screen is asleep."),
        };
        self.run_on_worker(working.into(), done.into(), move |reply| {
            Request::Power(event, reply)
        });
    }

    fn show_animation(&mut self) {
        if self.asus_running {
            self.status = "Quit the ROG STRIX LC app first.".into();
            return;
        }
        self.settings.source = Source::Animation;
        self.persist();
        self.run_on_worker(
            "Stopping the picture…".into(),
            "The pump animation comes back about a second after the picture stops.".into(),
            Request::Animation,
        );
    }

    fn show_dashboard(&mut self) {
        if self.asus_running {
            self.status = "Quit the ROG STRIX LC app first.".into();
            return;
        }
        if self.worker.tx.send(Request::Dashboard).is_err() {
            self.status = format!("{:#}", anyhow_worker());
            return;
        }
        self.settings.source = Source::Dashboard;
        self.persist();
        self.status = "Showing the stats dashboard. It updates once a second.".into();
    }

    fn apply_setting(&mut self, setting: Setting) {
        let (working, done) = match &setting {
            Setting::ScreenOn(true) => ("Turning the screen on…", "The screen is on.".into()),
            Setting::ScreenOn(false) => ("Turning the screen off…", "The screen is off.".into()),
            Setting::DisplayInSleep(true) => (
                "Setting standby mode…",
                "The standby clip plays while the computer sleeps.".into(),
            ),
            Setting::DisplayInSleep(false) => (
                "Setting standby mode…",
                "The screen turns off while the computer sleeps.".into(),
            ),
        };
        self.run_on_worker(working.into(), done, move |reply| {
            Request::Apply(setting, reply)
        });
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
        self.source_width = 0;
        self.source_height = 0;
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.chosen = Some(path.clone());
        self.status = "Loading preview…".into();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(media::preview_jpeg(&path));
        });
        self.preview_rx = Some(rx);
        self.persist();
        ctx.request_repaint();
    }

    fn send_chosen(&mut self) {
        if self.asus_running {
            self.status = "Quit the ROG STRIX LC app first.".into();
            return;
        }
        let Some(path) = self.chosen.clone() else {
            return;
        };
        if self.settings.source != Source::Picture {
            self.settings.source = Source::Picture;
            self.persist();
        }
        self.worker.stop_playback();
        self.busy = true;
        let (tx, rx) = mpsc::channel();
        let crop = Crop {
            zoom: self.zoom,
            pan_x: self.pan_x,
            pan_y: self.pan_y,
        };
        self.worker.set_speed(self.speed);
        if media::is_video(&path) {
            self.status = "Preparing the video…".into();
            let player = self.worker.clone();
            thread::spawn(move || match video_jpegs(&path, &crop) {
                Ok(clip) => {
                    let count = clip.frames.len();
                    let frame_ms = clip.frame_ms;
                    // Play from this thread. The window may be parked off-screen, and a
                    // parked window must not have to paint before the pump gets frames.
                    if let Err(error) = player.play(clip.frames, frame_ms) {
                        let _ = tx.send(JobResult::Failed(format!("{error:#}")));
                        return;
                    }
                    let _ = tx.send(JobResult::Playing(count));
                }
                Err(error) => {
                    let _ = tx.send(JobResult::Failed(format!("{error:#}")));
                }
            });
        } else {
            self.status = "Sending the photo…".into();
            let worker_tx = self.worker.tx.clone();
            thread::spawn(move || {
                let jpeg = match photo_jpeg(&path, &crop) {
                    Ok(jpeg) => jpeg,
                    Err(error) => {
                        let _ = tx.send(JobResult::Done(Err(error)));
                        return;
                    }
                };
                let (reply_tx, reply_rx) = mpsc::channel();
                if worker_tx.send(Request::ShowStill(jpeg, reply_tx)).is_err() {
                    let _ = tx.send(JobResult::Done(Err(anyhow_worker())));
                    return;
                }
                match reply_rx.recv() {
                    Ok(result) => {
                        let _ = tx.send(JobResult::Done(result));
                    }
                    Err(_) => {
                        let _ = tx.send(JobResult::Done(Err(anyhow_worker())));
                    }
                }
            });
        }
        self.pending = Some(rx);
    }
}

fn spawn_snapshot(worker: &Worker) -> Receiver<JobResult> {
    let (tx, rx) = mpsc::channel();
    let worker_tx = worker.tx.clone();
    thread::spawn(move || {
        let (reply_tx, reply_rx) = mpsc::channel();
        if worker_tx.send(Request::Refresh(reply_tx)).is_err() {
            let _ = tx.send(JobResult::Snapshot(Err(anyhow_worker())));
            return;
        }
        match reply_rx.recv() {
            Ok(result) => {
                let _ = tx.send(JobResult::Snapshot(result));
            }
            Err(_) => {
                let _ = tx.send(JobResult::Snapshot(Err(anyhow_worker())));
            }
        }
    });
    rx
}

/// What the stats panel thread should draw for these settings.
fn panel_config(settings: &Settings, window_open: bool) -> stats_panel::Config {
    stats_panel::Config {
        items: settings.stats_items.clone(),
        on_picture: settings.stats_on_picture && settings.source == Source::Picture,
        position: settings.stats_position,
        dashboard: settings.source == Source::Dashboard,
        window_open,
    }
}

fn spawn_lock_watcher(worker_tx: Sender<Request>, follow_lock: Arc<AtomicBool>) {
    thread::spawn(move || {
        let mut last: Option<bool> = None;
        loop {
            thread::sleep(lock::POLL);
            if !follow_lock.load(Ordering::Relaxed) || asus_app_running() {
                last = None;
                continue;
            }
            let Some(locked) = lock::session_locked() else {
                continue;
            };
            // The first reading only matters if the computer is already locked.
            let changed = match last {
                Some(before) => before != locked,
                None => locked,
            };
            last = Some(locked);
            if changed && worker_tx.send(Request::Locked(locked)).is_err() {
                return;
            }
        }
    });
}

fn asus_app_running() -> bool {
    #[cfg(windows)]
    {
        return process_exists("ROG STRIX LC & SLC IV Series.exe");
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
fn process_exists(exe_name: &str) -> bool {
    #[repr(C)]
    struct ProcessEntry {
        size: u32,
        usage: u32,
        process_id: u32,
        default_heap_id: usize,
        module_id: u32,
        threads: u32,
        parent_process_id: u32,
        pri_class_base: i32,
        flags: u32,
        exe_file: [u16; 260],
    }
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut core::ffi::c_void;
        fn Process32FirstW(snapshot: *mut core::ffi::c_void, entry: *mut ProcessEntry) -> i32;
        fn Process32NextW(snapshot: *mut core::ffi::c_void, entry: *mut ProcessEntry) -> i32;
        fn CloseHandle(handle: *mut core::ffi::c_void) -> i32;
    }
    const SNAPPROCESS: u32 = 0x0000_0002;
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot == -1isize as *mut core::ffi::c_void {
            return false;
        }
        let mut entry = ProcessEntry {
            size: std::mem::size_of::<ProcessEntry>() as u32,
            usage: 0,
            process_id: 0,
            default_heap_id: 0,
            module_id: 0,
            threads: 0,
            parent_process_id: 0,
            pri_class_base: 0,
            flags: 0,
            exe_file: [0; 260],
        };
        let mut found = false;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let len = entry
                    .exe_file
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.exe_file.len());
                let name = String::from_utf16_lossy(&entry.exe_file[..len]);
                if name.eq_ignore_ascii_case(exe_name) {
                    found = true;
                    break;
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        found
    }
}

fn anyhow_worker() -> anyhow::Error {
    anyhow::anyhow!("the screen worker stopped")
}

impl TrayMenu {
    fn build(tx: std::sync::mpsc::SyncSender<TrayAction>, ctx: egui::Context) -> Self {
        #[cfg(windows)]
        {
            build_windows_tray(tx, ctx)
        }
        #[cfg(target_os = "linux")]
        {
            build_linux_tray(tx, ctx)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (tx, ctx);
            Self { available: false }
        }
    }
}

#[cfg(windows)]
fn build_windows_tray(tx: std::sync::mpsc::SyncSender<TrayAction>, ctx: egui::Context) -> TrayMenu {
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
    let click_ctx = ctx.clone();
    tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
        if tray_click_shows_window(&event) {
            reveal_pump_window();
            let _ = click_tx.try_send(TrayAction::Show);
        }
        click_ctx.request_repaint();
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
            ctx.request_repaint();
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
    ctx: egui::Context,
}

#[cfg(target_os = "linux")]
impl PumpTray {
    fn send(&mut self, action: TrayAction) {
        let _ = self.tx.try_send(action);
        self.ctx.request_repaint();
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
        "video-display".into()
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
fn build_linux_tray(tx: std::sync::mpsc::SyncSender<TrayAction>, ctx: egui::Context) -> TrayMenu {
    use ksni::blocking::TrayMethods;
    let tray = PumpTray { tx, ctx };
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

fn sign_in_startup_label() -> &'static str {
    #[cfg(windows)]
    {
        "Start when I sign in to Windows"
    }
    #[cfg(not(windows))]
    {
        "Start when I sign in"
    }
}

#[cfg(target_os = "linux")]
fn pump_tray_pixmap() -> ksni::Icon {
    let size = 32i32;
    let mut data = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let dx = x - 15;
            let dy = y - 15;
            if dx * dx + dy * dy <= 13 * 13 {
                let index = ((y * size + x) * 4) as usize;
                data[index] = 255;
                data[index + 1] = 214;
                data[index + 2] = 64;
                data[index + 3] = 38;
            }
        }
    }
    ksni::Icon {
        width: size,
        height: size,
        data,
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
    let title: Vec<u16> = "Pump screen"
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
            // DWMSBT_TRANSIENTWINDOW is the acrylic blur. Mica is 2, mica alt is 4.
            let backdrop: i32 = 3;
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
            let margins = DwmMargins {
                cx_left_width: -1,
                cx_right_width: -1,
                cy_top_height: -1,
                cy_bottom_height: -1,
            };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        } else {
            enable_acrylic(hwnd);
        }
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
            color: 0xCC1A1218,
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
    fn the_frame_that_just_played_is_the_one_before_the_cursor() {
        assert_eq!(last_sent_index(0, 0, true), None);
        assert_eq!(last_sent_index(0, 4, false), None);
        assert_eq!(last_sent_index(1, 4, true), Some(0));
        assert_eq!(last_sent_index(0, 4, true), Some(3));
        assert_eq!(last_sent_index(0, 1, true), Some(0));
    }

    #[test]
    fn a_slow_frame_still_waits_a_full_slot() {
        let gap = Duration::from_millis(125);
        assert_eq!(
            next_frame_delay(gap, Duration::from_millis(40)),
            Duration::from_millis(85)
        );
        assert_eq!(next_frame_delay(gap, Duration::from_millis(125)), gap);
        assert_eq!(next_frame_delay(gap, Duration::from_millis(400)), gap);
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
