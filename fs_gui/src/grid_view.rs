use std::ops::Range;

use egui::Ui;
use fs_api::Mount;

use crate::{grid::CellGrid, legend, page::PageView, ruler};

/// Central area of a volume: the ruler map (top), pager + legend (bottom) and the block
/// grid. Owns the renderer (`CellGrid`) and the page source (`PageView`) — all the
/// lagged-capacity orchestration is hidden here, with a single `render` outside.
pub struct BlockGridView {
    grid: CellGrid,
    page: PageView,
}

impl BlockGridView {
    pub fn new() -> Self {
        Self {
            grid: CellGrid::new(),
            page: PageView::new(),
        }
    }

    /// Draws ruler/pager/grid and returns the absolute number of the clicked block.
    /// `spans` — selection highlight; `version` — the mesh invalidation axis.
    pub fn render(
        &mut self,
        ui: &mut Ui,
        mount: &dyn Mount,
        version: u64,
        spans: &[Range<u64>],
    ) -> Option<u64> {
        let total = mount.usage().total;
        // cap from the previous frame: the slider and the ruler window marker read it
        // before the grid renders. The one-frame lag is deliberate — it breaks the size
        // cycle within a frame.
        let cap_lag = self.grid.capacity().max(1) as u64;
        let last_page = total.div_ceil(cap_lag).max(1) - 1; // 0-based
        self.page.page = self.page.page.min(last_page);

        // Pager before the ruler, so the window marker is computed after all page edits
        // instead of lagging a frame behind its own click.
        egui::Panel::bottom("pager").show(ui, |ui| {
            legend::draw(ui, mount.legend());
            // On a single-page disk the slider degenerates to 0..=0 while Prev/Next stay
            // enabled and do nothing — hide the pager entirely.
            if last_page == 0 {
                return;
            }
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut self.page.page, 0..=last_page).text("page"));
                ui.label(format!("of {}", last_page + 1)); // last_page is 0-based
                if ui.button("Prev").clicked() {
                    self.page.page = self.page.page.saturating_sub(1);
                }
                if ui.button("Next").clicked() {
                    self.page.page = (self.page.page.saturating_add(1)).min(last_page);
                }
            });
        });

        let win_start = self.page.page * cap_lag;
        let window = win_start..(win_start + cap_lag).min(total);
        // Create the panel only when there is something to draw: for a driver without
        // groups it would be an empty strip stealing height from the grid.
        let groups = mount.geometry().groups();
        if !groups.is_empty() {
            egui::Panel::top("ruler").show(ui, |ui| {
                if let Some(block) = ruler::draw(ui, groups, total, window) {
                    self.page.page = (block / cap_lag).min(last_page);
                }
            });
        }

        // Data window uses this frame's EXACT capacity (after the panels are subtracted).
        let cap = CellGrid::capacity_for(ui.available_size()).max(1) as u64;
        let start = self.page.sync(mount, cap, version);
        self.grid
            .render(ui, start, self.page.colors(), version, spans)
    }

    /// Scroll to the page holding this block. cap is from the previous frame (lag is harmless).
    pub fn jump_to(&mut self, block: u64) {
        let cap = self.grid.capacity().max(1) as u64;
        self.page.page = block / cap;
    }
}
