use crate::handles::session::unexpected_response;
use crate::{Pane, PaneOptionMutation, Result};
use rmux_proto::{
    PaneOptionGetRequest, PaneOptionSetRequest, Request, Response, SetOptionMode,
    CAPABILITY_SDK_PANE_OPTIONS,
};

pub(super) async fn set_option(
    pane: &Pane,
    name: String,
    value: String,
) -> Result<PaneOptionMutation> {
    mutate_option(pane, name, Some(value), false).await
}

pub(super) async fn unset_option(pane: &Pane, name: String) -> Result<PaneOptionMutation> {
    mutate_option(pane, name, None, true).await
}

pub(super) async fn get_option(pane: &Pane, name: String) -> Result<Option<String>> {
    crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_OPTIONS]).await?;
    let response = pane
        .transport()
        .request(Request::PaneOptionGet(PaneOptionGetRequest {
            target: pane.required_resolved_proto_target_ref().await?,
            name,
        }))
        .await?;

    match response {
        Response::PaneOptionGet(response) => Ok(response.value),
        response => Err(unexpected_response("pane-option-get", response)),
    }
}

async fn mutate_option(
    pane: &Pane,
    name: String,
    value: Option<String>,
    unset: bool,
) -> Result<PaneOptionMutation> {
    crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_OPTIONS]).await?;
    let response = pane
        .transport()
        .request(Request::PaneOptionSet(PaneOptionSetRequest {
            target: pane.required_resolved_proto_target_ref().await?,
            name,
            value,
            mode: SetOptionMode::Replace,
            unset,
        }))
        .await?;

    match response {
        Response::PaneOptionSet(response) => {
            let response = *response;
            Ok(PaneOptionMutation {
                pane_id: response.pane_id,
                name: response.name,
                old_value: response.old_value,
                new_value: response.new_value,
                changed: response.changed,
            })
        }
        response => Err(unexpected_response("pane-option-set", response)),
    }
}
