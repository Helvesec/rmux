//! The compact plain-ASCII representation of a line: writing into it,
//! compacting a cell row into it, and materialising it back into cells.

use compact_str::CompactString;

use super::{GridCell, GridLine, GridLineFlags};

impl GridLine {
    pub(crate) fn write_plain_ascii_run(&mut self, start: u32, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        let start = start as usize;
        let Some(end) = start.checked_add(bytes.len()) else {
            return false;
        };
        if end > self.width as usize {
            return false;
        }
        if self.plain_text.is_some() && self.cells.is_empty() {
            self.update_plain_text_cache(start, bytes);
            self.touch();
            return true;
        }
        if end > self.cells.len() {
            return false;
        }
        if self.cells[start..end]
            .iter()
            .any(|cell| cell.width() != 1 || cell.is_padding())
        {
            return false;
        }
        if self.cells.get(end).is_some_and(GridCell::is_padding) {
            return false;
        }

        let can_cache_plain_text = self.can_cache_plain_ascii_run(start);
        for (cell, byte) in self.cells[start..end].iter_mut().zip(bytes) {
            cell.set_plain_ascii(*byte);
        }
        if can_cache_plain_text {
            self.update_plain_text_cache(start, bytes);
        } else {
            self.plain_text = None;
            self.plain_text_data_end = 0;
        }
        self.touch();
        true
    }

    pub(crate) fn compact_for_history(&mut self) {
        if self.try_compact_plain_text() {
            return;
        }
        if self.flags.contains(GridLineFlags::WRAPPED) {
            return;
        }

        let used_end = self.painted_end();
        if used_end < self.cells.len() {
            self.cells.truncate(used_end);
            self.cells.shrink_to_fit();
        }
    }

    fn try_compact_plain_text(&mut self) -> bool {
        if self.plain_text.is_some() {
            self.cells.clear();
            self.cells.shrink_to_fit();
            return true;
        }
        let Some(compacted) = self.compacted_plain_text_clone() else {
            return false;
        };
        *self = compacted;
        true
    }

    fn compacted_plain_text_clone(&self) -> Option<Self> {
        if let Some(text) = &self.plain_text {
            return Some(Self {
                cells: Vec::new(),
                plain_text: Some(text.clone()),
                width: self.width,
                plain_text_data_end: self.plain_text_data_end,
                flags: self.flags,
                time: self.time,
                revision: self.revision,
                hidden_columns: self.hidden_columns.clone(),
            });
        }
        if self.cells.iter().any(GridCell::is_reflow_gap) {
            return None;
        }
        let used_end = self.painted_end();
        // A compact row would join its wrap after its written cells, but a
        // short wrapped cell row joins at its full width (`wrapped_end`).
        if self.flags.contains(GridLineFlags::WRAPPED) && used_end < self.width as usize {
            return None;
        }
        if used_end == 0 {
            return Some(Self {
                cells: Vec::new(),
                plain_text: Some(CompactString::new("")),
                width: self.width,
                plain_text_data_end: 0,
                flags: self.flags,
                time: self.time,
                revision: self.revision,
                hidden_columns: self.hidden_columns.clone(),
            });
        }

        let cells = self.cells.get(..used_end)?;
        if !cells.iter().all(GridCell::is_plain_default_ascii) {
            return None;
        }

        let mut text = String::with_capacity(used_end);
        for cell in cells {
            text.push_str(cell.text.as_str());
        }
        Some(Self {
            cells: Vec::new(),
            plain_text: Some(CompactString::from(text)),
            width: self.width,
            plain_text_data_end: u16::try_from(used_end).unwrap_or(u16::MAX),
            flags: self.flags,
            time: self.time,
            revision: self.revision,
            hidden_columns: self.hidden_columns.clone(),
        })
    }

    fn can_cache_plain_ascii_run(&self, start: usize) -> bool {
        if self.plain_text.is_some() {
            return true;
        }
        // A wrapped cell row joins at its full width (`wrapped_end`), which a
        // compact row would not keep.
        start == 0
            && !self.flags.contains(GridLineFlags::WRAPPED)
            && self.cells.iter().all(GridCell::is_blank)
    }

    fn update_plain_text_cache(&mut self, start: usize, bytes: &[u8]) {
        let text = self.plain_text.get_or_insert_with(CompactString::default);
        if text.len() < start {
            for _ in text.len()..start {
                text.push(' ');
            }
        }
        let end = start + bytes.len();
        let replacement = std::str::from_utf8(bytes).expect("plain ascii run must be utf-8");
        if text.len() < end {
            text.truncate(start);
            text.push_str(replacement);
        } else {
            text.replace_range(start..end, replacement);
        }
        while text.ends_with(' ') {
            text.pop();
        }
        let data_end = usize::from(self.plain_text_data_end)
            .max(end)
            .max(text.len());
        self.plain_text_data_end = u16::try_from(data_end).unwrap_or(u16::MAX);
    }

    pub(super) fn materialize_plain_text(&mut self, width: usize) {
        let Some(text) = self.plain_text.take() else {
            return;
        };
        self.cells = text.bytes().map(GridCell::from_plain_ascii).collect();
        let data_end = usize::from(self.plain_text_data_end).min(width);
        self.cells
            .resize(data_end, GridCell::from_plain_ascii(b' '));
        if self.cells.len() > width {
            self.cells.truncate(width);
        }
        if self.cells.len() < width {
            self.cells.resize(width, GridCell::default());
        }
        self.plain_text_data_end = 0;
        self.width = u32::try_from(width).unwrap_or(u32::MAX);
    }
}
