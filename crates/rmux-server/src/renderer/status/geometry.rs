use rmux_core::{OptionStore, Session};
use rmux_proto::{OptionName, TerminalSize};

use crate::status_lines::status_line_count;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatusGeometry {
    pub(in crate::renderer) terminal_size: TerminalSize,
    pub(in crate::renderer) content_rows: u16,
    pub(in crate::renderer) content_y_offset: u16,
    pub(in crate::renderer) status_y: Option<u16>,
    pub(in crate::renderer) status_lines: u16,
}

impl StatusGeometry {
    pub(crate) fn for_session(session: &Session, options: &OptionStore) -> Self {
        let size = session.window().size();
        let status = options.resolve(Some(session.name()), OptionName::Status);
        if size.cols == 0 || size.rows == 0 || matches!(status, Some("off")) {
            return Self::without_status(size);
        }
        let status_lines = status_line_count(status, size.rows);

        match options.resolve(Some(session.name()), OptionName::StatusPosition) {
            Some("top") => Self {
                terminal_size: size,
                content_rows: size.rows.saturating_sub(status_lines),
                content_y_offset: status_lines,
                status_y: Some(0),
                status_lines,
            },
            _ => Self {
                terminal_size: size,
                content_rows: size.rows.saturating_sub(status_lines),
                content_y_offset: 0,
                status_y: Some(size.rows.saturating_sub(status_lines)),
                status_lines,
            },
        }
    }

    pub(in crate::renderer) const fn without_status(size: TerminalSize) -> Self {
        Self {
            terminal_size: size,
            content_rows: size.rows,
            content_y_offset: 0,
            status_y: None,
            status_lines: 0,
        }
    }

    pub(in crate::renderer) const fn content_size(self) -> TerminalSize {
        TerminalSize {
            cols: self.terminal_size.cols,
            rows: self.content_rows,
        }
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
}
