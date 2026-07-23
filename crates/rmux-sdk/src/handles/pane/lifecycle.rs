use crate::handles::session::unexpected_response;
use crate::{Pane, PaneCloseOutcome, PaneRef, PaneRespawnOptions, Result};
use rmux_proto::{
    PaneKillRequest, PaneRespawnRequest, Request, Response, CAPABILITY_SDK_PANE_BY_ID,
};

use super::target::{is_already_closed_pane_error, is_stale_pane_id_target_error};

pub(super) async fn close_pane(pane: Pane) -> Result<PaneCloseOutcome> {
    let target = pane.target.clone();
    crate::capabilities::require(&pane.transport, &[CAPABILITY_SDK_PANE_BY_ID]).await?;
    let Some(mut resolved_target) = pane.resolved_proto_target_ref().await? else {
        return Ok(PaneCloseOutcome::AlreadyClosed { target });
    };

    for attempt in 0..2 {
        let response = pane
            .transport
            .request(Request::PaneKill(PaneKillRequest {
                target: resolved_target.clone(),
                kill_all_except: false,
            }))
            .await;

        match response {
            Ok(Response::KillPane(response)) => {
                return Ok(PaneCloseOutcome::Closed {
                    target,
                    window_destroyed: response.window_destroyed,
                });
            }
            Ok(response) => return Err(unexpected_response("kill-pane", response)),
            Err(error)
                if pane.is_stable_id()
                    && is_stale_pane_id_target_error(&error, &resolved_target) =>
            {
                let Some(live_target) = pane.resolved_proto_target_ref().await? else {
                    return Ok(PaneCloseOutcome::AlreadyClosed { target });
                };
                if attempt == 0 {
                    resolved_target = live_target;
                    continue;
                }
                return Err(error);
            }
            Err(error) if is_already_closed_pane_error(&error, &target) => {
                return Ok(PaneCloseOutcome::AlreadyClosed { target });
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("pane close retry loop always returns")
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
