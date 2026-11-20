use super::*;

async fn snapshot_markers(handler: &RequestHandler, name: &SessionName) -> Vec<(PaneId, String)> {
    let slots = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(name)
            .unwrap()
            .windows()
            .flat_map(|(index, window)| {
                window.panes().iter().map(move |pane| {
                    (
                        pane.id(),
                        PaneTarget::with_window(name.clone(), index, pane.index()),
                    )
                })
            })
            .collect::<Vec<_>>()
    };
    let mut markers = Vec::new();
    for (id, target) in slots {
        let value = format!("pane-{}", id.as_u32());
        set_pane_marker(handler, target, "@keep", &value).await;
        markers.push((id, value));
    }
    markers
}

async fn assert_markers(
    handler: &RequestHandler,
    name: &SessionName,
    markers: &[(PaneId, String)],
) {
    let state = handler.state.lock().await;
    let session = state.sessions.session(name).expect("session survives");
    for (index, window) in session.windows() {
        for pane in window.panes() {
            let expected = markers
                .iter()
                .find(|(id, _)| *id == pane.id())
                .expect("known stable pane");
            let scope = OptionScopeSelector::Pane(PaneTarget::with_window(
                name.clone(),
                index,
                pane.index(),
            ));
            assert_eq!(
                state
                    .options
                    .explicit_value_by_name(&scope, "@keep")
                    .unwrap()
                    .1
                    .as_deref(),
                Some(expected.1.as_str()),
                "pane {} at {index}.{}",
                pane.id().as_u32(),
                pane.index()
            );
        }
    }
}

#[tokio::test]
async fn partial_pane_removal_and_kill_other_panes_preserve_pane_options() {
    for count in [2, 3] {
        for base in [0, 1] {
            for renumber in [false, true] {
                for kill_all_except in [false, true] {
                    let handler = RequestHandler::new();
                    set_option(
                        &handler,
                        ScopeSelector::Global,
                        OptionName::BaseIndex,
                        &base.to_string(),
                    )
                    .await;
                    let name = session_name(&format!(
                        "partial-options-{count}-{base}-{renumber}-{kill_all_except}"
                    ));
                    create_session(&handler, name.as_str()).await;
                    for index in base + 1..base + count {
                        insert_window(&handler, &name, index).await;
                    }
                    for _ in 0..2 {
                        let split = handler
                            .handle(Request::SplitWindow(SplitWindowRequest {
                                target: SplitWindowTarget::Pane(PaneTarget::with_window(
                                    name.clone(),
                                    base,
                                    0,
                                )),
                                direction: SplitDirection::Vertical,
                                before: false,
                                environment: None,
                            }))
                            .await;
                        assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
                    }
                    handler.wait_for_initial_panes_for_test().await;
                    set_option(
                        &handler,
                        ScopeSelector::Session(name.clone()),
                        OptionName::RenumberWindows,
                        if renumber { "on" } else { "off" },
                    )
                    .await;
                    let markers = snapshot_markers(&handler, &name).await;
                    let target = PaneTarget::with_window(
                        name.clone(),
                        base,
                        if kill_all_except { 1 } else { 0 },
                    );
                    let response = handler
                        .handle(Request::KillPane(KillPaneRequest {
                            target,
                            kill_all_except,
                        }))
                        .await;
                    assert!(matches!(response, Response::KillPane(_)), "{response:?}");
                    {
                        let state = handler.state.lock().await;
                        let session = state.sessions.session(&name).unwrap();
                        assert_eq!(
                            session.windows().map(|(i, _)| i).collect::<Vec<_>>(),
                            (base..base + count).collect::<Vec<_>>()
                        );
                        assert_eq!(
                            session.window_at(base).unwrap().pane_count(),
                            if kill_all_except { 1 } else { 2 }
                        );
                    }
                    assert_markers(&handler, &name, &markers).await;
                }
            }
        }
    }
}

#[tokio::test]
async fn last_session_pane_removal_does_not_transfer_options_to_recreated_session() {
    for removal in [Removal::Indexed, Removal::ById, Removal::Natural] {
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
                let name = session_name(&format!("last-options-{removal:?}-{base}-{renumber}"));
                create_session(&handler, name.as_str()).await;
                set_option(
                    &handler,
                    ScopeSelector::Session(name.clone()),
                    OptionName::RenumberWindows,
                    if renumber { "on" } else { "off" },
                )
                .await;
                let marker = snapshot_markers(&handler, &name).await;
                remove(
                    &handler,
                    PaneTarget::with_window(name.clone(), base, 0),
                    marker[0].0,
                    removal,
                )
                .await;
                assert!(handler.state.lock().await.sessions.session(&name).is_none());
                create_session(&handler, name.as_str()).await;
                let state = handler.state.lock().await;
                let session = state.sessions.session(&name).unwrap();
                assert_ne!(
                    session.window_at(base).unwrap().pane(0).unwrap().id(),
                    marker[0].0
                );
                assert_eq!(
                    state
                        .options
                        .explicit_value_by_name(
                            &OptionScopeSelector::Pane(PaneTarget::with_window(name, base, 0)),
                            "@keep"
                        )
                        .unwrap()
                        .1,
                    None
                );
            }
        }
    }
}

#[tokio::test]
async fn linked_alias_pane_removal_renumber_preserves_survivor_options() {
    for removal in [Removal::Indexed, Removal::ById, Removal::Natural] {
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
                    let owner = session_name(&format!(
                        "alias-options-owner-{removal:?}-{count}-{base}-{renumber}"
                    ));
                    let alias = session_name(&format!(
                        "alias-options-peer-{removal:?}-{count}-{base}-{renumber}"
                    ));
                    create_session(&handler, owner.as_str()).await;
                    for index in base + 1..base + count {
                        insert_window(&handler, &owner, index).await;
                    }
                    create_session(&handler, alias.as_str()).await;
                    let linked = handler
                        .handle(Request::LinkWindow(LinkWindowRequest {
                            source: WindowTarget::with_window(owner.clone(), base),
                            target: WindowTarget::with_window(alias.clone(), base + 1),
                            after: false,
                            before: false,
                            kill_destination: false,
                            detached: true,
                        }))
                        .await;
                    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
                    for name in [&owner, &alias] {
                        set_option(
                            &handler,
                            ScopeSelector::Session(name.clone()),
                            OptionName::RenumberWindows,
                            if renumber { "on" } else { "off" },
                        )
                        .await;
                    }
                    let owner_markers = snapshot_markers(&handler, &owner).await;
                    let alias_markers = snapshot_markers(&handler, &alias).await;
                    remove(
                        &handler,
                        PaneTarget::with_window(owner.clone(), base, 0),
                        owner_markers[0].0,
                        removal,
                    )
                    .await;
                    assert_markers(&handler, &owner, &owner_markers).await;
                    assert_markers(&handler, &alias, &alias_markers).await;
                }
            }
        }
    }
}

#[tokio::test]
async fn failed_last_pane_removal_renumber_restores_pane_options_and_runtime() {
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
                let name =
                    session_name(&format!("rollback-pane-options-{count}-{base}-{renumber}"));
                create_session(&handler, name.as_str()).await;
                for index in base + 1..base + count {
                    insert_window(&handler, &name, index).await;
                }
                handler.wait_for_initial_panes_for_test().await;
                set_option(
                    &handler,
                    ScopeSelector::Session(name.clone()),
                    OptionName::RenumberWindows,
                    if renumber { "on" } else { "off" },
                )
                .await;
                let markers = snapshot_markers(&handler, &name).await;
                let pids = {
                    let mut state = handler.state.lock().await;
                    let pids = (base..base + count)
                        .map(|index| state.pane_pid_in_window(&name, index, 0).unwrap())
                        .collect::<Vec<_>>();
                    state.fail_next_resize_for_test();
                    pids
                };
                let response = handler
                    .handle(Request::KillPane(KillPaneRequest {
                        target: PaneTarget::with_window(name.clone(), base, 0),
                        kill_all_except: false,
                    }))
                    .await;
                let Response::Error(error) = response else {
                    panic!("expected injected resize error: {response:?}");
                };
                assert_eq!(
                    error.error,
                    rmux_proto::RmuxError::Server(
                        "injected pane terminal resize failure".to_owned()
                    )
                );
                assert_markers(&handler, &name, &markers).await;
                let state = handler.state.lock().await;
                for (offset, index) in (base..base + count).enumerate() {
                    assert_eq!(
                        state
                            .sessions
                            .session(&name)
                            .unwrap()
                            .window_at(index)
                            .unwrap()
                            .pane(0)
                            .unwrap()
                            .id(),
                        markers[offset].0
                    );
                    assert_eq!(
                        state.pane_pid_in_window(&name, index, 0).unwrap(),
                        pids[offset]
                    );
                    state
                        .ensure_window_panes_exist(&name, index, &[markers[offset].0])
                        .expect("runtime restored");
                }
            }
        }
    }
}
