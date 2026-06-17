use eframe::egui;

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
    next_window_id: usize,
    windows: Vec<WorkspaceWindow>,
}

impl Default for WorkspaceApp {
    fn default() -> Self {
        let mut app = Self {
            next_window_id: 1,
            windows: Vec::new(),
        };
        app.spawn_window();
        app
    }
}

impl WorkspaceApp {
    fn spawn_window(&mut self) {
        let id = self.next_window_id;
        self.next_window_id += 1;

        let stagger = ((id - 1) % 8) as f32 * 28.0;
        self.windows.push(WorkspaceWindow {
            id,
            title: format!("Window {id}"),
            default_pos: egui::pos2(72.0 + stagger, 96.0 + stagger),
            click_count: 0,
        });
    }
}

struct WorkspaceWindow {
    id: usize,
    title: String,
    default_pos: egui::Pos2,
    click_count: usize,
}

impl eframe::App for WorkspaceApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.allocate_space(ui.available_size());
        });

        let ctx = ui.ctx().clone();

        for window in &mut self.windows {
            egui::Window::new(window.title.as_str())
                .id(egui::Id::new(("workspace_window", window.id)))
                .default_pos(window.default_pos)
                .default_size([260.0, 150.0])
                .resizable(true)
                .collapsible(true)
                .show(&ctx, |ui| {
                    ui.label("Drag this window by its title bar.");
                    ui.label("Resize it from the edges.");
                    ui.separator();

                    if ui.button("Count click").clicked() {
                        window.click_count += 1;
                    }

                    ui.label(format!("Clicks: {}", window.click_count));
                });
        }
    }
}
