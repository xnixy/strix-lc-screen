//! Colors, spacing, and the small controls the window is built from. Shared
//! with Blurman: a dark plum shell, rose accent, and rounded cards over the
//! window's frosted backdrop.

use eframe::egui;
use egui::{
    Align, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, Rect, Response, RichText,
    Sense, Shadow, Stroke, StrokeKind, Ui, Vec2,
};

pub const ACCENT: Color32 = Color32::from_rgb(196, 54, 78);
/// Behind everything when the system cannot frost the window.
pub const BG: Color32 = Color32::from_rgb(16, 14, 20);
/// Tooltips and menus, which float over content and need to stay readable.
pub const POPUP: Color32 = Color32::from_rgb(34, 28, 36);
pub const TEXT: Color32 = Color32::from_rgb(240, 236, 238);
pub const MUTED: Color32 = Color32::from_rgb(186, 180, 186);
pub const WARN: Color32 = Color32::from_rgb(230, 186, 120);
pub const OK: Color32 = Color32::from_rgb(126, 196, 154);
pub const ROW_HOVER: Color32 = Color32::from_rgba_premultiplied(18, 18, 18, 18);
/// Rose at about 22% opacity, premultiplied so it can live in a const.
pub const ROW_SELECTED: Color32 = Color32::from_rgba_premultiplied(43, 12, 17, 56);

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = shell_visuals();
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style.spacing.interact_size.y = 30.0;
    style.interaction.selectable_labels = false;
    ctx.set_style(style);
}

fn shell_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = POPUP;
    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.faint_bg_color = Color32::from_rgba_unmultiplied(255, 255, 255, 14);
    visuals.extreme_bg_color = Color32::from_rgba_unmultiplied(18, 14, 20, 170);
    visuals.widgets.noninteractive.fg_stroke.color = Color32::from_rgb(176, 170, 176);
    visuals.widgets.noninteractive.bg_stroke.color =
        Color32::from_rgba_unmultiplied(255, 255, 255, 28);
    let mut idle = visuals.widgets.inactive;
    idle.bg_fill = Color32::from_rgb(58, 50, 58);
    idle.weak_bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 18);
    idle.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 36));
    idle.corner_radius = CornerRadius::same(8);
    idle.fg_stroke.color = TEXT;
    let mut hovered = idle;
    hovered.weak_bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 30);
    hovered.bg_fill = Color32::from_rgb(78, 66, 74);
    let mut active = hovered;
    active.bg_fill = ACCENT;
    active.weak_bg_fill = ACCENT;
    active.fg_stroke.color = Color32::WHITE;
    visuals.widgets.inactive = idle;
    visuals.widgets.hovered = hovered;
    visuals.widgets.active = active;
    visuals.widgets.open = hovered;
    visuals.selection.bg_fill = Color32::from_rgb(190, 52, 74);
    visuals.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(230, 140, 152));
    visuals.hyperlink_color = Color32::from_rgb(255, 176, 186);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(10);
    visuals.window_stroke =
        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 32));
    visuals
}

/// The sidebar: a little darker than the pages, so the two read as separate areas.
pub fn sidebar(glass: bool) -> Frame {
    let fill = if glass {
        Color32::from_rgba_unmultiplied(10, 8, 12, 178)
    } else {
        Color32::from_rgb(12, 10, 15)
    };
    Frame::new()
        .fill(fill)
        .inner_margin(Margin::symmetric(14, 16))
}

/// The tint laid over the acrylic backdrop behind each page. Opaque when there is no backdrop.
pub fn page(glass: bool) -> Frame {
    let fill = if glass {
        Color32::from_rgba_unmultiplied(16, 12, 18, 140)
    } else {
        BG
    };
    Frame::new()
        .fill(fill)
        .inner_margin(Margin::symmetric(22, 18))
}

/// The strip under each page that says what just happened.
pub fn footer(glass: bool) -> Frame {
    let fill = if glass {
        Color32::from_rgba_unmultiplied(16, 12, 18, 140)
    } else {
        BG
    };
    Frame::new().fill(fill).inner_margin(Margin {
        left: 22,
        right: 22,
        top: 4,
        bottom: 12,
    })
}

pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    card_frame()
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

pub fn card_frame() -> Frame {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(34, 28, 36, 214))
        .stroke(Stroke::new(
            1.0_f32,
            Color32::from_rgba_unmultiplied(255, 255, 255, 32),
        ))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(16))
        .shadow(Shadow {
            offset: [0, 8],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(80),
        })
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).color(MUTED)
}

pub fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).strong().color(Color32::WHITE))
        .fill(ACCENT)
        .corner_radius(CornerRadius::same(10))
        .min_size(Vec2::new(150.0, 36.0))
}

/// A plain button drawn in the accent red, for things that cannot be undone.
pub fn danger_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).color(Color32::from_rgb(255, 196, 204)))
        .fill(ACCENT.gamma_multiply(0.35))
        .stroke(Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.8)))
}

/// An on/off switch. It reads as on or off at a glance where a checkbox does not.
pub fn switch(ui: &mut Ui, on: &mut bool) -> Response {
    let size = egui::vec2(38.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let track = Color32::from_rgb(70, 60, 70).lerp_to_gamma(ACCENT, t);
        let enabled = ui.is_enabled();
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(11),
            if enabled {
                track
            } else {
                track.gamma_multiply(0.5)
            },
            Stroke::new(
                1.0_f32,
                Color32::from_white_alpha(if response.hovered() { 60 } else { 30 }),
            ),
            StrokeKind::Inside,
        );
        let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
        painter.circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
    }
    response
}

/// A switch with a title and a line of detail beside it. True when it was flipped.
pub fn switch_row(ui: &mut Ui, on: &mut bool, title: &str, detail: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed = switch(ui, on).changed();
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).color(TEXT));
            if !detail.is_empty() {
                ui.add(egui::Label::new(muted(detail).small()).wrap());
            }
        });
    });
    ui.add_space(4.0);
    changed
}

/// One choice out of a few, drawn as joined buttons. Returns whether the choice changed.
pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    let enabled = ui.is_enabled();
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 12))
        .stroke(Stroke::new(1.0_f32, Color32::from_white_alpha(26)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                // Inside a right-to-left row the buttons are placed from the right, so
                // walk them backwards to keep the order they were given in.
                let backwards = ui.layout().prefer_right_to_left();
                let ordered: Vec<&(T, &str)> = if backwards {
                    options.iter().rev().collect()
                } else {
                    options.iter().collect()
                };
                for &&(option, label) in &ordered {
                    let selected = *value == option;
                    let text = if selected {
                        RichText::new(label).color(Color32::WHITE)
                    } else {
                        RichText::new(label).color(MUTED)
                    };
                    let fill = match (selected, enabled) {
                        (true, true) => ACCENT,
                        (true, false) => ACCENT.gamma_multiply(0.5),
                        _ => Color32::TRANSPARENT,
                    };
                    let button = egui::Button::new(text)
                        .fill(fill)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(8))
                        .min_size(egui::vec2(64.0, 28.0));
                    if ui.add(button).clicked() && !selected {
                        *value = option;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// A small upper-case heading over a group of rows.
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .size(11.0)
            .strong()
            .color(MUTED.gamma_multiply(0.85)),
    );
    ui.add_space(2.0);
}

/// The page title and its one-line explanation, with room on the right for controls.
pub fn page_header(ui: &mut Ui, title: &str, subtitle: &str, right: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).size(24.0).color(TEXT));
            ui.label(muted(subtitle));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), right);
    });
    ui.add_space(12.0);
}

/// The name at the start of a row, as wide as the one beside a slider.
pub fn row_label(ui: &mut Ui, label: &str) {
    let width = 96.0_f32.min(ui.available_width() * 0.35);
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.spacing().interact_size.y),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_width(width);
            ui.label(muted(label));
        },
    );
}

/// A label on the left and a slider filling the rest of the row, its number kept inside.
pub fn slider_row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> Response) -> Response {
    let budget = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(budget, 0.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_width(budget);
            row_label(ui, label);
            ui.spacing_mut().slider_width = slider_rail_width(ui);
            add(ui)
        },
    )
    .inner
}

/// The rail leaves room for the number button that egui draws after it.
fn slider_rail_width(ui: &Ui) -> f32 {
    let size = egui::TextStyle::Button.resolve(ui.style()).size;
    let number = ui
        .painter()
        .layout_no_wrap(
            "100.00×".to_owned(),
            egui::FontId::proportional(size),
            Color32::WHITE,
        )
        .size()
        .x
        + ui.spacing().button_padding.x * 2.0;
    let gap = ui.spacing().item_spacing.x;
    (ui.available_width() - gap - number.max(ui.spacing().interact_size.x) - 4.0).max(40.0)
}

/// A banner across the top of a page.
pub fn banner(ui: &mut Ui, color: Color32, add_contents: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(color.gamma_multiply(0.2))
        .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.5)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(add_contents);
        });
    ui.add_space(8.0);
}

/// A rounded label in a color, such as "On the pump".
pub fn pill(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.gamma_multiply(0.22))
        .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).size(11.5));
        });
}

/// The sidebar's pages, each with a small drawn icon so nothing depends on an icon font.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavIcon {
    Show,
    Widgets,
    Screen,
    Settings,
}

pub fn nav_item(ui: &mut Ui, icon: NavIcon, label: &str, selected: bool) -> Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 38.0), Sense::click());
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if selected {
            ROW_SELECTED
        } else if response.hovered() {
            ROW_HOVER
        } else {
            Color32::TRANSPARENT
        };
        painter.rect_filled(rect, CornerRadius::same(10), fill);
        if selected {
            let bar = Rect::from_min_size(
                rect.left_top() + egui::vec2(0.0, 10.0),
                egui::vec2(3.0, rect.height() - 20.0),
            );
            painter.rect_filled(bar, CornerRadius::same(2), ACCENT);
        }
        let color = if selected { TEXT } else { MUTED };
        let icon_rect = Rect::from_center_size(
            egui::pos2(rect.left() + 24.0, rect.center().y),
            egui::vec2(16.0, 16.0),
        );
        draw_nav_icon(
            painter,
            icon_rect,
            icon,
            if selected { ACCENT } else { color },
        );
        painter.text(
            egui::pos2(rect.left() + 44.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(15.0),
            color,
        );
    }
    response
}

fn draw_nav_icon(painter: &egui::Painter, r: Rect, icon: NavIcon, color: Color32) {
    let stroke = Stroke::new(1.6_f32, color);
    let c = r.center();
    match icon {
        NavIcon::Show => {
            painter.circle_stroke(c, r.width() / 2.0, stroke);
            let s = r.width() * 0.22;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + egui::vec2(-s * 0.7, -s),
                    c + egui::vec2(s * 1.1, 0.0),
                    c + egui::vec2(-s * 0.7, s),
                ],
                color,
                Stroke::NONE,
            ));
        }
        NavIcon::Widgets => {
            let big = Rect::from_min_size(r.min, egui::vec2(r.width() * 0.56, r.height()));
            painter.rect_stroke(big, CornerRadius::same(2), stroke, StrokeKind::Inside);
            let x = big.right() + 2.5;
            let small = egui::vec2(r.right() - x, r.height() * 0.45);
            painter.rect_stroke(
                Rect::from_min_size(egui::pos2(x, r.top()), small),
                CornerRadius::same(2),
                stroke,
                StrokeKind::Inside,
            );
            painter.rect_stroke(
                Rect::from_min_size(egui::pos2(x, r.bottom() - small.y), small),
                CornerRadius::same(2),
                stroke,
                StrokeKind::Inside,
            );
        }
        NavIcon::Screen => {
            painter.circle_stroke(c, r.width() * 0.22, stroke);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let dir = egui::vec2(a.cos(), a.sin());
                painter.line_segment(
                    [c + dir * (r.width() * 0.36), c + dir * (r.width() * 0.5)],
                    stroke,
                );
            }
        }
        NavIcon::Settings => {
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let dir = egui::vec2(a.cos(), a.sin());
                painter.line_segment(
                    [c + dir * (r.width() * 0.32), c + dir * (r.width() * 0.5)],
                    Stroke::new(2.2_f32, color),
                );
            }
            painter.circle_stroke(c, r.width() * 0.32, stroke);
            painter.circle_filled(c, 1.8, color);
        }
    }
}

pub fn color32(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// A row of color dots. Returns the one clicked.
pub fn swatches(ui: &mut Ui, current: [u8; 3], palette: &[[u8; 3]]) -> Option<[u8; 3]> {
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for &rgb in palette {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), Sense::click());
            let response = response.on_hover_cursor(CursorIcon::PointingHand);
            let painter = ui.painter();
            painter.circle_filled(rect.center(), 9.0, color32(rgb));
            if rgb == current {
                painter.circle_stroke(rect.center(), 11.5, Stroke::new(2.0_f32, TEXT));
            } else if response.hovered() {
                painter.circle_stroke(
                    rect.center(),
                    11.5,
                    Stroke::new(1.0_f32, Color32::from_white_alpha(90)),
                );
            }
            if response.clicked() {
                picked = Some(rgb);
            }
        }
    });
    picked
}
