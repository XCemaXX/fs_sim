/// The single source of theme colors. Widgets and `docs_visuals` read from here.
pub mod palette {
    use egui::Color32;

    // Background/borders (docs.rs dark).
    pub const BG: Color32 = Color32::from_rgb(0x35, 0x35, 0x35);
    pub const SUB: Color32 = Color32::from_rgb(0x2b, 0x2b, 0x2b);
    pub const BORDER: Color32 = Color32::from_rgb(0x4a, 0x4a, 0x4a);
    pub const ACCENT: Color32 = Color32::from_rgb(0xd2, 0x99, 0x1d); // links + Warn
    pub const SELECTION_BG: Color32 = Color32::from_rgb(0x3a, 0x4a, 0x5c);
    // ltreeview paints unfocused selection as a faded copy of this over BG; tuned to land near SELECTION_BG.
    pub const TREE_UNFOCUSED_SEL: Color32 = Color32::from_rgb(0x46, 0x7b, 0xb7);

    // Notice levels.
    pub const ERROR: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x75);
    pub const WARN: Color32 = ACCENT;
    pub const INFO: Color32 = Color32::GRAY;

    // Grid cell roles. Hues are spread around the wheel so neighbouring regions read
    // as clearly different. Free is neutral grey (empty); used ones are saturated.
    pub const FREE: Color32 = Color32::from_rgb(0x6b, 0x72, 0x7d); // grey = empty
    pub const RESERVED: Color32 = Color32::from_rgb(0xd0, 0x7b, 0x34); // orange = overhead
    pub const META: Color32 = Color32::from_rgb(0x4a, 0x9e, 0xde); // blue = metadata (base)
    pub const META_TABLE: Color32 = Color32::from_rgb(0x3f, 0x79, 0xcc); // blue = FAT/inode/alloc maps
    pub const META_DIR: Color32 = Color32::from_rgb(0x9b, 0x6e, 0xe0); // violet = directories
    pub const META_JOURNAL: Color32 = Color32::from_rgb(0x2f, 0xb3, 0xc9); // cyan = journal
    pub const DATA: Color32 = Color32::from_rgb(0x64, 0xc0, 0x4a); // green = content
    pub const UNKNOWN: Color32 = Color32::from_rgb(0xff, 0x2f, 0xce); // role outside the legend → bug

    // Ruler. Shades of one neutral hue: a region has no role of its own (it holds
    // blocks of several roles at once), so color here only marks boundaries. The hue
    // sits outside every role color — the bar must not read as a kind of block.
    pub const RULER_SEG: [Color32; 5] = [
        Color32::from_rgb(0x4a, 0x46, 0x36),
        Color32::from_rgb(0x63, 0x60, 0x4a),
        Color32::from_rgb(0x7d, 0x79, 0x60),
        Color32::from_rgb(0x97, 0x92, 0x77),
        Color32::from_rgb(0xb0, 0xac, 0x90),
    ];
    pub const RULER_GAP: Color32 = Color32::from_rgb(0x26, 0x24, 0x1e); // blocks outside any span

    // Cell outlines (not the role fill): hover and selection (block or file).
    pub const GRID_HOVER: Color32 = Color32::WHITE;
    pub const GRID_SELECTED: Color32 = Color32::YELLOW;
}

use fs_api::view::{Category, MetaKind, RoleDesc, RoleId};

/// Cell color by its role. `RoleId` is an index into `legend()`. The driver gives no
/// RGB — color is derived from `Category`. A role outside the legend is allowed by the
/// contract (the consumer must tolerate it) → `UNKNOWN`, not a panic.
pub fn role_color(legend: &[RoleDesc], role: RoleId) -> egui::Color32 {
    match legend.get(role.index()) {
        Some(desc) => desc_color(desc),
        None => palette::UNKNOWN,
    }
}

/// Color straight from the descriptor — for the legend panel, where `RoleId` is not
/// needed (we iterate `legend()`, and every `RoleDesc` already carries `category`).
pub fn desc_color(desc: &RoleDesc) -> egui::Color32 {
    category_color(desc.category)
}

fn category_color(cat: Category) -> egui::Color32 {
    match cat {
        Category::Free => palette::FREE,
        Category::Reserved => palette::RESERVED,
        Category::Data => palette::DATA,
        Category::Metadata(kind) => meta_color(kind),
    }
}

fn meta_color(kind: Option<MetaKind>) -> egui::Color32 {
    match kind {
        Some(MetaKind::AllocMap | MetaKind::FileTable | MetaKind::BlockMap) => palette::META_TABLE,
        Some(MetaKind::Directory) => palette::META_DIR,
        Some(MetaKind::Journal) => palette::META_JOURNAL,
        _ => palette::META, // headers + None + future variants (MetaKind is non_exhaustive)
    }
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::empty();
    fonts.font_data.insert(
        "term".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/JetBrainsMono-Regular.ttf"
        ))),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("term".into());
    }
    ctx.set_fonts(fonts);
}

pub fn docs_visuals() -> egui::Visuals {
    use palette::*;
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = SUB;
    v.faint_bg_color = SUB;
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = SELECTION_BG;
    v.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    v.window_stroke = egui::Stroke::new(1.0, BORDER);
    v
}
