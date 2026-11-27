use super::{AttachedKeyDecode, RequestHandler};
use rmux_core::{KeyCode, KEYC_USER};
use rmux_proto::OptionName;

pub(super) fn decode_user_key(
    input: &[u8],
    entries: &[(u32, String)],
) -> Option<AttachedKeyDecode> {
    let mut matched = None;
    for (index, value) in entries {
        if value.is_empty() {
            continue;
        }
        let bytes = value.as_bytes();
        if bytes.starts_with(input) && input.len() < bytes.len() {
            return Some(AttachedKeyDecode::Partial);
        }
        if input.starts_with(bytes)
            && matched
                .as_ref()
                .is_none_or(|(size, _)| *size <= bytes.len())
        {
            matched = Some((bytes.len(), KEYC_USER + KeyCode::from(*index)));
        }
    }
    matched.map(|(size, key)| AttachedKeyDecode::Matched { size, key })
}

impl RequestHandler {
    pub(super) async fn configured_user_keys(&self) -> Vec<(u32, String)> {
        self.state
            .lock()
            .await
            .options
            .resolve_array_entries(None, OptionName::UserKeys)
    }
    pub(super) async fn decode_configured_user_key(
        &self,
        input: &[u8],
    ) -> Option<AttachedKeyDecode> {
        let state = self.state.lock().await;
        decode_user_key(
            input,
            &state
                .options
                .resolve_array_entries(None, OptionName::UserKeys),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{
        NewSessionRequest, OptionScopeSelector, Request, SessionName, SetOptionMode, TerminalSize,
    };

    #[tokio::test]
    async fn configured_user_keys_dispatch_fragmented_sparse_binding_and_honor_unset() {
        let handler = RequestHandler::new();
        let name = SessionName::new("user-key-consumer").unwrap();
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: name.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
        let pid = std::process::id();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        handler.register_attach(pid, name.clone(), tx).await;
        let binding = rmux_core::command_parser::CommandParser::new()
            .parse_one_group("bind-key -n User7 rename-window USER")
            .unwrap();
        handler
            .execute_parsed_commands_for_test(pid, binding)
            .await
            .unwrap();
        handler
            .state
            .lock()
            .await
            .options
            .set_by_name(
                OptionScopeSelector::ServerGlobal,
                "user-keys[7]",
                Some("\x1b[902~".to_owned()),
                SetOptionMode::Replace,
                false,
                false,
                false,
            )
            .unwrap();
        let mut pending = Vec::new();
        handler
            .handle_attached_live_input_inner(pid, &mut pending, b"\x1b[90")
            .await
            .unwrap();
        assert_eq!(pending, b"\x1b[90");
        handler
            .handle_attached_live_input_inner(pid, &mut pending, b"2~")
            .await
            .unwrap();
        assert!(pending.is_empty());
        assert_eq!(
            handler
                .state
                .lock()
                .await
                .sessions
                .session(&name)
                .unwrap()
                .window()
                .name(),
            Some("USER")
        );
        handler
            .state
            .lock()
            .await
            .options
            .set_by_name(
                OptionScopeSelector::ServerGlobal,
                "user-keys[7]",
                None,
                SetOptionMode::Replace,
                false,
                true,
                false,
            )
            .unwrap();
        assert_eq!(handler.decode_configured_user_key(b"\x1b[902~").await, None);
    }
    #[test]
    fn sparse_user_key_sequences_keep_indexes_and_partial_boundaries() {
        let entries = [(2, "\x1b[901~".to_owned()), (7, "\x1b[902~".to_owned())];
        assert_eq!(
            decode_user_key(b"\x1b[90", &entries),
            Some(AttachedKeyDecode::Partial)
        );
        assert_eq!(
            decode_user_key(b"\x1b[902~rest", &entries),
            Some(AttachedKeyDecode::Matched {
                size: 6,
                key: KEYC_USER + 7
            })
        );
        assert_eq!(decode_user_key(b"\x1b[903~", &entries), None);
        assert_eq!(decode_user_key(b"\x1b[902~", &[]), None);
    }
}
