use rmux_proto::{ErrorResponse, Response, RmuxError, WebShareRequest};

use super::RequestHandler;

impl RequestHandler {
    pub(in crate::handler) async fn handle_web_share(&self, _request: WebShareRequest) -> Response {
        Response::Error(ErrorResponse {
            error: RmuxError::Server("web-share support is not enabled in this daemon".to_owned()),
        })
    }
}
