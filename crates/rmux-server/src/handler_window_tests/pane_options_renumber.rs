use super::*;
use crate::pane_io::PaneExitEvent;
use rmux_core::PaneId;
use rmux_proto::{OptionScopeSelector, PaneKillRequest, SetOptionByNameRequest};

#[derive(Clone, Copy, Debug)]
enum Removal {
    Indexed,
    ById,
    Natural,
}

async fn set_pane_marker(handler: &RequestHandler, target: PaneTarget, name: &str, value: &str) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Pane(target),
            name: name.to_owned(),
            value: Some(value.to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
}

async fn set_option(
    handler: &RequestHandler,
    scope: ScopeSelector,
    option: OptionName,
    value: &str,
) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope,
            option,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn remove(handler: &RequestHandler, target: PaneTarget, id: PaneId, removal: Removal) {
    match removal {
        Removal::Indexed => {
            let response = handler
                .handle(Request::KillPane(KillPaneRequest {
                    target,
                    kill_all_except: false,
                }))
                .await;
            assert!(matches!(response, Response::KillPane(_)), "{response:?}");
        }
        Removal::ById => {
            let response = handler
                .handle(Request::PaneKill(PaneKillRequest {
                    target: PaneTargetRef::by_id(target.session_name().clone(), id),
                    kill_all_except: false,
                }))
                .await;
            assert!(matches!(response, Response::KillPane(_)), "{response:?}");
        }
        Removal::Natural => {
            handler
                .state
                .lock()
                .await
                .mark_pane_dead_without_exit_details(&target)
                .expect("mark exited pane");
            handler
                .handle_pane_exit_event(PaneExitEvent::eof_published(
                    target.session_name().clone(),
                    id,
                    None,
                ))
                .await;
        }
    }
}

// Each entry path must preserve pane-scoped values by stable identity, including
// the survivor moved onto the removed pane's former window/pane coordinates.
async fn acceptance(removal: Removal) {
    let mut failures = Vec::new();
    for count in [2, 3] {
        for base in [0, 1] {
            for renumber in [false, true] {
                let handler = RequestHandler::new();
                set_option(
                    &handler,
                    ScopeSelector::Global,
                    OptionName::BaseIndex,
                    &base.to_string(),
                )
                .await;
                let name = session_name(&format!(
                    "pane-options-{removal:?}-{count}-{base}-{renumber}"
                ));
                create_session(&handler, name.as_str()).await;
                for index in base + 1..base + count {
                    insert_window(&handler, &name, index).await;
                }
                set_option(
                    &handler,
                    ScopeSelector::Session(name.clone()),
                    OptionName::RenumberWindows,
                    if renumber { "on" } else { "off" },
                )
                .await;
                let mut expected = Vec::new();
                for index in base..base + count {
                    let target = PaneTarget::with_window(name.clone(), index, 0);
                    let id = handler
                        .state
                        .lock()
                        .await
                        .sessions
                        .session(&name)
                        .unwrap()
                        .window_at(index)
                        .unwrap()
                        .pane(0)
                        .unwrap()
                        .id();
                    let marker = format!("pane-{}", id.as_u32());
                    set_pane_marker(&handler, target, "@keep", &marker).await;
                    expected.push((id, marker));
                }
                let removed_target = PaneTarget::with_window(name.clone(), base, 0);
                set_pane_marker(&handler, removed_target.clone(), "@removed-only", "discard").await;
                remove(&handler, removed_target, expected[0].0, removal).await;
                let state = handler.state.lock().await;
                let session = state.sessions.session(&name).expect("surviving session");
                assert_eq!(session.windows().count(), (count - 1) as usize);
                for (offset, (id, marker)) in expected.iter().skip(1).enumerate() {
                    let index = base + offset as u32 + u32::from(!renumber);
                    let pane = session.window_at(index).unwrap().pane(0).unwrap();
                    assert_eq!(pane.id(), *id, "stable pane identity");
                    let scope =
                        OptionScopeSelector::Pane(PaneTarget::with_window(name.clone(), index, 0));
                    let actual = state
                        .options
                        .explicit_value_by_name(&scope, "@keep")
                        .unwrap()
                        .1;
                    if actual.as_deref() != Some(marker.as_str()) {
                        failures.push(format!("{removal:?} windows={count} base={base} renumber={renumber} pane={} slot={index}.0 expected={marker} actual={actual:?}", id.as_u32()));
                    }
                    assert_eq!(
                        state
                            .options
                            .explicit_value_by_name(&scope, "@removed-only")
                            .unwrap()
                            .1,
                        None,
                        "removed options must not transfer to the neighbor"
                    );
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn acceptance_indexed_pane_removal_renumber_preserves_pane_options() {
    acceptance(Removal::Indexed).await;
}

#[tokio::test]
async fn acceptance_by_id_pane_removal_renumber_preserves_pane_options() {
    acceptance(Removal::ById).await;
}

#[tokio::test]
async fn acceptance_natural_pane_removal_renumber_preserves_pane_options() {
    acceptance(Removal::Natural).await;
}

#[path = "pane_options_renumber/neighbors.rs"]
mod neighbors;
