use rmux_core::{OptionStore, Session};
use rmux_proto::{OptionName, TerminalSize};

use crate::status_lines::status_line_count;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::renderer) enum StatusOverlayRow {
    Status(u16),
    Content(u16),
}

impl StatusOverlayRow {
    pub(in crate::renderer) const fn y(self) -> u16 {
        match self {
            Self::Status(y) | Self::Content(y) => y,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatusGeometry {
    pub(in crate::renderer) terminal_size: TerminalSize,
    content_cols: u16,
    pub(in crate::renderer) content_rows: u16,
    pub(in crate::renderer) content_y_offset: u16,
    pub(in crate::renderer) status_y: Option<u16>,
    pub(in crate::renderer) status_lines: u16,
}

impl StatusGeometry {
    pub(crate) fn for_session(session: &Session, options: &OptionStore) -> Self {
        let terminal_size = session.terminal_size();
        let layout_size = session.window().size();
        let status = options.resolve(Some(session.name()), OptionName::Status);
        if terminal_size.cols == 0 || terminal_size.rows == 0 || matches!(status, Some("off")) {
            return Self::without_status(terminal_size, layout_size);
        }
        let status_lines = status_line_count(status, terminal_size.rows);
        let available_content_rows = terminal_size.rows.saturating_sub(status_lines);
        let content_cols = layout_size.cols.min(terminal_size.cols);
        let content_rows = layout_size.rows.min(available_content_rows);

        match options.resolve(Some(session.name()), OptionName::StatusPosition) {
            Some("top") => Self {
                terminal_size,
                content_cols,
                content_rows,
                content_y_offset: status_lines,
                status_y: Some(0),
                status_lines,
            },
            _ => Self {
                terminal_size,
                content_cols,
                content_rows,
                content_y_offset: 0,
                status_y: Some(terminal_size.rows.saturating_sub(status_lines)),
                status_lines,
            },
        }
    }

    pub(in crate::renderer) const fn without_status(
        terminal_size: TerminalSize,
        layout_size: TerminalSize,
    ) -> Self {
        Self {
            terminal_size,
            content_cols: if layout_size.cols < terminal_size.cols {
                layout_size.cols
            } else {
                terminal_size.cols
            },
            content_rows: if layout_size.rows < terminal_size.rows {
                layout_size.rows
            } else {
                terminal_size.rows
            },
            content_y_offset: 0,
            status_y: None,
            status_lines: 0,
        }
    }

    pub(in crate::renderer) const fn content_size(self) -> TerminalSize {
        TerminalSize {
            cols: self.content_cols,
            rows: self.content_rows,
        }
    }

    pub(crate) fn status_rect(self) -> Option<crate::renderer::OverlayRect> {
        self.status_y.map(|y| crate::renderer::OverlayRect {
            x: 0,
            y,
            width: self.terminal_size.cols,
            height: self.status_lines,
        })
    }

    pub(crate) const fn content_y_offset(self) -> u16 {
        self.content_y_offset
    }

    pub(crate) const fn content_rows(self) -> u16 {
        self.content_rows
    }

    pub(in crate::renderer) const fn status_line_y(self, line: u16) -> Option<u16> {
        let status_y = match self.status_y {
            Some(status_y) => status_y,
            None => return None,
        };
        if line >= self.status_lines {
            return None;
        }
        Some(status_y.saturating_add(line))
    }

    pub(in crate::renderer) const fn message_overlay_row(
        self,
        status_line: u16,
    ) -> Option<StatusOverlayRow> {
        if self.terminal_size.cols == 0 || self.terminal_size.rows == 0 {
            return None;
        }
        match self.status_y {
            Some(_) => match self.status_line_y(status_line) {
                Some(y) => Some(StatusOverlayRow::Status(y)),
                None => None,
            },
            None => Some(StatusOverlayRow::Content(
                self.terminal_size.rows.saturating_sub(1),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StatusGeometry, StatusOverlayRow};
    use rmux_core::{OptionStore, Session};
    use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize};

    fn session() -> Session {
        Session::new(
            SessionName::new("overlay-row").expect("valid session name"),
            TerminalSize { cols: 20, rows: 6 },
        )
    }

    #[test]
    fn message_overlay_row_models_status_and_content_backing() {
        let session = session();
        let mut options = OptionStore::new();
        let status_geometry = StatusGeometry::for_session(&session, &options);
        assert_eq!(
            status_geometry.message_overlay_row(0),
            Some(StatusOverlayRow::Status(5))
        );

        options
            .set(
                ScopeSelector::Global,
                OptionName::Status,
                "off".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("status off");
        let content_geometry = StatusGeometry::for_session(&session, &options);
        assert_eq!(
            content_geometry.message_overlay_row(0),
            Some(StatusOverlayRow::Content(5))
        );
    }

    #[test]
    fn message_overlay_row_rejects_degenerate_terminal_sizes() {
        for size in [
            TerminalSize { cols: 0, rows: 6 },
            TerminalSize { cols: 20, rows: 0 },
        ] {
            let session = Session::new(
                SessionName::new("degenerate-overlay").expect("valid session name"),
                size,
            );
            let geometry = StatusGeometry::for_session(&session, &OptionStore::new());
            assert_eq!(geometry.message_overlay_row(0), None);
        }
    }
}
