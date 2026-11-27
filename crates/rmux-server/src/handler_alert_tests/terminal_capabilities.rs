use super::*;
use rmux_proto::ClientTerminalContext;

async fn terminal_report(handler: &RequestHandler, target: Option<&str>, jobs: bool) -> String {
    let response = handler
        .dispatch(
            23601,
            Request::ShowMessages(ShowMessagesRequest {
                jobs,
                terminals: true,
                target_client: target.map(str::to_owned),
            }),
        )
        .await
        .response;
    let Response::ShowMessages(response) = response else {
        panic!("expected terminal report: {response:?}");
    };
    String::from_utf8(response.output.stdout().to_vec()).expect("UTF-8 report")
}

fn capability<'a>(report: &'a str, name: &str) -> &'a str {
    let prefix = format!("{name}: ");
    let entries = report
        .lines()
        .filter_map(|line| line.trim_start().split_once(": ").map(|(_, entry)| entry))
        .filter_map(|entry| entry.strip_prefix(&prefix))
        .collect::<Vec<_>>();
    assert_eq!(
        entries.len(),
        1,
        "one {name} entry for selected client: {report}"
    );
    entries[0]
}

#[tokio::test]
async fn gh2610_show_messages_terminal_capabilities() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "terminal-capabilities").await;
    let mut receivers = Vec::new();
    for (pid, context) in [
        (
            23601,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        ),
        (23602, OuterTerminalContext::from_pairs(&[("TERM", "dumb")])),
        (23603, OuterTerminalContext::default()),
        (
            23604,
            OuterTerminalContext::from_pairs(&[("TERM", "dumb")]).with_client_terminal(
                &ClientTerminalContext {
                    terminal_features: vec!["clipboard".to_owned()],
                    ..ClientTerminalContext::default()
                },
            ),
        ),
    ] {
        let (tx, rx) = mpsc::unbounded_channel();
        receivers.push(rx);
        handler
            .register_attach_with_terminal_context(pid, session.clone(), tx, context)
            .await;
    }

    // Measured with tmux 3.7b on Linux: the actual template is escaped for
    // display, not expanded with a synthetic clipboard payload.
    let ms = r"(string) \033]52;%p1%s;%p2%s\a";
    for jobs in [false, true] {
        let report = terminal_report(&handler, Some("23601"), jobs).await;
        assert_eq!(capability(&report, "Ms"), ms);
        assert_eq!(capability(&report, "Enbp"), r"(string) \033[?2004h");
        assert!(
            !report.contains('\x1b'),
            "report must not execute terminal sequences"
        );
        assert!(
            !report.contains("client 23602"),
            "target must stay client-specific"
        );
    }
    assert_eq!(
        capability(&terminal_report(&handler, None, false).await, "Ms"),
        ms
    );
    for target in ["23602", "23603"] {
        assert_eq!(
            capability(&terminal_report(&handler, Some(target), false).await, "Ms"),
            "[missing]"
        );
    }
    assert_eq!(
        capability(&terminal_report(&handler, Some("23604"), false).await, "Ms"),
        ms
    );

    // Reporting follows capability availability, not the automatic-copy
    // policy: explicit load-buffer -w remains available with set-clipboard off.
    for policy in ["on", "external", "off"] {
        set_option(
            &handler,
            ScopeSelector::Global,
            OptionName::SetClipboard,
            policy,
        )
        .await;
        assert_eq!(
            capability(&terminal_report(&handler, Some("23601"), false).await, "Ms"),
            ms
        );
    }
    for (overrides, expected) in [
        ("xterm*:Ms@", "[missing]"),
        ("xterm*:Ms=", "(string) "),
        (r"xterm*:Ms=\E]52;c;%p2%s\a", r"(string) \033]52;c;%p2%s\a"),
    ] {
        set_option(
            &handler,
            ScopeSelector::Global,
            OptionName::TerminalOverrides,
            overrides,
        )
        .await;
        assert_eq!(
            capability(&terminal_report(&handler, Some("23601"), false).await, "Ms"),
            expected
        );
        assert_eq!(
            capability(&terminal_report(&handler, Some("23602"), false).await, "Ms"),
            "[missing]"
        );
    }

    let target_name = handler.active_attach.lock().await.by_pid[&23601]
        .client_name
        .clone();
    let report = terminal_report(&handler, Some(&target_name), false).await;
    assert_eq!(capability(&report, "Ms"), r"(string) \033]52;c;%p2%s\a");
}

#[tokio::test]
async fn named_client_reports_tmux_clipboard_template_after_override_removal() {
    let handler = RequestHandler::new();
    let session = create_quiet_session(&handler, "ms-restore").await;
    let (tx, _rx) = mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(
            23605,
            session,
            tx,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        )
        .await;
    let name = handler.active_attach.lock().await.by_pid[&23605]
        .client_name
        .clone();
    for (overrides, expected) in [
        ("xterm*:Ms@", "[missing]"),
        ("", r"(string) \033]52;%p1%s;%p2%s\a"),
    ] {
        set_option(
            &handler,
            ScopeSelector::Global,
            OptionName::TerminalOverrides,
            overrides,
        )
        .await;
        assert_eq!(
            capability(&terminal_report(&handler, Some(&name), false).await, "Ms"),
            expected
        );
    }
}
