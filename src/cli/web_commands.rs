use std::path::Path;

use rmux_proto::{
    CreateWebShareRequest, ListWebSharesRequest, LookupWebShareRequest, PaneTargetRef, Response,
    StopAllWebSharesRequest, StopWebShareRequest, WebShareConfigRequest, WebShareRequest,
    WebShareResponse,
};

use super::{
    connect_with_startserver, finish_command_success, resolve_current_pane_target,
    resolve_pane_target_spec, ExitFailure, StartupOptions,
};
use crate::cli_args::WebShareArgs;

pub(super) fn run_web_share(
    args: WebShareArgs,
    socket_path: &Path,
    startup: StartupOptions,
) -> Result<i32, ExitFailure> {
    let mut connection = connect_with_startserver(socket_path, startup)?;
    let request = build_web_share_request(args, &mut connection)?;
    let response = connection
        .web_share(request)
        .map_err(ExitFailure::from_client)?;
    warn_operator_url(&response);
    finish_command_success(response, "web-share")
}

fn warn_operator_url(response: &Response) {
    let Response::WebShare(WebShareResponse::Created(created)) = response else {
        return;
    };
    let Some(operator_url) = created.operator_url.as_deref() else {
        return;
    };
    eprintln!("rmux: operator URL (writable, keep private):");
    eprintln!("rmux:   {operator_url}");
}

fn build_web_share_request(
    args: WebShareArgs,
    connection: &mut rmux_client::Connection,
) -> Result<WebShareRequest, ExitFailure> {
    if args.list {
        return Ok(WebShareRequest::List(ListWebSharesRequest));
    }
    if let Some(share_id) = args.stop {
        return Ok(WebShareRequest::Stop(StopWebShareRequest { share_id }));
    }
    if args.stop_all {
        return Ok(WebShareRequest::StopAll(StopAllWebSharesRequest));
    }
    if let Some(share_id) = args.lookup {
        return Ok(WebShareRequest::Lookup(LookupWebShareRequest { share_id }));
    }
    if args.config {
        return Ok(WebShareRequest::Config(WebShareConfigRequest));
    }

    let target = match args.target.as_ref() {
        Some(target) => resolve_pane_target_spec(connection, target)?,
        None => resolve_current_pane_target(connection, "web-share")?,
    };
    Ok(WebShareRequest::Create(CreateWebShareRequest {
        target: PaneTargetRef::slot(target),
        public_base_url: args.public_base_url,
        ttl_seconds: args.ttl_seconds,
        max_viewers: args.max_viewers,
        writable: args.writable,
    }))
}
