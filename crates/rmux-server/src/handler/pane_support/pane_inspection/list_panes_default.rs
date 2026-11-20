use crate::pane_indices::visible_pane_index;
use crate::pane_terminals::HandlerState;
use crate::pane_transcript::PaneTranscriptReach;
use crate::pane_visible_geometry::visible_pane_content_geometry;
use rmux_core::formats::{
    DEFAULT_LIST_PANES_ALL_FORMAT, DEFAULT_LIST_PANES_SESSION_FORMAT,
    DEFAULT_LIST_PANES_WINDOW_FORMAT,
};
use rmux_core::Pane;

#[derive(Clone, Copy)]
pub(super) enum DefaultListPanesFormat {
    Window,
    Session,
    All,
}

impl DefaultListPanesFormat {
    pub(super) fn from_format(format: &str) -> Option<Self> {
        match format {
            DEFAULT_LIST_PANES_WINDOW_FORMAT => Some(Self::Window),
            DEFAULT_LIST_PANES_SESSION_FORMAT => Some(Self::Session),
            DEFAULT_LIST_PANES_ALL_FORMAT => Some(Self::All),
            _ => None,
        }
    }
}

pub(super) fn push_default_list_panes_line(
    stdout: &mut Vec<u8>,
    context: DefaultListPanesLineContext<'_>,
) -> bool {
    use std::fmt::Write as _;

    let DefaultListPanesLineContext {
        format,
        state,
        session,
        attached_count,
        window_index,
        pane,
        pane_active,
    } = context;

    // A poisoned pane falls back to the runtime format, which still lists it.
    let Some((history_size, history_limit, history_bytes)) = state.observe_pane_transcript(
        session.name(),
        pane.id(),
        PaneTranscriptReach::Reached,
        |transcript| {
            (
                transcript.history_size(),
                transcript.history_limit(),
                transcript.tmux_history_bytes(),
            )
        },
    ) else {
        return false;
    };

    let geometry = list_panes_default_geometry(state, session, attached_count, window_index, pane);
    let pane_index = visible_pane_index(session, &state.options, window_index, pane.index());
    let mut line = String::new();
    match format {
        DefaultListPanesFormat::Window => {
            let _ = write!(&mut line, "{pane_index}: ");
        }
        DefaultListPanesFormat::Session => {
            let _ = write!(&mut line, "{window_index}.{pane_index}: ");
        }
        DefaultListPanesFormat::All => {
            let _ = write!(
                &mut line,
                "{}:{window_index}.{pane_index}: ",
                session.name(),
            );
        }
    }
    let _ = write!(
        &mut line,
        "[{}x{}] [history {}/{}, {} bytes] {}",
        geometry.cols(),
        geometry.rows(),
        history_size,
        history_limit,
        history_bytes,
        pane.id()
    );
    if pane_active {
        line.push_str(" (active)");
    }
    if state.pane_is_dead(session.name(), pane.id()) {
        line.push_str(" (dead)");
    }
    stdout.extend_from_slice(line.as_bytes());
    true
}

pub(super) struct DefaultListPanesLineContext<'a> {
    pub(super) format: DefaultListPanesFormat,
    pub(super) state: &'a HandlerState,
    pub(super) session: rmux_core::ManagedSessionView<'a>,
    pub(super) attached_count: usize,
    pub(super) window_index: u32,
    pub(super) pane: &'a Pane,
    pub(super) pane_active: bool,
}

fn list_panes_default_geometry(
    state: &HandlerState,
    session: rmux_core::ManagedSessionView<'_>,
    attached_count: usize,
    window_index: u32,
    pane: &Pane,
) -> rmux_core::PaneGeometry {
    let geometry = pane.geometry();
    if attached_count == 0 {
        return geometry;
    }

    let size = session
        .window_at(window_index)
        .unwrap_or_else(|| session.window())
        .size();
    if size.cols == 0 || size.rows == 0 {
        return geometry;
    }

    visible_pane_content_geometry(
        &state.options,
        session.name(),
        window_index,
        geometry,
        size.rows,
    )
}

#[cfg(test)]
mod tests {
    use super::list_panes_default_geometry;
    use crate::pane_terminals::HandlerState;

    use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize};

    #[test]
    fn attached_default_geometry_uses_stored_content_rows() {
        let alpha = SessionName::new("alpha").expect("valid session name");
        let mut session = crate::session_fixture::SessionFixture::new(
            alpha.clone(),
            TerminalSize { cols: 80, rows: 24 },
        );
        session.resize_active_window_geometry(
            TerminalSize { cols: 80, rows: 24 },
            TerminalSize { cols: 80, rows: 22 },
        );
        session.touch_attached();
        let pane = session.active_pane().expect("active pane");
        let mut state = HandlerState::default();
        state
            .options
            .set(
                ScopeSelector::Session(alpha),
                OptionName::Status,
                "2".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("session status set succeeds");

        let geometry = list_panes_default_geometry(&state, session.view(), 1, 0, pane);

        assert_eq!(geometry.rows(), 22);
    }
}
