use super::{ActiveAttachIdentity, RequestHandler};
use crate::outer_terminal::OuterTerminal;
use rmux_proto::OptionName;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub(in crate::handler) struct AssumedPasteState {
    last_key: Option<Instant>,
    assuming: bool,
}

impl AssumedPasteState {
    fn key(&mut self, now: Instant, limit_ms: u32) -> bool {
        let elapsed = self
            .last_key
            .replace(now)
            .map(|last| now.saturating_duration_since(last));
        if limit_ms == 0 {
            return false;
        }
        let fast = elapsed.is_some_and(|elapsed| {
            elapsed.as_secs() == 0 && elapsed < Duration::from_millis(u64::from(limit_ms))
        });
        let paste = fast && self.assuming;
        self.assuming = fast;
        paste
    }
}

impl RequestHandler {
    pub(super) async fn attached_paste_limit(&self, identity: ActiveAttachIdentity) -> u32 {
        let state = self.state.lock().await;
        let clients = self.active_attach.lock().await;
        let Some(active) = clients
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| identity.matches_active(active))
        else {
            return 0;
        };
        let limit = super::super::super::option_value_u32(
            &state.options,
            Some(&active.session_name),
            OptionName::AssumePasteTime,
        );
        if limit == 0
            || active.terminal_context.term_name().is_empty()
            || OuterTerminal::resolve_for_session(
                &state.options,
                Some(&active.session_name),
                active.terminal_context.clone(),
            )
            .has_bracketed_paste()
        {
            0
        } else {
            limit
        }
    }

    pub(super) async fn assume_paste_key(
        &self,
        identity: ActiveAttachIdentity,
        limit_ms: u32,
    ) -> bool {
        if limit_ms == 0 {
            return false;
        }
        let mut clients = self.active_attach.lock().await;
        clients
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|active| identity.matches_active(active))
            .is_some_and(|active| active.assumed_paste.key(Instant::now(), limit_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timing_inference_requires_a_known_terminal_without_bracketed_paste() {
        let handler = RequestHandler::new();
        let session = rmux_proto::SessionName::new("paste-terminal-context").unwrap();
        crate::session_fixture::assert_new_session(
            &handler
                .handle(rmux_proto::Request::NewSession(
                    rmux_proto::NewSessionRequest {
                        session_name: session.clone(),
                        detached: true,
                        size: Some(rmux_proto::TerminalSize { cols: 80, rows: 24 }),
                        environment: None,
                    },
                ))
                .await,
        );
        let pid = std::process::id() + 9847;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        handler.register_attach(pid, session, tx).await;
        let identity = handler.active_attach.lock().await.by_pid[&pid].identity(pid);
        assert_eq!(handler.attached_paste_limit(identity).await, 0);
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get_mut(&pid)
            .unwrap()
            .terminal_context =
            crate::outer_terminal::OuterTerminalContext::from_environment(Some(
                &std::collections::HashMap::from([("TERM".into(), "xterm-256color".into())]),
            ));
        assert_eq!(handler.attached_paste_limit(identity).await, 0);
        handler
            .state
            .lock()
            .await
            .options
            .set(
                rmux_proto::ScopeSelector::Global,
                OptionName::TerminalOverrides,
                "xterm*:Enbp@".into(),
                rmux_proto::SetOptionMode::Replace,
            )
            .unwrap();
        assert_eq!(handler.attached_paste_limit(identity).await, 1);
    }
    #[test]
    fn paste_needs_two_fast_intervals_and_stops_on_a_slow_key() {
        let mut state = AssumedPasteState::default();
        let start = Instant::now();
        assert!(!state.key(start, 1000));
        assert!(!state.key(start + Duration::from_millis(5), 1000));
        assert!(state.key(start + Duration::from_millis(10), 1000));
        assert!(!state.key(start + Duration::from_millis(1200), 2000));
        assert!(!state.key(start + Duration::from_millis(1205), 0));
    }
}
