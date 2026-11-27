use super::{
    ControlClientFlags, ControlMode, ControlPaneOutputStates, ControlSessionAttachment,
    PaneOutputAttachedClientReadDemandGuard, PaneOutputReadDemandGuard, RequestHandler,
    SessionName,
};

#[cfg(any(unix, windows))]
pub(super) async fn reconcile_control_read_demand(
    handler: &RequestHandler,
    session_name: Option<&SessionName>,
    flags: ControlClientFlags,
    pane_output_states: &mut ControlPaneOutputStates,
    unrestricted_read_demand: &mut Option<PaneOutputReadDemandGuard>,
    attached_client_read_presence: &mut Option<PaneOutputAttachedClientReadDemandGuard>,
) {
    let live_pane_ids = handler.control_pane_output_ids().await;
    pane_output_states.prune_missing(&live_pane_ids);
    if flags.no_output {
        if unrestricted_read_demand.is_none() {
            *unrestricted_read_demand = Some(handler.acquire_unrestricted_pane_read_demand().await);
        }
    } else {
        let _ = unrestricted_read_demand.take();
    }

    if session_name.is_some() {
        if attached_client_read_presence.is_none() {
            *attached_client_read_presence =
                Some(handler.acquire_attached_pane_read_presence().await);
        }
        if let Some(attached_client_read_presence) = attached_client_read_presence.as_ref() {
            attached_client_read_presence
                .replace_off_panes(pane_output_states.read_blocked_panes());
        }
    } else {
        let _ = attached_client_read_presence.take();
    }
}

#[cfg(any(unix, windows))]
pub(super) fn control_control_waits_for_attached_session(
    mode: ControlMode,
    attachment: &ControlSessionAttachment,
) -> bool {
    mode.is_control_control() && attachment.is_attached()
}
