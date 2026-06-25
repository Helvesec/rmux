use crate::handles::session::unexpected_response;
use crate::{Pane, PaneCloseOutcome, PaneRef, PaneRespawnOptions, Result, RmuxError};
use rmux_proto::{
    KillPaneRequest, OptionName, PaneKillRequest, PaneRespawnRequest, Request, RespawnPaneRequest,
    Response, ScopeSelector, SetOptionMode, SetOptionRequest, CAPABILITY_SDK_PANE_BY_ID,
};

use super::target::is_already_closed_pane_error;

pub(super) async fn close_pane(pane: Pane) -> Result<PaneCloseOutcome> {
    let target = pane.target.clone();
    let stable_id = pane.stable_id;
    let response = if pane.stable_id.is_some() {
        crate::capabilities::require(&pane.transport, &[CAPABILITY_SDK_PANE_BY_ID]).await?;
        pane.transport
            .request(Request::PaneKill(PaneKillRequest {
                target: pane.proto_target_ref(),
                kill_all_except: false,
            }))
            .await
    } else {
        pane.transport
            .request(Request::KillPane(KillPaneRequest {
                target: (&target).into(),
                kill_all_except: false,
            }))
            .await
    };

    match response {
        Ok(Response::KillPane(response)) => Ok(PaneCloseOutcome::Closed {
            target: response.target.into(),
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
    let response = if pane.stable_id.is_some() {
        crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
        pane.transport()
            .request(Request::PaneRespawn(Box::new(PaneRespawnRequest {
                target: pane.proto_target_ref(),
                kill: options.kill,
                start_directory: options.start_directory,
                environment,
                command,
                process_command,
                keep_alive_on_exit: options.keep_alive_on_exit,
            })))
            .await?
    } else {
        if let Some(keep_alive) = options.keep_alive_on_exit {
            set_slot_keep_alive_on_exit(pane, keep_alive).await?;
        }
        pane.transport()
            .request(Request::RespawnPane(Box::new(RespawnPaneRequest {
                target: pane.target().into(),
                kill: options.kill,
                start_directory: options.start_directory,
                environment,
                command,
                process_command,
            })))
            .await?
    };

    match response {
        Response::RespawnPane(response) => Ok(response.target.into()),
        response => Err(unexpected_response("respawn-pane", response)),
    }
}

async fn set_slot_keep_alive_on_exit(pane: &Pane, keep_alive: bool) -> Result<()> {
    let response = pane
        .transport()
        .request(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Pane(pane.target().to_proto()),
            option: OptionName::RemainOnExit,
            value: if keep_alive { "on" } else { "off" }.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await?;

    match response {
        Response::SetOption(_) => Ok(()),
        response => Err(unexpected_response("set-option", response)),
    }
}
