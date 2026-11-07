use egui::{Color32, Response, Sense, TextStyle, Ui, Widget};
use fs_api::view::RoleDesc;

use crate::style;

/// Legend entry: color square + name in one `allocate`. A single widget so that
/// `horizontal_wrapped` wraps the pair as a whole; a nested `ui.horizontal` never
/// triggers a wrap (containers bypass the wrap logic).
struct LegendItem<'a> {
    color: Color32,
    name: &'a str,
}

impl<'a> LegendItem<'a> {
    fn new(color: Color32, name: &'a str) -> Self {
        Self { color, name }
    }
}

impl Widget for LegendItem<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let font = TextStyle::Body.resolve(ui.style());
        let color = ui.visuals().text_color();
        let galley = ui
            .painter()
            .layout_no_wrap(self.name.to_owned(), font, color);

        let text = galley.size();
        let side = text.y * 0.6; // square is smaller than the line and centered on it
        let gap = ui.spacing().item_spacing.x * 0.5;
        let size = egui::vec2(side + gap + text.x, text.y);
        let (rect, response) = ui.allocate_at_least(size, Sense::hover());

        let swatch = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - side / 2.0),
            egui::vec2(side, side),
        );
        let text_pos = egui::pos2(swatch.right() + gap, rect.center().y - text.y / 2.0);
        let painter = ui.painter();
        painter.rect_filled(swatch, 2.0, self.color);
        painter.galley(text_pos, galley, color);

        response
    }
}

/// Legend panel: always a single line. With wrapping, a narrow window turns six roles
/// into six lines and the legend eats into the grid — overflow goes to scrolling.
/// `RoleId` is not needed: the color comes straight from `RoleDesc`.
pub fn draw(ui: &mut Ui, legend: &[RoleDesc]) {
    egui::ScrollArea::horizontal().show(ui, |ui| {
        ui.horizontal(|ui| {
            for role in legend {
                ui.add(LegendItem::new(style::desc_color(role), &role.name))
                    .on_hover_text(&role.description);
            }
        });
    });
}
