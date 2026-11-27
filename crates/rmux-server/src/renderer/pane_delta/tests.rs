use std::sync::Arc;

use crate::pane_transcript::PaneTranscript;

use rmux_core::{input::InputParser, FloatingPaneRect, OptionStore, Screen};
use rmux_proto::{
    OptionName, ScopeSelector, SessionName, SetOptionMode, SplitDirection, TerminalSize,
    WindowTarget,
};

use super::{PaneRenderDelta, PaneRenderSnapshot, RenderedPaneRow, TerminalPaneSpan};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn screen_with(bytes: &[u8]) -> Screen {
    screen_with_size(bytes, TerminalSize { cols: 10, rows: 3 })
}

fn screen_with_size(bytes: &[u8], size: TerminalSize) -> Screen {
    let mut screen = Screen::new(size, 100);
    let mut parser = InputParser::new();
    parser.parse(bytes, &mut screen);
    screen
}

fn render_lines(lines: &[&[u8]]) -> Vec<Arc<RenderedPaneRow>> {
    lines
        .iter()
        .map(|line| {
            Arc::new(RenderedPaneRow::new(
                (*line).to_vec(),
                u16::try_from(line.len()).unwrap_or(u16::MAX),
            ))
        })
        .collect()
}

fn rect_spans(x: u16, y: u16, rows: u16, cols: u16) -> Vec<TerminalPaneSpan> {
    (0..usize::from(rows))
        .map(|row| TerminalPaneSpan {
            x,
            y: y.saturating_add(row as u16),
            source_row: row,
            source_col: 0,
            line_index: row,
            cols,
        })
        .collect()
}

fn float_pane(
    session: &mut crate::session_fixture::SessionFixture,
    pane_index: u32,
    rect: FloatingPaneRect,
) {
    session
        .store
        .window_layout_context_mut(&WindowTarget::with_window(
            session.name().clone(),
            session.active_window_index(),
        ))
        .expect("window layout context")
        .float_pane(pane_index, rect)
        .expect("pane can float");
}

mod cursor;
mod plain_output;
mod projection;
mod rows;
mod tail_clear;
