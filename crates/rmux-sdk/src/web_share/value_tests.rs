use super::{
    WebConfigInfo, WebShareHandle, WebShareLookup, WebSharePaneTarget, WebShareScope,
    WebShareSummary,
};
use crate::transport::TransportClient;
use crate::{PaneId, PaneRef, SessionName};
use rmux_proto as proto;
use serde::Deserialize;

#[derive(Deserialize)]
struct FrozenScope {
    input: String,
    name: String,
    display: String,
    json: serde_json::Value,
    bytes: Vec<u8>,
}

#[test]
fn scope_preserves_independent_base_values_and_serialization() {
    // Captured from ddfbc3ac protocol objects, before changing production.
    let cases: Vec<FrozenScope> = serde_json::from_str(include_str!(
        "../../tests/fixtures/web-scope-boundary/values.json"
    ))
    .unwrap();
    assert_eq!(cases.len(), 21);
    for case in cases {
        let wire: proto::WebShareScope = serde_json::from_value(case.json.clone()).unwrap();
        assert_eq!(SessionName::new(case.input).unwrap().as_str(), case.name);
        assert_eq!(wire.to_string(), case.display);
        assert_eq!(bincode::serialize(&wire).unwrap(), case.bytes);
        let value = WebShareScope::from_proto(wire);
        assert_eq!(value.to_string(), case.display);
        assert_eq!(serde_json::to_value(&value).unwrap(), case.json);
        assert_eq!(bincode::serialize(&value).unwrap(), case.bytes);
        assert_eq!(
            serde_json::from_value::<WebShareScope>(case.json).unwrap(),
            value
        );
        assert_eq!(
            bincode::deserialize::<WebShareScope>(&case.bytes).unwrap(),
            value
        );
        assert_eq!(value.clone(), value);
        match &value {
            WebShareScope::Session(name) => {
                assert!(value.is_session() && !value.is_pane());
                assert_eq!(name.as_str(), case.name);
            }
            WebShareScope::Pane(target) => {
                assert!(value.is_pane() && !value.is_session());
                assert_eq!(target.session_name().as_str(), case.name);
                let bytes = bincode::serialize(target).unwrap();
                assert_eq!(
                    bincode::deserialize::<WebSharePaneTarget>(&bytes).unwrap(),
                    *target
                );
                let json = serde_json::to_value(target).unwrap();
                assert_eq!(
                    serde_json::from_value::<WebSharePaneTarget>(json).unwrap(),
                    *target
                );
                match target {
                    WebSharePaneTarget::Slot(slot) => {
                        assert_eq!(target.pane_id(), None);
                        assert_eq!(WebSharePaneTarget::slot(slot.clone()), *target);
                        assert_eq!(WebSharePaneTarget::from(slot.clone()), *target);
                    }
                    WebSharePaneTarget::Id {
                        session_name,
                        pane_id,
                    } => {
                        assert_eq!(target.pane_id(), Some(*pane_id));
                        assert_eq!(
                            WebSharePaneTarget::by_id(session_name.clone(), *pane_id),
                            *target
                        );
                    }
                }
            }
        }
    }
}

pub(super) fn scopes() -> Vec<(proto::WebShareScope, WebShareScope)> {
    // A constructor would escape the literal backslash twice if used during mapping.
    let name = SessionName::new("a:b.c\n\\literal").unwrap();
    assert_eq!(name.as_str(), "a_b_c\\n\\\\literal");
    vec![
        (
            proto::WebShareScope::Session(name.clone()),
            WebShareScope::Session(name.clone()),
        ),
        (
            proto::WebShareScope::Pane(proto::PaneTargetRef::by_id(
                name.clone(),
                PaneId::new(u32::MAX),
            )),
            WebShareScope::Pane(WebSharePaneTarget::Id {
                session_name: name.clone(),
                pane_id: PaneId::new(u32::MAX),
            }),
        ),
        (
            proto::WebShareScope::Pane(proto::PaneTargetRef::slot(proto::PaneTarget::with_window(
                name.clone(),
                u32::MAX,
                17,
            ))),
            WebShareScope::Pane(WebSharePaneTarget::Slot(PaneRef::new(name, u32::MAX, 17))),
        ),
    ]
}

pub(super) fn summary(scope: proto::WebShareScope, variant: usize) -> proto::WebShareSummary {
    proto::WebShareSummary {
        share_id: " literal share id ".into(),
        scope,
        spectator_url: optional(variant, " redacted #t=… "),
        operator: variant != 0,
        spectator: variant == 0,
        controls: variant == 1,
        active_spectators: [0, 3, u16::MAX][variant],
        active_operators: [u16::MAX, 5, 0][variant],
        max_spectators: [None, Some(0), Some(u16::MAX)][variant],
        max_operators: [Some(u16::MAX), None, Some(0)][variant],
        expires_at_unix: [None, Some(0), Some(u64::MAX)][variant],
        kill_session_on_expire: variant == 2,
    }
}

pub(super) fn assert_summary(value: &WebShareSummary, scope: &WebShareScope, variant: usize) {
    assert_eq!(value.id, " literal share id ");
    assert_eq!(&value.scope, scope);
    assert_eq!(
        value.spectator_url_redacted.as_deref(),
        [None, Some(""), Some(" redacted #t=… ")][variant]
    );
    assert_eq!(
        (value.operator, value.spectator, value.controls),
        [
            (false, true, false),
            (true, false, true),
            (true, false, false)
        ][variant]
    );
    assert_eq!(
        (value.active_spectators, value.active_operators),
        [(0, u16::MAX), (3, 5), (u16::MAX, 0)][variant]
    );
    assert_eq!(
        value.max_spectators,
        [None, Some(0), Some(u16::MAX)][variant]
    );
    assert_eq!(
        value.max_operators,
        [Some(u16::MAX), None, Some(0)][variant]
    );
    assert_eq!(
        value.expires_at_unix,
        [None, Some(0), Some(u64::MAX)][variant]
    );
    assert_eq!(value.kill_session_on_expire, variant == 2);
    assert_eq!(value.clone(), *value);
    match scope {
        WebShareScope::Session(_) => assert!(value.pane_target().is_none()),
        WebShareScope::Pane(target) => assert_eq!(value.pane_target(), Some(target)),
    }
}

fn optional(variant: usize, text: &str) -> Option<String> {
    [None, Some(""), Some(text)][variant].map(str::to_owned)
}

pub(super) fn created(
    scope: proto::WebShareScope,
    variant: usize,
) -> proto::WebShareCreatedResponse {
    proto::WebShareCreatedResponse {
        share_id: " literal share id ".into(),
        scope,
        spectator_url: optional(variant, "https://example.invalid/#t=spectator&x=1"),
        operator_url: optional(variant, "https://example.invalid/#x=1&t=operator"),
        tunnel_provider: optional(variant, " literal provider "),
        tunnel_public_url: optional(variant, " literal origin "),
        operator_pairing_code: optional(variant, " 001234 "),
        spectator_pairing_code: optional(variant, " 009876 "),
        expires_at_unix: [None, Some(0), Some(u64::MAX)][variant],
        max_spectators: [None, Some(0), Some(u16::MAX)][variant],
        max_operators: [Some(u16::MAX), None, Some(0)][variant],
        operator: variant != 0,
        spectator: variant == 0,
        controls: variant == 1,
        kill_session_on_expire: variant == 2,
        output: proto::CommandOutput::from_stdout("CLI output is not share metadata"),
    }
}

pub(super) fn assert_handle(value: &WebShareHandle, scope: &WebShareScope, variant: usize) {
    assert_eq!(value.id(), " literal share id ");
    assert_eq!(value.scope(), scope);
    match scope {
        WebShareScope::Session(_) => assert!(value.pane_target().is_none()),
        WebShareScope::Pane(target) => assert_eq!(value.pane_target(), Some(target)),
    }
    assert_eq!(
        value.spectator_url(),
        [
            None,
            Some(""),
            Some("https://example.invalid/#t=spectator&x=1")
        ][variant]
    );
    assert_eq!(
        value.operator_url(),
        [
            None,
            Some(""),
            Some("https://example.invalid/#x=1&t=operator")
        ][variant]
    );
    assert_eq!(
        value.spectator_token(),
        [None, None, Some("spectator")][variant]
    );
    assert_eq!(
        value.operator_token(),
        [None, None, Some("operator")][variant]
    );
    assert_eq!(
        value.operator_pairing_code(),
        [None, Some(""), Some(" 001234 ")][variant]
    );
    assert_eq!(
        value.spectator_pairing_code(),
        [None, Some(""), Some(" 009876 ")][variant]
    );
    assert_eq!(
        value.tunnel_provider(),
        [None, Some(""), Some(" literal provider ")][variant]
    );
    assert_eq!(
        value.tunnel_public_url(),
        [None, Some(""), Some(" literal origin ")][variant]
    );
    assert_eq!(
        (value.operator(), value.spectator(), value.controls()),
        [
            (false, true, false),
            (true, false, true),
            (true, false, false)
        ][variant]
    );
    assert_eq!(
        value.max_spectators(),
        [None, Some(0), Some(u16::MAX)][variant]
    );
    assert_eq!(
        value.max_operators(),
        [Some(u16::MAX), None, Some(0)][variant]
    );
    assert_eq!(
        value.expires_at_unix(),
        [None, Some(0), Some(u64::MAX)][variant]
    );
    assert_eq!(value.kill_session_on_expire(), variant == 2);
}

#[tokio::test]
async fn all_response_values_preserve_fields_clones_and_owned_strings() {
    for (wire_scope, scope) in scopes() {
        for variant in 0..3 {
            let wire = summary(wire_scope.clone(), variant);
            let id_ptr = wire.share_id.as_ptr();
            let url_ptr = wire.spectator_url.as_ref().map(|s| s.as_ptr());
            let value = WebShareSummary::from_proto(wire);
            assert_eq!(value.id.as_ptr(), id_ptr);
            assert_eq!(
                value.spectator_url_redacted.as_ref().map(|s| s.as_ptr()),
                url_ptr
            );
            assert_summary(&value, &scope, variant);
            let (client, _server) = tokio::io::duplex(64);
            let transport = TransportClient::spawn(client);
            let lookup = WebShareLookup::new(transport.clone(), value);
            let lookup_clone = lookup.clone();
            drop(lookup);
            assert_summary(lookup_clone.cached_summary(), &scope, variant);
            assert_eq!(lookup_clone.scope(), &scope);
            assert_eq!(lookup_clone.id(), " literal share id ");
            assert_eq!(
                lookup_clone.pane_target(),
                lookup_clone.cached_summary().pane_target()
            );
            assert_eq!(
                lookup_clone.spectator_url_redacted(),
                lookup_clone
                    .cached_summary()
                    .spectator_url_redacted
                    .as_deref()
            );
            assert_eq!(
                (
                    lookup_clone.operator(),
                    lookup_clone.spectator(),
                    lookup_clone.controls()
                ),
                [
                    (false, true, false),
                    (true, false, true),
                    (true, false, false)
                ][variant]
            );
            let wire = created(wire_scope.clone(), variant);
            let pointers = [
                wire.share_id.as_ptr(),
                wire.operator_url
                    .as_ref()
                    .map_or(std::ptr::null(), |s| s.as_ptr()),
            ];
            let value = WebShareHandle::new(transport, wire);
            assert_eq!(value.id().as_ptr(), pointers[0]);
            assert_eq!(
                value
                    .operator_url()
                    .map_or(std::ptr::null(), |s| s.as_ptr()),
                pointers[1]
            );
            assert_handle(&value, &scope, variant);
            let cloned = value.clone();
            drop(value);
            assert_handle(&cloned, &scope, variant);
        }
    }
}

#[test]
fn listener_preserves_every_field_without_normalizing() {
    for (host, port, origin) in [("", 0, ""), (" [::1] ", u16::MAX, " literal origin /#? ")] {
        let wire = proto::WebShareListener {
            host: host.into(),
            port,
            frontend_origin: origin.into(),
        };
        let pointers = (wire.host.as_ptr(), wire.frontend_origin.as_ptr());
        let value = WebConfigInfo::from_proto(wire);
        assert_eq!(
            value,
            WebConfigInfo {
                host: host.into(),
                port,
                frontend_origin: origin.into()
            }
        );
        assert_eq!(
            (value.host.as_ptr(), value.frontend_origin.as_ptr()),
            pointers
        );
        assert_eq!(value.clone(), value);
    }
}
