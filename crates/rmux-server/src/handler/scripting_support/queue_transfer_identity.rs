//! Stable identities for the two panes a queued transfer names.
//!
//! `join-pane` and `move-pane` resolve a source and a destination under the
//! parsing lock, then the queue releases that lock and awaits before the
//! mutation runs. Without an identity captured at resolution time, a pane
//! replaced at the same slot during the wait is the one that gets transferred.
//!
//! The capture is deliberately unconditional: it does not depend on `-s`/`-t`
//! being explicit, nor on a retained lifecycle target, because an ordinary
//! queue has neither and is exactly the path the race reaches.

use rmux_proto::{PaneTarget, Request};

use super::queue::QueueInvocation;

/// The source and destination of a queued transfer, in that order.
///
/// The order matters: the handler refuses on the source first, so two stale
/// slots report the source, and both handlers read the same way.
pub(super) const fn queue_transfer_targets(
    invocation: &QueueInvocation,
) -> Option<(&PaneTarget, &PaneTarget)> {
    match invocation {
        QueueInvocation::Request(Request::JoinPane(request)) => {
            Some((&request.source, &request.target))
        }
        QueueInvocation::Request(Request::MovePane(request)) => {
            Some((&request.source, &request.target))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rmux_proto::{JoinPaneRequest, MovePaneRequest, SessionName, SplitDirection};

    use super::*;

    fn pane(value: &str, window_index: u32, pane_index: u32) -> PaneTarget {
        PaneTarget::with_window(
            SessionName::new(value).expect("valid session name"),
            window_index,
            pane_index,
        )
    }

    #[test]
    fn both_transfer_commands_expose_source_then_destination() {
        let source = pane("alpha", 0, 1);
        let destination = pane("beta", 2, 3);
        let join = QueueInvocation::Request(Request::JoinPane(JoinPaneRequest {
            source: source.clone(),
            target: destination.clone(),
            direction: SplitDirection::Vertical,
            detached: false,
            before: false,
            full_size: false,
            size: None,
        }));
        let moved = QueueInvocation::Request(Request::MovePane(MovePaneRequest {
            source: source.clone(),
            target: destination.clone(),
            direction: SplitDirection::Vertical,
            detached: false,
            before: false,
            full_size: false,
            size: None,
        }));
        for invocation in [&join, &moved] {
            let (captured_source, captured_destination) =
                queue_transfer_targets(invocation).expect("a transfer exposes both roles");
            assert_eq!(captured_source, &source);
            assert_eq!(captured_destination, &destination);
        }
    }

    #[test]
    fn other_invocations_expose_no_transfer_roles() {
        let invocation = QueueInvocation::Request(Request::KillPane(rmux_proto::KillPaneRequest {
            target: pane("alpha", 0, 0),
            kill_all_except: false,
        }));
        assert!(queue_transfer_targets(&invocation).is_none());
    }
}
