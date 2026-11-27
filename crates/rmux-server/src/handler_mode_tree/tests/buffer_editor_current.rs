use super::*;

#[derive(Clone, Copy, Debug)]
enum EditorRoute {
    Direct,
    Attached,
    SendKeys,
    SendKeysExt,
}

async fn edit_current(handler: &RequestHandler, pid: u32, label: &str, route: EditorRoute) {
    match route {
        EditorRoute::Direct => assert!(handler
            .handle_mode_tree_key_event(pid, PromptInputEvent::Char('e'))
            .await
            .unwrap()),
        EditorRoute::Attached => handler
            .handle_attached_live_input_for_test(pid, b"e")
            .await
            .unwrap(),
        EditorRoute::SendKeys | EditorRoute::SendKeysExt => {
            let target = PaneTarget::new(SessionName::new(label).unwrap(), 0);
            let request = if matches!(route, EditorRoute::SendKeys) {
                Request::SendKeys(rmux_proto::SendKeysRequest {
                    target,
                    keys: vec!["e".into()],
                })
            } else {
                Request::SendKeysExt(rmux_proto::SendKeysExtRequest {
                    target: Some(target),
                    keys: vec!["e".into()],
                    expand_formats: false,
                    hex: false,
                    literal: false,
                    dispatch_key_table: false,
                    copy_mode_command: false,
                    forward_mouse_event: false,
                    reset_terminal: false,
                    repeat_count: None,
                })
            };
            let response = handler.handle(request).await;
            assert!(matches!(response, Response::SendKeys(_)), "{response:?}");
        }
    }
}

async fn replace_buffer(handler: &RequestHandler, name: &str) {
    let response = handler
        .handle(Request::DeleteBuffer(rmux_proto::DeleteBufferRequest {
            name: Some(name.into()),
        }))
        .await;
    assert!(
        matches!(response, Response::DeleteBuffer(_)),
        "{response:?}"
    );
    let response = handler
        .handle(Request::SetBuffer(Box::new(rmux_proto::SetBufferRequest {
            name: Some(name.into()),
            content: b"replacement".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })))
        .await;
    assert!(matches!(response, Response::SetBuffer(_)), "{response:?}");
}

#[tokio::test]
async fn buffer_editor_edits_live_current_after_unrelated_tags_disappear() {
    let mut failures = Vec::new();
    for route in [
        EditorRoute::Direct,
        EditorRoute::Attached,
        EditorRoute::SendKeys,
        EditorRoute::SendKeysExt,
    ] {
        // None also checks the existing untagged stale-selection protection.
        for tags_live in [Some(false), Some(true), None] {
            for current_live in [true, false] {
                let label = format!("editor-current-{route:?}-{tags_live:?}-{current_live}");
                let (handler, pid, _rx) = choose_buffer_action_fixture(&label, 1207).await;
                let tagged_id = choose_buffer_item_id(&handler, pid, "stale").await;
                let current_id = choose_buffer_item_id(&handler, pid, "keep").await;
                {
                    let mut clients = handler.active_attach.lock().await;
                    let mode = clients
                        .by_pid
                        .get_mut(&pid)
                        .unwrap()
                        .mode_tree
                        .as_mut()
                        .unwrap();
                    mode.selected_id = Some(current_id.clone());
                    if tags_live.is_some() {
                        mode.tagged.insert(tagged_id);
                    }
                }
                // Mutate through public requests without refreshing the chooser:
                // the input rebuild must reconcile these captured identities.
                if tags_live == Some(false) {
                    replace_buffer(&handler, "stale").await;
                }
                if !current_live {
                    replace_buffer(&handler, "keep").await;
                }
                assert_eq!(
                    handler.active_attach.lock().await.by_pid[&pid]
                        .mode_tree
                        .as_ref()
                        .unwrap()
                        .selected_id
                        .as_ref(),
                    Some(&current_id)
                );
                let before = {
                    let state = handler.state.lock().await;
                    ["stale", "delete-me", "keep"].map(|name| {
                        let (_, content, order) =
                            state.buffers.show_with_order(Some(name)).unwrap();
                        (name, content.to_vec(), order)
                    })
                };
                let release = std::env::temp_dir().join(format!(
                    "rmux-editor-current-{}-{label}.go",
                    std::process::id()
                ));
                let _ = std::fs::remove_file(&release);
                handler.state.lock().await.options.set(
                    ScopeSelector::Global,
                    OptionName::Editor,
                    format!("/bin/sh -c 'while [ ! -f \"{}\" ]; do sleep 0.01; done; printf EDIT > \"$1\"' editor", release.display()),
                    SetOptionMode::Replace,
                ).unwrap();
                edit_current(&handler, pid, &label, route).await;
                let identity = handler.active_attach.lock().await.by_pid[&pid].identity(pid);
                let opened = handler.active_attach.lock().await.by_pid[&pid]
                    .overlay
                    .is_some();
                assert!(handler.active_attach.lock().await.by_pid[&pid]
                    .mode_tree
                    .is_some());
                assert_eq!(
                    handler.mode_tree_active_for_identity(identity).await,
                    !opened
                );
                // Retain the editor until all popup observations are complete (H1).
                std::fs::write(&release, b"go").unwrap();
                if opened {
                    tokio::time::timeout(std::time::Duration::from_secs(5), async {
                        loop {
                            if before.iter().any(|(name, _, _)| {
                                // try_lock avoids holding a lock across the timer.
                                handler.state.try_lock().is_ok_and(|state| {
                                    state.buffers.get(name) == Some(b"EDIT".as_slice())
                                })
                            }) {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .expect("editor completion replaces its captured buffer");
                }
                std::fs::remove_file(release).unwrap();
                if opened != current_live {
                    failures.push(format!(
                        "{label}: editor opened={opened}, expected={current_live}"
                    ));
                }
                let clients = handler.active_attach.lock().await;
                let mode = clients.by_pid[&pid].mode_tree.as_ref().unwrap();
                assert_eq!(mode.tagged.len(), usize::from(tags_live == Some(true)));
                drop(clients);
                let state = handler.state.lock().await;
                for (name, content, order) in before {
                    let (_, actual, actual_order) =
                        state.buffers.show_with_order(Some(name)).unwrap();
                    let expected = if current_live && name == "keep" {
                        b"EDIT".as_slice()
                    } else {
                        &content
                    };
                    if actual != expected || actual_order != order {
                        failures.push(format!("{label}: unexpected content/order for {name}"));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "current-only editor matrix:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn buffer_editor_rejects_current_deleted_between_rebuilds() {
    use super::super::super::mode_tree_test_support::{
        install_mode_tree_identity_pause, ModeTreeIdentityPausePoint,
    };

    let mut failures = Vec::new();
    for route in [
        EditorRoute::Direct,
        EditorRoute::Attached,
        EditorRoute::SendKeys,
        EditorRoute::SendKeysExt,
    ] {
        for recreate in [false, true] {
            let label = format!("editor-between-rebuilds-{route:?}-{recreate}");
            let (handler, pid, _rx) = choose_buffer_action_fixture(&label, 1208).await;
            let tagged_id = choose_buffer_item_id(&handler, pid, "stale").await;
            let current_id = choose_buffer_item_id(&handler, pid, "keep").await;
            {
                let mut clients = handler.active_attach.lock().await;
                let mode = clients
                    .by_pid
                    .get_mut(&pid)
                    .unwrap()
                    .mode_tree
                    .as_mut()
                    .unwrap();
                mode.selected_id = Some(current_id.clone());
                mode.tagged.insert(tagged_id);
            }
            replace_buffer(&handler, "stale").await;
            let release = std::env::temp_dir().join(format!(
                "rmux-editor-between-rebuilds-{}-{label}.go",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&release);
            handler.state.lock().await.options.set(
                ScopeSelector::Global,
                OptionName::Editor,
                format!("/bin/sh -c 'while [ ! -f \"{}\" ]; do sleep 0.01; done; printf EDIT > \"$1\"' editor", release.display()),
                SetOptionMode::Replace,
            ).unwrap();
            let pause = install_mode_tree_identity_pause(ModeTreeIdentityPausePoint::Store(pid));
            // Poll both futures in this task: the public mutation occurs only
            // after input's rebuild, before the editor's second rebuild.
            let (_, before) = tokio::join!(edit_current(&handler, pid, &label, route), async {
                tokio::time::timeout(std::time::Duration::from_secs(2), pause.reached.notified())
                    .await
                    .expect("editor input reaches Store after its first rebuild");
                assert_eq!(
                    handler.active_attach.lock().await.by_pid[&pid]
                        .mode_tree
                        .as_ref()
                        .unwrap()
                        .selected_id
                        .as_ref(),
                    Some(&current_id)
                );
                if recreate {
                    replace_buffer(&handler, "keep").await;
                } else {
                    let response = handler
                        .handle(Request::DeleteBuffer(rmux_proto::DeleteBufferRequest {
                            name: Some("keep".into()),
                        }))
                        .await;
                    assert!(
                        matches!(response, Response::DeleteBuffer(_)),
                        "{response:?}"
                    );
                }
                let state = handler.state.lock().await;
                let before = ["stale", "delete-me", "keep"].map(|name| {
                    let buffer = state
                        .buffers
                        .show_with_order(Some(name))
                        .ok()
                        .map(|(_, content, order)| (content.to_vec(), order));
                    (name, buffer)
                });
                drop(state);
                pause.release.notify_one();
                before
            });
            let (opened, chooser_active, tags_empty) = {
                let clients = handler.active_attach.lock().await;
                let active = &clients.by_pid[&pid];
                (
                    active.overlay.is_some(),
                    active.mode_tree.is_some(),
                    active.mode_tree.as_ref().unwrap().tagged.is_empty(),
                )
            };
            // Complete even an incorrectly opened editor before reporting red;
            // observe both popup admission and its real buffer write.
            std::fs::write(&release, b"go").unwrap();
            if opened {
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let edited = {
                            let state = handler.state.lock().await;
                            before.iter().any(|(name, _)| {
                                state.buffers.get(name) == Some(b"EDIT".as_slice())
                            })
                        };
                        if edited {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("incorrect editor completes before checking unchanged buffers");
            }
            std::fs::remove_file(release).unwrap();
            if opened || !chooser_active || !tags_empty {
                failures.push(format!(
                    "{label}: popup={opened}, chooser={chooser_active}, tags_empty={tags_empty}"
                ));
            }
            let state = handler.state.lock().await;
            for (name, expected) in before {
                let actual = state
                    .buffers
                    .show_with_order(Some(name))
                    .ok()
                    .map(|(_, content, order)| (content.to_vec(), order));
                if actual != expected {
                    failures.push(format!("{label}: unexpected content/order for {name}"));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "between-rebuild editor matrix:\n{}",
        failures.join("\n")
    );
}
