//! The window: a sidebar with the pump's status and the pages, in the same
//! style as Blurman.

use super::editor::{Canvas, Edit};
use super::{crop_view, App, Page, Setting, Source};
use crate::layout::{self, Layout, Widget};
use crate::media::{self, Crop};
use crate::settings;
use crate::theme::{self, ACCENT, MUTED, OK, TEXT, WARN};
use eframe::egui;
use egui::{Align, Color32, CornerRadius, Layout as Flow, Rect, RichText, Sense, Stroke, Ui};
use std::sync::atomic::Ordering;

const SIDEBAR_WIDTH: f32 = 236.0;
/// The column beside the widget editor's screen.
const SIDE_COLUMN: f32 = 350.0;
const UNDO_LIMIT: usize = 60;

impl Page {
    const ALL: [Page; 4] = [Page::Show, Page::Widgets, Page::Screen, Page::Settings];

    fn label(self) -> &'static str {
        match self {
            Page::Show => "Show",
            Page::Widgets => "Widgets",
            Page::Screen => "Screen",
            Page::Settings => "Settings",
        }
    }

    fn icon(self) -> theme::NavIcon {
        match self {
            Page::Show => theme::NavIcon::Show,
            Page::Widgets => theme::NavIcon::Widgets,
            Page::Screen => theme::NavIcon::Screen,
            Page::Settings => theme::NavIcon::Settings,
        }
    }
}

/// The part of the preview picture that the crop square covers, as texture coordinates.
fn cropped<'a>(
    preview: Option<&'a egui::TextureHandle>,
    width: u32,
    height: u32,
    crop: &Crop,
) -> Option<(&'a egui::TextureHandle, Rect)> {
    let texture = preview?;
    let (width, height) = (width.max(1), height.max(1));
    let (side, x, y) = crop.square(width, height);
    let (w, h) = (width as f32, height as f32);
    Some((
        texture,
        Rect::from_min_max(
            egui::pos2(x as f32 / w, y as f32 / h),
            egui::pos2((x + side) as f32 / w, (y + side) as f32 / h),
        ),
    ))
}

impl App {
    pub(super) fn draw(&mut self, ctx: &egui::Context) {
        let glass = cfg!(windows);
        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(SIDEBAR_WIDTH)
            .show_separator_line(false)
            .frame(theme::sidebar(glass))
            .show(ctx, |ui| self.sidebar(ui));
        egui::TopBottomPanel::bottom("status")
            .show_separator_line(false)
            .frame(theme::footer(glass))
            .show(ctx, |ui| self.footer(ui));
        egui::CentralPanel::default()
            .frame(theme::page(glass))
            .show(ctx, |ui| {
                let room = ui.available_size();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.banners(ui);
                        match self.page {
                            Page::Show => self.show_page(ui, ctx),
                            Page::Widgets => self.widgets_page(ui, room),
                            Page::Screen => self.screen_page(ui),
                            Page::Settings => self.settings_page(ui),
                        }
                    });
            });
        self.firmware_dialog(ctx);
    }

    /// Confirm a checked firmware package before it is sent.
    fn firmware_dialog(&mut self, ctx: &egui::Context) {
        let Some(package) = &self.firmware_offer else {
            return;
        };
        let installed = self
            .snapshot
            .as_ref()
            .map_or("unknown".to_string(), |shot| shot.firmware.clone());
        let same = crate::firmware::parse_version(&installed)
            == crate::firmware::parse_version(&package.version);
        let title = format!("Install firmware {}?", package.version);
        let mut install = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("firmware-dialog"))
            .frame(
                egui::Frame::new()
                    .fill(theme::POPUP)
                    .stroke(Stroke::new(1.0_f32, Color32::from_white_alpha(40)))
                    .corner_radius(CornerRadius::same(14))
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(ctx, |ui| {
                ui.set_width(440.0);
                ui.label(RichText::new(title).size(18.0).color(TEXT));
                ui.add_space(6.0);
                let first = if same {
                    format!("The pump already has {installed}. This installs it again.")
                } else {
                    format!("The pump's firmware is {installed} now.")
                };
                ui.add(egui::Label::new(theme::muted(first)).wrap());
                ui.add(
                    egui::Label::new(theme::muted(
                        "The screen goes dark while the pump installs it and restarts. That \
                         takes about half a minute, and this app reconnects on its own.",
                    ))
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(theme::muted(
                        "Keep the computer awake and the cooler plugged in until the screen is \
                         back. Cooling is not affected: the pump runs from the motherboard.",
                    ))
                    .wrap(),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.add(theme::primary_button("Install")).clicked() {
                        install = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if install {
            self.install_firmware();
        } else if cancel || modal.should_close() {
            self.firmware_offer = None;
            self.status = "Firmware update cancelled.".into();
        }
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 26.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, ACCENT);
            ui.label(RichText::new("Pump screen").size(20.0).color(TEXT));
        });
        ui.add_space(12.0);
        self.status_card(ui);
        ui.add_space(14.0);
        ui.spacing_mut().item_spacing.y = 2.0;
        for page in Page::ALL {
            if theme::nav_item(ui, page.icon(), page.label(), self.page == page).clicked() {
                self.page = page;
            }
        }
        ui.with_layout(Flow::bottom_up(Align::LEFT), |ui| {
            ui.add_space(4.0);
            ui.label(theme::muted(format!("Version {}", env!("CARGO_PKG_VERSION"))).small());
            let note = if self.settings.close_to_tray && self.tray.available {
                "Closing keeps it running in the tray."
            } else {
                "Closing hands the screen back to the pump."
            };
            ui.add(egui::Label::new(theme::muted(note).small()).wrap());
        });
    }

    /// Whether the pump is reachable, and what it shows.
    fn status_card(&mut self, ui: &mut Ui) {
        let screen_off = self.snapshot.as_ref().is_some_and(|shot| !shot.screen_on);
        let (title, color) = if self.asus_running {
            ("ASUS app is open", WARN)
        } else if screen_off {
            ("Screen is off", WARN)
        } else if self.snapshot.is_some() {
            ("Connected", OK)
        } else if self.busy {
            ("Connecting…", WARN)
        } else {
            ("Not connected", MUTED)
        };
        let showing = match self.settings.source {
            Source::Picture => self
                .chosen
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Nothing chosen yet".into()),
            Source::Dashboard => "Stats dashboard".into(),
            Source::Animation => "The pump's own animation".into(),
        };
        egui::Frame::new()
            .fill(color.gamma_multiply(0.12))
            .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.4)))
            .corner_radius(12)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 18.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, color);
                    ui.label(RichText::new(title).color(TEXT));
                });
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(
                    egui::Label::new(theme::muted(format!("Showing: {showing}")).small())
                        .truncate(),
                );
                if let Some(shot) = &self.snapshot {
                    ui.label(theme::muted(format!("Firmware {}", shot.firmware)).small());
                }
                if screen_off {
                    ui.add_space(6.0);
                    let button = theme::primary_button("Turn the screen on")
                        .min_size(egui::vec2(ui.available_width(), 30.0));
                    if ui.add_enabled(!self.busy, button).clicked() {
                        self.apply_setting(Setting::ScreenOn(true));
                    }
                }
            });
    }

    fn footer(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let color = if self.busy { WARN } else { MUTED };
            let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 16.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.0, color);
            ui.add(egui::Label::new(theme::muted(&self.status)).truncate())
                .on_hover_text(&self.status);
        });
    }

    fn banners(&mut self, ui: &mut Ui) {
        if self.asus_running {
            theme::banner(ui, WARN, |ui| {
                ui.label(
                    RichText::new(
                        "Quit ROG STRIX LC & SLC IV Series. It is holding the pump's USB connection.",
                    )
                    .color(TEXT),
                );
            });
        }
    }

    fn show_page(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        theme::page_header(
            ui,
            "Show",
            "Pick what the pump screen shows. It keeps playing from the tray.",
            |_| {},
        );
        ui.columns_const(|[picture, dashboard, animation]| {
            self.source_card(
                picture,
                Source::Picture,
                "Your photo or video",
                "Any picture, GIF, or short clip, cropped to the screen.",
            );
            self.source_card(
                dashboard,
                Source::Dashboard,
                "Stats dashboard",
                "Live readings you arrange on the Widgets page.",
            );
            self.source_card(
                animation,
                Source::Animation,
                "Pump animation",
                "The clip stored in the pump. Keeps playing after this app quits.",
            );
        });
        ui.add_space(14.0);
        match self.picture_tab {
            Source::Picture => self.picture_section(ui, ctx),
            Source::Dashboard => self.dashboard_section(ui),
            Source::Animation => self.animation_section(ui),
        }
    }

    /// One of the three things the pump can show. Clicking it opens its controls below.
    fn source_card(&mut self, ui: &mut Ui, source: Source, title: &str, detail: &str) {
        let selected = self.picture_tab == source;
        let live = self.settings.source == source;
        let frame = egui::Frame::new()
            .fill(if selected {
                theme::ROW_SELECTED
            } else {
                Color32::from_rgba_unmultiplied(34, 28, 36, 214)
            })
            .stroke(Stroke::new(
                if selected { 1.5_f32 } else { 1.0 },
                if selected {
                    ACCENT
                } else {
                    Color32::from_white_alpha(32)
                },
            ))
            .corner_radius(CornerRadius::same(14))
            .inner_margin(egui::Margin::same(14));
        let shown = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(86.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(16.0).color(TEXT));
                if live {
                    ui.with_layout(Flow::right_to_left(Align::Center), |ui| {
                        theme::pill(ui, "On the pump", OK);
                    });
                }
            });
            ui.add(egui::Label::new(theme::muted(detail).small()).wrap());
        });
        let response = ui
            .interact(
                shown.response.rect,
                ui.id().with(("source", source as u8)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.hovered() && !selected {
            ui.painter().rect_stroke(
                shown.response.rect,
                CornerRadius::same(14),
                Stroke::new(1.0_f32, Color32::from_white_alpha(70)),
                egui::StrokeKind::Inside,
            );
        }
        if response.clicked() {
            self.picture_tab = source;
        }
    }

    fn picture_section(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        theme::card(ui, |ui| {
            ui.horizontal_top(|ui| {
                let left = 300.0_f32.min(ui.available_width() * 0.45);
                ui.allocate_ui_with_layout(
                    egui::vec2(left, 0.0),
                    Flow::top_down(Align::Center),
                    |ui| {
                        ui.set_width(left);
                        if let Some(preview) = &self.preview {
                            let settled = crop_view(
                                ui,
                                preview,
                                self.source_width,
                                self.source_height,
                                &mut self.zoom,
                                &mut self.pan_x,
                                &mut self.pan_y,
                            );
                            if settled {
                                self.persist();
                            }
                            ui.label(theme::muted("Drag the square to crop.").small());
                        } else {
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(left, left * 0.7), Sense::click());
                            let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
                            ui.painter().rect(
                                rect,
                                CornerRadius::same(12),
                                Color32::from_white_alpha(if response.hovered() { 14 } else { 8 }),
                                Stroke::new(1.0_f32, Color32::from_white_alpha(40)),
                                egui::StrokeKind::Inside,
                            );
                            let message = if self.preview_rx.is_some() {
                                "Loading…"
                            } else {
                                "Choose a photo or video"
                            };
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                message,
                                egui::FontId::proportional(15.0),
                                MUTED,
                            );
                            if response.clicked() && !self.busy {
                                self.choose_file(ctx);
                            }
                        }
                    },
                );
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let name = self
                            .chosen
                            .as_ref()
                            .and_then(|path| path.file_name())
                            .map(|name| name.to_string_lossy().into_owned());
                        match name {
                            Some(name) => {
                                ui.add(
                                    egui::Label::new(RichText::new(name).size(16.0).color(TEXT))
                                        .truncate(),
                                )
                                .on_hover_text(
                                    self.chosen
                                        .as_ref()
                                        .map(|path| path.display().to_string())
                                        .unwrap_or_default(),
                                );
                            }
                            None => {
                                ui.label(theme::muted("No file chosen"));
                            }
                        }
                    });
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Choose file…"))
                        .clicked()
                    {
                        self.choose_file(ctx);
                    }
                    ui.add_space(6.0);
                    let zoom = theme::slider_row(ui, "Zoom", |ui| {
                        ui.add(
                            egui::Slider::new(&mut self.zoom, 1.0..=4.0)
                                .fixed_decimals(2)
                                .suffix("×"),
                        )
                    });
                    if zoom.drag_stopped() || zoom.lost_focus() {
                        self.persist();
                    }
                    let video = self.chosen.as_deref().is_some_and(media::is_video);
                    if video {
                        let speed = theme::slider_row(ui, "Speed", |ui| {
                            ui.add(
                                egui::Slider::new(&mut self.speed, 0.25..=2.0)
                                    .fixed_decimals(2)
                                    .suffix("×"),
                            )
                        });
                        if speed.changed() {
                            self.worker.set_speed(self.speed);
                        }
                        if speed.drag_stopped() || speed.lost_focus() {
                            self.persist();
                        }
                        ui.label(
                            theme::muted("1× matches the video. Speed changes while it plays.")
                                .small(),
                        );
                    }
                    if ui.button("Reset crop").clicked() {
                        self.zoom = 1.0;
                        self.pan_x = 0.0;
                        self.pan_y = 0.0;
                        self.persist();
                    }
                    ui.add_space(6.0);
                    let mut on_picture = self.settings.stats_on_picture;
                    if theme::switch_row(
                        ui,
                        &mut on_picture,
                        "Show widgets on the picture",
                        "Place live readings over it on the Widgets page.",
                    ) {
                        self.settings.stats_on_picture = on_picture;
                        self.persist();
                    }
                    if on_picture && ui.button("Edit widgets").clicked() {
                        self.page = Page::Widgets;
                        self.editor.canvas = Canvas::Picture;
                    }
                });
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let can_send = self.chosen.is_some() && !self.busy;
                if ui
                    .add_enabled(can_send, theme::primary_button("Show on pump"))
                    .clicked()
                {
                    self.send_chosen();
                }
                if self.chosen.as_deref().is_some_and(media::is_video)
                    && ui.button("Stop video").clicked()
                {
                    self.worker.stop_playback();
                    self.status = "Stopped. The last frame stays until you quit.".into();
                }
                ui.label(theme::muted("Crop and zoom apply the next time you press Show.").small());
            });
        });
    }

    fn dashboard_section(&mut self, ui: &mut Ui) {
        let readings = self.stats.readings();
        theme::card(ui, |ui| {
            ui.horizontal_top(|ui| {
                let layout = self.settings.dashboard().clone();
                self.editor.preview(
                    ui,
                    240.0,
                    Canvas::Dashboard,
                    &layout,
                    readings.as_ref(),
                    None,
                );
                ui.add_space(18.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("Stats dashboard").size(16.0).color(TEXT));
                    ui.add(
                        egui::Label::new(theme::muted(
                            "Live readings on a dark background. This app redraws it once a \
                             second, so it stops updating when the app quits.",
                        ))
                        .wrap(),
                    );
                    ui.add_space(8.0);
                    if self.settings.source == Source::Dashboard {
                        theme::pill(ui, "On the pump now", OK);
                    } else if ui
                        .add_enabled(!self.busy, theme::primary_button("Show dashboard on pump"))
                        .clicked()
                    {
                        self.show_dashboard();
                    }
                    ui.add_space(4.0);
                    if ui.button("Edit widgets").clicked() {
                        self.page = Page::Widgets;
                        self.editor.canvas = Canvas::Dashboard;
                    }
                });
            });
        });
    }

    fn animation_section(&mut self, ui: &mut Ui) {
        theme::card(ui, |ui| {
            ui.label(RichText::new("Pump animation").size(16.0).color(TEXT));
            ui.add(
                egui::Label::new(theme::muted(
                    "The animation stored in the pump. Nothing is sent from this computer, so \
                     it keeps playing after this app quits.",
                ))
                .wrap(),
            );
            ui.add_space(8.0);
            if self.settings.source == Source::Animation {
                theme::pill(ui, "On the pump now", OK);
            } else if ui
                .add_enabled(!self.busy, theme::primary_button("Show pump animation"))
                .clicked()
            {
                self.show_animation();
            }
        });
    }

    fn widgets_page(&mut self, ui: &mut Ui, room: egui::Vec2) {
        let mut canvas = self.editor.canvas;
        theme::page_header(
            ui,
            "Widgets",
            "Drag live readings anywhere on the screen. The pump follows along.",
            |ui| {
                theme::segmented(
                    ui,
                    &mut canvas,
                    &[
                        (Canvas::Picture, "On your picture"),
                        (Canvas::Dashboard, "Dashboard"),
                    ],
                );
            },
        );
        if canvas != self.editor.canvas {
            self.editor.canvas = canvas;
            self.editor.selected = None;
        }
        match canvas {
            Canvas::Picture if !self.settings.stats_on_picture => {
                theme::banner(ui, WARN, |ui| {
                    ui.label(RichText::new("Widgets on your picture are off.").color(TEXT));
                    if ui.button("Turn them on").clicked() {
                        self.settings.stats_on_picture = true;
                        self.persist();
                    }
                });
            }
            Canvas::Picture if self.settings.source != Source::Picture => {
                theme::banner(ui, MUTED, |ui| {
                    ui.label(
                        RichText::new("The pump is not showing your picture right now.")
                            .color(TEXT),
                    );
                });
            }
            Canvas::Dashboard if self.settings.source != Source::Dashboard => {
                theme::banner(ui, MUTED, |ui| {
                    ui.label(
                        RichText::new("The pump is not showing the dashboard right now.")
                            .color(TEXT),
                    );
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Show it"))
                        .clicked()
                    {
                        self.show_dashboard();
                    }
                });
            }
            _ => {}
        }
        let readings = self.stats.readings();
        // Leave room for the header, a banner, and the toolbar under the screen.
        let side = (room.x - SIDE_COLUMN - 70.0)
            .min(room.y - 270.0)
            .clamp(320.0, 640.0);
        let crop = Crop {
            zoom: self.zoom,
            pan_x: self.pan_x,
            pan_y: self.pan_y,
        };
        let picture = cropped(
            self.preview.as_ref(),
            self.source_width,
            self.source_height,
            &crop,
        );
        let layout = match canvas {
            Canvas::Dashboard => self
                .settings
                .dashboard_layout
                .get_or_insert_with(Layout::default),
            Canvas::Picture => self
                .settings
                .picture_layout
                .get_or_insert_with(Layout::default),
        };
        let before = layout.clone();
        let mut edit = Edit::default();
        let mut undo = false;
        ui.horizontal_top(|ui| {
            theme::card_frame().show(ui, |ui| {
                ui.set_width(side + 16.0);
                ui.vertical(|ui| {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        edit.merge(
                            self.editor
                                .edit(ui, side, layout, readings.as_ref(), picture),
                        );
                        ui.add_space(8.0);
                    });
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.set_max_width(side + 16.0);
                        theme::switch(ui, &mut self.editor.snap)
                            .on_hover_text("Snap to the middle, the edges, and other widgets.");
                        ui.label(theme::muted("Snap"));
                        ui.add_space(8.0);
                        if ui
                            .add_enabled(!self.undo.is_empty(), egui::Button::new("Undo"))
                            .on_hover_text("Ctrl+Z")
                            .clicked()
                        {
                            undo = true;
                        }
                        if ui
                            .button("Arrange for me")
                            .on_hover_text("Lay the same readings out in tidy rows.")
                            .clicked()
                        {
                            let metrics = metrics_of(&layout.widgets);
                            let background = layout.background;
                            *layout = match canvas {
                                Canvas::Dashboard => {
                                    layout::arrange_dashboard(&metrics, background)
                                }
                                Canvas::Picture => layout::arrange_band(&metrics, false),
                            };
                            self.editor.selected = None;
                            edit.merge(Edit::done());
                        }
                        if ui
                            .add_enabled(
                                !layout.widgets.is_empty(),
                                theme::danger_button("Remove all"),
                            )
                            .clicked()
                        {
                            layout.widgets.clear();
                            self.editor.selected = None;
                            edit.merge(Edit::done());
                        }
                    });
                });
            });
            ui.add_space(14.0);
            ui.vertical(|ui| {
                ui.set_width(SIDE_COLUMN.min(ui.available_width()));
                edit.merge(self.editor.inspector(ui, layout, readings.as_ref()));
                ui.add_space(10.0);
                if canvas == Canvas::Dashboard {
                    edit.merge(self.editor.background_card(ui, layout));
                    ui.add_space(10.0);
                }
                edit.merge(self.editor.library(ui, layout, readings.as_ref()));
            });
        });
        undo |= ui.input(|input| input.modifiers.command && input.key_pressed(egui::Key::Z))
            && !ui.ctx().wants_keyboard_input();
        // A drag is one step to undo, however many frames it took.
        if edit.changed && self.gesture.is_none() {
            self.gesture = Some(before);
        }
        if edit.settled {
            if let Some(start) = self.gesture.take() {
                self.undo.push((canvas, start));
                if self.undo.len() > UNDO_LIMIT {
                    self.undo.remove(0);
                }
            }
            self.persist();
        }
        if undo && self.gesture.is_none() {
            if let Some(index) = self.undo.iter().rposition(|(on, _)| *on == canvas) {
                let (_, previous) = self.undo.remove(index);
                match canvas {
                    Canvas::Dashboard => self.settings.dashboard_layout = Some(previous),
                    Canvas::Picture => self.settings.picture_layout = Some(previous),
                }
                self.editor.selected = None;
                self.persist();
            }
        }
    }

    fn screen_page(&mut self, ui: &mut Ui) {
        theme::page_header(
            ui,
            "Screen",
            "Brightness, rotation, and power. The pump remembers these.",
            |ui| {
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Refresh"))
                    .clicked()
                {
                    self.refresh();
                }
            },
        );
        ui.columns_const(|[left, right]| {
            theme::card(left, |ui| {
                ui.label(RichText::new("Display").size(16.0).color(TEXT));
                ui.add_space(6.0);
                let screen_on = self.snapshot.as_ref().is_none_or(|shot| shot.screen_on);
                let slider = theme::slider_row(ui, "Brightness", |ui| {
                    ui.add_enabled(
                        screen_on,
                        egui::Slider::new(&mut self.brightness, 0..=100).suffix("%"),
                    )
                });
                if (slider.drag_stopped() || slider.lost_focus()) && !self.busy {
                    self.apply_brightness();
                }
                ui.horizontal(|ui| {
                    theme::row_label(ui, "Rotation");
                    let current = self.snapshot.as_ref().map(|shot| shot.rotation);
                    let mut picked = current.unwrap_or(u16::MAX);
                    ui.add_enabled_ui(!self.busy && current.is_some(), |ui| {
                        if theme::segmented(
                            ui,
                            &mut picked,
                            &[(0, "0°"), (90, "90°"), (180, "180°"), (270, "270°")],
                        ) {
                            self.apply_rotation(picked);
                        }
                    });
                });
                ui.add_space(8.0);
                if let Some(shot) = self.snapshot.clone() {
                    let mut on = shot.screen_on;
                    ui.add_enabled_ui(!self.busy, |ui| {
                        if theme::switch_row(
                            ui,
                            &mut on,
                            "Screen on",
                            "Off keeps the panel dark until you turn it back on.",
                        ) {
                            self.apply_setting(Setting::ScreenOn(on));
                        }
                        let mut standby = shot.display_in_sleep;
                        if theme::switch_row(
                            ui,
                            &mut standby,
                            "Standby clip while the computer sleeps",
                            "Off turns the panel off while the computer sleeps.",
                        ) {
                            self.apply_setting(Setting::DisplayInSleep(standby));
                        }
                    });
                } else {
                    ui.label(theme::muted("Connect the pump to change these."));
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Wake"))
                        .on_hover_text("Wake the screen if it went to sleep.")
                        .clicked()
                    {
                        self.apply_power("resume");
                    }
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Sleep"))
                        .on_hover_text("Put the screen to sleep, like when the computer sleeps.")
                        .clicked()
                    {
                        self.apply_power("suspend");
                    }
                });
            });
            theme::card(right, |ui| {
                ui.label(RichText::new("This pump").size(16.0).color(TEXT));
                ui.add_space(6.0);
                let shot = self.snapshot.clone();
                egui::Grid::new("pump-facts")
                    .num_columns(2)
                    .spacing([18.0, 8.0])
                    .show(ui, |ui| {
                        let rows = [
                            ("Model", "ROG Strix LC IV".to_string()),
                            (
                                "Serial",
                                shot.as_ref()
                                    .map_or("—".to_string(), |shot| shot.serial.clone()),
                            ),
                            (
                                "Firmware",
                                shot.as_ref()
                                    .map_or("—".to_string(), |shot| shot.firmware.clone()),
                            ),
                            (
                                "Free space",
                                shot.as_ref()
                                    .map_or("—".to_string(), |shot| shot.free_space.clone()),
                            ),
                            ("USB", "0B05:1DE7".to_string()),
                        ];
                        for (name, value) in rows {
                            ui.label(theme::muted(name));
                            ui.label(RichText::new(value).color(TEXT));
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        theme::muted(
                            "The pump and fans run from the motherboard. This app only drives \
                             the screen.",
                        )
                        .small(),
                    )
                    .wrap(),
                );
                ui.add_space(8.0);
                theme::section_label(ui, "Firmware");
                ui.add(
                    egui::Label::new(theme::muted(
                        "Install a firmware package downloaded from ASUS. It is checked first, then sent the same way the ASUS app sends it.",
                    ))
                    .wrap(),
                );
                let ready = self.snapshot.is_some() && !self.busy && !self.asus_running;
                if ui
                    .add_enabled(ready, egui::Button::new("Update firmware…"))
                    .on_disabled_hover_text("Connect the pump first.")
                    .clicked()
                {
                    self.choose_firmware();
                }
            });
        });
    }

    fn settings_page(&mut self, ui: &mut Ui) {
        theme::page_header(ui, "Settings", "How the app starts and runs.", |_| {});
        ui.columns_const(|[left, right]| {
            theme::card(left, |ui| {
                ui.label(RichText::new("Startup and tray").size(16.0).color(TEXT));
                ui.add_space(6.0);
                let mut close_to_tray = self.settings.close_to_tray;
                if theme::switch_row(
                    ui,
                    &mut close_to_tray,
                    "Keep running in the tray",
                    "Closing the window leaves the picture playing.",
                ) {
                    self.settings.close_to_tray = close_to_tray;
                    self.persist();
                }
                let mut start = self.settings.start_with_windows;
                if theme::switch_row(ui, &mut start, super::sign_in_startup_label(), "") {
                    match settings::set_run_at_logon(start, self.settings.start_in_tray) {
                        Ok(()) => {
                            self.settings.start_with_windows = start;
                            self.persist();
                        }
                        Err(error) => self.status = format!("{error:#}"),
                    }
                }
                let mut in_tray = self.settings.start_in_tray;
                if theme::switch_row(
                    ui,
                    &mut in_tray,
                    "Start in the tray",
                    "At sign-in, stay in the tray instead of opening this window.",
                ) {
                    self.settings.start_in_tray = in_tray;
                    if self.settings.start_with_windows {
                        if let Err(error) = settings::set_run_at_logon(true, in_tray) {
                            self.status = format!("{error:#}");
                        }
                    }
                    self.persist();
                }
                let mut resume = self.settings.resume_last;
                if theme::switch_row(
                    ui,
                    &mut resume,
                    "Play the last picture when the app opens",
                    "Sends the last photo or video again on its own.",
                ) {
                    self.settings.resume_last = resume;
                    self.persist();
                }
                ui.add(
                    egui::Label::new(
                        theme::muted(
                            "Click the tray icon to show this window. Right-click it to stop \
                             or quit.",
                        )
                        .small(),
                    )
                    .wrap(),
                );
            });
            theme::card(right, |ui| {
                ui.label(RichText::new("Locking").size(16.0).color(TEXT));
                ui.add_space(6.0);
                let mut follow = self.settings.sleep_when_locked;
                if theme::switch_row(
                    ui,
                    &mut follow,
                    "Sleep the screen while locked",
                    "Wakes it again when you unlock.",
                ) {
                    self.settings.sleep_when_locked = follow;
                    self.follow_lock.store(follow, Ordering::Relaxed);
                    self.persist();
                }
            });
            right.add_space(10.0);
            theme::card(right, |ui| {
                ui.label(RichText::new("Readings").size(16.0).color(TEXT));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    theme::row_label(ui, "Temperature");
                    let mut fahrenheit = self.settings.fahrenheit;
                    if theme::segmented(ui, &mut fahrenheit, &[(false, "°C"), (true, "°F")]) {
                        self.settings.fahrenheit = fahrenheit;
                        self.persist();
                    }
                });
                ui.add_space(4.0);
                let text = if cfg!(windows) {
                    "CPU temperature and power, pump speed, coolant, and fan speeds come from \
                     HWiNFO. Run it with Shared Memory Support turned on in its settings. GPU \
                     readings need an NVIDIA card."
                } else {
                    "CPU temperature comes from the kernel. GPU readings need an NVIDIA card."
                };
                ui.add(egui::Label::new(theme::muted(text)).wrap());
                ui.add_space(6.0);
                ui.label(theme::muted("Settings file").small());
                let path = settings::settings_path().display().to_string();
                ui.add(egui::Label::new(theme::muted(&path).small()).truncate())
                    .on_hover_text(&path);
            });
        });
    }
}

/// Readings in the order they first appear, for laying a screen out again.
fn metrics_of(widgets: &[Widget]) -> Vec<crate::stats::Metric> {
    let mut metrics = Vec::new();
    for widget in widgets {
        if !metrics.contains(&widget.metric) {
            metrics.push(widget.metric);
        }
    }
    metrics
}
