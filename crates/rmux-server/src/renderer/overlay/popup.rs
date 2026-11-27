use rmux_core::Style;

use super::{
    draw_box, draw_formatted_text, inner_rect, OverlayRect, PopupContent, PopupRenderSpec,
    TitleAlign,
};

pub(crate) fn render_popup_overlay(spec: &PopupRenderSpec) -> Vec<u8> {
    let mut frame = Vec::new();
    // Paint each content row in its final state. Clearing the whole rectangle
    // first exposes a blank popup between PTY/SSH writes on every refresh.
    if spec.border_lines.visible() {
        draw_box(
            &mut frame,
            spec.rect,
            &spec.border_style,
            spec.border_lines,
            Some(spec.title.as_str()),
            TitleAlign::Centre,
        );
    }

    let inner = inner_rect(spec.rect, spec.border_lines);
    for index in 0..usize::from(inner.height) {
        let Some(y) = content_row_y(inner, index) else {
            break;
        };
        match &spec.content {
            PopupContent::Surface(rows) => {
                let row = rows.get(index).map_or(&[][..], Vec::as_slice);
                draw_surface_row(
                    &mut frame,
                    inner.x,
                    y,
                    row,
                    usize::from(inner.width),
                    &spec.style,
                );
            }
            PopupContent::Text(lines) => {
                let line = lines.get(index).map_or("", String::as_str);
                draw_formatted_text(
                    &mut frame,
                    inner.x,
                    y,
                    line,
                    &spec.style,
                    usize::from(inner.width),
                );
            }
        }
    }
    frame
}

fn content_row_y(inner: OverlayRect, index: usize) -> Option<u16> {
    let row = inner
        .y
        .saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
    (row < inner.y.saturating_add(inner.height)).then_some(row)
}

/// Draws one captured terminal row verbatim.
///
/// The row is clipped to the popup's inner width without splitting an escape
/// sequence, and is bracketed by resets so neither the popup style nor a
/// trailing attribute from the process bleeds into the rest of the frame.
fn draw_surface_row(frame: &mut Vec<u8>, x: u16, y: u16, row: &[u8], width: usize, style: &Style) {
    // Pad *after* the final text, using the popup background. This erases a
    // shortened/missing row without blanking text we are about to repaint.
    // Reuse the ANSI/grapheme-aware truncator for both clipping and padding.
    let mut padded = row.to_vec();
    padded.extend_from_slice(&crate::renderer::style_sgr_bytes(style, true));
    padded.resize(padded.len().saturating_add(width), b' ');
    let clipped = crate::renderer::truncate_rendered_pane_line(
        &padded,
        width,
        &rmux_core::Utf8Config::default(),
    );
    if clipped.is_empty() {
        return;
    }
    frame.extend_from_slice(b"\x1b7\x1b[0m");
    frame.extend_from_slice(crate::renderer::cursor_position_bytes(y, x).as_slice());
    frame.extend_from_slice(&clipped);
    frame.extend_from_slice(b"\x1b[0m\x1b8");
}

#[cfg(test)]
mod charset_width_tests {
    use super::*;

    // Exercise the real job output -> PopupSurface::frame -> draw_surface_row
    // route through crate-visible handler APIs, without exposing private state.
    #[cfg(unix)]
    #[tokio::test]
    async fn popup_surface_frame_keeps_last_cell_with_acs_style() {
        use crate::handler::RequestHandler;
        use crate::pane_io::AttachControl;
        use rmux_proto::{
            NewSessionRequest, Request, SessionName, SourceFileRequest, TerminalSize,
            INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH,
        };
        use tokio::sync::mpsc;
        use tokio::time::{timeout, Duration};

        fn command(text: &str) -> Request {
            Request::SourceFile(Box::new(SourceFileRequest {
                paths: vec![INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()],
                quiet: false,
                parse_only: false,
                verbose: false,
                expand_paths: false,
                target: None,
                caller_cwd: None,
                stdin: Some(text.to_owned()),
            }))
        }

        let handler = RequestHandler::new();
        let session = SessionName::new("popup-charset-width").unwrap();
        let size = TerminalSize { cols: 80, rows: 24 };
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(size),
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&created);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let pid = 975_523;
        handler.register_attach(pid, session, tx).await;
        handler.handle_attached_resize(pid, size).await.unwrap();

        // No -E: the completed printf leaves its surface visible until closed.
        let open = handler.handle(command(
            "display-popup -c 975523 -B -w 4 -h 1 -s acs 'printf ABCD'",
        ));
        let inspect = async {
            let observed = timeout(Duration::from_secs(10), async {
                loop {
                    let event = rx.recv().await.expect("attached client control stays open");
                    if let AttachControl::Overlay(overlay) = event.materialize_received() {
                        if overlay.frame.windows(4).any(|bytes| bytes == b"\x0eABC") {
                            break overlay.frame;
                        }
                    }
                }
            })
            .await;
            // Close before asserting, including when the observation timed out.
            handler.handle(command("display-popup -C -c 975523")).await;
            observed
        };
        let (_, observed) = tokio::join!(open, inspect);
        let frame = observed.expect("the actual popup surface produced ABC");
        assert!(
            frame.windows(5).any(|bytes| bytes == b"\x0eABCD"),
            "{frame:?}"
        );
    }

    #[test]
    fn surface_row_keeps_four_ascii_cells_after_shift_out() {
        let mut frame = Vec::new();
        draw_surface_row(
            &mut frame,
            0,
            0,
            b"\x0eABCD",
            4,
            &Style::parse("acs").unwrap(),
        );
        assert_eq!(frame, b"\x1b7\x1b[0m\x1b[1;1H\x0eABCD\x1b[0m\x1b[0m\x1b8");
    }

    #[test]
    fn surface_row_pads_after_charset_transitions() {
        let mut frame = Vec::new();
        draw_surface_row(&mut frame, 0, 0, b"\x0eA\x0fB", 4, &Style::default());
        assert_eq!(
            frame,
            b"\x1b7\x1b[0m\x1b[1;1H\x0eA\x0fB\x1b[0m  \x1b[0m\x1b8"
        );
    }
}
