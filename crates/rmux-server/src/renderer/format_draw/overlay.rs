use rmux_core::{style_tostring, Style};

use super::{CanvasSlot, FormattedLine};

impl FormattedLine {
    /// Copies already laid-out cells, clearing any intersected wide glyph.
    pub(crate) fn overlay(&mut self, offset: usize, source: &Self) {
        for (column, slot) in source.canvas.slots.iter().enumerate() {
            if let CanvasSlot::Lead(cell) = slot {
                self.canvas
                    .place_cell(offset.saturating_add(column), cell.clone());
            }
        }
        self.ranges = self.canvas.ranges();
    }

    /// Projects drawn cells back to markup without replaying layout directives.
    pub(crate) fn to_styled_text(&self) -> String {
        let mut text = String::new();
        let mut previous = None;
        for slot in &self.canvas.slots {
            let (style, value) = match slot {
                CanvasSlot::Lead(cell) => {
                    let mut style = Style::with_cell(cell.style.cell);
                    style.cell.bg = super::super::effective_background(&cell.style, true);
                    (style, cell.text.as_str())
                }
                CanvasSlot::Empty => (Style::default(), " "),
                CanvasSlot::Continuation(_) => continue,
            };
            if previous.as_ref() != Some(&style) {
                text.push_str("#[default]");
                text.push_str(&format!("#[{}]", style_tostring(&style)));
                previous = Some(style);
            }
            text.push_str(&value.replace('#', "##"));
        }
        text.push_str("#[default]");
        text
    }
}
