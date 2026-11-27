use std::ops::Range;

use super::{
    combine_char, CombineResult, CombinedSequenceAction, CombinedSequenceDiscard, Utf8Config,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TextCell {
    pub(super) text: String,
    pub(super) width: u8,
    pub(super) source: Range<usize>,
}

/// Fold display cells while retaining the contiguous source range that each
/// cell owns. Discarded zero-width and over-bound bytes belong to the nearest
/// cell so truncation can return an actual source prefix or suffix.
pub(super) fn fold_text_cells(value: &str, config: &Utf8Config) -> Vec<TextCell> {
    let mut cells = Vec::new();
    let mut discard = CombinedSequenceDiscard::default();
    let mut leading_discard_start = None;
    for (source_start, ch) in value.char_indices() {
        let source_end = source_start + ch.len_utf8();
        let action = discard.action_for(ch, config);
        if matches!(action, CombinedSequenceAction::Discard) {
            extend_discarded_source(
                &mut cells,
                &mut leading_discard_start,
                source_start,
                source_end,
            );
            continue;
        }
        let previous = match action {
            CombinedSequenceAction::Combine => cells
                .last()
                .map(|cell: &TextCell| (cell.text.as_str(), cell.width)),
            CombinedSequenceAction::Standalone => None,
            CombinedSequenceAction::Discard => unreachable!("discarded characters continue early"),
        };
        match combine_char(previous, ch, config) {
            CombineResult::Standalone { width } => cells.push(TextCell {
                text: ch.to_string(),
                width,
                source: leading_discard_start.take().unwrap_or(source_start)..source_end,
            }),
            CombineResult::Combined { text, width } => {
                if let Some(cell) = cells.last_mut() {
                    cell.text = text;
                    cell.width = width;
                    cell.source.end = source_end;
                }
            }
            CombineResult::Overflow(next) => {
                extend_discarded_source(
                    &mut cells,
                    &mut leading_discard_start,
                    source_start,
                    source_end,
                );
                discard = next;
            }
            CombineResult::Discard => extend_discarded_source(
                &mut cells,
                &mut leading_discard_start,
                source_start,
                source_end,
            ),
        }
    }
    cells
}

fn extend_discarded_source(
    cells: &mut [TextCell],
    leading_discard_start: &mut Option<usize>,
    source_start: usize,
    source_end: usize,
) {
    if let Some(cell) = cells.last_mut() {
        cell.source.end = source_end;
    } else {
        leading_discard_start.get_or_insert(source_start);
    }
}
