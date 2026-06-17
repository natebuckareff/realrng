use std::time::Duration;

use eframe::egui;
use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    query,
    utils::{ApiBackend, CameraInfo, RequestedFormat, RequestedFormatType},
};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("egui Workspace")
            .with_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "egui Workspace",
        options,
        Box::new(|_cc| Ok(Box::<WorkspaceApp>::default())),
    )
}

struct WorkspaceApp {
    webcam: WebcamWindow,
}

impl Default for WorkspaceApp {
    fn default() -> Self {
        Self {
            webcam: WebcamWindow::new(),
        }
    }
}

impl eframe::App for WorkspaceApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.allocate_space(ui.available_size());
        });

        let ctx = ui.ctx().clone();
        self.webcam.show(&ctx);
    }
}

struct WebcamWindow {
    default_pos: egui::Pos2,
    diff_default_pos: egui::Pos2,
    devices: Vec<CameraInfo>,
    selected_device: Option<usize>,
    camera: Option<Camera>,
    texture: Option<egui::TextureHandle>,
    diff_texture: Option<egui::TextureHandle>,
    previous_frame: Option<RgbFrame>,
    status: String,
}

struct RgbFrame {
    size: [usize; 2],
    pixels: Vec<u8>,
}

impl WebcamWindow {
    fn new() -> Self {
        let mut webcam = Self {
            default_pos: egui::pos2(48.0, 56.0),
            diff_default_pos: egui::pos2(600.0, 56.0),
            devices: Vec::new(),
            selected_device: None,
            camera: None,
            texture: None,
            diff_texture: None,
            previous_frame: None,
            status: String::new(),
        };

        webcam.refresh_devices();
        webcam
    }

    fn show(&mut self, ctx: &egui::Context) {
        if self.camera.is_some() {
            self.update_frame_textures(ctx);
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        self.show_webcam_window(ctx);
        self.show_difference_window(ctx);
    }

    fn show_webcam_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("Webcam")
            .id(egui::Id::new("webcam_window"))
            .default_pos(self.default_pos)
            .default_size([520.0, 420.0])
            .resizable(true)
            .collapsible(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    self.device_selector(ui);

                    if ui.button("Refresh").clicked() {
                        self.refresh_devices();
                    }
                });

                ui.horizontal(|ui| {
                    let can_start = self.selected_device.is_some() && !self.devices.is_empty();
                    if ui
                        .add_enabled(can_start, egui::Button::new("Start camera"))
                        .clicked()
                    {
                        self.start_selected_camera();
                    }

                    if ui
                        .add_enabled(self.camera.is_some(), egui::Button::new("Stop camera"))
                        .clicked()
                    {
                        self.stop_camera();
                    }
                });

                if !self.status.is_empty() {
                    ui.label(self.status.as_str());
                }

                ui.separator();
                self.frame_view(ui);
            });
    }

    fn show_difference_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("Frame Difference")
            .id(egui::Id::new("frame_difference_window"))
            .default_pos(self.diff_default_pos)
            .default_size([520.0, 420.0])
            .resizable(true)
            .collapsible(true)
            .show(ctx, |ui| {
                if let Some(texture) = &self.diff_texture {
                    ui.add(
                        egui::Image::from_texture(texture)
                            .max_size(egui::vec2(ui.available_width(), 380.0))
                            .shrink_to_fit(),
                    );
                } else {
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), 280.0),
                        egui::Layout::centered_and_justified(egui::Direction::TopDown),
                        |ui| {
                            ui.label("Start a camera to show frame differences here.");
                        },
                    );
                }
            });
    }

    fn device_selector(&mut self, ui: &mut egui::Ui) {
        let selected_text = self
            .selected_device
            .and_then(|idx| self.devices.get(idx))
            .map(device_label)
            .unwrap_or_else(|| "No camera selected".to_owned());

        egui::ComboBox::from_id_salt("webcam_device_selector")
            .selected_text(selected_text)
            .width(280.0)
            .show_ui(ui, |ui| {
                if self.devices.is_empty() {
                    ui.label("No cameras found");
                    return;
                }

                for (idx, device) in self.devices.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_device, Some(idx), device_label(device));
                }
            });
    }

    fn frame_view(&self, ui: &mut egui::Ui) {
        if let Some(texture) = &self.texture {
            ui.add(
                egui::Image::from_texture(texture)
                    .max_size(egui::vec2(ui.available_width(), 360.0))
                    .shrink_to_fit(),
            );
        } else {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 280.0),
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
                |ui| {
                    ui.label("Start a camera to show frames here.");
                },
            );
        }
    }

    fn refresh_devices(&mut self) {
        match query(ApiBackend::Auto) {
            Ok(devices) => {
                self.devices = devices;
                self.selected_device = match (self.selected_device, self.devices.is_empty()) {
                    (_, true) => None,
                    (Some(idx), false) if idx < self.devices.len() => Some(idx),
                    _ => Some(0),
                };

                self.status = if self.devices.is_empty() {
                    "No cameras found.".to_owned()
                } else {
                    format!("Found {} camera(s).", self.devices.len())
                };
            }
            Err(error) => {
                self.devices.clear();
                self.selected_device = None;
                self.stop_camera();
                self.status = format!("Could not query cameras: {error}");
            }
        }
    }

    fn start_selected_camera(&mut self) {
        let Some(device_idx) = self.selected_device else {
            self.status = "Choose a camera first.".to_owned();
            return;
        };

        let Some(device) = self.devices.get(device_idx) else {
            self.status = "Selected camera is no longer available.".to_owned();
            return;
        };

        let requested =
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);

        match Camera::new(device.index().clone(), requested) {
            Ok(mut camera) => match camera.open_stream() {
                Ok(()) => {
                    let format = camera.camera_format();
                    self.camera = Some(camera);
                    self.texture = None;
                    self.diff_texture = None;
                    self.previous_frame = None;
                    self.status = format!(
                        "Streaming {} at {}x{} {} FPS.",
                        device_label(device),
                        format.width(),
                        format.height(),
                        format.frame_rate()
                    );
                }
                Err(error) => {
                    self.camera = None;
                    self.texture = None;
                    self.diff_texture = None;
                    self.previous_frame = None;
                    self.status = format!("Could not open camera stream: {error}");
                }
            },
            Err(error) => {
                self.camera = None;
                self.texture = None;
                self.diff_texture = None;
                self.previous_frame = None;
                self.status = format!("Could not create camera: {error}");
            }
        }
    }

    fn stop_camera(&mut self) {
        self.camera = None;
        self.texture = None;
        self.diff_texture = None;
        self.previous_frame = None;
    }

    fn update_frame_textures(&mut self, ctx: &egui::Context) {
        let Some(camera) = self.camera.as_mut() else {
            return;
        };

        let frame = match camera.frame() {
            Ok(frame) => frame,
            Err(error) => {
                self.status = format!("Could not read frame: {error}");
                return;
            }
        };

        let decoded = match frame.decode_image::<RgbFormat>() {
            Ok(decoded) => decoded,
            Err(error) => {
                self.status = format!("Could not decode frame: {error}");
                return;
            }
        };

        let size = [decoded.width() as usize, decoded.height() as usize];
        let current_pixels = decoded.as_raw().to_vec();
        let image = egui::ColorImage::from_rgb(size, &current_pixels);

        if let Some(texture) = self.texture.as_mut() {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("webcam_frame", image, egui::TextureOptions::LINEAR));
        }

        if let Some(previous_frame) = &self.previous_frame {
            if previous_frame.size == size && previous_frame.pixels.len() == current_pixels.len() {
                let diff_pixels =
                    absolute_frame_difference(&current_pixels, &previous_frame.pixels);
                let diff_image = egui::ColorImage::from_rgb(size, &diff_pixels);

                if let Some(texture) = self.diff_texture.as_mut() {
                    texture.set(diff_image, egui::TextureOptions::LINEAR);
                } else {
                    self.diff_texture = Some(ctx.load_texture(
                        "webcam_frame_difference",
                        diff_image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            } else {
                self.diff_texture = None;
            }
        }

        self.previous_frame = Some(RgbFrame {
            size,
            pixels: current_pixels,
        });
    }
}

fn absolute_frame_difference(current: &[u8], previous: &[u8]) -> Vec<u8> {
    current
        .iter()
        .zip(previous)
        .map(|(current, previous)| current.abs_diff(*previous))
        .collect()
}

fn device_label(device: &CameraInfo) -> String {
    let name = device.human_name();
    if name.trim().is_empty() {
        format!("Camera {}", device.index())
    } else {
        name
    }
}
