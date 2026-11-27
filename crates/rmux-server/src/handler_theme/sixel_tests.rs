use super::*;
use crate::outer_terminal::OuterTerminalContext;
use rmux_proto::{OptionName, ScopeSelector, SetOptionMode, SetOptionRequest};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize},
    Arc,
};

const QUERY: &[u8] = b"\x1b[0c\x1b[?1;1S\x1b[16t";
const UNSUPPORTED: &[u8] = b"\x1b[?1;2c\x1b[6;32;16t";
const SUPPORTED: &[u8] = b"\x1b[?1;2;4c\x1b[?1;0;1024S\x1b[6;32;16t";

async fn set(handler: &RequestHandler, option: OptionName, value: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option,
            value: value.into(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

#[tokio::test]
async fn sixel_queries_follow_passthrough_terminal_features_and_detach() {
    let (handler, target) = fixture("sixel-caps").await;
    assert_eq!(query(&handler, &target, QUERY).await, UNSUPPORTED);
    let (tx, _rx) = mpsc::unbounded_channel();
    let pid = 938259;
    let uid = crate::server_access::current_owner_uid();
    let attach_id = handler
        .register_attach_with_access(
            pid,
            target.session_name().clone(),
            None,
            crate::handler::attach_support::AttachRegistration {
                control_tx: tx,
                control_backlog: Arc::new(AtomicUsize::new(0)),
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context: OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
                client_title: None,
                flags: Default::default(),
                // The native CLI uses the rendered stream too.
                render_stream: true,
                uid,
                user: handler.server_owner_identity(),
                can_write: true,
                client_size: None,
            },
        )
        .await
        .expect("native rendered-stream attach");
    set(&handler, OptionName::AllowPassthrough, "all").await;
    assert_eq!(query(&handler, &target, QUERY).await, UNSUPPORTED);
    set(&handler, OptionName::TerminalFeatures, "xterm*:sixel").await;
    assert_eq!(query(&handler, &target, QUERY).await, SUPPORTED);
    set(&handler, OptionName::AllowPassthrough, "off").await;
    assert_eq!(query(&handler, &target, QUERY).await, UNSUPPORTED);
    set(&handler, OptionName::AllowPassthrough, "on").await;
    assert_eq!(query(&handler, &target, QUERY).await, SUPPORTED);
    set(&handler, OptionName::TerminalFeatures, "").await;
    assert_eq!(query(&handler, &target, QUERY).await, UNSUPPORTED);
    set(&handler, OptionName::TerminalFeatures, "*:sixel").await;
    assert_eq!(query(&handler, &target, QUERY).await, SUPPORTED);
    set(&handler, OptionName::TerminalOverrides, "*:Sxl@").await;
    assert_eq!(query(&handler, &target, QUERY).await, UNSUPPORTED);
    set(&handler, OptionName::TerminalOverrides, "").await;
    assert_eq!(query(&handler, &target, QUERY).await, SUPPORTED);
    handler.finish_attach(pid, attach_id).await;
    assert_eq!(query(&handler, &target, QUERY).await, UNSUPPORTED);
}

#[tokio::test]
async fn sixel_queries_follow_suspend_resume_and_new_active_panes() {
    let (handler, target) = fixture("sixel-lifecycle").await;
    set(&handler, OptionName::AllowPassthrough, "all").await;
    let (tx, _rx) = mpsc::unbounded_channel();
    let pid = 938260;
    handler
        .register_attach_with_terminal_context(
            pid,
            target.session_name().clone(),
            tx,
            OuterTerminalContext::from_pairs(&[("TERM", "foot")]),
        )
        .await;
    assert_eq!(query(&handler, &target, QUERY).await, SUPPORTED);
    let identity = handler.active_attach_identity(pid).await.unwrap();
    let response = handler
        .handle(Request::SuspendClient(rmux_proto::SuspendClientRequest {
            target_client: Some(pid.to_string()),
        }))
        .await;
    assert!(matches!(response, Response::SuspendClient(_)));
    assert_eq!(query(&handler, &target, QUERY).await, UNSUPPORTED);
    assert!(handler.handle_attached_unlock_for_identity(identity).await);
    assert_eq!(query(&handler, &target, QUERY).await, SUPPORTED);

    let response = handler
        .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
            target: rmux_proto::SplitWindowTarget::Session(target.session_name().clone()),
            direction: rmux_proto::SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    let Response::SplitWindow(response) = response else {
        panic!("{response:?}")
    };
    assert_eq!(query(&handler, &response.pane, QUERY).await, SUPPORTED);
    assert_eq!(query(&handler, &target, QUERY).await, UNSUPPORTED);
}

#[cfg(feature = "web")]
#[tokio::test]
async fn sixel_queries_do_not_advertise_a_web_only_client() {
    let (handler, target) = fixture("sixel-web").await;
    set(&handler, OptionName::AllowPassthrough, "all").await;
    let (tx, _rx) = mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(
            938261,
            target.session_name().clone(),
            tx,
            OuterTerminalContext::from_pairs(&[("TERM", "foot")]).without_theme_reports(),
        )
        .await;
    assert_eq!(query(&handler, &target, QUERY).await, UNSUPPORTED);
}
