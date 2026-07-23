//! SDK-only pane extraction helpers.
//!
//! This module keeps the public extraction surface on the SDK side of the
//! daemon boundary. Raw helpers collect bytes from the pane-output stream,
//! while text helpers search rendered snapshot lines produced from structured
//! cells. There is deliberately no core or daemon regex search API here.

use crate::{Pane, PaneExitState, PaneOutputChunk, PaneOutputStart, PaneSnapshot, Result};

const COLLECT_OUTPUT_UNTIL_EXIT_OPERATION: &str = "collect pane output until exit";

/// Raw pane output collected while waiting for a pane process to exit.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct CollectedPaneOutput {
    /// Raw bytes emitted by the pane after collection started.
    ///
    /// The bytes are copied directly from [`PaneOutputChunk::Bytes`] items and
    /// are capped by the caller-supplied byte limit.
    pub bytes: Vec<u8>,
    /// Exit details observed after the output stream closed.
    ///
    /// `None` means the pane slot was already stale or vanished before the
    /// daemon could expose retained exit metadata.
    pub exit_state: Option<PaneExitState>,
    /// Whether output bytes were dropped because `bytes` reached the supplied
    /// byte limit.
    pub truncated: bool,
    /// Whether the underlying output stream reported at least one lag gap.
    pub lagged: bool,
    /// Total missed output events reported by lag notices.
    pub missed_events: u64,
}

impl CollectedPaneOutput {
    /// Returns the number of raw bytes retained in [`Self::bytes`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns whether no raw bytes were retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// A literal rendered-text match in a pane snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneTextMatch {
    /// Zero-based visible row where the match starts.
    pub start_row: u16,
    /// Zero-based visible column where the match starts.
    pub start_col: u16,
    /// Zero-based visible row where the match ends.
    pub end_row: u16,
    /// Exclusive visible column where the match ends.
    pub end_col: u16,
    /// The rendered text that matched.
    pub text: String,
}

impl PaneTextMatch {
    fn new(row: u16, start_col: u16, end_col: u16, text: String) -> Self {
        Self {
            start_row: row,
            start_col,
            end_row: row,
            end_col,
            text,
        }
    }
}

impl PaneSnapshot {
    /// Finds the first literal `needle` match in this snapshot's rendered
    /// visible text.
    ///
    /// Search is line-local and uses the same lossy rendered rows as
    /// [`Self::visible_lines`]: padding cells are skipped and trailing ASCII
    /// spaces are trimmed. Match coordinates are visible grid positions; a
    /// match that starts inside a wide glyph reports that glyph's owning
    /// cell column. Empty needles return `None`.
    #[must_use]
    pub fn find_text(&self, needle: impl AsRef<str>) -> Option<PaneTextMatch> {
        self.find_text_all(needle).into_iter().next()
    }

    /// Finds all literal `needle` matches in this snapshot's rendered visible
    /// text.
    ///
    /// See [`Self::find_text`] for rendering and coordinate semantics.
    #[must_use]
    pub fn find_text_all(&self, needle: impl AsRef<str>) -> Vec<PaneTextMatch> {
        find_text_in_snapshot(self, needle.as_ref())
    }
}

pub(crate) async fn find_text(pane: &Pane, needle: String) -> Result<Option<PaneTextMatch>> {
    Ok(pane.snapshot().await?.find_text(needle))
}

pub(crate) async fn find_text_all(pane: &Pane, needle: String) -> Result<Vec<PaneTextMatch>> {
    Ok(pane.snapshot().await?.find_text_all(needle))
}

pub(crate) async fn collect_output_until_exit(
    pane: &Pane,
    max_bytes: usize,
) -> Result<CollectedPaneOutput> {
    collect_output_until_exit_starting_at(pane, PaneOutputStart::Now, max_bytes).await
}

pub(crate) async fn collect_output_until_exit_starting_at(
    pane: &Pane,
    start: PaneOutputStart,
    max_bytes: usize,
) -> Result<CollectedPaneOutput> {
    let timeout = crate::wait::resolved_wait_timeout(pane.configured_default_timeout());
    let deadline = crate::wait::wait_deadline(timeout);
    let pane = pane.begin_operation_handle();
    let (pane, _) = crate::wait::with_wait_deadline(
        COLLECT_OUTPUT_UNTIL_EXIT_OPERATION,
        timeout,
        deadline,
        pane.pin_to_current_identity(),
    )
    .await?;
    crate::wait::with_wait_deadline(
        COLLECT_OUTPUT_UNTIL_EXIT_OPERATION,
        timeout,
        deadline,
        collect_output_until_exit_without_timeout(&pane, start, max_bytes),
    )
    .await
}

async fn collect_output_until_exit_without_timeout(
    pane: &Pane,
    start: PaneOutputStart,
    max_bytes: usize,
) -> Result<CollectedPaneOutput> {
    let mut collection = CollectedPaneOutput::default();
    let mut stream = match pane.output_stream_starting_at(start).await {
        Ok(stream) => stream,
        Err(error) if crate::handles::is_already_closed_pane_error(&error, pane.target()) => {
            collection.exit_state = exit_state_after_stream_close(pane).await?;
            return Ok(collection);
        }
        Err(error) => return Err(error),
    };

    if let crate::wait::PaneExitObservation::Exited(exit_state) =
        crate::wait::pane_exit_observation(pane).await?
    {
        drain_until_output_eof_or_close(&mut stream, &mut collection, max_bytes).await?;
        collection.exit_state = exit_state;
        return Ok(collection);
    }

    loop {
        let saw_ready_output =
            poll_ready_output_once(&mut stream, &mut collection, max_bytes).await?;
        if saw_ready_output.saw_eof {
            collection.exit_state = exit_state_after_stream_close(pane).await?;
            return Ok(collection);
        }
        if let crate::wait::PaneExitObservation::Exited(exit_state) =
            crate::wait::pane_exit_observation(pane).await?
        {
            drain_until_output_eof_or_close(&mut stream, &mut collection, max_bytes).await?;
            collection.exit_state = exit_state;
            return Ok(collection);
        }
        if !saw_ready_output.saw_output {
            tokio::time::sleep(crate::wait::TEXT_POLL_INTERVAL).await;
        }
    }
}

async fn drain_until_output_eof_or_close(
    stream: &mut crate::PaneOutputStream,
    collection: &mut CollectedPaneOutput,
    max_bytes: usize,
) -> Result<()> {
    loop {
        match stream.next().await? {
            Some(chunk) => {
                if ingest_chunk(collection, chunk, max_bytes) {
                    return Ok(());
                }
            }
            None => return Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ReadyOutputPoll {
    saw_output: bool,
    saw_eof: bool,
}

async fn poll_ready_output_once(
    stream: &mut crate::PaneOutputStream,
    collection: &mut CollectedPaneOutput,
    max_bytes: usize,
) -> Result<ReadyOutputPoll> {
    let chunks = stream.poll_once().await?;
    let saw_ready_output = !chunks.is_empty();
    let mut saw_eof = false;
    for chunk in chunks {
        saw_eof |= ingest_chunk(collection, chunk, max_bytes);
    }
    Ok(ReadyOutputPoll {
        saw_output: saw_ready_output,
        saw_eof,
    })
}

fn ingest_chunk(
    collection: &mut CollectedPaneOutput,
    chunk: PaneOutputChunk,
    max_bytes: usize,
) -> bool {
    match chunk {
        PaneOutputChunk::Bytes { bytes, .. } => {
            if bytes.is_empty() {
                return true;
            }
            collection.truncated |= extend_capped(&mut collection.bytes, &bytes, max_bytes);
            false
        }
        PaneOutputChunk::Lag(notice) => {
            collection.lagged = true;
            collection.missed_events = collection
                .missed_events
                .saturating_add(notice.missed_events);
            false
        }
    }
}

async fn exit_state_after_stream_close(pane: &Pane) -> Result<Option<PaneExitState>> {
    loop {
        match crate::wait::pane_exit_observation(pane).await? {
            crate::wait::PaneExitObservation::Running => {
                tokio::time::sleep(crate::wait::TEXT_POLL_INTERVAL).await;
            }
            crate::wait::PaneExitObservation::Exited(exit_state) => return Ok(exit_state),
        }
    }
}

fn extend_capped(target: &mut Vec<u8>, bytes: &[u8], max_bytes: usize) -> bool {
    let remaining = max_bytes.saturating_sub(target.len());
    if remaining >= bytes.len() {
        target.extend_from_slice(bytes);
        false
    } else {
        target.extend_from_slice(&bytes[..remaining]);
        true
    }
}

fn find_text_in_snapshot(snapshot: &PaneSnapshot, needle: &str) -> Vec<PaneTextMatch> {
    if needle.is_empty() {
        return Vec::new();
    }

    let lines = snapshot.visible_lines();
    let mut matches = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        let row = row as u16;
        for (start, end) in literal_match_ranges(line, needle) {
            if let Some(text_match) =
                text_match_for_rendered_row_range(snapshot, row, line, start, end)
            {
                matches.push(text_match);
            }
        }
    }
    matches
}

pub(crate) fn text_match_for_rendered_row_range(
    snapshot: &PaneSnapshot,
    row: u16,
    line: &str,
    start: usize,
    end: usize,
) -> Option<PaneTextMatch> {
    let coords = rendered_row_byte_coords(snapshot, row, line);
    let start_coord = coords.get(start)?;
    let end_coord = end.checked_sub(1).and_then(|index| coords.get(index))?;
    Some(PaneTextMatch::new(
        row,
        start_coord.start_col,
        end_coord.end_col,
        line.get(start..end)?.to_owned(),
    ))
}

#[derive(Debug, Clone, Copy)]
struct ByteCoord {
    start_col: u16,
    end_col: u16,
}

fn rendered_row_byte_coords(snapshot: &PaneSnapshot, row: u16, line: &str) -> Vec<ByteCoord> {
    let mut coords = Vec::new();
    for col in 0..snapshot.cols {
        let Some(cell) = snapshot.cell(row, col) else {
            break;
        };
        if cell.is_padding() {
            continue;
        }

        let Some(owner_col) = snapshot.owning_cell_col(row, col) else {
            continue;
        };
        let end_col = owner_col
            .saturating_add(u16::from(cell.glyph.width.max(1)))
            .min(snapshot.cols);
        coords.extend(cell.text().bytes().map(|_| ByteCoord {
            start_col: owner_col,
            end_col,
        }));
    }
    coords.truncate(line.len());
    coords
}

fn literal_match_ranges(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut search_start = 0;
    while search_start <= haystack.len() {
        let Some(relative) = haystack[search_start..].find(needle) else {
            break;
        };
        let start = search_start + relative;
        let end = start + needle.len();
        ranges.push((start, end));
        search_start = next_char_boundary_after(haystack, start);
    }
    ranges
}

fn next_char_boundary_after(value: &str, index: usize) -> usize {
    value[index..]
        .chars()
        .next()
        .map_or(value.len() + 1, |character| index + character.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PaneCell, PaneCursor, PaneGlyph};

    fn cell(text: &str) -> PaneCell {
        PaneCell::new(PaneGlyph::new(text, 1))
    }

    fn wide(text: &str, width: u8) -> PaneCell {
        PaneCell::new(PaneGlyph::new(text, width))
    }

    #[test]
    fn capped_extend_never_exceeds_limit() {
        let mut bytes = b"abc".to_vec();
        assert!(extend_capped(&mut bytes, b"def", 5));
        assert_eq!(bytes, b"abcde");
        assert!(extend_capped(&mut bytes, b"g", 5));
        assert_eq!(bytes, b"abcde");
    }

    #[test]
    fn capped_extend_handles_zero_limit() {
        let mut bytes = Vec::new();
        assert!(extend_capped(&mut bytes, b"abc", 0));
        assert!(bytes.is_empty());
    }

    #[test]
    fn literal_ranges_include_overlapping_matches() {
        assert_eq!(
            literal_match_ranges("aaaa", "aa"),
            vec![(0, 2), (1, 3), (2, 4)]
        );
    }

    #[test]
    fn find_text_uses_visible_lines_and_wide_cell_owner_columns() {
        let snapshot = PaneSnapshot::new(
            6,
            1,
            vec![
                cell("A"),
                wide("界", 2),
                PaneCell::padding(),
                cell("B"),
                cell(" "),
                cell(" "),
            ],
            PaneCursor::default(),
        )
        .expect("valid snapshot");

        let matches = snapshot.find_text_all("界B");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start_row, 0);
        assert_eq!(matches[0].start_col, 1);
        assert_eq!(matches[0].end_col, 4);
        assert_eq!(matches[0].text, "界B");
    }

    #[test]
    fn find_text_clamps_malformed_wide_match_end_to_visible_width() {
        let snapshot = PaneSnapshot::new(
            3,
            1,
            vec![cell("A"), wide("界", 4), PaneCell::padding()],
            PaneCursor::default(),
        )
        .expect("valid snapshot");

        let text_match = snapshot.find_text("界").expect("wide match found");
        assert_eq!(text_match.start_col, 1);
        assert_eq!(text_match.end_col, 3);
    }

    #[test]
    fn find_text_returns_none_for_empty_needle() {
        let snapshot = PaneSnapshot::new(
            3,
            1,
            vec![cell("a"), cell("b"), cell("c")],
            PaneCursor::default(),
        )
        .expect("valid snapshot");

        assert!(snapshot.find_text("").is_none());
        assert!(snapshot.find_text_all("").is_empty());
    }

    #[test]
    fn find_text_returns_none_on_default_snapshot() {
        let snapshot = PaneSnapshot::default();
        assert!(snapshot.find_text("anything").is_none());
        assert!(snapshot.find_text_all("anything").is_empty());
    }

    #[test]
    fn find_text_returns_none_when_needle_exceeds_visible_text() {
        let snapshot = PaneSnapshot::new(2, 1, vec![cell("a"), cell("b")], PaneCursor::default())
            .expect("valid snapshot");

        assert!(snapshot.find_text("abc").is_none());
    }

    #[test]
    fn capped_extend_marks_truncated_when_zero_limit_meets_nonempty_bytes() {
        let mut bytes = Vec::new();
        // Empty input at zero cap is not truncation, but any non-empty input at
        // zero cap must report truncation so callers can distinguish empty-data
        // exits from cap-limited collections.
        assert!(!extend_capped(&mut bytes, b"", 0));
        assert!(extend_capped(&mut bytes, b"x", 0));
        assert!(bytes.is_empty());
    }
}
