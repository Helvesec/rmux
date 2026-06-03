use super::*;

#[tokio::test]
async fn kill_window_renumbers_when_session_option_is_enabled() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;

    let set_renumber = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::RenumberWindows,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_renumber, Response::SetOption(_)));

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 1),
            kill_all_others: false,
        }))
        .await;

    assert_eq!(
        response,
        Response::KillWindow(rmux_proto::KillWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 0),
        })
    );

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&alpha)
        .expect("session should exist");
    assert_eq!(
        session.windows().keys().copied().collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(session.active_window_index(), 0);
}
