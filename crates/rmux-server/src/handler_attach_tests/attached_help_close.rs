//! How a test knows that an Escape closed the help it opened.
//!
//! Every overlay frame of a client carries a higher generation than the one before it. The help's
//! opening frame has one; a session refresh while the help is up redraws the same help with the
//! next one, and may be queued ahead of the close; the close sends an empty frame with the
//! generation the client's state then holds. The receiver takes only that shape.

use super::*;
use crate::handler::overlay_support::ClientOverlayState;
use crate::pane_io::OverlayFrame;

/// An overlay frame as the client received it.
#[derive(Debug, Clone)]
pub(super) struct OverlaySeen {
    generation: u64,
    frame: Vec<u8>,
}

impl From<OverlayFrame> for OverlaySeen {
    fn from(overlay: OverlayFrame) -> Self {
        Self {
            generation: overlay.overlay_generation,
            frame: overlay.frame,
        }
    }
}

/// What the server holds for the client's overlay.
struct ClientOverlay {
    generation: u64,
    shows_help: bool,
    shows_nothing: bool,
}

async fn client_overlay(handler: &RequestHandler, attach_pid: u32) -> ClientOverlay {
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attached client remains registered");
    ClientOverlay {
        generation: active.overlay_generation,
        shows_help: matches!(
            active.overlay.as_ref(),
            Some(ClientOverlayState::Popup(popup)) if popup.scrollable_text.is_some()
        ),
        shows_nothing: active.overlay.is_none(),
    }
}

/// Opens the help with C-b ? and returns the frame that drew it, which the client's state confirms.
pub(super) async fn open_help_seen(
    handler: &RequestHandler,
    attach_pid: u32,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> OverlaySeen {
    let mut pending_input = Vec::new();
    let forwarded = handler
        .handle_attached_live_input_inner(attach_pid, &mut pending_input, b"\x02?")
        .await
        .expect("prefix help input succeeds");
    assert!(!forwarded && pending_input.is_empty(), "C-b ? is the help");
    let opened = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if let AttachControl::Overlay(overlay) = control_rx.recv().await.expect("help opening")
            {
                break OverlaySeen::from(overlay);
            }
        }
    })
    .await
    .expect("the help draws");
    let client = client_overlay(handler, attach_pid).await;
    assert!(
        !opened.frame.is_empty() && client.shows_help && client.generation == opened.generation,
        "the frame received is the help the client shows: frame {}, client {}",
        opened.generation,
        client.generation
    );
    opened
}

/// Receives what the client gets after an Escape closed the help `opened` and checks it: redraws of
/// that help, then this close's clear, then nothing for 200 ms; the client no longer shows the help.
/// Returns how many redraws came first.
pub(super) async fn recv_help_close(
    handler: &RequestHandler,
    attach_pid: u32,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    opened: &OverlaySeen,
    context: &str,
) -> usize {
    let mut received = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if let AttachControl::Overlay(overlay) = control_rx.recv().await.expect(context) {
                let clear = overlay.frame.is_empty();
                received.push(OverlaySeen::from(overlay));
                if clear {
                    break;
                }
            }
        }
    })
    .await;
    if received.last().is_some_and(|seen| seen.frame.is_empty()) {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), async {
            loop {
                if let AttachControl::Overlay(overlay) = control_rx.recv().await.expect(context) {
                    received.push(OverlaySeen::from(overlay));
                }
            }
        })
        .await;
    }
    let client = client_overlay(handler, attach_pid).await;
    assert!(
        client.shows_nothing,
        "the client no longer shows the help ({context})"
    );
    check_help_close(opened, &received, client.generation).unwrap_or_else(|problem| {
        panic!("Escape must clear the help overlay: {problem:?} ({context})")
    })
}

#[tokio::test]
async fn attached_help_escape_clear_is_found_past_a_help_redraw_queued_before_it() {
    let handler = RequestHandler::new();
    let attach_pid = std::process::id();
    let alpha = session_name("help-escape-redraw");
    let mut control_rx = create_quiet_attached_session(&handler, attach_pid, &alpha).await;
    drain_attach_controls(&mut control_rx);
    let opened = open_help_seen(&handler, attach_pid, &mut control_rx).await;
    let before = capture_pane_print(&handler, PaneTarget::new(alpha.clone(), 0)).await;
    // A session refresh while the help is up redraws it; that frame waits ahead of the close.
    handler.refresh_attached_session(&alpha).await;

    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(attach_pid, &mut pending_input, b"\x1b")
        .await
        .expect("help Escape is retained");
    handler
        .flush_attached_pending_escape_input(attach_pid, &mut pending_input)
        .await
        .expect("help Escape flush succeeds");
    let redraws = recv_help_close(
        &handler,
        attach_pid,
        &mut control_rx,
        &opened,
        "help Escape close",
    )
    .await;
    assert_eq!(
        redraws, 1,
        "the help's redraw queued before the close came first"
    );
    let after = capture_pane_print(&handler, PaneTarget::new(alpha, 0)).await;
    assert_eq!(after, before, "Escape must not reach the underlying pane");
}

/// Why the frames received after a help's close are not that close.
#[derive(Debug, PartialEq, Eq)]
enum HelpCloseProblem {
    NoClear,
    NotTheHelp { generation: u64 },
    NotAfter { generation: u64, previous: u64 },
    NotThisClose { generation: u64, closed: u64 },
    AfterClear { frames: usize },
}

/// The frames a client received after the help `opened` was closed, in order, against the
/// generation its state holds after the close: only redraws of that help - its very bytes - with
/// strictly increasing generations above the opening, then the empty frame of this close, then
/// nothing. Returns the number of redraws.
fn check_help_close(
    opened: &OverlaySeen,
    received: &[OverlaySeen],
    closed: u64,
) -> Result<usize, HelpCloseProblem> {
    let clear_at = received
        .iter()
        .position(|seen| seen.frame.is_empty())
        .ok_or(HelpCloseProblem::NoClear)?;
    let mut previous = opened.generation;
    for seen in &received[..=clear_at] {
        if seen.generation <= previous {
            return Err(HelpCloseProblem::NotAfter {
                generation: seen.generation,
                previous,
            });
        }
        if !seen.frame.is_empty() && seen.frame != opened.frame {
            return Err(HelpCloseProblem::NotTheHelp {
                generation: seen.generation,
            });
        }
        previous = seen.generation;
    }
    if received[clear_at].generation != closed {
        return Err(HelpCloseProblem::NotThisClose {
            generation: received[clear_at].generation,
            closed,
        });
    }
    match received.len() - clear_at - 1 {
        0 => Ok(clear_at),
        frames => Err(HelpCloseProblem::AfterClear { frames }),
    }
}

const HELP: &[u8] = b"\x1b[1;1Hhelp page";

fn seen(generation: u64, frame: &[u8]) -> OverlaySeen {
    OverlaySeen {
        generation,
        frame: frame.to_vec(),
    }
}

fn opened() -> OverlaySeen {
    seen(1, HELP)
}

#[test]
fn the_natural_close_and_a_redraw_queued_before_it_are_accepted() {
    assert_eq!(check_help_close(&opened(), &[seen(2, b"")], 2), Ok(0));
    assert_eq!(
        check_help_close(&opened(), &[seen(2, HELP), seen(3, b"")], 3),
        Ok(1)
    );
}

#[test]
fn a_clear_not_after_the_opening_is_refused() {
    // A-1: an empty frame at or below the opening's generation is not this close, even when the
    // client's generation says the same.
    assert_eq!(
        check_help_close(&opened(), &[seen(1, b"")], 1),
        Err(HelpCloseProblem::NotAfter {
            generation: 1,
            previous: 1
        })
    );
    assert_eq!(
        check_help_close(&opened(), &[seen(0, b"")], 0),
        Err(HelpCloseProblem::NotAfter {
            generation: 0,
            previous: 1
        })
    );
}

#[test]
fn repeated_redraw_generations_are_refused() {
    // A-2: two frames with generation 2, then a clear 3.
    let received = [seen(2, HELP), seen(2, HELP), seen(3, b"")];
    assert_eq!(
        check_help_close(&opened(), &received, 3),
        Err(HelpCloseProblem::NotAfter {
            generation: 2,
            previous: 2
        })
    );
}

#[test]
fn a_frame_that_is_not_the_help_opened_is_refused() {
    // A-3: another non-empty frame at generation 2, then a clear 3.
    let received = [seen(2, b"\x1b[1;1Hanother popup"), seen(3, b"")];
    assert_eq!(
        check_help_close(&opened(), &received, 3),
        Err(HelpCloseProblem::NotTheHelp { generation: 2 })
    );
}

#[test]
fn a_clear_of_another_close_is_refused() {
    assert_eq!(
        check_help_close(&opened(), &[seen(2, b"")], 3),
        Err(HelpCloseProblem::NotThisClose {
            generation: 2,
            closed: 3
        })
    );
}

#[test]
fn a_missing_clear_or_a_frame_after_it_is_refused() {
    assert_eq!(
        check_help_close(&opened(), &[seen(2, HELP)], 2),
        Err(HelpCloseProblem::NoClear)
    );
    assert_eq!(
        check_help_close(&opened(), &[], 1),
        Err(HelpCloseProblem::NoClear)
    );
    assert_eq!(
        check_help_close(&opened(), &[seen(2, b""), seen(3, HELP)], 2),
        Err(HelpCloseProblem::AfterClear { frames: 1 })
    );
}
