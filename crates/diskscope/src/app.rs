//! Application state and the window's three bands: toolbar, map, readout.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use diskscope_core::summary::{kind_totals, largest_files, KindTotal};
use diskscope_core::tree::flags;
use diskscope_core::{fmt, KindTable, NodeId, ScanOptions, Tree, ROOT};
use egui::{Sense, Stroke, StrokeKind};

use crate::canvas::{Canvas, CanvasInput};
use crate::sidebar::{self, Sidebar, Tab};
use crate::theme::{self, color};
use crate::{actions, job::ScanJob};

/// How many files the Largest tab lists.
const LARGEST_COUNT: usize = 40;

/// Smallest block, in points², that the map will subdivide down to. Below this
/// a subtree is drawn as one aggregate block.
const MIN_BLOCK_AREA: f32 = 9.0;

pub enum Action {
    Select(NodeId),
    Drill(NodeId),
    GoUp,
    Reveal(NodeId),
    Open(NodeId),
    CopyPath(NodeId),
    ConfirmTrash(NodeId),
    ToggleKind(u16),
    Rescan,
    PickFolder,
    ScanPath(PathBuf),
}

pub struct App {
    kinds: Arc<KindTable>,
    kinds_warning: Option<String>,
    scan_opts: ScanOptions,

    tree: Option<Tree>,
    generation: u64,
    job: Option<ScanJob>,
    last_root: Option<PathBuf>,

    view_root: NodeId,
    selected: Option<NodeId>,
    hovered: Option<NodeId>,
    highlight: Option<u16>,
    expanded: HashSet<NodeId>,
    tab: Tab,
    physical: bool,
    collapse_packages: bool,

    totals: Vec<KindTotal>,
    largest: Vec<NodeId>,

    canvas: Canvas,
    notice: Option<(String, Instant)>,
    pending_trash: Option<NodeId>,
    show_settings: bool,
    screenshot: Option<Screenshot>,
}

/// Drives `--screenshot`: wait for the scan, let the map render, ask the
/// window to capture itself, save the reply, quit.
///
/// The app photographs its own framebuffer rather than going through
/// `screencapture`, which needs Screen Recording permission and cannot be
/// scripted on a fresh machine.
struct Screenshot {
    path: PathBuf,
    /// Frames to let pass after the scan lands, so the map texture is up.
    warmup: u32,
    requested: bool,
    /// Frames waited in total, so a scan that never finishes still exits.
    waited: u32,
}

/// Frames between "the tree is ready" and asking for the capture.
const SHOT_WARMUP_FRAMES: u32 = 4;
/// Roughly 30 s at the repaint rate below, then give up.
const SHOT_MAX_FRAMES: u32 = 2000;

impl App {
    /// Builds the window. With `shot` set it scans, photographs itself into
    /// that file and quits — see [`Screenshot`].
    pub fn new(ctx: &egui::Context, start: Option<PathBuf>, shot: Option<PathBuf>) -> Self {
        theme::install(ctx);
        let (kinds, kinds_warning) = KindTable::load_or_builtin();
        let mut app = Self {
            kinds: Arc::new(kinds),
            kinds_warning,
            scan_opts: ScanOptions::default(),
            tree: None,
            generation: 0,
            job: None,
            last_root: None,
            view_root: ROOT,
            selected: None,
            hovered: None,
            highlight: None,
            expanded: HashSet::new(),
            tab: Tab::default(),
            physical: true,
            collapse_packages: true,
            totals: Vec::new(),
            largest: Vec::new(),
            canvas: Canvas::default(),
            notice: None,
            pending_trash: None,
            show_settings: false,
            screenshot: shot.map(|path| Screenshot {
                path,
                warmup: SHOT_WARMUP_FRAMES,
                requested: false,
                waited: 0,
            }),
        };
        if let Some(path) = start {
            app.start_scan(path);
        }
        app
    }

    fn start_scan(&mut self, root: PathBuf) {
        self.last_root = Some(root.clone());
        self.job = Some(ScanJob::spawn(
            root,
            self.scan_opts.clone(),
            Arc::clone(&self.kinds),
        ));
    }

    fn adopt(&mut self, tree: Tree) {
        self.generation += 1;
        self.view_root = ROOT;
        self.selected = None;
        self.hovered = None;
        self.highlight = None;
        self.expanded.clear();
        self.tree = Some(tree);
        self.canvas.invalidate();
        self.recompute();
    }

    /// Rollups for whatever subtree is in view. Cheap enough to redo on every
    /// change of view or metric, so nothing is cached per node.
    fn recompute(&mut self) {
        let Some(tree) = &self.tree else { return };
        self.totals = kind_totals(tree, self.view_root, self.kinds.len(), self.physical);
        self.largest = largest_files(tree, self.view_root, LARGEST_COUNT, self.physical);
    }

    fn note(&mut self, message: impl Into<String>) {
        self.notice = Some((message.into(), Instant::now()));
    }

    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Select(id) => self.selected = Some(id),
            Action::Drill(id) => {
                let Some(tree) = &self.tree else { return };
                // Drilling into a file would leave nothing to draw, so a file
                // drills into its folder instead.
                let target = if tree.node(id).is_dir() && tree.node(id).child_len > 0 {
                    id
                } else {
                    tree.node(id).parent
                };
                if target != self.view_root {
                    self.view_root = target;
                    self.selected = None;
                    self.canvas.invalidate();
                    self.recompute();
                }
            }
            Action::GoUp => {
                if self.view_root != ROOT {
                    if let Some(tree) = &self.tree {
                        self.view_root = tree.node(self.view_root).parent;
                        self.canvas.invalidate();
                        self.recompute();
                    }
                }
            }
            Action::Reveal(id) => {
                if let Some(tree) = &self.tree {
                    let path = tree.path(id);
                    if let Err(e) = actions::reveal_in_finder(&path) {
                        self.note(format!("Can't show that in the Finder: {e}"));
                    }
                }
            }
            Action::Open(id) => {
                if let Some(tree) = &self.tree {
                    let path = tree.path(id);
                    if let Err(e) = actions::open_path(&path) {
                        self.note(format!("Can't open that: {e}"));
                    }
                }
            }
            Action::CopyPath(id) => {
                if let Some(tree) = &self.tree {
                    let path = tree.display_path(id);
                    ctx.copy_text(path.clone());
                    self.note(format!("Copied {path}"));
                }
            }
            Action::ConfirmTrash(id) => self.pending_trash = Some(id),
            Action::ToggleKind(kind) => {
                self.highlight = if self.highlight == Some(kind) {
                    None
                } else {
                    Some(kind)
                };
                self.canvas.invalidate();
            }
            Action::Rescan => {
                if let Some(root) = self.last_root.clone() {
                    self.start_scan(root);
                }
            }
            Action::PickFolder => {
                if let Some(path) = actions::pick_folder(self.last_root.as_deref()) {
                    self.start_scan(path);
                }
            }
            Action::ScanPath(path) => self.start_scan(path),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

impl App {
    /// The whole window, independent of eframe so it can be driven by a bare
    /// [`egui::Context`] in tests.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.collect_finished_scan();
        self.accept_dropped_folder(&ctx);
        let mut queue = self.keyboard(&ctx);

        egui::Panel::top("toolbar")
            .frame(band(10.0))
            .show(ui, |ui| queue.extend(self.toolbar(ui)));

        egui::Panel::bottom("readout")
            .frame(band(8.0))
            .show(ui, |ui| queue.extend(self.readout(ui)));

        if self.tree.is_some() {
            egui::Panel::left("rail")
                .frame(band(10.0))
                .default_size(268.0)
                .size_range(220.0..=420.0)
                .show(ui, |ui| queue.extend(self.rail(ui)));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(color::CHART))
            .show(ui, |ui| queue.extend(self.map(ui)));

        self.trash_dialog(&ctx);
        self.settings_window(&ctx);

        for action in queue {
            self.apply(action, &ctx);
        }

        self.drive_screenshot(&ctx);

        if self.is_scanning() {
            // Live counters are only worth animating while they move.
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed().as_secs_f32() > 6.0)
        {
            self.notice = None;
        }
    }

    fn is_scanning(&self) -> bool {
        self.job.is_some()
    }

    fn drive_screenshot(&mut self, ctx: &egui::Context) {
        // Taken out for the duration so the rest of `self` stays borrowable,
        // and put back on every path that is still waiting.
        let Some(mut shot) = self.screenshot.take() else {
            return;
        };

        // The reply to a capture request arrives as an input event on a later
        // frame, with the image already read back off the GPU.
        let captured = ctx.input(|i| {
            i.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(Arc::clone(image)),
                _ => None,
            })
        });
        if let Some(image) = captured {
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            let [width, height] = image.size;
            match diskscope_core::bmp::write_rgba(&shot.path, width as u32, height as u32, &rgba) {
                Ok(()) => eprintln!("wrote {} ({width}x{height})", shot.path.display()),
                Err(e) => eprintln!("diskscope: could not write {}: {e}", shot.path.display()),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        shot.waited += 1;
        if shot.waited > SHOT_MAX_FRAMES {
            eprintln!("diskscope: gave up waiting for something to photograph");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Keep frames coming; nothing else would ask for a repaint once the
        // scan is done and the pointer is not moving.
        ctx.request_repaint();

        if !self.is_scanning() && self.tree.is_some() {
            if shot.warmup > 0 {
                shot.warmup -= 1;
                if shot.warmup == 0 {
                    // A picture of the readout band is worth more with
                    // something in it, so point the app at its own biggest find.
                    if let Some(tree) = &self.tree {
                        self.selected =
                            largest_files(tree, ROOT, 1, self.physical).first().copied();
                    }
                }
            } else if !shot.requested {
                shot.requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
        }

        self.screenshot = Some(shot);
    }

    fn collect_finished_scan(&mut self) {
        let Some(job) = &self.job else { return };
        let Some(outcome) = job.take_result() else {
            return;
        };
        let cancelled = job.is_cancelled();
        let root = job.root.clone();
        self.job = None;
        match outcome {
            Ok(tree) => {
                let errors = tree.stats.errors;
                self.adopt(tree);
                if cancelled {
                    self.note("Scan stopped. Showing what was measured before you stopped it.");
                } else if errors > 0 {
                    self.note(format!(
                        "{} items couldn't be read. Grant Full Disk Access in System Settings ▸ Privacy & Security to include them.",
                        fmt::count(errors)
                    ));
                }
            }
            Err(e) => self.note(format!("Can't scan {}: {e}", root.display())),
        }
    }

    fn accept_dropped_folder(&mut self, ctx: &egui::Context) {
        let dropped: Option<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped {
            self.start_scan(path);
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context) -> Vec<Action> {
        let mut queue = Vec::new();
        ctx.input(|i| {
            let cmd = i.modifiers.command;
            if i.key_pressed(egui::Key::Backspace) {
                match (cmd, self.selected) {
                    (true, Some(id)) => queue.push(Action::ConfirmTrash(id)),
                    _ => queue.push(Action::GoUp),
                }
            }
            if i.key_pressed(egui::Key::Escape) {
                self.selected = None;
                if self.highlight.take().is_some() {
                    self.canvas.invalidate();
                }
            }
            if cmd && i.key_pressed(egui::Key::R) {
                queue.push(Action::Rescan);
            }
            if cmd && i.key_pressed(egui::Key::O) {
                queue.push(Action::PickFolder);
            }
            if i.key_pressed(egui::Key::Enter) {
                if let Some(id) = self.selected {
                    queue.push(Action::Reveal(id));
                }
            }
        });
        queue
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut queue = Vec::new();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(theme::spaced("DISKSCOPE"))
                    .font(theme::structure(17.0))
                    .color(color::PAPER),
            );
            ui.add_space(14.0);

            let current = self
                .last_root
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Choose a target".into());
            egui::ComboBox::from_id_salt("target")
                .selected_text(egui::RichText::new(current).font(theme::name(12.5)))
                .width(240.0)
                .show_ui(ui, |ui| {
                    for (label, path) in actions::scan_targets() {
                        let text = format!("{label}  ·  {}", path.display());
                        if ui.selectable_label(false, egui::RichText::new(text).font(theme::name(12.0))).clicked() {
                            queue.push(Action::ScanPath(path));
                        }
                    }
                    ui.separator();
                    if ui.selectable_label(false, egui::RichText::new("Choose a folder…").font(theme::name(12.0))).clicked() {
                        queue.push(Action::PickFolder);
                    }
                });

            if self.is_scanning() {
                if ui.button("Stop").clicked() {
                    if let Some(job) = &self.job {
                        job.cancel();
                    }
                }
            } else {
                let can_rescan = self.last_root.is_some();
                if ui
                    .add_enabled(can_rescan, egui::Button::new("Scan"))
                    .on_hover_text("⌘R")
                    .clicked()
                {
                    queue.push(Action::Rescan);
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .selectable_label(self.show_settings, "⚙")
                    .on_hover_text("Scan settings")
                    .clicked()
                {
                    self.show_settings = !self.show_settings;
                }
                ui.add_space(8.0);
                // Physical is the default because it answers the question that
                // matters — how much of the disk would you get back.
                let metric = if self.physical { "ON DISK" } else { "APPARENT" };
                if ui
                    .button(egui::RichText::new(theme::spaced(metric)).font(theme::structure(12.0)))
                    .on_hover_text(if self.physical {
                        "Showing blocks actually allocated. Click for apparent size."
                    } else {
                        "Showing apparent size, which ignores compression and sparse files. Click for size on disk."
                    })
                    .clicked()
                {
                    self.physical = !self.physical;
                    self.canvas.invalidate();
                    self.recompute();
                }
                if self.tree.is_some() {
                    ui.add_space(8.0);
                    let on = self.collapse_packages;
                    if ui
                        .selectable_label(on, egui::RichText::new(theme::spaced("BUNDLES")).font(theme::structure(12.0)))
                        .on_hover_text("Draw .app and .framework folders as a single block")
                        .clicked()
                    {
                        self.collapse_packages = !on;
                        self.canvas.invalidate();
                    }
                }
            });
        });

        if self.tree.is_some() {
            ui.add_space(6.0);
            queue.extend(self.breadcrumb(ui));
            // Room for the share bars drawn under the crumbs.
            ui.add_space(5.0);
        }
        queue
    }

    /// The drill-down path, drawn as a proportional route.
    ///
    /// Each step is underlined by a bar whose length is that folder's share of
    /// the one before it, so the toolbar itself shows how the bytes narrowed
    /// as you went in — the whole point of drilling down, made visible in the
    /// one place you always look.
    fn breadcrumb(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut queue = Vec::new();
        let Some(tree) = &self.tree else { return queue };
        let chain = tree.ancestry(self.view_root);

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (i, &id) in chain.iter().enumerate() {
                if i > 0 {
                    ui.label(
                        egui::RichText::new("▸")
                            .font(theme::data(10.0))
                            .color(color::RULE),
                    );
                }
                let last = i + 1 == chain.len();
                let label = if i == 0 {
                    tree.root_path.to_string_lossy().into_owned()
                } else {
                    tree.name(id).into_owned()
                };
                let text = egui::RichText::new(label)
                    .font(theme::name(12.5))
                    .color(if last { color::PAPER } else { color::SOUNDING });
                let response = ui.add(egui::Button::new(text).frame(false));

                let share = if i == 0 {
                    1.0
                } else {
                    let parent = tree.size(chain[i - 1], self.physical).max(1);
                    (tree.size(id, self.physical) as f64 / parent as f64).clamp(0.0, 1.0) as f32
                };
                let bar = response.rect;
                let y = bar.bottom() + 1.0;
                ui.painter()
                    .hline(bar.x_range(), y, Stroke::new(2.0, color::RULE));
                ui.painter().hline(
                    bar.left()..=bar.left() + bar.width() * share,
                    y,
                    Stroke::new(
                        2.0,
                        if last {
                            color::MAGENTA
                        } else {
                            color::SOUNDING
                        },
                    ),
                );
                if response.clicked() {
                    queue.push(Action::Drill(id));
                }
                response.on_hover_text(format!(
                    "{} — {} of the level above",
                    fmt::bytes(tree.size(id, self.physical)),
                    fmt::percent((share * 1000.0) as u64, 1000)
                ));
            }
        });
        queue
    }

    fn rail(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let mut panel = Sidebar {
            tree,
            kinds: &self.kinds,
            totals: &self.totals,
            largest: &self.largest,
            view_root: self.view_root,
            physical: self.physical,
            highlight: self.highlight,
            selected: self.selected,
            tab: &mut self.tab,
            expanded: &mut self.expanded,
        };
        panel.show(ui)
    }

    fn map(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut queue = Vec::new();
        if self.is_scanning() {
            self.scanning_readout(ui);
            return queue;
        }
        let Some(tree) = &self.tree else {
            self.empty_state(ui);
            return queue;
        };

        let input = CanvasInput {
            tree,
            kinds: &self.kinds,
            root: self.view_root,
            physical: self.physical,
            collapse_packages: self.collapse_packages,
            highlight: self.highlight,
            generation: self.generation,
            selected: self.selected,
            min_area: MIN_BLOCK_AREA,
        };
        let out = self.canvas.show(ui, &input);
        self.hovered = out.hovered;
        if let Some(id) = out.clicked {
            queue.push(Action::Select(id));
        }
        if let Some(id) = out.double_clicked {
            queue.push(Action::Drill(id));
        }
        if let Some(id) = out.context_target {
            self.selected = Some(id);
        }
        queue
    }

    /// While a scan runs the map cannot be drawn yet, so the canvas shows the
    /// counters instead. They move on their own; nothing else animates.
    fn scanning_readout(&mut self, ui: &mut egui::Ui) {
        let Some(job) = &self.job else { return };
        let p = &job.progress;
        use std::sync::atomic::Ordering::Relaxed;
        let entries = p.entries();
        let bytes = p.bytes.load(Relaxed);
        let secs = job.elapsed();

        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.28);
            sidebar::eyebrow(
                ui,
                if job.is_cancelled() {
                    "STOPPING"
                } else {
                    "MEASURING"
                },
            );
            ui.label(
                egui::RichText::new(fmt::count(entries))
                    .font(theme::structure(56.0))
                    .color(color::PAPER),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{} · {} · {}",
                    fmt::bytes(bytes),
                    fmt::rate(entries, secs).replace("bytes/s", "entries/s"),
                    job.root.to_string_lossy()
                ))
                .font(theme::data(12.0))
                .color(color::SOUNDING),
            );
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} folders · {} errors",
                    fmt::count(p.dirs.load(Relaxed)),
                    fmt::count(p.errors.load(Relaxed))
                ))
                .font(theme::data(11.0))
                .color(color::SOUNDING.gamma_multiply(0.8)),
            );
        });
    }

    fn empty_state(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.32);
            sidebar::eyebrow(ui, "NOTHING MEASURED YET");
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Pick a volume or folder above, or drop one on this window.")
                    .font(theme::name(14.0))
                    .color(color::PAPER),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Scanning all of / needs Full Disk Access in System Settings ▸ Privacy & Security.",
                )
                .font(theme::name(12.0))
                .color(color::SOUNDING),
            );
        });
    }

    fn readout(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut queue = Vec::new();

        if let Some((message, _)) = self.notice.clone() {
            egui::Sides::new().shrink_left().show(
                ui,
                |ui| {
                    ui.label(
                        egui::RichText::new("!")
                            .font(theme::structure(13.0))
                            .color(color::AMBER),
                    );
                    ui.label(
                        egui::RichText::new(message)
                            .font(theme::name(12.0))
                            .color(color::PAPER),
                    );
                },
                |ui| {
                    if ui.small_button("Dismiss").clicked() {
                        self.notice = None;
                    }
                },
            );
            ui.add_space(4.0);
        }

        let Some(tree) = &self.tree else {
            if let Some(warning) = &self.kinds_warning {
                ui.label(
                    egui::RichText::new(format!("Using the built-in kinds: {warning}"))
                        .font(theme::name(11.5))
                        .color(color::AMBER),
                );
            }
            return queue;
        };

        let focus = self.selected.or(self.hovered);
        // `Sides` with a shrinking left half is what keeps a deep path from
        // running underneath the buttons: the path gives up width, the numbers
        // and the actions never do.
        egui::Sides::new().shrink_left().show(
            ui,
            |ui| match focus {
                Some(id) => {
                    swatch_dot(ui, self.kinds.kind(tree.node(id).kind).color);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(tree.display_path(id))
                                .font(theme::name(12.5))
                                .color(color::PAPER),
                        )
                        .truncate(),
                    );
                }
                None => {
                    let stats = &tree.stats;
                    ui.label(
                        egui::RichText::new(format!(
                            "{} entries in {:.2}s · {} blocks drawn in {:.0} ms{}",
                            fmt::count(tree.len() as u64),
                            stats.elapsed.as_secs_f64(),
                            fmt::count(self.canvas.block_count() as u64),
                            self.canvas.last_render_ms,
                            if stats.used_fallback {
                                " · portable walker"
                            } else {
                                ""
                            },
                        ))
                        .font(theme::data(11.5))
                        .color(color::SOUNDING),
                    );
                }
            },
            |ui| {
                // Laid out right to left, so this reads bottom-up: the commit
                // sits furthest right, then the actions, then the numbers.
                if let Some(sha) = crate::COMMIT {
                    ui.label(
                        egui::RichText::new(sha)
                            .font(theme::data(10.5))
                            .color(color::SOUNDING.gamma_multiply(0.6)),
                    );
                    ui.add_space(4.0);
                }
                if let Some(id) = self.selected {
                    if ui.button("Move to Trash").on_hover_text("⌘⌫").clicked() {
                        queue.push(Action::ConfirmTrash(id));
                    }
                    if ui.button("Copy path").clicked() {
                        queue.push(Action::CopyPath(id));
                    }
                    if ui.button("Open").clicked() {
                        queue.push(Action::Open(id));
                    }
                    if ui.button("Reveal in Finder").on_hover_text("↩").clicked() {
                        queue.push(Action::Reveal(id));
                    }
                }
                if let Some(id) = focus {
                    let node = tree.node(id);
                    for (flag, label) in [
                        (flags::UNREADABLE, "couldn't be read"),
                        (flags::OTHER_FS, "another volume, not scanned"),
                        (flags::PACKAGE, "bundle"),
                        (flags::HARDLINK_DUP, "extra hard link, not counted"),
                        (flags::SYMLINK, "symlink"),
                    ] {
                        if node.has(flag) {
                            ui.label(
                                egui::RichText::new(label)
                                    .font(theme::name(11.0))
                                    .color(color::AMBER),
                            );
                        }
                    }
                    let size = tree.size(id, self.physical);
                    let view_total = tree.size(self.view_root, self.physical);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · {} · {} of view",
                            self.kinds.kind(node.kind).name,
                            fmt::bytes(size),
                            fmt::percent(size, view_total)
                        ))
                        .font(theme::data(11.5))
                        .color(color::SOUNDING),
                    );
                }
            },
        );
        queue
    }

    fn trash_dialog(&mut self, ctx: &egui::Context) {
        let Some(id) = self.pending_trash else { return };
        let Some(tree) = &self.tree else {
            self.pending_trash = None;
            return;
        };
        let path = tree.path(id);
        let node = tree.node(id);
        let size = fmt::bytes(tree.size(id, self.physical));
        let is_dir = node.is_dir();
        let count = if is_dir {
            Some(diskscope_core::summary::subtree_len(tree, id))
        } else {
            None
        };

        egui::Modal::new(egui::Id::new("confirm-trash")).show(ctx, |ui| {
            ui.set_width(430.0);
            sidebar::eyebrow(ui, "MOVE TO TRASH");
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(path.to_string_lossy())
                    .font(theme::name(13.0))
                    .color(color::PAPER),
            );
            ui.add_space(4.0);
            let detail = match count {
                Some(n) => format!("{size} · this folder and {} items inside it", fmt::count(n as u64 - 1)),
                None => size.clone(),
            };
            ui.label(egui::RichText::new(detail).font(theme::data(12.0)).color(color::SOUNDING));
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("You can put it back from the Trash. The map won't change until you scan again.")
                    .font(theme::name(11.5))
                    .color(color::SOUNDING),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Move to Trash").clicked() {
                    match actions::move_to_trash(&path) {
                        Ok(()) => {
                            self.notice = Some((
                                format!("Moved {} to the Trash. Scan again to update the map.", path.display()),
                                Instant::now(),
                            ));
                            self.selected = None;
                        }
                        Err(e) => {
                            self.notice = Some((format!("Couldn't move that to the Trash: {e}"), Instant::now()))
                        }
                    }
                    self.pending_trash = None;
                }
                if ui.button("Cancel").clicked() {
                    self.pending_trash = None;
                }
            });
        });
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut changed = false;
        egui::Window::new(
            egui::RichText::new(theme::spaced("SCAN SETTINGS")).font(theme::structure(13.0)),
        )
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.set_width(320.0);
            changed |= ui
                .checkbox(
                    &mut self.scan_opts.dedupe_hardlinks,
                    "Count hard links once",
                )
                .on_hover_text(
                    "A file with several names is measured once, at the first name found.",
                )
                .changed();
            changed |= ui
                .checkbox(
                    &mut self.scan_opts.cross_mounts,
                    "Follow into other volumes",
                )
                .on_hover_text(
                    "Off by default, so a mounted disk isn't counted as part of this one.",
                )
                .changed();
            changed |= ui
                .checkbox(&mut self.scan_opts.detect_packages, "Recognise bundles")
                .on_hover_text(
                    "Marks .app and .framework folders so they can be drawn as one block.",
                )
                .changed();
            changed |= ui
                .checkbox(
                    &mut self.scan_opts.include_free_space,
                    "Show free space as a block",
                )
                .on_hover_text("Only when the scan starts at a volume's root.")
                .changed();
            changed |= ui
                .checkbox(&mut self.scan_opts.prefer_bulk, "Use bulk directory reads")
                .on_hover_text("getattrlistbulk. Turn off to use the portable readdir walker.")
                .changed();
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(if changed {
                    "Scan again to apply."
                } else {
                    "These take effect on the next scan."
                })
                .font(theme::name(11.5))
                .color(if changed {
                    color::AMBER
                } else {
                    color::SOUNDING
                }),
            );
        });
        self.show_settings = open;
    }
}

fn band(inner: f32) -> egui::Frame {
    egui::Frame::new()
        .fill(color::INK)
        .inner_margin(egui::Margin::symmetric(inner as i8, 7))
        .stroke(Stroke::new(1.0, color::RULE))
}

fn swatch_dot(ui: &mut egui::Ui, rgb: [u8; 3]) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, 1.0, egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
    ui.painter().rect_stroke(
        rect,
        1.0,
        Stroke::new(1.0, egui::Color32::from_black_alpha(80)),
        StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    /// Drives the real window against a bare `egui::Context` — no eframe, no
    /// GPU. Everything the UI does short of presenting a frame runs here, so a
    /// panic in layout, painting or an action handler fails the build instead
    /// of the app.
    struct Harness {
        ctx: egui::Context,
        app: App,
        pointer: egui::Pos2,
    }

    impl Harness {
        fn new(root: &std::path::Path) -> Self {
            let ctx = egui::Context::default();
            let app = App::new(&ctx, Some(root.to_path_buf()), None);
            Self {
                ctx,
                app,
                pointer: egui::pos2(700.0, 420.0),
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 820.0),
                )),
                events,
                ..Default::default()
            };
            let app = &mut self.app;
            let output = self.ctx.run_ui(input, |ui| app.show(ui));
            discard(output);
        }

        /// Pumps frames until the scan lands, then one more so the map renders.
        fn settle(&mut self) {
            for _ in 0..400 {
                self.frame(vec![]);
                if !self.app.is_scanning() {
                    self.frame(vec![]);
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("the scan never finished");
        }

        fn move_to(&mut self, x: f32, y: f32) {
            self.pointer = egui::pos2(x, y);
            self.frame(vec![egui::Event::PointerMoved(self.pointer)]);
        }

        fn click(&mut self) {
            let at = self.pointer;
            self.frame(vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ]);
            self.frame(vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }]);
        }

        fn key(&mut self, key: egui::Key, command: bool) {
            let modifiers = egui::Modifiers {
                command,
                mac_cmd: command,
                ..Default::default()
            };
            // Held-modifier state is carried by `ModifiersChanged`, which is
            // what `InputState::modifiers` tracks; putting them only on the key
            // event leaves `i.modifiers` empty.
            self.frame(vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
            ]);
            self.frame(vec![egui::Event::ModifiersChanged(
                egui::Modifiers::default(),
            )]);
        }
    }

    /// Without a renderer nobody uploads the texture deltas egui produced, and
    /// dropping them unhandled is a panic. Throwing them away is exactly right
    /// here: the test cares that painting *ran*, not that it reached a screen.
    fn discard(mut output: egui::FullOutput) {
        output.textures_delta.clear();
    }

    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for (dir, count, size) in [
            ("media", 6, 80_000),
            ("code", 40, 2_000),
            ("logs", 300, 400),
        ] {
            for i in 0..count {
                let path = root.join(dir).join(format!(
                    "item{i:03}.{}",
                    match dir {
                        "media" => "mp4",
                        "code" => "rs",
                        _ => "log",
                    }
                ));
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, vec![b'x'; size]).unwrap();
            }
        }
        fs::create_dir_all(root.join("Thing.app/Contents")).unwrap();
        fs::write(root.join("Thing.app/Contents/binary"), vec![0u8; 30_000]).unwrap();
        tmp
    }

    #[test]
    fn scans_draws_and_survives_being_poked() {
        let tmp = fixture();
        let mut h = Harness::new(tmp.path());
        h.settle();

        let tree = h.app.tree.as_ref().expect("a tree was adopted");
        assert!(tree.len() > 340, "only {} entries", tree.len());
        assert!(
            h.app.canvas.block_count() > 100,
            "map drew {} blocks",
            h.app.canvas.block_count()
        );
        assert!(!h.app.totals.is_empty(), "the kinds table is populated");
        assert_eq!(
            h.app.largest.len(),
            LARGEST_COUNT.min(tree.stats.files as usize)
        );

        // Pointing at the middle of the map must land on a real node.
        h.move_to(700.0, 420.0);
        let hovered = h.app.hovered.expect("the pointer is over a block");
        assert!((hovered as usize) < h.app.tree.as_ref().unwrap().len());

        // Clicking selects it, and the readout renders for the selection.
        h.click();
        assert_eq!(h.app.selected, Some(hovered));
        h.frame(vec![]);

        // Escape clears, Backspace at the root is a no-op rather than a panic.
        h.key(egui::Key::Escape, false);
        assert_eq!(h.app.selected, None);
        h.key(egui::Key::Backspace, false);
        assert_eq!(h.app.view_root, ROOT);
    }

    #[test]
    fn drilling_in_and_back_out_keeps_the_view_consistent() {
        let tmp = fixture();
        let mut h = Harness::new(tmp.path());
        h.settle();

        let media = {
            let tree = h.app.tree.as_ref().unwrap();
            tree.children(ROOT)
                .find(|&c| tree.name(c) == "media")
                .expect("the media folder is in the tree")
        };

        h.app.apply(Action::Drill(media), &h.ctx.clone());
        h.frame(vec![]);
        assert_eq!(h.app.view_root, media);
        // Rollups follow the view: inside media everything is one kind.
        assert_eq!(h.app.totals.len(), 1);
        assert_eq!(h.app.kinds.kind(h.app.totals[0].kind).name, "Video");

        h.key(egui::Key::Backspace, false);
        assert_eq!(h.app.view_root, ROOT);
        assert!(h.app.totals.len() > 1);
    }

    #[test]
    fn highlighting_a_kind_and_switching_metric_redraw_without_complaint() {
        let tmp = fixture();
        let mut h = Harness::new(tmp.path());
        h.settle();

        let kind = h.app.totals[0].kind;
        h.app.apply(Action::ToggleKind(kind), &h.ctx.clone());
        h.frame(vec![]);
        assert_eq!(h.app.highlight, Some(kind));
        h.app.apply(Action::ToggleKind(kind), &h.ctx.clone());
        h.frame(vec![]);
        assert_eq!(h.app.highlight, None);

        h.app.physical = false;
        h.app.canvas.invalidate();
        h.app.recompute();
        h.frame(vec![]);
        assert!(h.app.canvas.block_count() > 100);
    }

    #[test]
    fn every_sidebar_tab_renders() {
        let tmp = fixture();
        let mut h = Harness::new(tmp.path());
        h.settle();

        for tab in [Tab::Kinds, Tab::Folders, Tab::Largest] {
            h.app.tab = tab;
            h.frame(vec![]);
        }
        // Expanded folders render their children too.
        let media = {
            let tree = h.app.tree.as_ref().unwrap();
            tree.children(ROOT)
                .find(|&c| tree.name(c) == "media")
                .unwrap()
        };
        h.app.tab = Tab::Folders;
        h.app.expanded.insert(media);
        h.frame(vec![]);
    }

    #[test]
    fn an_empty_window_and_a_failed_scan_both_render() {
        let ctx = egui::Context::default();
        let mut app = App::new(&ctx, None, None);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 600.0),
            )),
            ..Default::default()
        };
        discard(ctx.run_ui(input.clone(), |ui| app.show(ui)));
        assert!(app.tree.is_none());

        app.start_scan(std::path::PathBuf::from("/definitely/not/a/path"));
        for _ in 0..200 {
            discard(ctx.run_ui(input.clone(), |ui| app.show(ui)));
            if !app.is_scanning() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            app.tree.is_none(),
            "a failed scan must not replace the tree"
        );
        assert!(app.notice.is_some(), "and it must say why");
        discard(ctx.run_ui(input, |ui| app.show(ui)));
    }

    #[test]
    fn the_trash_dialog_opens_and_cancels_without_touching_the_file() {
        let tmp = fixture();
        let mut h = Harness::new(tmp.path());
        h.settle();

        let victim = {
            let tree = h.app.tree.as_ref().unwrap();
            largest_files(tree, ROOT, 1, true)[0]
        };
        let path = h.app.tree.as_ref().unwrap().path(victim);
        h.app.selected = Some(victim);
        h.key(egui::Key::Backspace, true);
        assert_eq!(h.app.pending_trash, Some(victim), "⌘⌫ asks first");
        h.frame(vec![]);

        h.app.pending_trash = None;
        h.frame(vec![]);
        assert!(path.exists(), "cancelling must leave the file alone");
    }
}
