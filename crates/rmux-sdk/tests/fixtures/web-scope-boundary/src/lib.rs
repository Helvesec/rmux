//! SDK-only downstream construction and actual facade signatures; no daemon needed.
use rmux_sdk::{
    Pane, PaneId, PaneRef, Rmux, Session, SessionName, WebConfigInfo, WebShareHandle,
    WebShareLookup, WebSharePaneTarget, WebShareScope, WebShareSummary,
};

pub fn values(name: SessionName) -> [WebShareScope; 3] {
    [
        WebShareScope::Session(name.clone()),
        WebShareScope::Pane(WebSharePaneTarget::slot(PaneRef::new(
            name.clone(),
            u32::MAX,
            17,
        ))),
        WebShareScope::Pane(WebSharePaneTarget::by_id(name, PaneId::new(u32::MAX))),
    ]
}

pub fn summary(scope: WebShareScope) -> WebShareSummary {
    WebShareSummary {
        id: " literal id ".into(),
        scope,
        spectator_url_redacted: Some(String::new()),
        operator: false,
        spectator: true,
        controls: false,
        active_spectators: u16::MAX,
        active_operators: 0,
        max_spectators: None,
        max_operators: Some(0),
        expires_at_unix: Some(u64::MAX),
        kill_session_on_expire: true,
    }
}

pub fn getters(handle: &WebShareHandle, lookup: &WebShareLookup) {
    let _: &WebShareScope = handle.scope();
    let _: Option<&WebSharePaneTarget> = handle.pane_target();
    let _: &WebShareScope = lookup.scope();
    let _: Option<&WebSharePaneTarget> = lookup.pane_target();
    let _: &WebShareSummary = lookup.cached_summary();
    let _: Option<&WebSharePaneTarget> = lookup.cached_summary().pane_target();
    let _: Option<&str> = handle.operator_pairing_code();
    let _: Option<&str> = handle.spectator_pairing_code();
    let _: Option<&str> = handle.tunnel_provider();
    let _: Option<&str> = handle.tunnel_public_url();
    let _: bool = handle.controls();
    let _: bool = lookup.controls();
    let _ = (handle.clone(), lookup.clone());
}

// These futures are compiled, never polled by fixture tests.
pub async fn facade(rmux: &Rmux, session: &Session, pane: &Pane) -> rmux_sdk::Result<()> {
    let _: Vec<WebShareSummary> = rmux.list_web_shares().await?;
    let _: WebConfigInfo = rmux.web_config().await?;
    let lookup: WebShareLookup = rmux.web_share_by_id("inert").await?;
    let _: WebShareSummary = lookup.summary().await?;
    lookup.stop().await?;
    let handle: WebShareHandle = session.share().await?;
    let _: WebShareSummary = handle.summary().await?;
    let _: u16 = handle.spectators_active().await?;
    let _: u16 = handle.operators_active().await?;
    handle.stop().await?;
    let _: WebShareHandle = pane.share().await?;
    let _: bool = rmux.stop_web_share("inert").await?;
    let _: usize = rmux.stop_all_web_shares().await?;
    Ok(())
}

#[test]
fn sdk_values_are_lossless_and_owned() {
    let name = SessionName::new("a:b.c\n\\literal").unwrap();
    let scopes = values(name.clone());
    for scope in scopes {
        let metadata = summary(scope.clone());
        assert_eq!(metadata.clone(), metadata);
        match scope {
            WebShareScope::Session(value) => {
                assert_eq!(value, name);
                assert!(metadata.scope.is_session());
                assert!(metadata.pane_target().is_none());
            }
            WebShareScope::Pane(target) => {
                assert!(metadata.scope.is_pane());
                assert_eq!(metadata.pane_target(), Some(&target));
                assert_eq!(target.session_name(), &name);
                match &target {
                    WebSharePaneTarget::Slot(slot) => {
                        assert_eq!((slot.window_index, slot.pane_index), (u32::MAX, 17));
                        assert_eq!(target.pane_id(), None);
                        assert_eq!(WebSharePaneTarget::from(slot.clone()), target);
                    }
                    WebSharePaneTarget::Id {
                        session_name,
                        pane_id,
                    } => {
                        assert_eq!(session_name, &name);
                        assert_eq!(pane_id.as_u32(), u32::MAX);
                        assert_eq!(target.pane_id(), Some(*pane_id));
                    }
                }
                let mut keys = std::collections::HashSet::new();
                assert!(keys.insert(target.clone()));
                assert!(!keys.insert(target));
            }
        }
        assert!(metadata.spectator_url_redacted.as_ref().unwrap().is_empty());
    }
    for name in [
        std::any::type_name::<WebShareScope>(),
        std::any::type_name::<WebSharePaneTarget>(),
    ] {
        assert!(name.starts_with("rmux_sdk::"), "{name}");
    }
    let config = WebConfigInfo {
        host: String::new(),
        port: u16::MAX,
        frontend_origin: " literal ".into(),
    };
    assert_eq!(config.clone(), config);
    assert_eq!(config.port, u16::MAX);
}
