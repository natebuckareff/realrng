use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError, TrySendError},
    },
    time::{Duration, Instant},
};

use eframe::egui;
use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    query,
    utils::{
        ApiBackend, CameraFormat, CameraIndex, CameraInfo, FrameFormat, RequestedFormat,
        RequestedFormatType,
    },
};
use rayon::prelude::*;

const CHI_SQUARE_CHUNK_SIZE: usize = 4096;
const CHI_SQUARE_HISTORY_LIMIT: usize = 256;
const SOURCE_FRAME_FORMATS: [FrameFormat; 6] = [
    FrameFormat::YUYV,
    FrameFormat::MJPEG,
    FrameFormat::NV12,
    FrameFormat::GRAY,
    FrameFormat::RAWRGB,
    FrameFormat::RAWBGR,
];

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
    diff_chi_square_default_pos: egui::Pos2,
    devices: Vec<CameraInfo>,
    selected_device: Option<usize>,
    selected_frame_format: FrameFormat,
    stream: Option<CameraStream>,
    texture: Option<egui::TextureHandle>,
    diff_texture: Option<egui::TextureHandle>,
    diff_chi_square: ChiSquareTracker,
    status: String,
}

struct RgbFrame {
    size: [usize; 2],
    pixels: Vec<u8>,
}

#[derive(Default)]
struct ChiSquareTracker {
    pending: Vec<u8>,
    z_scores: VecDeque<f64>,
}

struct CameraStream {
    stop_signal: Arc<AtomicBool>,
    receiver: Receiver<StreamMessage>,
}

struct ProcessedFrame {
    size: [usize; 2],
    pixels: Vec<u8>,
    diff_pixels: Option<Vec<u8>>,
    extracted_noise_bytes: Option<Vec<u8>>,
}

enum StreamMessage {
    Status(String),
    Frame(ProcessedFrame),
    Error(String),
}

impl WebcamWindow {
    fn new() -> Self {
        let mut webcam = Self {
            default_pos: egui::pos2(48.0, 56.0),
            diff_default_pos: egui::pos2(600.0, 56.0),
            diff_chi_square_default_pos: egui::pos2(600.0, 520.0),
            devices: Vec::new(),
            selected_device: None,
            selected_frame_format: FrameFormat::YUYV,
            stream: None,
            texture: None,
            diff_texture: None,
            diff_chi_square: ChiSquareTracker::default(),
            status: String::new(),
        };

        webcam.refresh_devices();
        webcam
    }

    fn show(&mut self, ctx: &egui::Context) {
        if self.stream.is_some() {
            self.drain_stream_messages(ctx);
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        self.show_webcam_window(ctx);
        self.show_difference_window(ctx);
        self.show_chi_square_window(
            ctx,
            "Von Neumann LSB Chi-Square",
            "von_neumann_lsb_chi_square_window",
            self.diff_chi_square_default_pos,
            &self.diff_chi_square,
        );
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

                    ui.add_enabled_ui(self.stream.is_none(), |ui| {
                        self.format_selector(ui);
                    });

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
                        .add_enabled(self.stream.is_some(), egui::Button::new("Stop camera"))
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

    fn show_chi_square_window(
        &self,
        ctx: &egui::Context,
        title: &str,
        id: &'static str,
        default_pos: egui::Pos2,
        tracker: &ChiSquareTracker,
    ) {
        egui::Window::new(title)
            .id(egui::Id::new(id))
            .default_pos(default_pos)
            .default_size([520.0, 260.0])
            .resizable(true)
            .collapsible(true)
            .show(ctx, |ui| {
                chi_square_graph(ui, tracker);
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

    fn format_selector(&mut self, ui: &mut egui::Ui) {
        egui::ComboBox::from_id_salt("webcam_format_selector")
            .selected_text(self.selected_frame_format.to_string())
            .width(96.0)
            .show_ui(ui, |ui| {
                for format in SOURCE_FRAME_FORMATS {
                    ui.selectable_value(
                        &mut self.selected_frame_format,
                        format,
                        format.to_string(),
                    );
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
        let camera_index = device.index().clone();
        let camera_label = device_label(device);
        let frame_format = self.selected_frame_format;

        self.stop_camera();

        let (sender, receiver) = mpsc::sync_channel(2);
        let stop_signal = Arc::new(AtomicBool::new(false));

        spawn_camera_worker(
            camera_index,
            camera_label,
            frame_format,
            stop_signal.clone(),
            sender,
        );

        self.stream = Some(CameraStream {
            stop_signal,
            receiver,
        });
        self.texture = None;
        self.diff_texture = None;
        self.diff_chi_square.clear();
        self.status = "Starting camera...".to_owned();
    }

    fn stop_camera(&mut self) {
        if let Some(stream) = self.stream.take() {
            stream.stop_signal.store(true, Ordering::Relaxed);
        }

        self.texture = None;
        self.diff_texture = None;
        self.diff_chi_square.clear();
    }

    fn drain_stream_messages(&mut self, ctx: &egui::Context) {
        let mut disconnected = false;
        let mut latest_frame = None;

        for _ in 0..8 {
            let Some(message) =
                self.stream
                    .as_ref()
                    .and_then(|stream| match stream.receiver.try_recv() {
                        Ok(message) => Some(message),
                        Err(TryRecvError::Empty) => None,
                        Err(TryRecvError::Disconnected) => {
                            disconnected = true;
                            None
                        }
                    })
            else {
                break;
            };

            match message {
                StreamMessage::Status(status) => self.status = status,
                StreamMessage::Frame(frame) => latest_frame = Some(frame),
                StreamMessage::Error(error) => {
                    self.status = error;
                    self.stream = None;
                    self.texture = None;
                    self.diff_texture = None;
                    break;
                }
            }
        }

        if let Some(frame) = latest_frame {
            self.upload_processed_frame(ctx, frame);
        }

        if disconnected {
            self.stream = None;
        }
    }

    fn upload_processed_frame(&mut self, ctx: &egui::Context, frame: ProcessedFrame) {
        let image = egui::ColorImage::from_rgb(frame.size, &frame.pixels);

        if let Some(texture) = self.texture.as_mut() {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("webcam_frame", image, egui::TextureOptions::LINEAR));
        }

        if let Some(extracted_noise_bytes) = &frame.extracted_noise_bytes {
            self.diff_chi_square.push_bytes(extracted_noise_bytes);
        }

        if let Some(diff_pixels) = frame.diff_pixels {
            let diff_image = egui::ColorImage::from_rgb(frame.size, &diff_pixels);

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
}

impl ChiSquareTracker {
    fn push_bytes(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);

        while self.pending.len() >= CHI_SQUARE_CHUNK_SIZE {
            let chunk = self
                .pending
                .drain(..CHI_SQUARE_CHUNK_SIZE)
                .collect::<Vec<_>>();
            let z_score = byte_frequency_z_score(&chunk);

            if self.z_scores.len() == CHI_SQUARE_HISTORY_LIMIT {
                self.z_scores.pop_front();
            }
            self.z_scores.push_back(z_score);
        }
    }

    fn clear(&mut self) {
        self.pending.clear();
        self.z_scores.clear();
    }
}

fn byte_frequency_z_score(bytes: &[u8]) -> f64 {
    let mut counts = [0_usize; 256];
    for byte in bytes {
        counts[*byte as usize] += 1;
    }

    let expected = bytes.len() as f64 / 256.0;
    let chi_square = counts
        .iter()
        .map(|count| {
            let delta = *count as f64 - expected;
            delta * delta / expected
        })
        .sum::<f64>();

    let degrees_of_freedom = 255.0;
    (chi_square - degrees_of_freedom) / (2.0_f64 * degrees_of_freedom).sqrt()
}

fn chi_square_graph(ui: &mut egui::Ui, tracker: &ChiSquareTracker) {
    let z_scores = &tracker.z_scores;
    let desired_size = egui::vec2(ui.available_width(), 210.0);
    let (rect, _) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );

    let (min_z, max_z) = chi_square_y_range(z_scores);

    for z in [-5.0_f64, -3.0, 0.0, 3.0, 5.0] {
        if z < min_z || z > max_z {
            continue;
        }

        let y = z_to_y(rect, z, min_z, max_z);
        let color = if z == 0.0 {
            ui.visuals().widgets.noninteractive.bg_stroke.color
        } else {
            ui.visuals()
                .widgets
                .noninteractive
                .bg_stroke
                .color
                .linear_multiply(0.45)
        };
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, color),
        );
    }

    let label = match z_scores.back() {
        Some(z_score) => format!(
            "samples: {}  latest z: {:.2}  pending: {}/{} bytes",
            z_scores.len(),
            z_score,
            tracker.pending.len(),
            CHI_SQUARE_CHUNK_SIZE
        ),
        None => format!(
            "samples: 0  pending: {}/{} bytes",
            tracker.pending.len(),
            CHI_SQUARE_CHUNK_SIZE
        ),
    };

    painter.text(
        rect.left_top() + egui::vec2(8.0, 8.0),
        egui::Align2::LEFT_TOP,
        label,
        egui::TextStyle::Small.resolve(ui.style()),
        ui.visuals().weak_text_color(),
    );

    if z_scores.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("Waiting for {} bytes per chunk.", CHI_SQUARE_CHUNK_SIZE),
            egui::TextStyle::Body.resolve(ui.style()),
            ui.visuals().weak_text_color(),
        );
        return;
    }

    if z_scores.len() == 1 {
        let y = z_to_y(rect, *z_scores.front().unwrap(), min_z, max_z);
        painter.circle_filled(
            rect.center_top() + egui::vec2(0.0, y - rect.top()),
            3.0,
            egui::Color32::from_rgb(120, 180, 255),
        );
        return;
    }

    let last_index = (z_scores.len() - 1) as f32;
    let points = z_scores
        .iter()
        .enumerate()
        .map(|(idx, z_score)| {
            let x = egui::lerp(rect.left()..=rect.right(), idx as f32 / last_index);
            egui::pos2(x, z_to_y(rect, *z_score, min_z, max_z))
        })
        .collect::<Vec<_>>();

    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 180, 255)),
    ));
}

fn chi_square_y_range(z_scores: &VecDeque<f64>) -> (f64, f64) {
    let mut min_z = -6.0_f64;
    let mut max_z = 6.0_f64;

    for z_score in z_scores {
        min_z = min_z.min(*z_score);
        max_z = max_z.max(*z_score);
    }

    if (max_z - min_z).abs() < f64::EPSILON {
        (min_z - 1.0, max_z + 1.0)
    } else {
        let padding = (max_z - min_z) * 0.08;
        (min_z - padding, max_z + padding)
    }
}

fn z_to_y(rect: egui::Rect, z_score: f64, min_z: f64, max_z: f64) -> f32 {
    let fraction = ((z_score - min_z) / (max_z - min_z)) as f32;
    egui::lerp(rect.bottom()..=rect.top(), fraction)
}

fn absolute_frame_difference(current: &[u8], previous: &[u8]) -> Vec<u8> {
    current
        .par_iter()
        .zip(previous.par_iter())
        .map(|(current, previous)| current.abs_diff(*previous))
        .collect()
}

fn extract_lsb_transition_bytes(current: &[u8], previous: &[u8]) -> Vec<u8> {
    let mut packer = BitPacker::with_capacity(current.len() / 16);
    let mut pending_bit = None;

    for (current, previous) in current.iter().zip(previous) {
        let bit = (current ^ previous) & 1;

        if let Some(first_bit) = pending_bit.take() {
            match (first_bit, bit) {
                (0, 1) => packer.push_bit(0),
                (1, 0) => packer.push_bit(1),
                _ => {}
            }
        } else {
            pending_bit = Some(bit);
        }
    }

    packer.finish()
}

struct BitPacker {
    bytes: Vec<u8>,
    byte: u8,
    bit_index: u8,
}

impl BitPacker {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
            byte: 0,
            bit_index: 0,
        }
    }

    fn push_bit(&mut self, bit: u8) {
        self.byte |= (bit & 1) << self.bit_index;
        self.bit_index += 1;

        if self.bit_index == 8 {
            self.bytes.push(self.byte);
            self.byte = 0;
            self.bit_index = 0;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bit_index != 0 {
            self.bytes.push(self.byte);
        }

        self.bytes
    }
}

fn spawn_camera_worker(
    index: CameraIndex,
    label: String,
    source_format: FrameFormat,
    stop_signal: Arc<AtomicBool>,
    sender: mpsc::SyncSender<StreamMessage>,
) {
    let _ = std::thread::Builder::new()
        .name("camera-worker".to_owned())
        .spawn(move || {
            run_camera_worker(index, label, source_format, stop_signal, sender);
        });
}

fn run_camera_worker(
    index: CameraIndex,
    label: String,
    source_format: FrameFormat,
    stop_signal: Arc<AtomicBool>,
    sender: mpsc::SyncSender<StreamMessage>,
) {
    let requested = RequestedFormat::with_formats(RequestedFormatType::None, &SOURCE_FRAME_FORMATS);

    let mut camera = match Camera::new(index, requested) {
        Ok(camera) => camera,
        Err(error) => {
            send_stream_message(
                &sender,
                StreamMessage::Error(format!("Could not create camera: {error}")),
            );
            return;
        }
    };

    match camera.compatible_camera_formats() {
        Ok(formats) => {
            let Some(format) = best_source_format(&formats, source_format) else {
                send_stream_message(
                    &sender,
                    StreamMessage::Error(format!(
                        "Selected camera does not report any {source_format} formats."
                    )),
                );
                return;
            };

            let accepted_formats = [source_format];
            let requested_format = RequestedFormat::with_formats(
                RequestedFormatType::Exact(format),
                &accepted_formats,
            );
            if let Err(error) = camera.set_camera_requset(requested_format) {
                send_stream_message(
                    &sender,
                    StreamMessage::Error(format!(
                        "Could not set {source_format} camera format: {error}"
                    )),
                );
                return;
            }
        }
        Err(error) if camera.camera_format().format() != source_format => {
            send_stream_message(
                &sender,
                StreamMessage::Error(format!(
                    "Could not choose {source_format} camera format: {error}"
                )),
            );
            return;
        }
        Err(_) => {}
    }

    if let Err(error) = camera.open_stream() {
        send_stream_message(
            &sender,
            StreamMessage::Error(format!("Could not open camera stream: {error}")),
        );
        return;
    }

    let format = camera.camera_format();
    send_stream_message(
        &sender,
        StreamMessage::Status(format!(
            "Streaming {label} at {}x{} {} FPS, {} source.",
            format.width(),
            format.height(),
            format.frame_rate(),
            format.format()
        )),
    );

    let mut previous_frame: Option<RgbFrame> = None;

    while !stop_signal.load(Ordering::Relaxed) {
        let frame_started_at = Instant::now();

        let frame = match camera.frame() {
            Ok(frame) => frame,
            Err(error) => {
                send_stream_message(
                    &sender,
                    StreamMessage::Error(format!("Could not read frame: {error}")),
                );
                return;
            }
        };

        let decoded = match frame.decode_image::<RgbFormat>() {
            Ok(decoded) => decoded,
            Err(error) => {
                send_stream_message(
                    &sender,
                    StreamMessage::Error(format!("Could not decode frame: {error}")),
                );
                return;
            }
        };

        let size = [decoded.width() as usize, decoded.height() as usize];
        let current_pixels = decoded.as_raw().to_vec();

        let (diff_pixels, extracted_noise_bytes) = previous_frame
            .as_ref()
            .filter(|previous| {
                previous.size == size && previous.pixels.len() == current_pixels.len()
            })
            .map(|previous| {
                (
                    absolute_frame_difference(&current_pixels, &previous.pixels),
                    extract_lsb_transition_bytes(&current_pixels, &previous.pixels),
                )
            })
            .map_or((None, None), |(diff_pixels, extracted_noise_bytes)| {
                (Some(diff_pixels), Some(extracted_noise_bytes))
            });

        previous_frame = Some(RgbFrame {
            size,
            pixels: current_pixels.clone(),
        });

        send_stream_message(
            &sender,
            StreamMessage::Frame(ProcessedFrame {
                size,
                pixels: current_pixels,
                diff_pixels,
                extracted_noise_bytes,
            }),
        );

        let elapsed = frame_started_at.elapsed();
        if elapsed < Duration::from_millis(16) {
            std::thread::sleep(Duration::from_millis(16) - elapsed);
        }
    }
}

fn best_source_format(
    formats: &[CameraFormat],
    source_format: FrameFormat,
) -> Option<CameraFormat> {
    formats
        .iter()
        .copied()
        .filter(|format| format.format() == source_format)
        .max_by_key(|format| (format.frame_rate(), format.resolution()))
}

fn send_stream_message(sender: &mpsc::SyncSender<StreamMessage>, message: StreamMessage) {
    match sender.try_send(message) {
        Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
    }
}

fn device_label(device: &CameraInfo) -> String {
    let name = device.human_name();
    if name.trim().is_empty() {
        format!("Camera {}", device.index())
    } else {
        name
    }
}
