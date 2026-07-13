use crate::handles::session::unexpected_response;
use crate::{Pane, Result, TerminalSizeSpec};
use rmux_proto::{
    PaneInputRequest, PaneResizeRequest, Request, ResizePaneAdjustment, Response,
    CAPABILITY_SDK_PANE_BY_ID,
};

pub(super) async fn send_text(pane: &Pane, text: &str) -> Result<()> {
    crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
    let response = pane
        .transport()
        .request(Request::PaneInput(PaneInputRequest {
            target: pane.required_resolved_proto_target_ref().await?,
            keys: vec![text.to_owned()],
            literal: true,
        }))
        .await?;

    match response {
        Response::SendKeys(_) => Ok(()),
        response => Err(unexpected_response("send-keys", response)),
    }
}

pub(super) async fn send_key(pane: &Pane, key: String) -> Result<()> {
    crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
    let response = pane
        .transport()
        .request(Request::PaneInput(PaneInputRequest {
            target: pane.required_resolved_proto_target_ref().await?,
            keys: vec![key],
            literal: false,
        }))
        .await?;

    match response {
        Response::SendKeys(_) => Ok(()),
        response => Err(unexpected_response("send-keys", response)),
    }
}

pub(super) async fn resize_to_size(pane: &Pane, requested: TerminalSizeSpec) -> Result<()> {
    let current = live_pane_size(pane).await?;
    let mut sent_non_noop_adjustment = false;

    if current.cols != requested.cols {
        request_resize_pane(
            pane,
            ResizePaneAdjustment::AbsoluteWidth {
                columns: requested.cols,
            },
        )
        .await?;
        sent_non_noop_adjustment = true;
    }

    if current.rows != requested.rows {
        request_resize_pane(
            pane,
            ResizePaneAdjustment::AbsoluteHeight {
                rows: requested.rows,
            },
        )
        .await?;
        sent_non_noop_adjustment = true;
    }

    if !sent_non_noop_adjustment {
        request_resize_pane(pane, ResizePaneAdjustment::NoOp).await?;
    }

    Ok(())
}

async fn live_pane_size(pane: &Pane) -> Result<TerminalSizeSpec> {
    let snapshot = pane.snapshot().await?;
    Ok(TerminalSizeSpec::new(snapshot.cols, snapshot.rows))
}

async fn request_resize_pane(pane: &Pane, adjustment: ResizePaneAdjustment) -> Result<()> {
    crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_BY_ID]).await?;
    let response = pane
        .transport()
        .request(Request::PaneResize(PaneResizeRequest {
            target: pane.required_resolved_proto_target_ref().await?,
            adjustment,
        }))
        .await?;

    match response {
        Response::ResizePane(_) => Ok(()),
        response => Err(unexpected_response("resize-pane", response)),
    }
}
