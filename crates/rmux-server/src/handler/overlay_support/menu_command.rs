//! The ordinary command of a chosen menu item, run as a binding's commands are.
//!
//! A menu item is chosen from its client's input path. Awaited inline there, a popup the item
//! shows could not wait for its own close - the popup's input comes through that same path - so
//! whatever followed the popup in the item ran at once. Run as separate work of the client, the
//! popup holds the rest of the item until it closes and releases the input path meanwhile; the
//! client's ordinary input then waits behind the item, as tmux queues keys behind a command.
//! The menu's replacement surface is already queued. A command that suspends must release the
//! input path even without opening a popup, so the attach loop can transmit that repaint.

use rmux_core::command_parser::ParsedCommands;
use rmux_proto::RmuxError;

use super::super::attach_support::{ActiveAttachIdentity, InputPathWork};
use super::super::scripting_support::QueueExecutionContext;
use super::super::{RequestHandler, RequesterOrigin};

impl RequestHandler {
    /// Runs a menu item's `commands` for the client `attach_pid` under the menu's `origin`.
    pub(super) async fn execute_menu_command(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        origin: RequesterOrigin,
        commands: ParsedCommands,
        context: QueueExecutionContext,
    ) -> Result<(), RmuxError> {
        let task_handler = self.clone();
        let work = async move {
            let _access = task_handler.begin_requester_origin_access(&origin);
            task_handler
                .execute_parsed_commands(origin.requester_pid(), commands, context)
                .await
                .map(|_| ())
        };
        match self.run_input_path_work_for_repaint(attach_pid, work).await {
            InputPathWork::Finished(joined) => joined.map_err(menu_command_task_failed)?,
            InputPathWork::Released(released) => {
                // With no caller left to receive an error, the client reports it.
                let handler = self.clone();
                self.spawn_background_task("rmux-released-menu-command", move || async move {
                    let (joined, _outstanding) = released.join().await;
                    if let Err(error) = joined.map_err(menu_command_task_failed).and_then(|r| r) {
                        let session_name = handler
                            .active_attach
                            .lock()
                            .await
                            .by_pid
                            .get(&attach_pid)
                            .filter(|active| {
                                identity.is_none_or(|identity| identity.matches_active(active))
                            })
                            .map(|active| active.session_name.clone());
                        if let Some(session_name) = session_name {
                            handler
                                .report_attached_command_error(&session_name, attach_pid, &error)
                                .await;
                        }
                    }
                })
            }
        }
    }
}

fn menu_command_task_failed(error: tokio::task::JoinError) -> RmuxError {
    RmuxError::Server(format!("menu command task failed: {error}"))
}
