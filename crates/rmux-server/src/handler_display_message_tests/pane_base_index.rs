use super::*;
use rmux_core::formats::{DEFAULT_LIST_PANES_ALL_FORMAT, DEFAULT_LIST_PANES_SESSION_FORMAT};
use rmux_proto::{CommandOutput, DisplayMessageResponse, ListPanesRequest};

fn stdout_string(output: &CommandOutput) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is utf-8")
}

fn default_list_pane_labels(output: &CommandOutput) -> Vec<&str> {
    std::str::from_utf8(output.stdout())
        .expect("list-panes output is utf-8")
        .lines()
        .map(|line| {
            line.split_once(": [")
                .expect("default list-panes line has a geometry suffix")
                .0
        })
        .collect()
}

#[tokio::test]
async fn pane_index_formats_use_window_local_pane_base_index() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 6 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
                option: OptionName::PaneBaseIndex,
                value: "10".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let list = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha.clone(),
            target_window_index: Some(0),
            format: Some("#{pane_index}:#{pane-base-index}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;
    let Response::ListPanes(list) = list else {
        panic!("list-panes should succeed, got {list:?}");
    };
    assert_eq!(stdout_string(&list.output), "10:10\n11:10\n");

    for (format, expected) in [
        (None, vec!["10", "11"]),
        (
            Some(DEFAULT_LIST_PANES_SESSION_FORMAT.to_owned()),
            vec!["0.10", "0.11"],
        ),
        (
            Some(DEFAULT_LIST_PANES_ALL_FORMAT.to_owned()),
            vec!["alpha:0.10", "alpha:0.11"],
        ),
    ] {
        let list = handler
            .handle(Request::ListPanes(Box::new(ListPanesRequest {
                target: alpha.clone(),
                target_window_index: Some(0),
                format,
                filter: None,
                sort_order: None,
                reversed: false,
            })))
            .await;
        let Response::ListPanes(list) = list else {
            panic!("default list-panes should succeed, got {list:?}");
        };
        assert_eq!(default_list_pane_labels(&list.output), expected);
    }

    let display = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(PaneTarget::with_window(alpha.clone(), 0, 1))),
            message: Some("#{pane_index}:#P".to_owned()),
            print: true,
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(DisplayMessageResponse { output, .. }) = display else {
        panic!("display-message should succeed, got {display:?}");
    };
    let output = output.expect("print output exists");
    assert_eq!(stdout_string(&output), "11:11\n");
}

#[tokio::test]
async fn default_list_panes_uses_global_pane_base_index_without_window_override() {
    let handler = RequestHandler::new();
    let beta = session_name("beta");
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::PaneBaseIndex,
                value: "7".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: beta.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 6 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(beta.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));

    let list = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: beta.clone(),
            target_window_index: Some(0),
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;
    let Response::ListPanes(list) = list else {
        panic!("default list-panes should succeed, got {list:?}");
    };
    assert_eq!(default_list_pane_labels(&list.output), ["7", "8"]);

    let list = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: beta,
            target_window_index: Some(0),
            format: Some("#{pane_index}:#{pane-base-index}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;
    let Response::ListPanes(list) = list else {
        panic!("list-panes option format should succeed, got {list:?}");
    };
    assert_eq!(stdout_string(&list.output), "7:7\n8:7\n");
}

#[tokio::test]
async fn target_resolution_uses_visible_pane_base_index() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 6 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
                option: OptionName::PaneBaseIndex,
                value: "10".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let resolved = handler
        .handle(Request::ResolveTarget(rmux_proto::ResolveTargetRequest {
            target: Some("alpha:0.11".to_owned()),
            target_type: rmux_proto::ResolveTargetType::Pane,
            window_index: false,
            prefer_unattached: false,
        }))
        .await;
    let Response::ResolveTarget(resolved) = resolved else {
        panic!("visible pane target should resolve, got {resolved:?}");
    };
    assert_eq!(
        resolved.target,
        Target::Pane(PaneTarget::with_window(alpha, 0, 1))
    );
}
