use super::*;

impl RequestHandler {
    pub(crate) fn new_with_web_authentication_limits(
        max_connections: usize,
        max_waiters: usize,
        max_waiters_per_key: usize,
        max_waiters_per_peer: usize,
    ) -> Self {
        let mut handler = Self::new();
        handler.web_shares = Arc::new(
            crate::web::WebShareRegistry::new_with_authentication_limits(
                max_connections,
                max_waiters,
                max_waiters_per_key,
                max_waiters_per_peer,
            ),
        );
        handler
    }

    pub(crate) fn web_authentication_wait_count(&self) -> usize {
        self.web_shares.authentication_wait_count()
    }

    pub(crate) fn record_web_authentication_failures_for_test(
        &self,
        token_id: &str,
        failures: u32,
    ) {
        self.web_shares
            .record_authentication_failures_for_test(token_id, failures);
    }

    pub(crate) async fn publish_web_pane_bytes_for_test(
        &self,
        target: &PaneTargetRef,
        bytes: Vec<u8>,
    ) -> Result<(), RmuxError> {
        let (output, transcript) = {
            let state = self.state.lock().await;
            let target = resolve_pane_target_ref(&state, target)?;
            let output = state.pane_output_for_target(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )?;
            let transcript = state.transcript_handle(&target)?;
            (output, transcript)
        };
        pane_io::publish_pane_bytes_for_test(&transcript, &output, bytes);
        Ok(())
    }

    pub(crate) async fn float_web_pane_for_test(
        &self,
        target: &WindowTarget,
        pane_index: u32,
        rect: FloatingPaneRect,
    ) {
        self.state
            .lock()
            .await
            .sessions
            .window_layout_context_mut(target)
            .expect("window layout context")
            .float_pane(pane_index, rect)
            .expect("pane becomes floating");
    }

    pub(crate) async fn web_session_attach_pid_for_test(&self, session_name: &SessionName) -> u32 {
        self.active_attach
            .lock()
            .await
            .by_pid
            .iter()
            .find_map(|(pid, active)| {
                (active.render_stream && &active.session_name == session_name).then_some(*pid)
            })
            .expect("Web session attach is registered")
    }

    pub(crate) async fn web_window_id_for_test(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> WindowId {
        self.state
            .lock()
            .await
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
            .expect("Web test window exists")
            .id()
    }

    pub(crate) async fn open_web_share(
        &self,
        token: &str,
        pin: Option<&str>,
    ) -> Result<WebShareStream, RmuxError> {
        let token_id = crate::web::SecretHashForCrypto::from_secret(token).token_id();
        self.open_web_share_token_id(&token_id, pin).await
    }
}
