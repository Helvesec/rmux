use std::path::Path;

use rmux_proto::{PaneTarget, RmuxError, Target};

use super::super::scripting_support::{
    apply_client_loop_context, format_context_for_target_with_server_values,
};
use crate::client_format::ClientLoopSnapshot;
use crate::format_runtime::render_runtime_template;
use crate::pane_terminals::HandlerState;

pub(super) struct SendKeyFormatContext<'a> {
    pub(super) attached_count: usize,
    pub(super) socket_path: &'a Path,
    pub(super) client_loop_snapshots: Option<&'a [ClientLoopSnapshot]>,
    pub(super) client_name: Option<&'a str>,
}

pub(super) fn expand_send_key_tokens(
    state: &HandlerState,
    target: &PaneTarget,
    tokens: &[String],
    expand_formats: bool,
    formats: SendKeyFormatContext<'_>,
) -> Result<Vec<String>, RmuxError> {
    let SendKeyFormatContext {
        attached_count,
        socket_path,
        client_loop_snapshots,
        client_name,
    } = formats;
    if !expand_formats {
        return Ok(tokens.to_vec());
    }
    super::super::require_expected_pane_identity(state, target)?;
    let context = format_context_for_target_with_server_values(
        state,
        &Target::Pane(target.clone()),
        attached_count,
        socket_path,
    )?;
    let context = apply_client_loop_context(context, client_loop_snapshots, client_name);
    // Each argument remains one key token, including whitespace and empty results.
    Ok(tokens
        .iter()
        .map(|token| render_runtime_template(token, &context, false))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{SessionName, TerminalSize};

    #[test]
    fn format_tokens_keep_boundaries_and_target_values() {
        let mut state = HandlerState::default();
        let name = SessionName::new("target-session").unwrap();
        state
            .sessions
            .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
            .unwrap();
        let pane = state
            .sessions
            .split_pane(
                &rmux_proto::WindowTarget::with_window(name.clone(), 0),
                0,
                rmux_core::ManagedPaneSplit {
                    direction: rmux_proto::SplitDirection::Vertical,
                    before: false,
                    detached: true,
                    full_size: false,
                },
            )
            .unwrap();
        assert_eq!(pane.pane_index, 1);
        let target = PaneTarget::with_window(name, 0, pane.pane_index);
        let tokens: Vec<String> = [
            "#{session_name}",
            "#{window_index}.#{pane_index}:#{pane_active}",
            "a b",
            "#{missing_send_key}",
            "",
            "#{?session_name,Enter,Escape}",
            "#{session_attached}",
            "#{socket_path}",
            "##",
            "'quoted'",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let socket = Path::new("/owned/socket");
        assert_eq!(
            expand_send_key_tokens(
                &state,
                &target,
                &tokens,
                false,
                SendKeyFormatContext {
                    attached_count: 2,
                    socket_path: socket,
                    client_loop_snapshots: None,
                    client_name: None,
                },
            )
            .unwrap(),
            tokens
        );
        assert_eq!(
            expand_send_key_tokens(
                &state,
                &target,
                &tokens,
                true,
                SendKeyFormatContext {
                    attached_count: 2,
                    socket_path: socket,
                    client_loop_snapshots: None,
                    client_name: None,
                },
            )
            .unwrap(),
            [
                "target-session",
                "0.1:0",
                "a b",
                "",
                "",
                "Enter",
                "2",
                "/owned/socket",
                "#",
                "'quoted'"
            ]
        );
    }

    #[tokio::test]
    async fn format_expansion_rejects_retained_identity_mismatch() {
        use crate::handler::{with_expected_stable_target_identities, StableTargetIdentity};
        let mut state = HandlerState::default();
        let name = SessionName::new("replacement").unwrap();
        state
            .sessions
            .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
            .unwrap();
        let target = PaneTarget::new(name, 0);
        let retained = StableTargetIdentity::pane_for_test(target.clone());
        with_expected_stable_target_identities(vec![retained], None, None, async {
            assert!(expand_send_key_tokens(
                &state,
                &target,
                &["#{session_name}".into()],
                true,
                SendKeyFormatContext {
                    attached_count: 0,
                    socket_path: Path::new("/owned/socket"),
                    client_loop_snapshots: None,
                    client_name: None,
                },
            )
            .is_err());
        })
        .await;
    }

    #[test]
    fn format_expansion_keeps_the_client_loop_snapshot() {
        let mut state = HandlerState::default();
        let name = SessionName::new("client-loop-send-keys").unwrap();
        state
            .sessions
            .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
            .unwrap();
        let target = PaneTarget::new(name.clone(), 0);
        let snapshots = [ClientLoopSnapshot {
            bindings: crate::client_format::ClientFormatBindings {
                name: "client-loop".to_owned(),
                session: name.to_string(),
                ..Default::default()
            },
            order: 1,
            activity_at: crate::client_activity::ClientActivityTimestamp::from_parts(1, 0),
        }];

        assert_eq!(
            expand_send_key_tokens(
                &state,
                &target,
                &["#{L:#{client_name}}".to_owned()],
                true,
                SendKeyFormatContext {
                    attached_count: 1,
                    socket_path: Path::new("/owned/socket"),
                    client_loop_snapshots: Some(&snapshots),
                    client_name: Some("client-loop"),
                },
            )
            .unwrap(),
            vec!["client-loop".to_owned()]
        );
    }
}
