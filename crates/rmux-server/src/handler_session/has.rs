use rmux_proto::{ErrorResponse, HasSessionResponse, Response};

use super::super::{resolve_session_lookup, RequestHandler, SessionLookup};

impl RequestHandler {
    pub(in crate::handler) async fn handle_has_session(
        &self,
        request: rmux_proto::HasSessionRequest,
    ) -> Response {
        let state = self.state.lock().await;
        let exists = match resolve_session_lookup(&state.sessions, "has-session", &request.target) {
            Ok(SessionLookup::Found(_)) => true,
            Ok(SessionLookup::Missing) => false,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        Response::HasSession(HasSessionResponse { exists })
    }
}
