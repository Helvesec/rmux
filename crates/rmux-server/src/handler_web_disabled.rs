use rmux_core::PaneId;
use rmux_proto::{ErrorResponse, Response, RmuxError, SessionId, SessionName, WebShareRequest};

use super::RequestHandler;

impl RequestHandler {
    pub(in crate::handler) async fn handle_web_share(&self, _request: WebShareRequest) -> Response {
        Response::Error(ErrorResponse {
            error: RmuxError::Server("web-share support is not enabled in this daemon".to_owned()),
        })
    }

    pub(in crate::handler) fn prune_web_session(&self, _removed: Option<(SessionName, SessionId)>) {
    }

    pub(in crate::handler) fn prune_web_panes(&self, _pane_ids: &[PaneId]) {}

    pub(in crate::handler) fn rekey_web_session(
        &self,
        _old_name: &SessionName,
        _new_name: &SessionName,
        _session_id: SessionId,
    ) {
    }
}
