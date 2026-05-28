use rmux_proto::{CommandOutput, RmuxError};

use crate::format_runtime::render_runtime_template;

use super::super::{scripting_support::format_context_for_target, RequestHandler};

impl RequestHandler {
    pub(super) async fn render_new_session_output(
        &self,
        session_name: &rmux_proto::SessionName,
        template: Option<&str>,
    ) -> Result<CommandOutput, RmuxError> {
        const NEW_SESSION_TEMPLATE: &str = "#{session_name}:";

        let attached_count = self.attached_count(session_name).await;
        let state = self.state.lock().await;
        let mut runtime = format_context_for_target(
            &state,
            &rmux_proto::Target::Session(session_name.clone()),
            attached_count,
        )?;
        if attached_count == 0 {
            runtime = runtime.with_unclipped_geometry();
        }
        let expanded =
            render_runtime_template(template.unwrap_or(NEW_SESSION_TEMPLATE), &runtime, false);
        Ok(CommandOutput::from_stdout(
            format!("{expanded}\n").into_bytes(),
        ))
    }
}
