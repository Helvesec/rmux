use rmux_core::formats::{is_truthy, FormatContext};
use rmux_proto::{ListClientsRequest, ListClientsResponse, Response};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};

use super::super::{
    command_output_from_lines, format_requester_uid, sort_list_clients, RequestHandler,
    LIST_CLIENTS_TEMPLATE,
};

impl RequestHandler {
    pub(in crate::handler) async fn handle_list_clients(
        &self,
        requester_pid: u32,
        request: ListClientsRequest,
    ) -> Response {
        let socket_path = self.socket_path();
        let requester_uid = self.requester_uid(requester_pid).await;
        let mut clients = self.list_clients_snapshot().await;
        let client_loop_snapshots = clients
            .iter()
            .map(|client| client.client_loop_snapshot())
            .collect::<Vec<_>>();
        clients.retain(|client| client.session_name.is_some());
        if let Some(target_session) = request.target_session.as_ref() {
            clients.retain(|client| client.session_name.as_ref() == Some(target_session));
        }
        sort_list_clients(
            &mut clients,
            request.sort_order.as_deref(),
            request.reversed,
        );

        let state = self.state.lock().await;
        let lines = clients
            .iter()
            .filter_map(|client| {
                let session_name = client.session_name.as_ref()?;
                let session = state
                    .sessions
                    .session(session_name)
                    .filter(|session| client.session_id == Some(session.id()));
                let attached_count = client_loop_snapshots
                    .iter()
                    .filter(|snapshot| snapshot.bindings.session == session_name.as_str())
                    .count();
                let base = session
                    .map_or_else(FormatContext::new, |session| {
                        FormatContext::new().with_managed_session(session)
                    })
                    .with_session_attached(attached_count);
                let mut context = RuntimeFormatContext::new(base)
                    .with_state(&state)
                    .with_socket_path(&socket_path)
                    .with_client_loop_snapshots(&client_loop_snapshots);
                if let Some(session) = session {
                    context = context.with_session(session);
                }
                let context = client
                    .format_bindings()
                    .apply(context)
                    .with_named_value(
                        "session_id",
                        client
                            .session_id
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    )
                    .with_named_value("session_name", session_name.to_string())
                    .with_named_value("uid", format_requester_uid(requester_uid));
                if let Some(filter) = request.filter.as_deref() {
                    let expanded = render_runtime_template(filter, &context, false);
                    if !is_truthy(&expanded) {
                        return None;
                    }
                }

                Some(render_runtime_template(
                    request.format.as_deref().unwrap_or(LIST_CLIENTS_TEMPLATE),
                    &context,
                    false,
                ))
            })
            .collect::<Vec<_>>();

        Response::ListClients(ListClientsResponse {
            match_count: lines.len(),
            output: command_output_from_lines(&lines),
        })
    }
}
