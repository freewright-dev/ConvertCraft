#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod converter;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use converter::{Category, Options};
use eframe::egui::{self, Color32, RichText};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Transmute")
            .with_inner_size([860.0, 580.0])
            .with_min_inner_size([620.0, 400.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "Transmute",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}

enum Status {
    Pending,
    Running,
    Done(PathBuf),
    Failed(String),
}

struct FileItem {
    path: PathBuf,
    category: Category,
    target: &'static str,
    status: Status,
}

enum Msg {
    Started(usize),
    Finished(usize, Result<PathBuf, String>),
}

struct Job {
    idx: usize,
    input: PathBuf,
    output: PathBuf,
    category: Category,
    target: &'static str,
}

struct App {
    files: Vec<FileItem>,
    out_dir: Option<PathBuf>,
    opts: Options,
    ffmpeg: bool,
    rx: Option<Receiver<Msg>>,
    total: usize,
    finished: usize,
    skipped: usize,
}

impl App {
    fn new() -> Self {
        Self {
            files: Vec::new(),
            out_dir: None,
            opts: Options::default(),
            ffmpeg: converter::ffmpeg_available(),
            rx: None,
            total: 0,
            finished: 0,
            skipped: 0,
        }
    }

    fn running(&self) -> bool {
        self.rx.is_some()
    }

    fn add_path(&mut self, path: PathBuf) {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                let mut children: Vec<_> = entries.flatten().map(|e| e.path()).collect();
                children.sort();
                for child in children {
                    self.add_path(child);
                }
            }
            return;
        }
        if self.files.iter().any(|f| f.path == path) {
            return;
        }
        match converter::detect(&path) {
            Some(category) => self.files.push(FileItem {
                target: converter::default_target(&path, category),
                path,
                category,
                status: Status::Pending,
            }),
            None => self.skipped += 1,
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let mut reserved = HashSet::new();
        let mut jobs = Vec::new();
        for (idx, f) in self.files.iter_mut().enumerate() {
            if matches!(f.status, Status::Done(_)) {
                continue;
            }
            if f.category.needs_ffmpeg() && !self.ffmpeg {
                f.status = Status::Failed("ffmpeg not found on PATH".into());
                continue;
            }
            let output = converter::plan_output(
                &f.path,
                self.out_dir.as_deref(),
                f.target,
                self.opts.overwrite,
                &mut reserved,
            );
            f.status = Status::Pending;
            jobs.push(Job { idx, input: f.path.clone(), output, category: f.category, target: f.target });
        }
        if jobs.is_empty() {
            return;
        }

        self.total = jobs.len();
        self.finished = 0;
        jobs.reverse(); // workers pop from the end; keep list order
        let queue = Arc::new(Mutex::new(jobs));
        let (tx, rx) = mpsc::channel();
        let workers = thread::available_parallelism().map_or(2, |n| n.get()).min(4);

        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let tx = tx.clone();
            let ctx = ctx.clone();
            let opts = self.opts.clone();
            thread::spawn(move || loop {
                let Some(job) = queue.lock().unwrap().pop() else { break };
                let _ = tx.send(Msg::Started(job.idx));
                ctx.request_repaint();
                let result = converter::convert(&job.input, &job.output, job.category, job.target, &opts)
                    .map(|()| job.output)
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(Msg::Finished(job.idx, result));
                ctx.request_repaint();
            });
        }
        self.rx = Some(rx);
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        loop {
            match rx.try_recv() {
                Ok(Msg::Started(i)) => self.files[i].status = Status::Running,
                Ok(Msg::Finished(i, result)) => {
                    self.finished += 1;
                    self.files[i].status = match result {
                        Ok(path) => Status::Done(path),
                        Err(e) => Status::Failed(e),
                    };
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.rx = None;
                    break;
                }
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Transmute");
            ui.separator();
            ui.add_enabled_ui(!self.running(), |ui| {
                if ui.button("➕ Add files…").clicked() {
                    if let Some(paths) = rfd::FileDialog::new().pick_files() {
                        paths.into_iter().for_each(|p| self.add_path(p));
                    }
                }
                if ui.button("📁 Add folder…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.add_path(dir);
                    }
                }
                if ui.button("Clear finished").clicked() {
                    self.files.retain(|f| !matches!(f.status, Status::Done(_)));
                }
                if ui.button("Clear all").clicked() {
                    self.files.clear();
                    self.skipped = 0;
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.ffmpeg {
                    ui.label(RichText::new("ffmpeg ✔").color(Color32::from_rgb(90, 180, 110)))
                        .on_hover_text("Audio and video conversion available");
                } else {
                    ui.label(RichText::new("ffmpeg missing").color(Color32::from_rgb(220, 160, 60)))
                        .on_hover_text("Install ffmpeg and add it to PATH to convert audio and video");
                }
            });
        });
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.add_enabled_ui(!self.running(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Output:");
                if ui.radio(self.out_dir.is_none(), "Next to source file").clicked() {
                    self.out_dir = None;
                }
                let label = match &self.out_dir {
                    Some(dir) => format!("Folder: {}", dir.display()),
                    None => "Choose folder…".to_string(),
                };
                if ui.radio(self.out_dir.is_some(), label).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.out_dir = Some(dir);
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("JPEG quality:");
                ui.add(egui::Slider::new(&mut self.opts.jpeg_quality, 10..=100));
                ui.separator();
                ui.checkbox(&mut self.opts.overwrite, "Overwrite existing files");
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let pending = self.files.iter().filter(|f| !matches!(f.status, Status::Done(_))).count();
            let button = egui::Button::new(RichText::new("Convert").size(16.0).strong())
                .min_size(egui::vec2(120.0, 32.0));
            if ui.add_enabled(!self.running() && pending > 0, button).clicked() {
                self.start(ui.ctx());
            }
            if self.total > 0 {
                let fraction = self.finished as f32 / self.total as f32;
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .text(format!("{} / {}", self.finished, self.total))
                        .animate(self.running()),
                );
            }
        });
        ui.add_space(6.0);
    }

    fn file_list(&mut self, ui: &mut egui::Ui) {
        if self.files.is_empty() {
            let response = ui.centered_and_justified(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new("Drop files or folders here\nor click to browse")
                            .size(22.0)
                            .weak(),
                    )
                    .sense(egui::Sense::click()),
                )
            });
            if response.inner.clicked() {
                if let Some(paths) = rfd::FileDialog::new().pick_files() {
                    paths.into_iter().for_each(|p| self.add_path(p));
                }
            }
            return;
        }

        let running = self.running();

        // Bulk target selection for each category present.
        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Convert all:");
                for cat in Category::ALL {
                    if !self.files.iter().any(|f| f.category == cat) {
                        continue;
                    }
                    egui::ComboBox::from_id_salt(("all", cat))
                        .selected_text(cat.label())
                        .width(80.0)
                        .show_ui(ui, |ui| {
                            for &t in cat.targets() {
                                if ui.selectable_label(false, format!("→ {t}")).clicked() {
                                    self.files
                                        .iter_mut()
                                        .filter(|f| f.category == cat)
                                        .for_each(|f| f.target = t);
                                }
                            }
                        });
                }
                if self.skipped > 0 {
                    ui.label(RichText::new(format!("({} unsupported files skipped)", self.skipped)).weak());
                }
            });
        });
        ui.separator();

        let mut remove = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("files")
                .num_columns(5)
                .striped(true)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.strong("File");
                    ui.strong("Type");
                    ui.strong("Convert to");
                    ui.strong("Status");
                    ui.label("");
                    ui.end_row();

                    for (i, f) in self.files.iter_mut().enumerate() {
                        let name = f.path.file_name().map_or_else(
                            || f.path.display().to_string(),
                            |n| n.to_string_lossy().into_owned(),
                        );
                        ui.label(name).on_hover_text(f.path.display().to_string());
                        ui.label(f.category.label());

                        ui.add_enabled_ui(!running, |ui| {
                            egui::ComboBox::from_id_salt(("target", i))
                                .selected_text(f.target)
                                .width(70.0)
                                .show_ui(ui, |ui| {
                                    for &t in f.category.targets() {
                                        ui.selectable_value(&mut f.target, t, t);
                                    }
                                });
                        });

                        match &f.status {
                            Status::Pending => {
                                ui.label(RichText::new("Pending").weak());
                            }
                            Status::Running => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label("Converting…");
                                });
                            }
                            Status::Done(out) => {
                                let link = ui
                                    .link(RichText::new("Done").color(Color32::from_rgb(90, 180, 110)))
                                    .on_hover_text(format!("{}\nClick to show in folder", out.display()));
                                if link.clicked() {
                                    reveal(out);
                                }
                            }
                            Status::Failed(err) => {
                                ui.label(RichText::new("Failed").color(Color32::from_rgb(220, 90, 90)))
                                    .on_hover_text(err);
                            }
                        }

                        if ui.add_enabled(!running, egui::Button::new("✖").small()).clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
        });
        if let Some(i) = remove {
            self.files.remove(i);
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !self.running() {
            dropped.into_iter().for_each(|p| self.add_path(p));
        }

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
            let rect = ctx.screen_rect();
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(160));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop to add",
                egui::FontId::proportional(28.0),
                Color32::WHITE,
            );
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        self.handle_drops(ctx);

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(4.0);
        });
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| self.bottom_bar(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.file_list(ui));
    }
}

/// Open the system file manager with the converted file selected.
fn reveal(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(dir) = path.parent() {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}
