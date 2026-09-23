#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use sight_relay_capture::Roi;
#[cfg(target_os = "windows")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "windows")]
const MIN_SELECTION: f32 = 0.02;

#[derive(Clone, Copy)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
struct SelectionChrome {
    halo_width: f32,
    outline_width: f32,
    handle_length: f32,
    center_radius: f32,
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn selection_chrome() -> SelectionChrome {
    SelectionChrome {
        halo_width: 5.0,
        outline_width: 2.0,
        handle_length: 22.0,
        center_radius: 18.0,
    }
}

#[cfg(target_os = "windows")]
fn draw_selection_chrome(painter: &egui::Painter, selection: Rect) {
    let chrome = selection_chrome();
    let edge = Color32::from_rgb(105, 220, 178);
    painter.rect_stroke(
        selection,
        4.0,
        Stroke::new(chrome.halo_width, Color32::from_black_alpha(155)),
        egui::StrokeKind::Outside,
    );
    painter.rect_stroke(
        selection,
        4.0,
        Stroke::new(chrome.outline_width, edge),
        egui::StrokeKind::Outside,
    );

    let handle_length = chrome
        .handle_length
        .min(selection.width() * 0.36)
        .min(selection.height() * 0.36);
    let handle_shadow = Stroke::new(5.0_f32, Color32::from_black_alpha(170));
    let handle_stroke = Stroke::new(3.0_f32, Color32::WHITE);
    for (point, horizontal, vertical) in [
        (selection.left_top(), 1.0, 1.0),
        (selection.right_top(), -1.0, 1.0),
        (selection.left_bottom(), 1.0, -1.0),
        (selection.right_bottom(), -1.0, -1.0),
    ] {
        let horizontal_end = point + Vec2::new(horizontal * handle_length, 0.0);
        let vertical_end = point + Vec2::new(0.0, vertical * handle_length);
        painter.line_segment([point, horizontal_end], handle_shadow);
        painter.line_segment([point, vertical_end], handle_shadow);
        painter.line_segment([point, horizontal_end], handle_stroke);
        painter.line_segment([point, vertical_end], handle_stroke);
        painter.circle_filled(point, 3.0, edge);
    }

    if selection.width() >= chrome.center_radius * 3.0
        && selection.height() >= chrome.center_radius * 3.0
    {
        let center = selection.center();
        let cross_length = chrome.center_radius * 0.56;
        painter.circle_filled(center, chrome.center_radius, Color32::from_black_alpha(180));
        painter.circle_stroke(
            center,
            chrome.center_radius,
            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(182, 248, 221, 210)),
        );
        painter.line_segment(
            [
                center - Vec2::new(cross_length, 0.0),
                center + Vec2::new(cross_length, 0.0),
            ],
            Stroke::new(1.5_f32, Color32::WHITE),
        );
        painter.line_segment(
            [
                center - Vec2::new(0.0, cross_length),
                center + Vec2::new(0.0, cross_length),
            ],
            Stroke::new(1.5_f32, Color32::WHITE),
        );
        painter.circle_filled(center, 2.5, edge);
    }
}

#[cfg(target_os = "windows")]
fn chinese_font_candidates() -> &'static [&'static str] {
    &[
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyhbd.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ]
}

#[cfg(target_os = "windows")]
fn configure_fonts(ctx: &egui::Context) {
    let Some(path) = chinese_font_candidates()
        .iter()
        .find(|path| std::path::Path::new(path).is_file())
    else {
        return;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "windows_chinese".into(),
        egui::FontData::from_owned(bytes).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "windows_chinese".into());
    }
    ctx.set_fonts(fonts);
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn roi_from_canvas_points(start: [f32; 2], end: [f32; 2], canvas: [f32; 2]) -> Roi {
    let width = canvas[0].max(1.0);
    let height = canvas[1].max(1.0);
    let left = start[0].min(end[0]).clamp(0.0, width);
    let top = start[1].min(end[1]).clamp(0.0, height);
    let right = start[0].max(end[0]).clamp(0.0, width);
    let bottom = start[1].max(end[1]).clamp(0.0, height);
    Roi {
        x: left / width,
        y: top / height,
        width: ((right - left) / width).max(1.0 / width),
        height: ((bottom - top) / height).max(1.0 / height),
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy)]
enum Corner {
    NorthWest,
    NorthEast,
    SouthWest,
    SouthEast,
}

#[cfg(target_os = "windows")]
enum DragAction {
    New(Pos2),
    Move {
        start: Pos2,
        roi: Roi,
    },
    Resize {
        start: Pos2,
        roi: Roi,
        corner: Corner,
    },
}

#[cfg(target_os = "windows")]
struct RoiOverlay {
    texture: egui::TextureHandle,
    image_size: Vec2,
    roi: Roi,
    drag: Option<DragAction>,
    result: Arc<Mutex<Option<Roi>>>,
}

#[cfg(target_os = "windows")]
impl RoiOverlay {
    fn screen_rect(&self, rect: Rect) -> Rect {
        let image_ratio = self.image_size.x / self.image_size.y;
        let viewport_ratio = rect.width() / rect.height();
        if viewport_ratio > image_ratio {
            Rect::from_center_size(
                rect.center(),
                Vec2::new(rect.height() * image_ratio, rect.height()),
            )
        } else {
            Rect::from_center_size(
                rect.center(),
                Vec2::new(rect.width(), rect.width() / image_ratio),
            )
        }
    }

    fn selection_rect(&self, screen: Rect) -> Rect {
        Rect::from_min_size(
            Pos2::new(
                screen.left() + screen.width() * self.roi.x,
                screen.top() + screen.height() * self.roi.y,
            ),
            Vec2::new(
                screen.width() * self.roi.width,
                screen.height() * self.roi.height,
            ),
        )
    }

    fn canvas_point(screen: Rect, point: Pos2) -> [f32; 2] {
        [
            (point.x - screen.left()).clamp(0.0, screen.width()),
            (point.y - screen.top()).clamp(0.0, screen.height()),
        ]
    }

    fn hit_corner(selection: Rect, point: Pos2) -> Option<Corner> {
        const RADIUS: f32 = 18.0;
        [
            (selection.left_top(), Corner::NorthWest),
            (selection.right_top(), Corner::NorthEast),
            (selection.left_bottom(), Corner::SouthWest),
            (selection.right_bottom(), Corner::SouthEast),
        ]
        .into_iter()
        .find(|(corner, _)| corner.distance(point) <= RADIUS)
        .map(|(_, corner)| corner)
    }

    fn update_drag(&mut self, screen: Rect, point: Pos2) {
        let Some(drag) = self.drag.as_ref() else {
            return;
        };
        match *drag {
            DragAction::New(start) => {
                self.roi = roi_from_canvas_points(
                    Self::canvas_point(screen, start),
                    Self::canvas_point(screen, point),
                    [screen.width(), screen.height()],
                );
            }
            DragAction::Move { start, roi } => {
                let x = (roi.x + (point.x - start.x) / screen.width()).clamp(0.0, 1.0 - roi.width);
                let y =
                    (roi.y + (point.y - start.y) / screen.height()).clamp(0.0, 1.0 - roi.height);
                self.roi = Roi { x, y, ..roi };
            }
            DragAction::Resize { start, roi, corner } => {
                let dx = (point.x - start.x) / screen.width();
                let dy = (point.y - start.y) / screen.height();
                let (mut left, mut top, mut right, mut bottom) =
                    (roi.x, roi.y, roi.x + roi.width, roi.y + roi.height);
                match corner {
                    Corner::NorthWest => {
                        left = (left + dx).clamp(0.0, right - MIN_SELECTION);
                        top = (top + dy).clamp(0.0, bottom - MIN_SELECTION);
                    }
                    Corner::NorthEast => {
                        right = (right + dx).clamp(left + MIN_SELECTION, 1.0);
                        top = (top + dy).clamp(0.0, bottom - MIN_SELECTION);
                    }
                    Corner::SouthWest => {
                        left = (left + dx).clamp(0.0, right - MIN_SELECTION);
                        bottom = (bottom + dy).clamp(top + MIN_SELECTION, 1.0);
                    }
                    Corner::SouthEast => {
                        right = (right + dx).clamp(left + MIN_SELECTION, 1.0);
                        bottom = (bottom + dy).clamp(top + MIN_SELECTION, 1.0);
                    }
                }
                self.roi = Roi {
                    x: left,
                    y: top,
                    width: right - left,
                    height: bottom - top,
                };
            }
        }
    }

    fn finish(&self, ctx: &egui::Context, value: Option<Roi>) {
        *self.result.lock().unwrap() = value;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

#[cfg(target_os = "windows")]
impl eframe::App for RoiOverlay {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.finish(ctx, None);
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Enter)) {
            self.finish(ctx, Some(self.roi));
            return;
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let screen = self.screen_rect(ui.max_rect());
                let painter = ui.painter_at(screen);
                painter.image(
                    self.texture.id(),
                    screen,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                let response = ui.interact(screen, ui.id().with("selection"), Sense::drag());
                let selection = self.selection_rect(screen);
                if response.drag_started() {
                    if let Some(point) = response.interact_pointer_pos() {
                        self.drag = if let Some(corner) = Self::hit_corner(selection, point) {
                            Some(DragAction::Resize {
                                start: point,
                                roi: self.roi,
                                corner,
                            })
                        } else if selection.contains(point) {
                            Some(DragAction::Move {
                                start: point,
                                roi: self.roi,
                            })
                        } else {
                            Some(DragAction::New(point))
                        };
                    }
                }
                if response.dragged() {
                    if let Some(point) = response.interact_pointer_pos() {
                        self.update_drag(screen, point);
                    }
                }
                if response.drag_stopped() {
                    self.drag = None;
                }

                let selection = self.selection_rect(screen);
                let shade = Color32::from_black_alpha(135);
                painter.rect_filled(
                    Rect::from_min_max(
                        screen.left_top(),
                        Pos2::new(screen.right(), selection.top()),
                    ),
                    0.0,
                    shade,
                );
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(screen.left(), selection.bottom()),
                        screen.right_bottom(),
                    ),
                    0.0,
                    shade,
                );
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(screen.left(), selection.top()),
                        selection.left_bottom(),
                    ),
                    0.0,
                    shade,
                );
                painter.rect_filled(
                    Rect::from_min_max(
                        selection.right_top(),
                        Pos2::new(screen.right(), selection.bottom()),
                    ),
                    0.0,
                    shade,
                );
                draw_selection_chrome(&painter, selection);
            });
        egui::Area::new(egui::Id::new("selection-toolbar"))
            .fixed_pos(Pos2::new(24.0, 24.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(13, 27, 45, 232))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(64, 95, 131)))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new("设置捕获范围")
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.label(
                            egui::RichText::new("拖动框内移动，拖动四角缩放")
                                .color(Color32::from_rgb(192, 211, 236)),
                        );
                        ui.horizontal(|ui| {
                            if ui.button("恢复全屏").clicked() {
                                self.roi = Roi::full();
                            }
                            if ui.button("取消 Esc").clicked() {
                                self.finish(ctx, None);
                            }
                            if ui.button("保存 Enter").clicked() {
                                self.finish(ctx, Some(self.roi));
                            }
                        });
                    });
            });
    }
}

#[cfg(target_os = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 6 {
        eprintln!("Usage: roi-overlay display_id preview_path x y width height");
        std::process::exit(2);
    }
    let display_id = args[0].parse::<u32>().unwrap_or_else(|_| {
        eprintln!("Invalid display id");
        std::process::exit(2);
    });
    let value = |index: usize, fallback: f32| args[index].parse::<f32>().unwrap_or(fallback);
    let mut initial_roi = Roi {
        x: value(2, 0.0).clamp(0.0, 1.0),
        y: value(3, 0.0).clamp(0.0, 1.0),
        width: value(4, 1.0).clamp(MIN_SELECTION, 1.0),
        height: value(5, 1.0).clamp(MIN_SELECTION, 1.0),
    };
    initial_roi.width = initial_roi.width.min(1.0 - initial_roi.x);
    initial_roi.height = initial_roi.height.min(1.0 - initial_roi.y);
    let rgba = image::open(&args[1])
        .unwrap_or_else(|error| {
            eprintln!("Unable to load screen preview: {error}");
            std::process::exit(1);
        })
        .to_rgba8();
    let image_size = Vec2::new(rgba.width() as f32, rgba.height() as f32);
    let monitor = xcap::Monitor::all().ok().and_then(|monitors| {
        monitors
            .into_iter()
            .find(|monitor| monitor.id().ok() == Some(display_id))
    });
    let (position, scale) = monitor
        .as_ref()
        .map(|monitor| {
            (
                [
                    monitor.x().unwrap_or(0) as f32,
                    monitor.y().unwrap_or(0) as f32,
                ],
                monitor.scale_factor().unwrap_or(1.0).max(1.0),
            )
        })
        .unwrap_or(([0.0, 0.0], 1.0));
    let result = Arc::new(Mutex::new(None));
    let output = result.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_decorations(false)
            .with_always_on_top()
            .with_position([position[0] / scale, position[1] / scale])
            .with_inner_size([image_size.x / scale, image_size.y / scale]),
        ..Default::default()
    };
    let app = move |cc: &eframe::CreationContext<'_>| {
        configure_fonts(&cc.egui_ctx);
        let texture = cc.egui_ctx.load_texture(
            "screen-preview",
            egui::ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            ),
            egui::TextureOptions::LINEAR,
        );
        Ok::<Box<dyn eframe::App>, _>(Box::new(RoiOverlay {
            texture,
            image_size,
            roi: initial_roi,
            drag: None,
            result: output,
        }))
    };
    if let Err(error) = eframe::run_native("Sight Relay Capture Region", options, Box::new(app)) {
        eprintln!("Unable to show range editor: {error}");
        std::process::exit(1);
    }
    if let Some(roi) = *result.lock().unwrap() {
        println!("{}", serde_json::to_string(&roi).expect("serialize ROI"));
    }
}

#[cfg(all(not(target_os = "windows"), not(test)))]
fn main() {
    eprintln!("roi-overlay is only available on Windows");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    #[test]
    fn selection_chrome_uses_precise_corner_handles_and_a_center_crosshair() {
        let chrome = super::selection_chrome();
        assert_eq!(chrome.handle_length, 22.0);
        assert_eq!(chrome.center_radius, 18.0);
        assert!(chrome.halo_width > chrome.outline_width);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_overlay_declares_a_chinese_font_fallback() {
        assert!(super::chinese_font_candidates().contains(&r"C:\Windows\Fonts\msyh.ttc"));
    }

    #[test]
    fn drag_selection_is_clamped_and_normalized_to_the_captured_screen() {
        let roi = super::roi_from_canvas_points([900.0, -10.0], [400.0, 700.0], [800.0, 600.0]);
        assert_eq!(roi.x, 0.5);
        assert_eq!(roi.y, 0.0);
        assert_eq!(roi.width, 0.5);
        assert_eq!(roi.height, 1.0);
    }
}
