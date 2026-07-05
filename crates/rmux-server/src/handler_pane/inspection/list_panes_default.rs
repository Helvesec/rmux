use rmux_core::formats::{
    DEFAULT_LIST_PANES_ALL_FORMAT, DEFAULT_LIST_PANES_SESSION_FORMAT,
    DEFAULT_LIST_PANES_WINDOW_FORMAT,
};
use rmux_core::{Pane, Session};
use rmux_proto::OptionName;

use crate::pane_terminals::HandlerState;
use crate::pane_visible_geometry::visible_pane_content_geometry;

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

    let Some(history_stats) = state.pane_history_size_stats(session.name(), pane.id()) else {
        return false;
    };
    let Some(history_bytes) = state.pane_history_bytes(session.name(), pane.id()) else {
        return false;
    };

    let geometry = list_panes_default_geometry(state, session, attached_count, window_index, pane);
    let mut line = String::new();
    match format {
        DefaultListPanesFormat::Window => {
            let _ = write!(&mut line, "{}: ", pane.index());
        }
        DefaultListPanesFormat::Session => {
            let _ = write!(&mut line, "{}.{}: ", window_index, pane.index());
        }
        DefaultListPanesFormat::All => {
            let _ = write!(
                &mut line,
                "{}:{}.{}: ",
                session.name(),
                window_index,
                pane.index()
            );
        }
    }
    let _ = write!(
        &mut line,
        "[{}x{}] [history {}/{}, {} bytes] {}",
        geometry.cols(),
        geometry.rows(),
        history_stats.size,
        history_stats.limit,
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
    pub(super) session: &'a Session,
    pub(super) attached_count: usize,
    pub(super) window_index: u32,
    pub(super) pane: &'a Pane,
    pub(super) pane_active: bool,
}

fn list_panes_default_geometry(
    state: &HandlerState,
    session: &Session,
    attached_count: usize,
    window_index: u32,
    pane: &Pane,
) -> rmux_core::PaneGeometry {
    let geometry = pane.geometry();
    if attached_count == 0 {
        return geometry;
    }

    let size = session.window().size();
    if size.cols == 0 || size.rows == 0 {
        return geometry;
    }

    let content_rows = crate::status_lines::content_rows_for_status(
        state
            .options
            .resolve(Some(session.name()), OptionName::Status),
        size.rows,
    );
    visible_pane_content_geometry(
        &state.options,
        session.name(),
        window_index,
        geometry,
        content_rows,
    )
}

#[cfg(test)]
mod tests {
    use super::list_panes_default_geometry;
    use crate::pane_terminals::HandlerState;
    use rmux_core::Session;
    use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize};

    #[test]
    fn attached_default_geometry_uses_multi_line_status_rows() {
        let alpha = SessionName::new("alpha").expect("valid session name");
        let mut session = Session::new(alpha.clone(), TerminalSize { cols: 80, rows: 24 });
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

        let geometry = list_panes_default_geometry(&state, &session, 1, 0, pane);

        assert_eq!(geometry.rows(), 22);
    }
}
