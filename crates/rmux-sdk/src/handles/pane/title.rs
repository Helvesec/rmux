use crate::handles::session::unexpected_response;
use crate::{Pane, Result};
use rmux_proto::{
    DisplayMessageRequest, PaneSelectRequest, Request, Response, SelectPaneRequest, Target,
    CAPABILITY_SDK_PANE_BY_ID,
};

use super::info::current_pane_ref_for_id;
use super::target::is_already_closed_error;

const PANE_TITLE_FORMAT: &str = "#{pane_title}";

pub(super) async fn set_title(pane: &Pane, title: String) -> Result<()> {
    let response = if pane.stable_id.is_some() {
        crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
        pane.transport()
            .request(Request::PaneSelect(PaneSelectRequest {
                target: pane.proto_target_ref(),
                title: Some(title),
            }))
            .await?
    } else {
        pane.transport()
            .request(Request::SelectPane(Box::new(SelectPaneRequest {
                target: pane.target().into(),
                title: Some(title),
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await?
    };

    match response {
        Response::SelectPane(_) => Ok(()),
        response => Err(unexpected_response("select-pane", response)),
    }
}

pub(super) async fn get_title(pane: &Pane) -> Result<Option<String>> {
    let target = match pane.stable_id {
        Some(pane_id) => {
            current_pane_ref_for_id(pane.transport(), &pane.target.session_name, pane_id).await?
        }
        None => Some(pane.target.clone()),
    };
    let Some(target) = target else {
        return Ok(None);
    };

    let response = pane
        .transport()
        .request(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(target.to_proto())),
            print: true,
            message: Some(PANE_TITLE_FORMAT.to_owned()),
            empty_target_context: false,
        }))
        .await;

    match response {
        Ok(Response::DisplayMessage(response)) => Ok(response.output.map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim_end_matches(['\r', '\n'])
                .to_owned()
        })),
        Ok(response) => Err(unexpected_response("display-message", response)),
        Err(error) if is_already_closed_error(&error, &target) => Ok(None),
        Err(error) => Err(error),
    }
}
