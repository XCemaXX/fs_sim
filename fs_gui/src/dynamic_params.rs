use egui::Ui;

use fs_api::{
    driver::{Level, Notice},
    params::{ParamField, ParamKind, ParamSchema, ParamValue, ParamValues, SizeDisplay},
};

use crate::style::palette;

const KB: u64 = 1024;

pub struct Params {
    schema: ParamSchema,
    values: ParamValues,
}

impl Params {
    pub fn values(&self) -> &ParamValues {
        &self.values
    }

    pub fn from_schema(schema: ParamSchema) -> Self {
        let values = ParamValues::from_schema(&schema);
        Self { schema, values }
    }

    pub fn draw(&mut self, ui: &mut Ui, id: &str, label: &str) {
        ui.label(label);
        let widths = self.measure(ui, id);
        let cols = best_columns(&widths, ui.available_width(), ui.spacing().item_spacing.x);
        egui::Grid::new(id)
            .num_columns(cols)
            .striped(true)
            .show(ui, |ui| {
                for (i, field) in self.schema.fields.iter().enumerate() {
                    ui.horizontal(|ui| draw_field(ui, field, &mut self.values));
                    if (i + 1) % cols == 0 {
                        ui.end_row();
                    }
                }
            });
    }

    /// Natural width of each field via an invisible sizing pass. A separate `Ui` (not
    /// `scope_builder`), because that one would move the panel cursor and eat space.
    fn measure(&mut self, ui: &Ui, id: &str) -> Vec<f32> {
        let big = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(1e5));
        let mut scratch = Ui::new(
            ui.ctx().clone(),
            ui.scope_id().with(("param_measure", id)),
            egui::UiBuilder::new()
                .sizing_pass()
                .invisible()
                .max_rect(big),
        );
        self.schema
            .fields
            .iter()
            .map(|field| {
                scratch
                    .horizontal(|ui| draw_field(ui, field, &mut self.values))
                    .response
                    .rect
                    .width()
            })
            .collect()
    }

    pub fn draw_notices(&self, ui: &mut egui::Ui, notices: &[Notice]) {
        for n in notices {
            let text = match &n.field {
                Some(key) => format!("{}: {}", self.label_of(key), n.msg),
                None => n.msg.clone(),
            };
            ui.colored_label(level_color(n.level), text);
        }
    }

    fn label_of<'a>(&'a self, key: &'a str) -> &'a str {
        self.schema
            .fields
            .iter()
            .find(|f| f.key == key)
            .map_or(key, |f| f.label.as_str())
    }
}

/// Draws one field: `label` + control, in a row. A free function (not a method) so
/// `&self.schema` and `&mut self.values` can be borrowed separately; the same code runs
/// for measuring and for real drawing → the measured width matches the real one.
fn draw_field(ui: &mut Ui, field: &ParamField, values: &mut ParamValues) {
    // The value always exists and has the right type: `Params::from_schema` seeded every
    // field and `set` keeps the type. `expect` over `unwrap_or(default)` — do not hide
    // a desync.
    let seeded = "field seeded by Params::from_schema";
    match &field.kind {
        ParamKind::Int { min, max, .. } => {
            ui.label(&field.label).on_hover_text(&field.help);
            let was = values.int(&field.key).expect(seeded);
            let mut v = was;
            ui.add(egui::Slider::new(&mut v, *min..=*max));
            if v != was {
                values.set(&field.key, ParamValue::Int(v));
            }
        }
        ParamKind::Size {
            min, max, display, ..
        } => {
            ui.label(&field.label).on_hover_text(&field.help);
            let was = values.size(&field.key).expect(seeded);
            let mut v = was;
            // Widget choice comes from the schema (display), not from the key string.
            let widget = match display {
                SizeDisplay::Pow2 => slider_pow2(ui, &mut v, *min, *max),
                SizeDisplay::Kb => slider_kb(ui, &mut v, *min, *max),
                _ => ui.add(egui::Slider::new(&mut v, *min..=*max).logarithmic(true)),
            };
            widget.on_hover_text(&field.help);
            if v != was {
                values.set(&field.key, ParamValue::Size(v));
            }
        }
        ParamKind::Bool { .. } => {
            ui.label(&field.label).on_hover_text(&field.help);
            let was = values.bool(&field.key).expect(seeded);
            let mut v = was;
            ui.checkbox(&mut v, "");
            if v != was {
                values.set(&field.key, ParamValue::Bool(v));
            }
        }
        ParamKind::Choice { options, .. } => {
            ui.label(&field.label).on_hover_text(&field.help);
            let was = values.choice(&field.key).expect(seeded);
            let mut v = was;
            egui::ComboBox::from_id_salt(&field.key)
                .selected_text(v)
                .show_ui(ui, |ui| {
                    for (val, label) in options {
                        ui.selectable_value(&mut v, val, label);
                    }
                });
            if v != was {
                values.set(&field.key, ParamValue::Choice(v.to_owned()));
            }
        }
        _ => {
            ui.label(&field.label).on_hover_text(&field.help);
            ui.colored_label(palette::WARN, "unsupported parameter type");
        }
    }
}

/// Largest column count whose rows still fit `avail`. A column's width is the max field
/// in it under row-major layout. n ≤ 20, so brute force is fine.
fn best_columns(widths: &[f32], avail: f32, spacing: f32) -> usize {
    let n = widths.len();
    let mut best = 1;
    for k in 1..=n {
        let mut total = spacing * (k - 1) as f32;
        for j in 0..k {
            let col = widths
                .iter()
                .skip(j)
                .step_by(k)
                .copied()
                .fold(0.0, f32::max);
            total += col;
        }
        if total <= avail {
            best = k;
        }
    }
    best
}

fn level_color(level: Level) -> egui::Color32 {
    match level {
        Level::Error => palette::ERROR,
        Level::Warn => palette::WARN,
        Level::Info => palette::INFO,
    }
}

/// Smallest `n` with `2^n >= v`.
fn ilog2_ceil(v: u64) -> u32 {
    let v = v.max(1);
    let n = v.ilog2();
    if v > 1 << n { n + 1 } else { n }
}

/// Power-of-two slider: it moves over the exponent `n`, the value is `2^n`. The
/// formatter shows bytes, not the exponent.
fn slider_pow2(ui: &mut Ui, v: &mut u64, min: u64, max: u64) -> egui::Response {
    // Bounds are rounded inward: rounding min down or max up would let the slider leave
    // the range allowed by the schema.
    let lo = ilog2_ceil(min);
    let hi = max.max(1).ilog2();
    let mut exp = (*v).max(1).ilog2().clamp(lo, hi);
    let r = ui.add(
        egui::Slider::new(&mut exp, lo..=hi)
            .custom_formatter(|n, _| (1u64 << n as u32).to_string()),
    );
    *v = 1u64 << exp;
    r
}

/// Large size input in KB. `v` (bytes) is read and written as-is — KB is only the
/// presentation. Logarithmic: the range spans many orders of magnitude.
fn slider_kb(ui: &mut Ui, v: &mut u64, min: u64, max: u64) -> egui::Response {
    let mut kb = *v / KB;
    let r = ui.add(
        egui::Slider::new(&mut kb, min / KB..=max / KB)
            .suffix(" KB")
            .logarithmic(true),
    );
    *v = kb * KB;
    r
}
