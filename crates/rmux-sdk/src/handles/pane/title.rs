use crate::handles::session::unexpected_response;
use crate::{Pane, Result};
use rmux_proto::{PaneSelectRequest, Request, Response, CAPABILITY_SDK_PANE_BY_ID};

use super::info::{current_pane_title_for_id, pane_title_for_id};
use super::target::is_stale_pane_id_target_error;

pub(super) async fn set_title(pane: &Pane, title: String) -> Result<()> {
    crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
    let mut target = pane.required_resolved_proto_target_ref().await?;
    for attempt in 0..2 {
        let response = pane
            .transport()
            .request(Request::PaneSelect(PaneSelectRequest {
                target: target.clone(),
                title: Some(title.clone()),
            }))
            .await;

        match response {
            Ok(Response::SelectPane(_)) => return Ok(()),
            Ok(response) => return Err(unexpected_response("select-pane", response)),
            Err(error)
                if attempt == 0
                    && pane.is_stable_id()
                    && is_stale_pane_id_target_error(&error, &target) =>
            {
                let Some(retry_target) = pane.resolved_proto_target_ref().await? else {
                    return Err(error);
                };
                target = retry_target;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("pane title retry loop always returns")
}

pub(super) async fn get_title(pane: &Pane) -> Result<Option<String>> {
    if let Some(pane_id) = pane.stable_id {
        return current_pane_title_for_id(pane.transport(), &pane.target.session_name, pane_id)
            .await;
    }
    let Some(target) = pane.resolved_proto_target_ref().await? else {
        return Ok(None);
    };
    let pane_id = target
        .pane_id()
        .expect("resolved SDK pane title targets are id-based");
    pane_title_for_id(pane.transport(), target.session_name(), pane_id).await
}
