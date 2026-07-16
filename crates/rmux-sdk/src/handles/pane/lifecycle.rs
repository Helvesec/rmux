use crate::handles::session::unexpected_response;
use crate::{Pane, PaneCloseOutcome, PaneRef, PaneRespawnOptions, Result, RmuxError};
use rmux_proto::{
    PaneKillRequest, PaneRespawnRequest, Request, Response, CAPABILITY_SDK_PANE_BY_ID,
};

use super::target::is_already_closed_pane_error;

pub(super) async fn close_pane(pane: Pane) -> Result<PaneCloseOutcome> {
    let target = pane.target.clone();
    let stable_id = pane.stable_id;
    let response = async {
        crate::capabilities::require(&pane.transport, &[CAPABILITY_SDK_PANE_BY_ID]).await?;
        pane.transport
            .request(Request::PaneKill(PaneKillRequest {
                target: pane.required_resolved_proto_target_ref().await?,
                kill_all_except: false,
            }))
            .await
    }
    .await;

    match response {
        Ok(Response::KillPane(response)) => Ok(PaneCloseOutcome::Closed {
            target,
            window_destroyed: response.window_destroyed,
        }),
        Ok(response) => Err(unexpected_response("kill-pane", response)),
        Err(error) if is_already_closed_pane_error(&error, &target) => {
            Ok(PaneCloseOutcome::AlreadyClosed { target })
        }
        Err(RmuxError::PaneNotFound {
            session_name,
            pane_id,
        }) if stable_id == Some(pane_id) && session_name == target.session_name => {
            Ok(PaneCloseOutcome::AlreadyClosed { target })
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn respawn_pane(pane: &Pane, options: PaneRespawnOptions) -> Result<PaneRef> {
    let (command, process_command, environment) = options.process.into_proto_parts();
    crate::capabilities::require_process_command_if_present(
        pane.transport(),
        process_command.as_ref(),
    )
    .await?;
    crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
    let response = pane
        .transport()
        .request(Request::PaneRespawn(Box::new(PaneRespawnRequest {
            target: pane.required_resolved_proto_target_ref().await?,
            kill: options.kill,
            start_directory: options.start_directory,
            environment,
            command,
            process_command,
            keep_alive_on_exit: options.keep_alive_on_exit,
        })))
        .await?;

    match response {
        Response::RespawnPane(_) => pane.current_target().await,
        response => Err(unexpected_response("respawn-pane", response)),
    }
}
