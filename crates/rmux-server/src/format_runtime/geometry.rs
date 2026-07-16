use rmux_core::{OptionStore, Pane, PaneGeometry, Session, Window};
use rmux_proto::{OptionName, ResizePaneAdjustment, TerminalSize};

use crate::pane_visible_geometry::{clip_pane_geometry, visible_pane_content_geometry};
use crate::status_lines::content_rows_for_status;

use super::RuntimeFormatContext;

impl RuntimeFormatContext<'_> {
    pub(super) fn visible_session_snapshot(&self) -> Option<Session> {
        let mut session = self.session?.clone();
        if !self.use_unclipped_geometry {
            let size =
                visible_session_size(self.option_store(), &session, self.session_attached_count());
            if size != session.window().size() {
                session.resize_terminal(size);
            }
        }
        Some(session)
    }

    pub(super) fn visible_window_snapshot(&self) -> Option<Window> {
        let session = self.visible_session_snapshot()?;
        let window_index = self
            .window_index
            .unwrap_or_else(|| session.active_window_index());
        session.window_at(window_index).cloned()
    }

    pub(super) fn layout_window_snapshot(&self) -> Option<Window> {
        let mut session = self.session?.clone();
        for window_index in session.windows().keys().copied().collect::<Vec<_>>() {
            let Some(active_pane_index) = session
                .window_at(window_index)
                .map(|window| (window.is_zoomed(), window.active_pane_index()))
                .and_then(|(zoomed, active_pane_index)| zoomed.then_some(active_pane_index))
            else {
                continue;
            };
            let _ = session.resize_pane_in_window(
                window_index,
                active_pane_index,
                ResizePaneAdjustment::Zoom,
            );
        }
        if !self.use_unclipped_geometry {
            let size =
                visible_session_size(self.option_store(), &session, self.session_attached_count());
            if size != session.window().size() {
                session.resize_terminal(size);
            }
        }
        let window_index = self
            .window_index
            .unwrap_or_else(|| session.active_window_index());
        session.window_at(window_index).cloned()
    }

    pub(super) fn visible_pane_snapshot(&self) -> Option<Pane> {
        let session = self.visible_session_snapshot()?;
        let window_index = self
            .window_index
            .unwrap_or_else(|| session.active_window_index());
        let window = session.window_at(window_index)?;
        let pane_index = self
            .pane
            .map(Pane::index)
            .unwrap_or_else(|| window.active_pane_index());
        window.pane(pane_index).cloned()
    }

    pub(super) fn visible_pane_geometry(&self) -> Option<PaneGeometry> {
        let session = self.session?;
        let window = self.visible_window_snapshot()?;
        let window_index = self
            .window_index
            .unwrap_or_else(|| session.active_window_index());
        let content_rows = window.size().rows;
        let pane_index = self
            .pane
            .map(Pane::index)
            .unwrap_or_else(|| window.active_pane_index());
        if window.is_zoomed() && pane_index == window.active_pane_index() {
            let size = window.size();
            let geometry = PaneGeometry::new(0, 0, size.cols, size.rows);
            return Some(match self.option_store() {
                Some(options) => visible_pane_content_geometry(
                    options,
                    session.name(),
                    window_index,
                    geometry,
                    content_rows,
                ),
                None => clip_pane_geometry(geometry, content_rows),
            });
        }
        self.visible_pane_snapshot().map(|pane| {
            let geometry = pane.geometry();
            match self.option_store() {
                Some(options) => visible_pane_content_geometry(
                    options,
                    session.name(),
                    window_index,
                    geometry,
                    content_rows,
                ),
                None => clip_pane_geometry(geometry, content_rows),
            }
        })
    }
}

fn visible_session_size(
    options: Option<&OptionStore>,
    session: &Session,
    attached_count: usize,
) -> TerminalSize {
    let size = session.window().size();
    if size.cols == 0 || size.rows == 0 {
        return size;
    }
    if attached_count == 0 {
        return size;
    }

    let Some(options) = options else {
        return size;
    };

    TerminalSize {
        cols: size.cols,
        rows: content_rows_for_status(
            options.resolve(Some(session.name()), OptionName::Status),
            size.rows,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::visible_session_size;
    use rmux_core::{OptionStore, Session};
    use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize};

    #[test]
    fn visible_session_size_uses_multi_line_status_rows() {
        let alpha = SessionName::new("alpha").expect("valid session name");
        let session = Session::new(alpha.clone(), TerminalSize { cols: 80, rows: 24 });
        let mut options = OptionStore::default();
        options
            .set(
                ScopeSelector::Session(alpha),
                OptionName::Status,
                "3".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("session status set succeeds");

        assert_eq!(
            visible_session_size(Some(&options), &session, 1),
            TerminalSize { cols: 80, rows: 21 }
        );
    }
}
