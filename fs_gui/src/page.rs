use egui::Color32;
use fs_api::{Mount, disk::BlockRange};

use crate::style::{self, palette};

/// A disk window filled with role colors. The buffer is rebuilt lazily — only when
/// `(version, page, cap)` changes; by contract `page_cells`/`legend` are expensive and
/// must not be called every frame.
pub struct PageView {
    pub page: u64,        // current page, 0-based
    colors: Vec<Color32>, // private: must match `built`, which `sync` maintains
    built: Option<BuildKey>,
}

#[derive(Clone, Copy, PartialEq)]
struct BuildKey {
    version: u64,
    page: u64,
    cap: u64,
}

impl PageView {
    pub fn new() -> Self {
        Self {
            page: 0,
            colors: Vec::new(),
            built: None,
        }
    }

    pub fn colors(&self) -> &[Color32] {
        &self.colors
    }

    /// Bring the buffer in line with `(version, page, cap)`, rebuilding it on change.
    /// Returns the absolute number of the window's first block (`base` for the grid).
    pub fn sync(&mut self, mount: &dyn Mount, cap: u64, version: u64) -> u64 {
        // The caller picks the page from the previous frame's capacity; when the window
        // grows it can run past the end of the disk. Clamp here — the only place that
        // knows the page, this frame's real capacity and the disk size at once.
        let last_page = mount.usage().total.div_ceil(cap.max(1)).max(1) - 1;
        self.page = self.page.min(last_page);

        let start = self.page * cap;
        let key = BuildKey {
            version,
            page: self.page,
            cap,
        };
        if self.built != Some(key) {
            self.rebuild(mount, start, cap);
            self.built = Some(key);
        }
        start
    }

    fn rebuild(&mut self, mount: &dyn Mount, start: u64, cap: u64) {
        let legend = mount.legend();
        let cells = mount.page_cells(BlockRange::new(start, start + cap));

        self.colors.clear();
        self.colors
            .resize(cells.window().len() as usize, palette::UNKNOWN);
        for run in cells.cells() {
            let color = style::role_color(legend, run.role);
            // run.range is absolute; local index = block − start
            let lo = run.range.start().saturating_sub(start);
            let hi = run
                .range
                .end()
                .saturating_sub(start)
                .min(self.colors.len() as u64);
            for i in lo..hi {
                self.colors[i as usize] = color;
            }
        }
    }
}
