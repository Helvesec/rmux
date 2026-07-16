use crate::handles::session::unexpected_response;
use crate::{Pane, Result};
use rmux_proto::{PaneSelectRequest, Request, Response, CAPABILITY_SDK_PANE_BY_ID};

use super::info::pane_title_for_id;

pub(super) async fn set_title(pane: &Pane, title: String) -> Result<()> {
    crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
    let response = pane
        .transport()
        .request(Request::PaneSelect(PaneSelectRequest {
            target: pane.required_resolved_proto_target_ref().await?,
            title: Some(title),
        }))
        .await?;

    match response {
        Response::SelectPane(_) => Ok(()),
        response => Err(unexpected_response("select-pane", response)),
    }
}

pub(super) async fn get_title(pane: &Pane) -> Result<Option<String>> {
    let Some(target) = pane.resolved_proto_target_ref().await? else {
        return Ok(None);
    };
    let pane_id = target
        .pane_id()
        .expect("resolved SDK pane title targets are id-based");
    pane_title_for_id(pane.transport(), target.session_name(), pane_id).await
}
