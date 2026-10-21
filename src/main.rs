#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

mod converter;
mod import;
mod output;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use converter::{Category, Options};
use eframe::egui::{self, Color32, RichText};

const APP_NAME: &str = "ConvertCraft";
/// Import errors kept for the tooltip; the total is still counted.
const MAX_REPORTED_ERRORS: usize = 50;

fn main() -> eframe::Result {
    let logo = image::load_from_memory(include_bytes!("../assets/logos/logo.png"))
        .expect("embedded logo must be a valid image")
        .resize(256, 256, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    let icon = egui::IconData {
        width: logo.width(),
        height: logo.height(),
        rgba: logo.into_raw(),
    };
    let logo = egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.rgba,
    );
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_icon(icon)
            .with_inner_size([860.0, 580.0])
            .with_min_inner_size([620.0, 400.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(App::new(&cc.egui_ctx, logo)))),
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
    output: output::OutputPlan,
    category: Category,
    target: &'static str,
}

/// A folder/file import running on a background thread.
struct Import {
    rx: Receiver<import::Event>,
    cancel: Arc<AtomicBool>,
    // Paths already queued, so duplicates are dropped in O(1).
    queued: HashSet<PathBuf>,
    found: usize,
}

#[derive(Default)]
struct ImportReport {
    unsupported: usize,
    links: usize,
    too_deep: usize,
    errors: Vec<String>,
    error_count: usize,
    stopped: Option<String>,
}

impl ImportReport {
    fn is_empty(&self) -> bool {
        self.unsupported == 0
            && self.links == 0
            && self.too_deep == 0
            && self.error_count == 0
            && self.stopped.is_none()
    }
}

struct App {
    logo: egui::TextureHandle,
    files: Vec<FileItem>,
    out_dir: Option<PathBuf>,
    opts: Options,
    ffmpeg: bool,
    rx: Option<Receiver<Msg>>,
    total: usize,
    finished: usize,
    import: Option<Import>,
    report: ImportReport,
}

impl App {
    fn new(ctx: &egui::Context, logo: egui::ColorImage) -> Self {
        Self {
            logo: ctx.load_texture("convertcraft-logo", logo, egui::TextureOptions::LINEAR),
            files: Vec::new(),
            out_dir: None,
            opts: Options::default(),
            ffmpeg: converter::ffmpeg_available(),
            rx: None,
            total: 0,
            finished: 0,
            import: None,
            report: ImportReport::default(),
        }
    }

    fn running(&self) -> bool {
        self.rx.is_some()
    }

    /// Converting or importing; the queue must not change.
    fn busy(&self) -> bool {
        self.running() || self.import.is_some()
    }

    fn add_paths(&mut self, ctx: &egui::Context, roots: Vec<PathBuf>) {
        if roots.is_empty() || self.busy() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let mut sent = 0usize;
            import::scan(roots, &import::Limits::default(), &flag, |event| {
                let _ = tx.send(event);
                sent += 1;
                if sent.is_multiple_of(256) {
                    ctx.request_repaint();
                }
            });
            // Disconnect before the final repaint so the UI sees the scan end.
            drop(tx);
            ctx.request_repaint();
        });
        self.import = Some(Import {
            rx,
            cancel,
            queued: self.files.iter().map(|f| f.path.clone()).collect(),
            found: 0,
        });
    }

    fn poll_import(&mut self) {
        let Some(import) = &mut self.import else {
            return;
        };
        loop {
            match import.rx.try_recv() {
                Ok(import::Event::File(path, category)) => {
                    if import.queued.insert(path.clone()) {
                        import.found += 1;
                        self.files.push(FileItem {
                            target: converter::default_target(&path, category),
                            path,
                            category,
                            status: Status::Pending,
                        });
                    }
                }
                Ok(import::Event::Skipped(import::Skip::Unsupported)) => {
                    self.report.unsupported += 1;
                }
                Ok(import::Event::Skipped(import::Skip::Link)) => self.report.links += 1,
                Ok(import::Event::Skipped(import::Skip::TooDeep)) => {
                    self.report.too_deep += 1;
                }
                Ok(import::Event::Error(path, error)) => {
                    self.report.error_count += 1;
                    if self.report.errors.len() < MAX_REPORTED_ERRORS {
                        self.report
                            .errors
                            .push(format!("{}: {error}", path.display()));
                    }
                }
                Ok(import::Event::Stopped(message)) => self.report.stopped = Some(message),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.import = None;
                    break;
                }
            }
        }
    }

    fn import_status(&self, ui: &mut egui::Ui) {
        if let Some(import) = &self.import {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Scanning… {} files found", import.found));
                if ui.button("Cancel").clicked() {
                    import.cancel.store(true, Ordering::Relaxed);
                }
            });
        }
        let report = &self.report;
        if report.is_empty() {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if report.unsupported > 0 {
                ui.label(
                    RichText::new(format!("{} unsupported files skipped", report.unsupported))
                        .weak(),
                );
            }
            if report.links > 0 {
                ui.label(RichText::new(format!("{} links not followed", report.links)).weak())
                    .on_hover_text("Symbolic links and junctions inside folders are skipped");
            }
            if report.too_deep > 0 {
                ui.label(
                    RichText::new(format!("{} folders nested too deeply", report.too_deep)).weak(),
                );
            }
            if report.error_count > 0 {
                let mut details = report.errors.join("\n");
                let hidden = report.error_count - report.errors.len();
                if hidden > 0 {
                    details.push_str(&format!("\n… and {hidden} more"));
                }
                ui.label(
                    RichText::new(format!("{} import errors", report.error_count))
                        .color(Color32::from_rgb(220, 90, 90)),
                )
                .on_hover_text(details);
            }
            if let Some(stopped) = &report.stopped {
                ui.label(RichText::new(stopped).color(Color32::from_rgb(220, 160, 60)));
            }
        });
    }

    fn start(&mut self, ctx: &egui::Context) {
        // Every queued source is protected, including completed rows. A source that
        // cannot be inspected fails only its own row (and is ignored if already done).
        let (mut planner, errors) =
            output::OutputPlanner::new(self.files.iter().map(|file| file.path.as_path()));
        let mut errors: HashMap<usize, anyhow::Error> = errors.into_iter().collect();
        let mut jobs = Vec::new();
        for (idx, f) in self.files.iter_mut().enumerate() {
            if matches!(f.status, Status::Done(_)) {
                continue;
            }
            if let Some(error) = errors.remove(&idx) {
                f.status = Status::Failed(format!("{error:#}"));
                continue;
            }
            if f.category.needs_ffmpeg() && !self.ffmpeg {
                f.status = Status::Failed("ffmpeg not found on PATH".into());
                continue;
            }
            let output = match planner.plan(
                &f.path,
                self.out_dir.as_deref(),
                f.target,
                self.opts.overwrite,
            ) {
                Ok(output) => output,
                Err(error) => {
                    f.status = Status::Failed(format!("{error:#}"));
                    continue;
                }
            };
            f.status = Status::Pending;
            jobs.push(Job {
                idx,
                input: f.path.clone(),
                output,
                category: f.category,
                target: f.target,
            });
        }
        if jobs.is_empty() {
            return;
        }

        self.total = jobs.len();
        self.finished = 0;
        jobs.reverse(); // workers pop from the end; keep list order
        let queue = Arc::new(Mutex::new(jobs));
        let (tx, rx) = mpsc::channel();
        let workers = thread::available_parallelism()
            .map_or(2, |n| n.get())
            .min(4);

        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let tx = tx.clone();
            let ctx = ctx.clone();
            let opts = self.opts.clone();
            thread::spawn(move || {
                loop {
                    let Some(job) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    let _ = tx.send(Msg::Started(job.idx));
                    ctx.request_repaint();
                    let result =
                        converter::convert(&job.input, job.output, job.category, job.target, &opts)
                            .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(Msg::Finished(job.idx, result));
                    ctx.request_repaint();
                }
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
            ui.image((self.logo.id(), egui::vec2(36.0, 36.0)));
            ui.vertical(|ui| {
                ui.heading(APP_NAME);
                ui.label("Images, data, audio & video. Converted locally.");
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.ffmpeg {
                    ui.label(RichText::new("ffmpeg ✔").color(Color32::from_rgb(90, 180, 110)))
                        .on_hover_text("Audio and video conversion available");
                } else {
                    ui.label(
                        RichText::new("ffmpeg missing").color(Color32::from_rgb(220, 160, 60)),
                    )
                    .on_hover_text("Install ffmpeg and add it to PATH to convert audio and video");
                }
            });
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!self.busy(), |ui| {
                if ui.button("➕ Add files…").clicked()
                    && let Some(paths) = rfd::FileDialog::new().pick_files()
                {
                    self.add_paths(ui.ctx(), paths);
                }
                if ui.button("📁 Add folder…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.add_paths(ui.ctx(), vec![dir]);
                }
                if ui.button("Clear finished").clicked() {
                    self.files.retain(|f| !matches!(f.status, Status::Done(_)));
                }
                if ui.button("Clear all").clicked() {
                    self.files.clear();
                    self.report = ImportReport::default();
                }
            });
        });
        self.import_status(ui);
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.add_enabled_ui(!self.running(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Output:");
                if ui
                    .radio(self.out_dir.is_none(), "Next to source file")
                    .clicked()
                {
                    self.out_dir = None;
                }
                let label = match &self.out_dir {
                    Some(dir) => format!("Folder: {}", dir.display()),
                    None => "Choose folder…".to_string(),
                };
                if ui.radio(self.out_dir.is_some(), label).clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.out_dir = Some(dir);
                }
            });
            ui.horizontal(|ui| {
                ui.label("JPEG quality:");
                ui.add(egui::Slider::new(&mut self.opts.jpeg_quality, 10..=100));
                ui.separator();
                ui.checkbox(&mut self.opts.overwrite, "Overwrite existing files");
                ui.separator();
                ui.checkbox(
                    &mut self.opts.csv_formula_guard,
                    "Safe CSV for spreadsheets",
                )
                .on_hover_text(
                    "Prefix CSV cells starting with = + - @ tab or CR with an apostrophe \
                         so spreadsheets show them as text instead of running them as formulas",
                );
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let pending = self
                .files
                .iter()
                .filter(|f| !matches!(f.status, Status::Done(_)))
                .count();
            let button = egui::Button::new(RichText::new("Convert").size(16.0).strong())
                .min_size(egui::vec2(120.0, 32.0));
            if ui
                .add_enabled(!self.busy() && pending > 0, button)
                .clicked()
            {
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
            let response = ui.vertical_centered(|ui| {
                ui.add_space(((ui.available_height() - 190.0) / 2.0).max(0.0));
                ui.image((self.logo.id(), egui::vec2(96.0, 96.0)));
                ui.add_space(12.0);
                ui.add(
                    egui::Label::new(
                        RichText::new("Drop files or folders here\nor click to browse").size(22.0),
                    )
                    .sense(egui::Sense::click()),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
            });
            if response.inner.clicked()
                && let Some(paths) = rfd::FileDialog::new().pick_files()
            {
                self.add_paths(ui.ctx(), paths);
            }
            return;
        }

        let running = self.busy();

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
            });
        });
        ui.separator();

        let mut remove = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
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
                                    let link =
                                        ui.link(
                                            RichText::new("Done")
                                                .color(Color32::from_rgb(90, 180, 110)),
                                        )
                                        .on_hover_text(
                                            format!("{}\nClick to show in folder", out.display()),
                                        );
                                    if link.clicked() {
                                        reveal(out);
                                    }
                                }
                                Status::Failed(err) => {
                                    ui.label(
                                        RichText::new("Failed")
                                            .color(Color32::from_rgb(220, 90, 90)),
                                    )
                                    .on_hover_text(err);
                                }
                            }

                            if ui
                                .add_enabled(!running, egui::Button::new("✖").small())
                                .clicked()
                            {
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
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !self.busy() {
            self.add_paths(ctx, dropped);
        }

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop"),
            ));
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
        self.poll_import();
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
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(dir) = path.parent() {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}
