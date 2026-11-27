use super::*;

#[tokio::test]
async fn switching_last_client_reaps_only_unattached_opted_in_source() {
    let mut failures = Vec::new();
    for attached in [false, true] {
        for resident in [None, Some(false), Some(true)] {
            for enabled in [false, true] {
                let handler = RequestHandler::new();
                let owner = session_name("cleanup-owner");
                new_session(&handler, &owner).await;
                let source = create_grouped_session(&handler, "cleanup-view", &owner).await;
                let destination = session_name("cleanup-destination");
                new_session(&handler, &destination).await;
                let pid = std::process::id();
                let mut attached_receivers = Vec::new();
                let mut control_receivers = Vec::new();
                for (client_pid, is_attached) in std::iter::once((pid, attached))
                    .chain(resident.map(|is_attached| (pid + 1, is_attached)))
                {
                    if is_attached {
                        let (tx, rx) = mpsc::unbounded_channel();
                        handler
                            .register_attach(client_pid, source.clone(), tx)
                            .await;
                        attached_receivers.push(rx);
                    } else {
                        control_receivers.push(
                            register_control_session(&handler, client_pid, source.clone()).await,
                        );
                    }
                }
                let window_id = {
                    let mut state = handler.state.lock().await;
                    state
                        .options
                        .set(
                            ScopeSelector::Session(source.clone()),
                            OptionName::DestroyUnattached,
                            if enabled { "on" } else { "off" }.into(),
                            SetOptionMode::Replace,
                        )
                        .unwrap();
                    state.sessions.session(&owner).unwrap().window().id()
                };
                let response = handler
                    .dispatch(
                        pid,
                        Request::SwitchClient(rmux_proto::SwitchClientRequest {
                            target: destination.clone(),
                        }),
                    )
                    .await
                    .response;
                assert!(
                    matches!(response, Response::SwitchClient(_)),
                    "{response:?}"
                );
                let state = handler.state.lock().await;
                let reaped = state.sessions.session(&source).is_none();
                let expected = enabled && resident.is_none();
                if reaped != expected {
                    failures.push(format!(
                        "attached={attached}, resident={resident:?}, enabled={enabled}: reaped={reaped}, expected={expected}"
                    ));
                }
                assert_eq!(
                    state.sessions.session(&owner).unwrap().window().id(),
                    window_id
                );
                assert!(state.sessions.session(&destination).is_some());
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
