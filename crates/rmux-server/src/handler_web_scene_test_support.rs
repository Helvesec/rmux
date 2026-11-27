use super::*;

const READY: &str = "SCENE-READY";

/// Keep a second writer from changing an injected scene. The child's first
/// clear is deliberately held until Enter, making late startup reproducible.
/// After announcing readiness it only consumes input and never draws again.
pub(super) async fn pin_ready_producer(handler: &RequestHandler) {
    #[cfg(unix)]
    let command = format!("read -r ready; printf '\\033[2J\\033[H{READY}'; cat >/dev/null");
    #[cfg(windows)]
    let command = crate::test_shell::powershell_encoded_command(&format!(
        "$null=[Console]::In.ReadLine(); \
         [Console]::Out.Write([string][char]27 + '[2J' + [char]27 + '[H{READY}'); \
         $inputStream=[Console]::OpenStandardInput(); \
         $inputStream.CopyTo([System.IO.Stream]::Null)"
    ));
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultCommand,
            value: command,
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

/// Startup completion drains pending input; only the child's output marker
/// proves that its initial screen clear has reached the pane transcript.
pub(super) async fn settle_ready_producers(handler: &RequestHandler, session: &SessionName) {
    for index in 0..2 {
        let target = PaneTarget::with_window(session.clone(), 0, index);
        let (sender, transcript) = {
            let state = handler.state.lock().await;
            (
                state
                    .pane_output_for_target(session, 0, index)
                    .expect("fixture output"),
                state
                    .transcript_handle(&target)
                    .expect("fixture transcript"),
            )
        };
        let _demand = sender.acquire_read_demand();
        let mut output = sender.subscribe();
        let response = handler
            .handle(Request::SendKeys(rmux_proto::SendKeysRequest {
                target,
                keys: vec!["Enter".to_owned()],
            }))
            .await;
        assert!(matches!(response, Response::SendKeys(_)), "{response:?}");
        timeout(Duration::from_secs(15), async {
            loop {
                let bytes = transcript
                    .lock()
                    .screen()
                    .capture_transcript(Default::default(), Default::default());
                if bytes
                    .windows(READY.len())
                    .any(|value| value == READY.as_bytes())
                {
                    break;
                }
                let _ = output.recv_observed().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("pane {index} never published its readiness marker"));
    }
}
