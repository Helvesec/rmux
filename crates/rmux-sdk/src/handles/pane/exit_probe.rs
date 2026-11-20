//! Exit evidence for a pane that left the live inventory.
//!
//! A process exit and an explicit close both remove a pane, and both make its
//! snapshot vacant. Only an exit leaves metadata behind: the daemon retains it
//! by pane id for a short time after removal. This module asks the daemon for
//! that evidence through the same internal pane-exit probe the CLI waits use.

use std::time::Duration;

use rmux_proto::{encode_internal_pane_exit_probe, ListPanesRequest, Request, Response};
use tokio::time::Instant;

use crate::{PaneId, Result, SessionId};

use super::info::current_session_id;
use super::target::is_already_closed_error;
use super::Pane;

/// The daemon removes an exited pane before it records the retained exit, so
/// an exit can briefly look like a close. Matches the CLI handoff grace.
const RETAINED_EXIT_HANDOFF_GRACE: Duration = Duration::from_millis(100);
const RETAINED_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Stands in for the session of a pane whose session is gone as well. The
/// probe only matches live panes inside the named session, so an unknown id
/// leaves the answer to the daemon's retained exits, which are keyed by pane.
const DESTROYED_SESSION_ID: SessionId = SessionId::new(u32::MAX);

impl Pane {
    /// Reports whether a pane that is no longer listed exited on its own.
    ///
    /// `false` means the daemon kept no exit for it within the handoff grace:
    /// the pane was closed, or its session was destroyed around it.
    pub(crate) async fn vanished_pane_exited(&self, pane_id: PaneId) -> Result<bool> {
        let session_id = current_session_id(self.transport(), &self.target().session_name)
            .await?
            .unwrap_or(DESTROYED_SESSION_ID);
        let request = ListPanesRequest {
            target: self.target().session_name.clone(),
            target_window_index: None,
            format: Some(encode_internal_pane_exit_probe(session_id, pane_id)),
            filter: None,
            sort_order: None,
            reversed: false,
        };
        let grace_ends = Instant::now() + RETAINED_EXIT_HANDOFF_GRACE;
        loop {
            let exited = match self
                .transport()
                .request(Request::ListPanes(Box::new(request.clone())))
                .await
            {
                Ok(response) => probe_reports_exit(&response, pane_id),
                // Only a daemon without the probe resolves its session target.
                Err(error) if is_already_closed_error(&error, self.target()) => false,
                Err(error) => return Err(error),
            };
            if exited {
                return Ok(true);
            }
            if Instant::now() >= grace_ends {
                return Ok(false);
            }
            tokio::time::sleep(RETAINED_EXIT_POLL_INTERVAL).await;
        }
    }
}

/// Reads `%id\tdead\tstatus\tsignal\tretained` probe lines. A daemon that does
/// not know the probe, or no longer knows the session, never reports a death.
fn probe_reports_exit(response: &Response, pane_id: PaneId) -> bool {
    let Response::ListPanes(response) = response else {
        return false;
    };
    let pane_field = pane_id.to_string();
    String::from_utf8_lossy(&response.output.stdout)
        .lines()
        .any(|line| {
            let mut fields = line.split('\t');
            fields.next() == Some(pane_field.as_str()) && fields.next() == Some("1")
        })
}

#[cfg(test)]
mod tests {
    use rmux_proto::{CommandOutput, ListPanesResponse};

    use super::*;

    fn list_panes(stdout: &str) -> Response {
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout(stdout.as_bytes().to_vec()),
        })
    }

    #[test]
    fn probe_reports_exit_only_for_a_dead_line_of_the_same_pane() {
        let pane_id = PaneId::new(9);
        assert!(probe_reports_exit(&list_panes("%9\t1\t7\t\t1\n"), pane_id));
        assert!(probe_reports_exit(&list_panes("%9\t1\t\t15\t0\n"), pane_id));
        assert!(!probe_reports_exit(&list_panes("%9\t0\t\t\t0\n"), pane_id));
        assert!(!probe_reports_exit(
            &list_panes("%19\t1\t7\t\t1\n"),
            pane_id
        ));
        assert!(!probe_reports_exit(&list_panes(""), pane_id));
        // A daemon without the probe renders the format literally per pane.
        assert!(!probe_reports_exit(
            &list_panes("\0rmux-pane-exit-probe-v1:3:9\n"),
            pane_id
        ));
    }
}
