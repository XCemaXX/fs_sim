use std::ops::Range;

use egui::{Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use fs_api::view::GroupLevel;

use crate::style::palette;

const BAR_H: f32 = 16.0;
const SWATCH: f32 = 11.0;
// The cap is on levels, not on height: depth comes from the driver, which has no
// reason to hold it back.
const MAX_LEVELS: usize = 3;

/// Proportional map of the whole disk by group levels: one tier per level, outermost on
/// top. `window` is the slice the grid shows right now (drawn as a marker). Returns the
/// block under the cursor if the bar was clicked (to jump to that page).
pub fn draw(ui: &mut Ui, levels: &[GroupLevel], total: u64, window: Range<u64>) -> Option<u64> {
    let shown: Vec<&GroupLevel> = levels
        .iter()
        .filter(|l| !l.spans().is_empty())
        .take(MAX_LEVELS)
        .collect();
    if total == 0 || shown.is_empty() {
        return None;
    }

    let mut clicked = None;
    let mut unnamed: Vec<(Color32, &str)> = Vec::new();
    let mut stack: Option<Rect> = None;

    for (tier, level) in shown.iter().enumerate() {
        // Level name goes above the bar. On the left it would take different widths per
        // tier, and the bars would stop lining up on x.
        if shown.len() > 1 {
            ui.label(egui::RichText::new(level.name()).small());
        }
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), BAR_H), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let xmap = |b: u64| rect.left() + rect.width() * (b as f32 / total as f32);

        // The contract only requires spans to be increasing and non-overlapping.
        // Uncovered space must look uncovered, not like the panel background.
        painter.rect_filled(rect, CornerRadius::ZERO, palette::RULER_GAP);

        for (i, s) in level.spans().iter().enumerate() {
            let seg = Rect::from_min_max(
                pos2(xmap(s.range().start()), rect.top()),
                pos2(xmap(s.range().end()), rect.bottom()),
            );
            // Shift per tier: a nested span must not blend into its parent.
            let color = palette::RULER_SEG[(i + tier) % palette::RULER_SEG.len()];
            painter.rect_filled(seg, CornerRadius::ZERO, color);

            // Measure the laid-out text, not the string length: in bytes non-ASCII is
            // twice as long, so a label would be dropped where it actually fits.
            let galley = painter.layout_no_wrap(
                s.label().to_owned(),
                FontId::proportional(11.0),
                palette::BG,
            );
            if seg.width() > galley.size().x {
                painter.galley(seg.center() - galley.size() / 2.0, galley, palette::BG);
            } else {
                unnamed.push((color, s.label()));
            }
        }

        stack = Some(stack.map_or(rect, |r: Rect| r.union(rect)));
        // Click and drag, like a slider — not a single click only.
        if (response.clicked() || response.dragged())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let frac = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            clicked = Some(((frac as f64 * total as f64) as u64).min(total.saturating_sub(1)));
        }
    }

    // One grid-window marker for the whole stack: tiers share a single x scale.
    if let Some(stack) = stack {
        let xmap = |b: u64| stack.left() + stack.width() * (b as f32 / total as f32);
        ui.painter().rect_stroke(
            Rect::from_min_max(
                pos2(xmap(window.start), stack.top() - 1.0),
                pos2(xmap(window.end.min(total)), stack.bottom() + 1.0),
            ),
            CornerRadius::ZERO,
            Stroke::new(2.0, palette::GRID_HOVER),
            StrokeKind::Inside,
        );
    }

    // The legend names only the regions too narrow for an inline label, so it does not
    // grow with the number of tiers.
    if !unnamed.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for (color, label) in unnamed {
                let (sw, _) = ui.allocate_exact_size(vec2(SWATCH, SWATCH), Sense::hover());
                ui.painter().rect_filled(sw, CornerRadius::same(2), color);
                ui.label(label);
            }
        });
    }

    clicked
}
