#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PopupCursor {
    Hidden,
    Projected { x: u16, y: u16, visible: bool },
}

pub(crate) fn render_popup_cursor(cursor: PopupCursor) -> Vec<u8> {
    let mut frame = Vec::new();
    if let PopupCursor::Projected { x, y, visible } = cursor {
        frame.extend_from_slice(super::cursor_position_bytes(y, x).as_slice());
        frame.extend_from_slice(if visible { b"\x1b[?25h" } else { b"\x1b[?25l" });
    } else {
        frame.extend_from_slice(b"\x1b[?25l");
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::{render_popup_cursor, PopupCursor};

    #[test]
    fn projected_popup_cursor_uses_absolute_position_and_visibility() {
        assert_eq!(
            render_popup_cursor(PopupCursor::Projected {
                x: 12,
                y: 4,
                visible: true,
            }),
            b"\x1b[5;13H\x1b[?25h"
        );
        assert_eq!(
            render_popup_cursor(PopupCursor::Projected {
                x: 7,
                y: 9,
                visible: false,
            }),
            b"\x1b[10;8H\x1b[?25l"
        );
    }

    #[test]
    fn noninteractive_popup_cursor_is_hidden_without_repositioning() {
        assert_eq!(render_popup_cursor(PopupCursor::Hidden), b"\x1b[?25l");
    }
}
