use rmux_core::PaneId;
use rmux_proto::{RmuxError, SessionName};

use super::{HandlerState, WindowLinkSlot};

pub(in crate::pane_terminals) struct DetachedWindowLinkRuntimeTransfer {
    source_runtime: SessionName,
    destination_runtime: SessionName,
    pane_ids: Vec<PaneId>,
}

impl HandlerState {
    pub(in crate::pane_terminals) fn transfer_detached_window_link_runtime(
        &mut self,
        source_runtime: &SessionName,
        survivor_slot: &WindowLinkSlot,
        pane_ids: &[PaneId],
    ) -> Result<Option<DetachedWindowLinkRuntimeTransfer>, RmuxError> {
        let destination_runtime = self.runtime_session_name(&survivor_slot.session_name);
        self.set_window_link_runtime_session_for_slot(survivor_slot, destination_runtime.clone());
        if source_runtime == &destination_runtime || pane_ids.is_empty() {
            return Ok(None);
        }

        self.terminals.move_panes_between_sessions(
            source_runtime,
            &destination_runtime,
            pane_ids,
        )?;
        if let Err(error) =
            self.move_pane_outputs_between_sessions(source_runtime, &destination_runtime, pane_ids)
        {
            self.terminals.move_panes_between_sessions(
                &destination_runtime,
                source_runtime,
                pane_ids,
            )?;
            return Err(error);
        }

        Ok(Some(DetachedWindowLinkRuntimeTransfer {
            source_runtime: source_runtime.clone(),
            destination_runtime,
            pane_ids: pane_ids.to_vec(),
        }))
    }

    pub(in crate::pane_terminals) fn rollback_detached_window_link_runtime(
        &mut self,
        transfer: &Option<DetachedWindowLinkRuntimeTransfer>,
    ) -> Result<(), RmuxError> {
        let Some(transfer) = transfer else {
            return Ok(());
        };
        self.move_pane_outputs_between_sessions(
            &transfer.destination_runtime,
            &transfer.source_runtime,
            &transfer.pane_ids,
        )?;
        self.terminals.move_panes_between_sessions(
            &transfer.destination_runtime,
            &transfer.source_runtime,
            &transfer.pane_ids,
        )
    }
}
