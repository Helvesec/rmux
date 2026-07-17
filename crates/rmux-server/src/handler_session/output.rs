use rmux_proto::{CommandOutput, RmuxError, SessionId, SessionName};

use crate::format_runtime::render_runtime_template;

use super::super::{scripting_support::format_context_for_target, RequestHandler};

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct NewSessionOutputPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static NEW_SESSION_OUTPUT_PAUSE: std::sync::Mutex<
    Option<(SessionId, std::sync::Arc<NewSessionOutputPause>)>,
> = std::sync::Mutex::new(None);

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) fn install_new_session_output_pause(
        &self,
        session_id: SessionId,
    ) -> std::sync::Arc<NewSessionOutputPause> {
        let pause = std::sync::Arc::new(NewSessionOutputPause::default());
        *NEW_SESSION_OUTPUT_PAUSE
            .lock()
            .expect("new-session output pause mutex") = Some((session_id, pause.clone()));
        pause
    }

    #[cfg(test)]
    async fn pause_before_new_session_output(&self, session_id: SessionId) {
        let pause = {
            let mut installed = NEW_SESSION_OUTPUT_PAUSE
                .lock()
                .expect("new-session output pause mutex");
            installed
                .as_ref()
                .is_some_and(|(expected_id, _)| *expected_id == session_id)
                .then(|| {
                    installed
                        .take()
                        .expect("matching new-session output pause remains installed")
                        .1
                })
        };
        let Some(pause) = pause else {
            return;
        };
        pause.reached.notify_one();
        pause.release.notified().await;
    }

    pub(super) async fn render_new_session_output(
        &self,
        session_id: SessionId,
        template: Option<&str>,
    ) -> Result<(SessionName, CommandOutput), RmuxError> {
        const NEW_SESSION_TEMPLATE: &str = "#{session_name}:";

        #[cfg(test)]
        self.pause_before_new_session_output(session_id).await;

        loop {
            let session_name = self
                .state
                .lock()
                .await
                .sessions
                .session_by_id(session_id)
                .map(|session| session.name().clone())
                .ok_or_else(|| RmuxError::SessionNotFound(session_id.to_string()))?;
            let attached_count = self.attached_count(&session_name).await;
            let state = self.state.lock().await;
            let Some(current_session_name) = state
                .sessions
                .session_by_id(session_id)
                .map(|session| session.name().clone())
            else {
                return Err(RmuxError::SessionNotFound(session_id.to_string()));
            };
            if current_session_name != session_name {
                continue;
            }
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
            return Ok((
                session_name,
                CommandOutput::from_stdout(format!("{expanded}\n").into_bytes()),
            ));
        }
    }
}
