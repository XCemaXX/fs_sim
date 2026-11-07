use std::sync::Arc;

use egui::{Color32, CornerRadius, Mesh, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::style::palette;

/// Pure renderer of a block window. The caller owns the data and the page.
pub struct CellGrid {
    cols: usize,
    rows: usize,
    mesh: MeshCache,
    cap: usize,            // page capacity from the previous render (for the slider)
    last_hit: Option<u64>, // block under a held pointer last frame
}

impl CellGrid {
    pub const CELL_SIZE: Vec2 = egui::vec2(10.0, 10.0);
    pub const SPACING: f32 = 1.0;

    pub fn new() -> Self {
        Self {
            cols: 0,
            rows: 0,
            mesh: MeshCache::new(),
            cap: 1, // until the first render; keeps the slider from dividing by 0
            last_hit: None,
        }
    }

    fn step() -> Vec2 {
        egui::vec2(
            Self::CELL_SIZE.x + Self::SPACING,
            Self::CELL_SIZE.y + Self::SPACING,
        )
    }

    // +SPACING: the last cell in a row/column has no trailing gap.
    fn cols_for(width: f32) -> usize {
        (((width + Self::SPACING) / Self::step().x).floor() as usize).max(1)
    }
    fn rows_for(height: f32) -> usize {
        (((height + Self::SPACING) / Self::step().y).floor() as usize).max(1)
    }

    // Page capacity for an area: a pure function of the viewport, independent of data.
    pub fn capacity_for(avail: Vec2) -> usize {
        Self::cols_for(avail.x) * Self::rows_for(avail.y)
    }

    /// Page capacity from the previous render. The slider reads it before `render`, so
    /// the value lags by one frame — deliberate, to avoid a size cycle within a frame.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    // cols/rows for the current area; rows shrink to the rows actually used.
    fn layout(&mut self, avail: Vec2, len: usize) -> Vec2 {
        self.cols = Self::cols_for(avail.x);
        let n = (self.cols * Self::rows_for(avail.y)).min(len);
        self.rows = n.div_ceil(self.cols).max(1);
        self.size()
    }

    fn size(&self) -> Vec2 {
        egui::vec2(
            self.cols as f32 * Self::CELL_SIZE.x + (self.cols - 1) as f32 * Self::SPACING,
            self.rows as f32 * Self::CELL_SIZE.y + (self.rows - 1) as f32 * Self::SPACING,
        )
        .max(Self::step())
    }

    fn visible(&self, n: usize) -> usize {
        (self.rows * self.cols).min(n)
    }

    fn cell_rect(&self, origin: Pos2, i: usize) -> Rect {
        let step = Self::step();
        let (cx, cy) = (i % self.cols, i / self.cols);
        Rect::from_min_size(
            origin + egui::vec2(cx as f32 * step.x, cy as f32 * step.y),
            Self::CELL_SIZE,
        )
    }

    // A click in the gap snaps to the neighbouring cell — intended.
    fn cell_at(&self, origin: Pos2, n: usize, p: Pos2) -> Option<usize> {
        let local = p - origin;
        if local.x < 0.0 || local.y < 0.0 {
            return None;
        }
        let step = Self::step();
        let col = (local.x / step.x).floor() as usize;
        let row = (local.y / step.y).floor() as usize;
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let idx = row * self.cols + col;
        (idx < n).then_some(idx)
    }

    fn build_mesh(&self, origin: Pos2, cells: &[Color32]) -> Mesh {
        let n = self.visible(cells.len());
        let mut m = Mesh::default();
        m.reserve_vertices(n * 4);
        m.reserve_triangles(n * 2);
        for (i, &color) in cells.iter().take(n).enumerate() {
            m.add_colored_rect(self.cell_rect(origin, i), color);
        }
        m
    }

    /// `base` — absolute number of the window's first block; `cells` — window colors;
    /// `version` is bumped by the caller on any data change; `spans` — absolute ranges
    /// to outline as selected (anything outside the window is dropped). Returns the
    /// absolute index of the clicked block; a drag reports each block once, not per frame.
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        base: u64,
        cells: &[Color32],
        version: u64,
        spans: &[std::ops::Range<u64>],
    ) -> Option<u64> {
        let avail = ui.available_size();
        self.cap = Self::capacity_for(avail); // remembered for next frame's slider
        let desired = self.layout(avail, cells.len());
        let (response, painter) = ui.allocate_painter(desired, egui::Sense::click_and_drag());
        let origin = response.rect.min;

        let key = MeshKey {
            version,
            base,
            cols: self.cols,
            rows: self.rows,
            origin,
        };
        if self.mesh.is_invalidated(&key) {
            let mesh = self.build_mesh(origin, cells);
            self.mesh.store(mesh, key);
        }
        painter.add(self.mesh.mesh.clone()); // Arc<Mesh>: cheap clone, no vertex copy

        let n = self.visible(cells.len());
        let mut hit = None;
        if (response.clicked() || response.dragged())
            && let Some(p) = response.interact_pointer_pos()
            && let Some(i) = self.cell_at(origin, n, p)
        {
            hit = Some(base + i as u64);
        }
        // Frames without a hit reset it, so a new click on the same block still reports.
        let clicked = hit.filter(|_| hit != self.last_hit);
        self.last_hit = hit;

        self.highlight(&painter, origin, base, n, spans);
        // Hover and selection frames are live strokes over the cache; the mesh stays.
        if let Some(p) = response.hover_pos()
            && let Some(i) = self.cell_at(origin, n, p)
        {
            painter.rect_stroke(
                self.cell_rect(origin, i),
                CornerRadius::ZERO,
                Stroke::new(1.5, palette::GRID_HOVER),
                StrokeKind::Inside,
            );
        }

        clicked
    }

    /// Outline the selected ranges over the mesh. Ranges outside the window are dropped.
    fn highlight(
        &self,
        painter: &egui::Painter,
        origin: Pos2,
        base: u64,
        shown: usize,
        spans: &[std::ops::Range<u64>],
    ) {
        for span in spans {
            let lo = span.start.max(base);
            let hi = span.end.min(base + shown as u64);
            for b in lo..hi {
                painter.rect_stroke(
                    self.cell_rect(origin, (b - base) as usize),
                    CornerRadius::ZERO,
                    Stroke::new(1.5, palette::GRID_SELECTED),
                    StrokeKind::Inside,
                );
            }
        }
    }
}

// The mesh stays valid while every key value matches. Each one is its own
// invalidation axis.
#[derive(Clone, Copy, PartialEq)]
struct MeshKey {
    version: u64, // window data (bumped on any change to blocks)
    base: u64,    // window position (page)
    cols: usize,  // grid geometry
    rows: usize,
    origin: Pos2, // screen position: the mesh bakes in absolute coordinates
}

struct MeshCache {
    mesh: Arc<Mesh>,
    key: Option<MeshKey>, // None = never built
}

impl MeshCache {
    fn new() -> Self {
        Self {
            mesh: Arc::new(Mesh::default()),
            key: None,
        }
    }

    fn is_invalidated(&self, key: &MeshKey) -> bool {
        self.key != Some(*key)
    }

    fn store(&mut self, mesh: Mesh, key: MeshKey) {
        self.mesh = Arc::new(mesh);
        self.key = Some(key);
    }
}
