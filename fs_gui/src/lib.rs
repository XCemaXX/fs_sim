use eframe::egui::{self, Ui};
use egui::Button;
use fs_api::{
    disk::Disk,
    driver::{FsEntry, Level, Notice, can_format},
    path::VPath,
    view::{ExtentKind, Fact, FileExtents, FsTree, NodeKind},
};

use crate::{grid_view::BlockGridView, session::Session, style::palette, tree::FileTree};

mod dynamic_params;
mod grid;
mod grid_view;
mod legend;
mod page;
mod ruler;
mod session;
mod style;
mod tree;

struct MyEguiApp {
    session: Session,
    grid_view: BlockGridView,
    status: Status,
    file_ops: FileOpsPanel,
    tree: FileTree,
    format_error: Option<String>, // last Format attempt; None = ok or never tried
    selected_cells: Vec<std::ops::Range<u64>>,
    mode: ViewMode,
    ops_h: f32, // file-ops form height from the previous frame
}

// On web eframe already multiplies by devicePixelRatio — keep our own factor smaller.
#[cfg(target_arch = "wasm32")]
const ZOOM: f32 = 1.2;
#[cfg(not(target_arch = "wasm32"))]
const ZOOM: f32 = 2.0;

/// Config and Mounted are indistinguishable in Session state (both have a mount)
/// but draw differently — so this is pure view state, not derived from the model.
enum ViewMode {
    Config,  // disk/fs choice in the central area
    Mounted, // block grid + tree
}

struct FileOpsPanel {
    size: u64,
    name: String,
    seeded: Option<VPath>, // selection the fields were seeded for
}

/// Status line with its outcome: errors differ by color. Lives until the next write.
#[derive(Default)]
struct Status {
    text: String,
    error: bool,
}

impl Status {
    fn ok(text: String) -> Self {
        Self { text, error: false }
    }

    fn err(text: String) -> Self {
        Self { text, error: true }
    }
}

/// File panel command — one per frame.
#[derive(Clone, Copy)]
enum FileCmd {
    CreateFile,
    CreateDir,
    Remove,
    Rename,
    Resize,
}

/// `disabled` — why it is blocked; `None` = active. A greyed-out button must explain
/// itself, otherwise the user cannot tell what to do to enable it.
fn cmd_button(ui: &mut Ui, disabled: Option<&str>, label: &str) -> bool {
    let r = ui.add_enabled(disabled.is_none(), Button::new(label));
    match disabled {
        Some(why) => r.on_disabled_hover_text(why).clicked(),
        None => r.clicked(),
    }
}

/// Row of buttons, wrapped if they do not fit the width. All of them are drawn (a
/// click must not hide neighbours for a frame); the first pressed one lands in `cmd`.
fn button_row(ui: &mut Ui, buttons: &[(Option<&str>, String, FileCmd)], cmd: &mut Option<FileCmd>) {
    ui.horizontal_wrapped(|ui| {
        for (disabled, label, c) in buttons {
            if cmd_button(ui, *disabled, label) && cmd.is_none() {
                *cmd = Some(*c);
            }
        }
    });
}

/// What to do with the selection after a successful operation.
enum AfterOp {
    Keep,    // create: the selected parent is intact
    Clear,   // remove/rename: node is gone or changed its path
    Refresh, // resize: same node, new extents
}

fn extent_spans(fx: Option<&FileExtents>) -> Vec<std::ops::Range<u64>> {
    fx.map(|fx| fx.iter().map(|e| e.range.start()..e.range.end()).collect())
        .unwrap_or_default()
}

fn facts_line<'a>(facts: impl Iterator<Item = &'a Fact>) -> String {
    facts
        .map(|f| format!("{}: {}", f.key(), f.value()))
        .collect::<Vec<_>>()
        .join(" · ")
}

impl FileOpsPanel {
    fn new() -> Self {
        Self {
            size: 4096,
            name: "new".into(),
            seeded: None,
        }
    }
}

impl MyEguiApp {
    /// Central area in Config mode: fs choice, params, load-json, Format/Back.
    fn gui_config(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label("Filesystem");
            let cur = &self.session.get_current().reg.display_name;
            egui::ComboBox::from_id_salt("fs")
                .selected_text(cur)
                .show_ui(ui, |ui| {
                    let mut clicked = None;
                    for (i, fs) in self.session.entries().iter().enumerate() {
                        let selected = i == self.session.selected();
                        if ui
                            .add_enabled(
                                fs.is_implemented(),
                                egui::Button::selectable(selected, &fs.reg.display_name),
                            )
                            .on_disabled_hover_text("Not implemented yet")
                            .clicked()
                        {
                            clicked = Some(i);
                        }
                    }
                    if let Some(i) = clicked {
                        self.session.select_fs(i); // rebuilds fs_params itself
                    }
                });
            cmd_button(ui, Some("Not supported yet"), "Load JSON"); // TODO: image loading step
        });

        self.session.disk_params.draw(ui, "disk_params", "Disk");
        self.session.fs_params.draw(ui, "fs_params", "Filesystem");
        self.gui_format_gate(ui);
    }

    /// Form validation + the Format button gate.
    fn gui_format_gate(&mut self, ui: &mut Ui) {
        let s = &self.session;
        let disk = Disk::from_params(s.disk_params.values(), &s.limits); // Disk: Copy

        // Invalid disk → show its errors; otherwise validate the fs params.
        let (form, notices) = match &disk {
            Ok(disk) => (&s.fs_params, self.fs_notices(disk)),
            Err(_) => (
                &s.disk_params,
                s.disk_params.values().check(&Disk::schema(&s.limits)),
            ),
        };
        form.draw_notices(ui, &notices);

        ui.horizontal(|ui| {
            let blocked = (!can_format(&notices)).then_some("Fix the errors above first");
            if cmd_button(ui, blocked, "Format") {
                // Every click records an outcome exactly once, invalid disk included —
                // otherwise the screen keeps showing the answer to the previous click.
                self.format_error = match disk {
                    Ok(disk) => self.session.format(disk).err().map(|e| e.to_string()),
                    Err(e) => Some(e.to_string()),
                };
                if self.format_error.is_none() {
                    self.mode = ViewMode::Mounted;
                    self.reset_view(); // new volume → drop old status/selection/page
                }
            }
            // Back: return to the live mount without reformatting. The live mount is
            // the "previous fs" — format replaces it only on success.
            let no_mount = self
                .session
                .current_mount()
                .is_none()
                .then_some("Nothing is mounted yet — format first");
            if cmd_button(ui, no_mount, "Back") {
                self.mode = ViewMode::Mounted;
                self.format_error = None;
            }
        });

        // drawn from state, so it survives the next frame
        if let Some(err) = &self.format_error {
            ui.colored_label(palette::ERROR, err);
        }
    }

    /// Fs notices for a valid disk; a single error for a not-implemented fs.
    fn fs_notices(&self, disk: &Disk) -> Vec<Notice> {
        match self.session.current_driver() {
            Some(drv) => drv.check(disk, self.session.fs_params.values()),
            None => vec![Notice {
                level: Level::Error,
                field: None,
                msg: "filesystem not implemented".into(),
            }],
        }
    }

    fn gui_block_stats(&mut self, ui: &mut Ui) {
        let text = egui::RichText::new(&self.status.text);
        let text = if self.status.error {
            text.color(palette::ERROR)
        } else {
            text
        };
        ui.add(egui::Label::new(text).truncate())
            .on_hover_text(&self.status.text);

        if let Ok(usage) = self.session.usage() {
            let line = format!(
                "Blocks. Total: {}. Used: {}. Free: {}",
                usage.total, usage.used, usage.free
            );
            ui.add(egui::Label::new(&line).truncate())
                .on_hover_text(&line);
        }
    }

    fn new(cc: &eframe::CreationContext<'_>, registry: Vec<FsEntry>) -> Self {
        cc.egui_ctx.set_zoom_factor(ZOOM);
        style::install_fonts(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(style::docs_visuals());

        Self {
            status: Status::default(),
            grid_view: BlockGridView::new(),
            file_ops: FileOpsPanel::new(),
            tree: FileTree::new(),
            session: Session::new(registry),
            format_error: None,
            selected_cells: Vec::new(),
            mode: ViewMode::Config,
            ops_h: 260.0, // until the first frame; then measured from content
        }
    }

    fn gui_cells(&mut self, ui: &mut Ui) {
        let Some(mount) = self.session.current_mount() else {
            return;
        };
        let version = self.session.version();
        if let Some(block) = self
            .grid_view
            .render(ui, mount, version, &self.selected_cells)
        {
            self.selected_cells = vec![std::ops::Range {
                start: block,
                end: block + 1,
            }];
            self.status = Status::ok(self.session.describe_block(block));
        }
    }

    /// Tree body. File-ops sit in a pinned bottom sub-panel so the tree scrolls above them.
    fn gui_tree_body(&mut self, ui: &mut Ui) {
        let Some(tree) = self.session.tree() else {
            return;
        };
        let response = ui.horizontal(|ui| self.tree.render(ui, tree));
        if let Some(event) = response.inner {
            let fx = self.session.file_extents(&event.path);
            self.status = Status::ok(describe_node(tree, fx.as_ref(), &event.path));
            // No extents (node gone) → clear, else the highlight sticks on foreign blocks.
            self.selected_cells = extent_spans(fx.as_ref());
            if event.activated {
                self.jump_to_selection();
            }
        }
    }

    /// Top panel in Mounted mode: volume summary + geometry facts + switch to Config.
    fn gui_mount_summary(&mut self, ui: &mut Ui) {
        let head = self.session.mount_info().map(|mi| {
            format!(
                "{} · {} · {} B blocks",
                mi.fs_name,
                human_size(mi.disk.capacity()),
                mi.disk.block_size()
            )
        });
        // Facts come from the mount — real format geometry, not the re-derived draft.
        let facts = self
            .session
            .current_mount()
            .map(|m| facts_line(m.geometry().facts().iter()));

        // The button reserves its space first, the summary takes the rest and is
        // truncated — so "Change" never runs off the edge at any window width.
        // horizontal is required: it bounds the row height. Without it Align::Center
        // centers inside the unbounded panel height, and the panel grows every frame.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Change").clicked() {
                    self.mode = ViewMode::Config;
                    self.format_error = None; // stale error must not greet the settings screen
                }
                if let Some(head) = &head {
                    ui.add(egui::Label::new(head).truncate());
                }
            });
        });
        // One line at any width: with wrapping a narrow window inflates the panel to
        // half the screen. Full text on hover.
        if let Some(facts) = &facts {
            ui.add(egui::Label::new(facts).truncate())
                .on_hover_text(facts);
        }
    }

    /// Drop the previous volume's view state: status, highlight, tree selection, page.
    fn reset_view(&mut self) {
        self.status = Status::default();
        self.selected_cells.clear();
        self.tree.clear_selection();
        self.grid_view.jump_to(0);
    }

    /// Scroll to the page holding the first selected block.
    fn jump_to_selection(&mut self) {
        if let Some(first) = self.selected_cells.iter().map(|r| r.start).min() {
            self.grid_view.jump_to(first);
        }
    }

    /// Seed the fields from the selected node: Rename edits the current name, Resize
    /// shows the current size. Without it a destructive button acts on a foreign value.
    fn seed_file_ops(&mut self) {
        let sel = self.tree.selected().cloned();
        if self.file_ops.seeded == sel {
            return;
        }
        self.file_ops.seeded = sel.clone();
        // Root has no name and is not an edit target — the field stays for creation.
        let Some(path) = sel.filter(|p| !p.is_root()) else {
            return;
        };
        self.file_ops.name = path.file_name().to_owned();
        if let Some(node) = self.session.tree().and_then(|t| t.find(&path))
            && matches!(node.kind(), NodeKind::File)
        {
            self.file_ops.size = node.size();
        }
    }

    fn gui_file_ops(&mut self, ui: &mut Ui) {
        self.seed_file_ops();
        ui.vertical(|ui| {
            ui.label("File operations");
            ui.add(
                egui::Label::new(match self.tree.selected() {
                    Some(p) => format!("Target: {p}"),
                    None => "Target:".to_owned(),
                })
                .truncate(),
            );
            let name_label = ui.label("Name:");
            ui.add(
                egui::TextEdit::singleline(&mut self.file_ops.name).hint_text("new or new name"),
            )
            .labelled_by(name_label.id);

            let size_label = ui.label("Size:");
            let cap = self.session.disk_capacity();
            ui.add(
                egui::Slider::new(&mut self.file_ops.size, 0..=cap)
                    .logarithmic(true)
                    .suffix(" B"),
            )
            .labelled_by(size_label.id);

            let sel = self.tree.selected();
            let name = &self.file_ops.name;
            let trimmed = name.trim();
            // an empty name would make join collapse into the directory itself
            let unnamed = trimmed.is_empty().then_some("Enter a name first");
            // root and empty selection are not edit targets
            let not_editable = match sel {
                None => Some("Select a file or folder in the tree"),
                Some(p) if p.is_root() => Some("Cannot modify the root directory"),
                Some(_) => None,
            };

            // Two groups: create and edit, each its own wrapping row. The button face
            // shows the effect of the click, so a destructive value is visible before
            // pressing, not after.
            let create = [
                (unnamed, "Create file".to_owned(), FileCmd::CreateFile),
                (unnamed, "Create folder".to_owned(), FileCmd::CreateDir),
            ];
            let edit = [
                (not_editable, "Remove".to_owned(), FileCmd::Remove),
                (
                    not_editable,
                    format!("Resize to {}", human_size(self.file_ops.size)),
                    FileCmd::Resize,
                ),
                (
                    not_editable.or(unnamed),
                    format!("Rename to '{trimmed}'"),
                    FileCmd::Rename,
                ),
            ];
            let mut cmd = None;
            button_row(ui, &create, &mut cmd);
            button_row(ui, &edit, &mut cmd);
            let Some(cmd) = cmd else { return };

            // remove/resize act on the node itself (sel), not on the name field.
            let subject = sel.map_or("", |p| p.as_str());
            let size = self.file_ops.size;
            // Status is built from the operation result: only it knows the path, and
            // the name field does not say where exactly the node showed up.
            let (res, after) = match cmd {
                FileCmd::CreateFile => (
                    self.session
                        .add_file(sel, name, size)
                        .map(|p| format!("Created file {p} · {}", human_size(size))),
                    AfterOp::Keep,
                ),
                FileCmd::CreateDir => (
                    self.session
                        .add_dir(sel, name)
                        .map(|p| format!("Created folder {p}")),
                    AfterOp::Keep,
                ),
                FileCmd::Remove => (
                    self.session
                        .remove(sel)
                        .map(|()| format!("Removed {subject}")),
                    AfterOp::Clear,
                ),
                FileCmd::Rename => (
                    self.session
                        .rename(sel, name)
                        .map(|p| format!("Renamed {subject} to {p}")),
                    AfterOp::Clear,
                ),
                FileCmd::Resize => (
                    self.session
                        .resize(sel, size)
                        .map(|()| format!("Resized {subject} to {}", human_size(size))),
                    AfterOp::Refresh,
                ),
            };
            match res {
                Ok(status) => {
                    match after {
                        AfterOp::Keep => {}
                        AfterOp::Clear => {
                            self.tree.clear_selection();
                            self.selected_cells.clear();
                        }
                        // Same node, new extents — re-read the highlight.
                        AfterOp::Refresh => {
                            let fx = sel.and_then(|p| self.session.file_extents(p));
                            self.selected_cells = extent_spans(fx.as_ref());
                        }
                    }
                    self.status = Status::ok(status);
                }
                Err(e) => self.status = Status::err(format!("Error: {e}")),
            }
        });
    }
}

/// Status line for a tree node click: kind, path, size, its blocks.
fn describe_node(tree: &FsTree, fx: Option<&FileExtents>, path: &VPath) -> String {
    let node = tree.find(path);
    let mut s = match node {
        Some(n) => match n.kind() {
            NodeKind::Dir => format!("Dir {path}"),
            NodeKind::File => format!("File {path} · {}", human_size(n.size())),
        },
        None => format!("{path}"),
    };
    let list: Vec<_> = fx
        .iter()
        .flat_map(|fx| fx.iter())
        .map(|e| format!("{}{}", e.range, kind_suffix(e.kind)))
        .collect();
    if !list.is_empty() {
        s += &format!(" · blocks {}", list.join(", "));
    } else if node.is_some_and(|n| matches!(n.kind(), NodeKind::File)) {
        s += " · no data blocks (empty file)";
    }
    s
}

/// Content is the default relation and stays unmarked; only exceptions get a suffix.
fn kind_suffix(kind: ExtentKind) -> &'static str {
    match kind {
        ExtentKind::Content => "",
        ExtentKind::Hosted => " (hosted)",
        ExtentKind::Index => " (index)",
    }
}

/// Exact bytes below 1 KiB, one decimal above — the status line needs readability,
/// not precision.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

impl eframe::App for MyEguiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        match self.mode {
            ViewMode::Config => {
                egui::CentralPanel::default().show(ui, |ui| {
                    // Tall form → scroll instead of growing the window.
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.gui_config(ui));
                });
            }
            ViewMode::Mounted => {
                // Tree goes first so it starts at the very top (full height on the
                // right). Cost: status/summary only span the width left of the tree.
                egui::Panel::right("tree")
                    .resizable(true)
                    .default_size(200.0)
                    .size_range(160.0..=420.0)
                    .show(ui, |ui| {
                        // Height comes from the previous frame's content, capped at a
                        // share of the remaining space. A content-sized panel with a
                        // scroll inside would loop (panel height ← scroll height ←
                        // available height in the panel); the one-frame lag breaks it.
                        // A persisted size will not do: clamping on window shrink would
                        // store the trimmed value and the panel would never recover.
                        let h = self.ops_h.min(ui.available_height() * 0.6);
                        egui::Panel::bottom("file_ops")
                            .resizable(false)
                            .exact_size(h)
                            .show(ui, |ui| {
                                let out = egui::ScrollArea::vertical()
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| self.gui_file_ops(ui));
                                // exact_size sets the outer size, content_size the
                                // inner one. Measure the difference (frame padding)
                                // instead of guessing it, else a gap with a scrollbar
                                // stays.
                                let frame = h - ui.max_rect().height();
                                self.ops_h = out.content_size.y + frame;
                            });
                        // both: a deep tree runs off to the right, and the path must
                        // not be clipped.
                        egui::ScrollArea::both()
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.gui_tree_body(ui));
                    });
                egui::Panel::top("summary").show(ui, |ui| self.gui_mount_summary(ui));
                egui::Panel::bottom("status").show(ui, |ui| self.gui_block_stats(ui));
                egui::CentralPanel::default().show(ui, |ui| self.gui_cells(ui));
            }
        }
    }
}

/// Run as a native window (WSLg / desktop).
#[cfg(not(target_arch = "wasm32"))]
pub fn run(registry: Vec<FsEntry>) -> eframe::Result {
    // Window size is in OS points while the UI lives in egui points: multiply by ZOOM,
    // else a zoom change collapses the window to an unreadable size.
    let ui_min = egui::vec2(480.0, 380.0);
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(ui_min * ZOOM)
            .with_min_inner_size(ui_min * ZOOM),
        ..Default::default()
    };
    eframe::run_native(
        "fs_sim",
        native_options,
        Box::new(|cc| Ok(Box::new(MyEguiApp::new(cc, registry)))),
    )
}

/// Run inside a canvas (wasm / GitHub Pages).
#[cfg(target_arch = "wasm32")]
pub fn run_web(registry: Vec<FsEntry>) {
    use eframe::wasm_bindgen::JsCast as _;
    let opts = eframe::WebOptions::default();
    wasm_bindgen_futures::spawn_local(async {
        // Distinct messages: a bare unwrap gives the same nameless panic for each step.
        let canvas = web_sys::window()
            .expect("no browser window")
            .document()
            .expect("no document in window")
            .get_element_by_id("the_canvas_id")
            .expect("index.html has no element with id 'the_canvas_id'")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("element 'the_canvas_id' is not a <canvas>");
        eframe::WebRunner::new()
            .start(
                canvas,
                opts,
                Box::new(|cc| Ok(Box::new(MyEguiApp::new(cc, registry)))),
            )
            .await
            .expect("eframe failed to start (WebGL unavailable?)");
    });
}
