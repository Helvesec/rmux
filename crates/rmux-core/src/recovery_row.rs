use crate::RenderedLineSpan;

/// One rendered recovery row: the bytes to replay, the soft-wrap flag, and the
/// columns those bytes paint.
///
/// A replay places the next row either by autowrap or by an explicit line
/// break, so the emitting side needs the painted span, not the byte length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryRow {
    /// Bytes that repaint the row from a fresh ANSI state.
    pub bytes: Vec<u8>,
    /// Whether the row soft-wraps onto the following row.
    pub wrapped: bool,
    /// Columns painted by the row.
    pub span: RenderedLineSpan,
    /// Written extent of a compact ASCII row, including explicit trailing spaces.
    /// Cell-backed rows retain their physical extent and report `None`.
    pub written_columns: Option<u32>,
    /// Columns a pre-wrapped wide glyph left unused at the end of the row.
    pub trailing_reflow_gap: u32,
}

const EXTENT_PREFIX: &[u8] = b"rmux;row-extent;1;";

impl RecoveryRow {
    /// Encodes the version-one RMUX DCS for removing replay-only space padding
    /// from the current row without moving the cursor or clearing pending wrap.
    /// Other terminals ignore this metadata and retain the same painted cells.
    #[must_use]
    pub fn written_extent_restore_ansi(&self) -> Vec<u8> {
        let Some(columns) = self.written_columns else {
            return Vec::new();
        };
        let mut bytes = b"\x1bP".to_vec();
        bytes.extend_from_slice(EXTENT_PREFIX);
        bytes.extend_from_slice(columns.to_string().as_bytes());
        bytes.extend_from_slice(b"\x1b\\");
        bytes
    }
}

pub(crate) fn parse_written_extent(payload: &[u8]) -> Option<u32> {
    let digits = payload.strip_prefix(EXTENT_PREFIX)?;
    if digits.is_empty() || digits.len() > 5 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits)
        .ok()?
        .parse::<u16>()
        .ok()
        .map(u32::from)
}
