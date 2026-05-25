use rmux_proto::{CreateWebShareRequest, ErrorResponse, PaneTargetRef, Response, WebShareRequest};

use super::pane_support::resolve_pane_target_ref;
use super::RequestHandler;
use crate::pane_terminal_lookup::pane_id_for_target;

impl RequestHandler {
    pub(in crate::handler) async fn handle_web_share(&self, request: WebShareRequest) -> Response {
        let request = match self.resolve_web_share_targets(request).await {
            Ok(request) => request,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        match self.web_shares.handle(request) {
            Ok(response) => Response::WebShare(response),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    async fn resolve_web_share_targets(
        &self,
        request: WebShareRequest,
    ) -> Result<WebShareRequest, rmux_proto::RmuxError> {
        match request {
            WebShareRequest::Create(request) => self.resolve_create_web_share(request).await,
            other => Ok(other),
        }
    }

    async fn resolve_create_web_share(
        &self,
        mut request: CreateWebShareRequest,
    ) -> Result<WebShareRequest, rmux_proto::RmuxError> {
        let state = self.state.lock().await;
        let target = resolve_pane_target_ref(&state, &request.target)?;
        let pane_id = pane_id_for_target(
            &state.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        request.target = PaneTargetRef::by_id(target.session_name().clone(), pane_id);
        Ok(WebShareRequest::Create(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{
        CreateWebShareRequest, NewSessionRequest, Request, Response, SessionName, TerminalSize,
    };

    #[tokio::test]
    async fn web_share_create_resolves_slot_target_to_stable_pane_id() {
        let handler = RequestHandler::new();
        let session_name = SessionName::new("alpha").expect("valid session");
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));

        let response = handler
            .handle(Request::WebShare(WebShareRequest::Create(
                CreateWebShareRequest {
                    target: rmux_proto::PaneTarget::new(session_name.clone(), 0).into(),
                    public_base_url: Some("https://share.example".to_owned()),
                    ttl_seconds: None,
                    max_viewers: Some(1),
                    writable: false,
                },
            )))
            .await;

        let Response::WebShare(rmux_proto::WebShareResponse::Created(created)) = response else {
            panic!("expected created web-share response");
        };
        assert!(matches!(
            created.target,
            PaneTargetRef::Id {
                session_name: ref actual,
                ..
            } if actual == &session_name
        ));
        assert!(created.viewer_url.contains("?key="));
    }
}
