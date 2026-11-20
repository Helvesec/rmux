//! The only way a final-sink test reaches the attached input path.
//!
//! Output refresh prunes every attached client whose control channel is
//! closed, after which input fails with "attached client disappeared". Both
//! final-sink suites therefore attach through this guard, which owns the
//! control receiver for as long as it can send input.
//!
//! Private fields alone did not close that hole: the guard registered
//! `std::process::id()`, which every test can name, so
//! `let _ = attach_to(..)` followed by input under the ambient pid compiled,
//! dropped the receiver at once and passed whenever no refresh happened to run
//! in between. The guard now registers a pid that only it knows. Dropping it
//! leaves no way to address that attach, and input under any other pid finds
//! no client at all, refresh or not.
//!
//! The server never needs that pid to be alive: it keys the attach, and the
//! client name falls back to the number when the pid has no terminal.

use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::mpsc;

use crate::handler::RequestHandler;
use crate::pane_io::AttachControl;

/// Above every Unix `pid_max` (Linux caps it at 2^22, macOS at 99 999) and far
/// above the Windows pids seen in practice; `unique_guard_pid` still refuses
/// the ambient pid outright.
const FIRST_GUARD_PID: u32 = 0xF000_0000;

static NEXT_GUARD_PID: AtomicU32 = AtomicU32::new(FIRST_GUARD_PID);

/// A pid no other guard and no process in this test run uses.
fn unique_guard_pid() -> u32 {
    let pid = NEXT_GUARD_PID.fetch_add(1, Ordering::Relaxed);
    assert!(
        pid >= FIRST_GUARD_PID,
        "final-sink guard pids wrapped around"
    );
    assert_ne!(
        pid,
        std::process::id(),
        "a guard pid must not be the ambient test-process pid"
    );
    pid
}

#[must_use = "dropping the client closes its control receiver and refresh prunes the attach"]
pub(crate) struct AttachedFinalSinkClient {
    requester_pid: u32,
    _control_rx: mpsc::UnboundedReceiver<AttachControl>,
}

impl AttachedFinalSinkClient {
    /// Registers a fresh attached client of `session` under a pid only this
    /// guard knows.
    pub(crate) async fn register(
        handler: &RequestHandler,
        session: &rmux_proto::SessionName,
    ) -> Self {
        let requester_pid = unique_guard_pid();
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let _attach_id = handler
            .register_attach(requester_pid, session.clone(), control_tx)
            .await;
        Self {
            requester_pid,
            _control_rx: control_rx,
        }
    }

    pub(crate) async fn send(&self, handler: &RequestHandler, bytes: &[u8]) -> std::io::Result<()> {
        handler
            .handle_attached_live_input_for_test(self.requester_pid, bytes)
            .await
    }

    /// Sends one read of a fragmented input, carrying the partial sequence
    /// between reads exactly as the attach reader does.
    pub(crate) async fn send_read(
        &self,
        handler: &RequestHandler,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        handler
            .handle_attached_live_input(self.requester_pid, pending_input, bytes)
            .await
    }
}
