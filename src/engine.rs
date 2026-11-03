//! What the app does, without a window: the screen worker that talks to the
//! pump, the jobs that wait on it, and the state both front ends show. The
//! egui window on Windows and the GTK window on Linux draw from `Controller`.

use crate::firmware;
use crate::lock;
use crate::media::{self, photo_jpeg, video_jpegs, Crop};
// `Panel` is the USB session with the screen; `stats_panel` draws the widgets.
use crate::panel::{self as stats_panel, Composer};
use crate::session::{Panel, Setting};
use crate::settings::{self, Settings, Source};
use crate::tray::{TrayAction, TrayMenu, Wake};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// The pump state, the chosen picture, and the settings. Every action the
/// window offers is a method here, so both windows behave the same.
pub struct Controller {
    pub worker: Worker,
    pub snapshot: Option<Snapshot>,
    /// The brightness slider. It follows the pump unless the screen is off.
    pub brightness: u8,
    pub status: String,
    pub busy: bool,
    pub chosen: Option<PathBuf>,
    pub source_width: u32,
    pub source_height: u32,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
    pub speed: f32,
    pending: Option<Receiver<JobResult>>,
    preview_rx: Option<Receiver<Result<media::Preview>>>,
    pub asus_running: bool,
    asus_checked: Instant,
    pub settings: Settings,
    pub tray: TrayMenu,
    pub tray_rx: Receiver<TrayAction>,
    resume_when_ready: bool,
    /// Read by the lock watcher.
    follow_lock: Arc<AtomicBool>,
    /// A checked firmware package waiting for the user to confirm.
    pub firmware_offer: Option<firmware::Package>,
    /// Shared with the stats panel thread and the screen worker.
    pub stats: Arc<stats_panel::Shared>,
    /// The window is on screen. The panel thread reads readings faster then.
    pub window_open: bool,
}

impl Controller {
    /// Loads the settings, starts the worker, and reads the pump. `wake` is
    /// called from other threads when the tray wants the window's attention.
    pub fn new(wake: Wake) -> Self {
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
                preview_rx = Some(spawn_preview(path.clone()));
                chosen = Some(path);
            }
        }
        if saved.start_with_windows {
            let _ = settings::set_run_at_logon(true, saved.start_in_tray);
        }
        let follow_lock = Arc::new(AtomicBool::new(saved.sleep_when_locked));
        spawn_lock_watcher(worker.tx.clone(), Arc::clone(&follow_lock));
        let (tray_tx, tray_rx) = mpsc::sync_channel(64);
        Self {
            worker,
            snapshot: None,
            brightness: 100,
            status: "Reading the pump…".into(),
            busy: true,
            chosen,
            source_width: 0,
            source_height: 0,
            zoom: saved.zoom.clamp(1.0, 4.0),
            pan_x: saved.pan_x.clamp(-1.0, 1.0),
            pan_y: saved.pan_y.clamp(-1.0, 1.0),
            speed: saved.speed.clamp(0.25, 2.0),
            pending,
            preview_rx,
            asus_running: false,
            asus_checked: Instant::now()
                .checked_sub(Duration::from_secs(10))
                .unwrap_or_else(Instant::now),
            settings: saved,
            tray: TrayMenu::build(tray_tx, wake),
            tray_rx,
            resume_when_ready,
            follow_lock,
            firmware_offer: demo_firmware(),
            stats,
            window_open: true,
        }
    }

    #[cfg_attr(windows, allow(dead_code))]
    pub fn crop(&self) -> Crop {
        Crop {
            zoom: self.zoom,
            pan_x: self.pan_x,
            pan_y: self.pan_y,
        }
    }

    /// The preview of the chosen file is still being made.
    pub fn loading_preview(&self) -> bool {
        self.preview_rx.is_some()
    }

    #[cfg_attr(windows, allow(dead_code))]
    pub fn is_video(&self) -> bool {
        self.chosen.as_deref().is_some_and(media::is_video)
    }

    #[cfg_attr(windows, allow(dead_code))]
    pub fn chosen_name(&self) -> Option<String> {
        self.chosen
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
    }

    /// Closing the window keeps the app running in the tray.
    pub fn closes_to_tray(&self) -> bool {
        self.settings.close_to_tray && self.tray.available
    }

    /// Tell the panel thread what to draw. Cheap enough to call every frame.
    pub fn sync_stats_panel(&self) {
        let config = panel_config(&self.settings, self.window_open);
        if self.stats.config() != config {
            self.stats.configure(config);
        }
    }

    pub fn persist(&mut self) {
        self.settings.zoom = self.zoom;
        self.settings.pan_x = self.pan_x;
        self.settings.pan_y = self.pan_y;
        self.settings.speed = self.speed;
        self.settings.last_path = self.chosen.as_ref().map(|path| path.display().to_string());
        if let Err(error) = self.settings.save() {
            self.status = format!("{error:#}");
        }
    }

    /// The periodic work: finished jobs, the ASUS app check, and what the
    /// panel thread draws. Returns true when a job finished.
    pub fn tick(&mut self) -> bool {
        let finished = self.poll();
        self.refresh_asus_flag();
        self.sync_stats_panel();
        finished
    }

    /// Something is still running that the window should check on soon.
    #[cfg(windows)]
    pub fn waiting(&self) -> bool {
        self.pending.is_some() || self.preview_rx.is_some()
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

    pub fn refresh_asus_flag(&mut self) {
        if self.asus_checked.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.asus_running = asus_app_running();
        self.asus_checked = Instant::now();
    }

    /// Takes a finished job. Returns true when there was one.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = self.pending.as_ref() else {
            return false;
        };
        let Ok(result) = rx.try_recv() else {
            return false;
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
            JobResult::Firmware(result) => {
                self.status = match result {
                    Ok(()) => "Firmware sent. The pump is installing it and restarting. \
                               This takes about half a minute."
                        .into(),
                    Err(error) => format!("Firmware was not installed: {error:#}"),
                };
                // Read the pump again once it is back, then show what was on it.
                self.snapshot = None;
                self.busy = true;
                self.resume_when_ready = true;
                self.pending = Some(spawn_snapshot_after_restart(&self.worker));
            }
        }
        true
    }

    /// Takes the preview of the chosen file once it is made. The window turns
    /// it into whatever it draws pictures with.
    pub fn take_preview(&mut self) -> Option<media::Preview> {
        let rx = self.preview_rx.as_ref()?;
        let result = rx.try_recv().ok()?;
        self.preview_rx = None;
        match result {
            Ok(preview) => {
                self.source_width = preview.width;
                self.source_height = preview.height;
                self.status = "Drag the square to crop, then show it on the pump.".into();
                Some(preview)
            }
            Err(error) => {
                self.status = format!("{error:#}");
                None
            }
        }
    }

    /// Check a firmware package the user picked. A good one waits in
    /// `firmware_offer` for the user to confirm.
    pub fn offer_firmware(&mut self, path: &Path) {
        match firmware::read(path) {
            Ok(package) => {
                let installed = self.snapshot.as_ref().map(|shot| shot.firmware.clone());
                if installed
                    .as_deref()
                    .is_some_and(|installed| firmware::is_downgrade(&package.version, installed))
                {
                    self.status = format!(
                        "{} is older than the pump's {}. The pump does not go back to older firmware.",
                        package.version,
                        installed.unwrap_or_default()
                    );
                    return;
                }
                self.firmware_offer = Some(package);
            }
            Err(error) => self.status = format!("{error:#}"),
        }
    }

    pub fn install_firmware(&mut self) {
        let Some(package) = self.firmware_offer.take() else {
            return;
        };
        if self.asus_running {
            self.status = "Quit the ROG STRIX LC app first.".into();
            return;
        }
        self.busy = true;
        self.status = format!(
            "Sending firmware {}. Keep the computer awake…",
            package.version
        );
        let (tx, rx) = mpsc::channel();
        let worker_tx = self.worker.tx.clone();
        thread::spawn(move || {
            let (reply_tx, reply_rx) = mpsc::channel();
            let result = if worker_tx
                .send(Request::Firmware(package.bytes, reply_tx))
                .is_err()
            {
                Err(anyhow_worker())
            } else {
                reply_rx.recv().unwrap_or_else(|_| Err(anyhow_worker()))
            };
            let _ = tx.send(JobResult::Firmware(result));
        });
        self.pending = Some(rx);
    }

    pub fn refresh(&mut self) {
        self.busy = true;
        self.status = "Reading the pump…".into();
        self.pending = Some(spawn_snapshot(&self.worker));
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

    pub fn apply_brightness(&mut self) {
        let value = self.brightness;
        self.run_on_worker(
            "Setting brightness…".into(),
            format!("Brightness is {value}%."),
            move |reply| Request::Brightness(value, reply),
        );
    }

    pub fn apply_rotation(&mut self, degrees: u16) {
        self.run_on_worker(
            format!("Rotating to {degrees}°…"),
            format!("Rotated to {degrees}°."),
            move |reply| Request::Rotate(degrees, reply),
        );
    }

    pub fn apply_power(&mut self, event: &'static str) {
        let (working, done) = match event {
            "resume" => ("Waking the screen…", "The screen is awake."),
            _ => ("Putting the screen to sleep…", "The screen is asleep."),
        };
        self.run_on_worker(working.into(), done.into(), move |reply| {
            Request::Power(event, reply)
        });
    }

    pub fn show_animation(&mut self) {
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

    pub fn show_dashboard(&mut self) {
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

    pub fn apply_setting(&mut self, setting: Setting) {
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

    pub fn send_chosen(&mut self) {
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

    pub fn cancel_firmware(&mut self) {
        self.firmware_offer = None;
        self.status = "Firmware update cancelled.".into();
    }

    /// Use this photo or video from now on, starting with a fresh crop.
    pub fn choose(&mut self, path: PathBuf) {
        self.source_width = 0;
        self.source_height = 0;
        self.reset_crop();
        self.chosen = Some(path.clone());
        self.status = "Loading preview…".into();
        self.preview_rx = Some(spawn_preview(path));
        self.persist();
    }

    pub fn reset_crop(&mut self) {
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed.clamp(0.25, 2.0);
        self.worker.set_speed(self.speed);
    }

    pub fn stop_video(&mut self) {
        self.worker.stop_playback();
        self.status = "Stopped. The last frame stays until you quit.".into();
    }

    pub fn set_sleep_when_locked(&mut self, follow: bool) {
        self.settings.sleep_when_locked = follow;
        self.follow_lock.store(follow, Ordering::Relaxed);
        self.persist();
    }

    pub fn set_start_at_sign_in(&mut self, start: bool) {
        match settings::set_run_at_logon(start, self.settings.start_in_tray) {
            Ok(()) => {
                self.settings.start_with_windows = start;
                self.persist();
            }
            Err(error) => self.status = format!("{error:#}"),
        }
    }

    pub fn set_start_in_tray(&mut self, in_tray: bool) {
        self.settings.start_in_tray = in_tray;
        if self.settings.start_with_windows {
            if let Err(error) = settings::set_run_at_logon(true, in_tray) {
                self.status = format!("{error:#}");
            }
        }
        self.persist();
    }
}

fn spawn_preview(path: PathBuf) -> Receiver<Result<media::Preview>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(media::preview_jpeg(&path));
    });
    rx
}

/// A demo opened with "firmware" shows the dialog for the downloaded package.
fn demo_firmware() -> Option<firmware::Package> {
    if !(demo() && std::env::var("STRIX_LC_SCREEN_DEMO").as_deref() == Ok("firmware")) {
        return None;
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let path = PathBuf::from(home)
        .join("Downloads")
        .join("FW_ROG_Strix_LC_IV_360_ARGB_LCD_V1.0.6.zip");
    firmware::read(&path).ok()
}

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
    /// Install a checked firmware package. The pump restarts afterwards.
    Firmware(Vec<u8>, Sender<Result<()>>),
}

#[derive(Clone)]
pub struct Snapshot {
    pub serial: String,
    pub firmware: String,
    pub brightness: u8,
    pub rotation: u16,
    pub free_space: String,
    pub screen_on: bool,
    pub display_in_sleep: bool,
}

#[derive(Clone)]
pub struct Worker {
    tx: Sender<Request>,
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
    if demo() {
        return None;
    }
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
                    let effect = match panel.as_mut() {
                        Some(panel) => lock_change(panel, locked),
                        // Right after the computer wakes the pump may still be
                        // enumerating. Playback opens it once it is back.
                        None if !locked => AfterCommand::Wake,
                        None => AfterCommand::Keep,
                    };
                    after_command(&mut playing, effect);
                }
                Request::Firmware(package, reply) => {
                    // No frames may go out during the transfer: the pump writes
                    // every report it gets into the file.
                    playing = None;
                    cancel.store(false, Ordering::Relaxed);
                    if panel.is_none() {
                        panel = open_panel();
                    }
                    let result = (|| {
                        let panel = panel.as_mut().context("pump screen is not connected")?;
                        let state = panel.state()?;
                        if state["bootFinish"].as_u64() != Some(1) {
                            anyhow::bail!("the pump is still starting up. Try again in a minute");
                        }
                        let free = state["space"].as_u64().unwrap_or(0) * 1024;
                        if free < package.len() as u64 + 4 * 1024 * 1024 {
                            anyhow::bail!(
                                "the pump does not have enough free space for this package"
                            );
                        }
                        panel.install_firmware(&package)
                    })();
                    // The pump restarts after a good transfer, and a failed one
                    // may have left the handle unusable. Open a new one later.
                    panel = None;
                    let _ = reply.send(result);
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
            // At sign-in and after the computer wakes, the pump USB can take a
            // while to come back, and the screen may have closed the live
            // channel. Keep the clip and try again, slower once the quick
            // retries run out. Giving up here left the screen on its own clip.
            if let Some(play) = playing.as_mut() {
                play.prepared = false;
                let limit = if play.delivered { 3 } else { 40 };
                play.retries = play.retries.saturating_add(1);
                let delay = if play.retries <= limit {
                    Duration::from_millis(500)
                } else {
                    if play.retries == limit + 1 {
                        // The handle may be stale without reporting an error.
                        panel = None;
                    } else if panel.as_mut().is_some_and(|panel| {
                        panel
                            .state()
                            .is_ok_and(|state| state["waterBlockScreen"].as_u64() == Some(0))
                    }) {
                        // Switched off. Turning it on again wakes playback.
                        play.paused = true;
                    }
                    Duration::from_secs(5)
                };
                play.next_at = Instant::now() + delay;
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
    // Wake playback even if this fails. After the computer sleeps the handle is
    // often dead, and playback opens a new one and sends resume itself.
    let _ = panel.power("resume");
    AfterCommand::Wake
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
        Request::Firmware(_, reply) => {
            let _ = reply.send(Err(anyhow::anyhow!(
                "the firmware was not handed to the pump"
            )));
        }
        Request::Play(_, _) | Request::Stop | Request::Dashboard | Request::Locked(_) => {}
    }
}

fn snapshot(panel: &mut Option<Panel>) -> Result<Snapshot> {
    if demo() {
        anyhow::bail!("demo mode does not open the pump");
    }
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

/// Set in debug builds by STRIX_LC_SCREEN_DEMO, to look at the window next to a
/// running copy: no USB, no single-instance check, and its own window title.
pub fn demo() -> bool {
    cfg!(debug_assertions) && std::env::var_os("STRIX_LC_SCREEN_DEMO").is_some()
}

enum JobResult {
    Snapshot(Result<Snapshot>),
    /// A setting was sent. The string replaces the "working" status on success.
    Applied(Result<Snapshot>, String),
    Done(Result<()>),
    /// The worker is already playing. The UI only needs the count for its status line.
    Playing(usize),
    Firmware(Result<()>),
    Failed(String),
}

/// Wait for the pump to restart, then read it. It is gone for about ten
/// seconds and takes a while longer to finish booting.
fn spawn_snapshot_after_restart(worker: &Worker) -> Receiver<JobResult> {
    let (tx, rx) = mpsc::channel();
    let worker_tx = worker.tx.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(15));
        let mut last = Err(anyhow_worker());
        for _ in 0..20 {
            let (reply_tx, reply_rx) = mpsc::channel();
            if worker_tx.send(Request::Refresh(reply_tx)).is_err() {
                break;
            }
            last = reply_rx.recv().unwrap_or_else(|_| Err(anyhow_worker()));
            if last.is_ok() {
                break;
            }
            thread::sleep(Duration::from_secs(3));
        }
        let _ = tx.send(JobResult::Snapshot(last));
    });
    rx
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
    let picture = (settings.stats_on_picture && settings.source == Source::Picture).then(|| {
        settings
            .picture_layout
            .as_ref()
            .map(|layout| layout.widgets.clone())
            .unwrap_or_default()
    });
    let dashboard = (settings.source == Source::Dashboard)
        .then(|| settings.dashboard_layout.clone().unwrap_or_default());
    stats_panel::Config {
        picture,
        dashboard,
        window_open,
        fahrenheit: settings.fahrenheit,
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

pub fn sign_in_startup_label() -> &'static str {
    #[cfg(windows)]
    {
        "Start when I sign in to Windows"
    }
    #[cfg(not(windows))]
    {
        "Start when I sign in"
    }
}

const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

pub fn icon_rgba() -> image::RgbaImage {
    image::load_from_memory_with_format(ICON_PNG, image::ImageFormat::Png)
        .expect("assets/icon.png is a PNG")
        .into_rgba8()
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
