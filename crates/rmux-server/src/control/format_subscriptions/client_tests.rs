use super::*;
use crate::client_names::control_client_name;
use crate::control::format_subscriptions::MAX_CONTROL_FORMAT_SUBSCRIPTIONS;

#[tokio::test(start_paused = true)]
async fn refresh_client_format_subscription_expands_coherent_client_loop_snapshot() {
    // A ready response must finish before the next client snapshot is requested.
    let handler = Arc::new(RequestHandler::new());
    let alpha = SessionName::new("loop-alpha").expect("valid session name");
    let beta = SessionName::new("loop-beta").expect("valid session name");
    for session_name in [&alpha, &beta] {
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session_name.clone(),
                    detached: true,
                    size: None,
                    environment: None,
                }))
                .await,
        );
    }

    let mut alpha_control = OpenControlTestClient::spawn(Arc::clone(&handler), 42_451).await;
    alpha_control
        .command(format!("attach-session -t {alpha}"))
        .await;
    let mut beta_control = OpenControlTestClient::spawn(Arc::clone(&handler), 42_452).await;
    beta_control
        .command(format!("attach-session -t {beta}"))
        .await;

    let expected_value = format!(
        "loop-alpha@loop-alpha:{};loop-beta@loop-alpha:{};",
        control_client_name(42_451),
        control_client_name(42_452)
    );
    alpha_control
        .command("list-clients -F '#{L:#{client_session}@#{session_name}:#{client_name};}'")
        .await;
    assert!(
        alpha_control.rendered().contains(&expected_value),
        "list-clients must expand L from its client snapshot: {:?}",
        alpha_control.rendered()
    );
    alpha_control
        .command("display-message -p '#{L:#{client_session}@#{session_name}:#{client_name};}'")
        .await;
    assert!(
        alpha_control.rendered().contains(&expected_value),
        "display-message must expand L from its client snapshot: {:?}",
        alpha_control.rendered()
    );
    let nested_expected = format!(
        "{alpha_name}|{alpha_name}|{beta_name}|{beta_name}|",
        alpha_name = control_client_name(42_451),
        beta_name = control_client_name(42_452),
    );
    alpha_control
        .command("display-message -p '#{L:#{S:#{client_name}|}}'")
        .await;
    assert!(
        alpha_control.rendered().contains(&nested_expected),
        "nested S loop must retain each L client's bindings: {:?}",
        alpha_control.rendered()
    );
    alpha_control
        .command("if-shell -F '#{L:#{client_name}}' 'display-message -p SCRIPTING-L-OK'")
        .await;
    assert!(
        alpha_control.rendered().contains("SCRIPTING-L-OK"),
        "queued if-shell must expand L from its client snapshot: {:?}",
        alpha_control.rendered()
    );
    alpha_control
        .command(
            "refresh-client -B 'clients:%0:#{L:#{client_session}@#{session_name}:#{client_name};}'",
        )
        .await;
    alpha_control
        .advance_and_drain(Duration::from_secs(1))
        .await;

    let lines = subscription_lines_named(&alpha_control.output, "clients");
    assert_eq!(lines.len(), 1, "unexpected subscription output: {lines:?}");
    assert!(
        lines[0].ends_with(&format!(" : {expected_value}")),
        "client loop must retain the outer session while changing client bindings: {lines:?}"
    );

    let _ = alpha_control.close().await;
    let _ = beta_control.close().await;
}

#[tokio::test(start_paused = true)]
async fn format_subscriptions_with_same_name_belong_to_each_registration() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("wildcard-clients").unwrap();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await,
    );
    let mut panes = OpenControlTestClient::spawn(Arc::clone(&handler), 42_451).await;
    panes
        .command(format!("attach-session -t {session_name}"))
        .await;
    let identity = "#{client_name}|#{client_pid}|#{client_control_mode}|#{session_name}";
    panes
        .command(format!(
            "refresh-client -B 'same:%*:PANES {identity}' -B 'who::{identity}'"
        ))
        .await;
    let mut windows = OpenControlTestClient::spawn(Arc::clone(&handler), 42_452).await;
    windows
        .command(format!("attach-session -t {session_name}"))
        .await;
    windows
        .command(format!(
            "refresh-client -B 'same:@*:WINDOWS {identity}' -B 'who::{identity}'"
        ))
        .await;

    panes.advance_and_drain(Duration::from_secs(1)).await;
    windows.yield_and_drain_available().await;

    let panes = panes.close().await;
    let windows = windows.close().await;
    for (rendered, pid, target) in [
        (panes, 42_451, "@0 0 %0 : PANES"),
        (windows, 42_452, "@0 0 - : WINDOWS"),
    ] {
        let transcript = parse_strict_control_transcript(&rendered);
        assert_eq!(transcript.frames.len(), 3, "{rendered}");
        assert!(
            transcript
                .frames
                .iter()
                .all(|frame| frame.terminal == TestGuardTerminal::End),
            "{rendered}"
        );
        let changes: Vec<_> = transcript
            .asynchronous_notifications
            .iter()
            .filter(|line| line.starts_with("%subscription-changed "))
            .map(String::as_str)
            .collect();
        let who = format!("{}|{pid}|1|{session_name}", control_client_name(pid));
        assert_eq!(changes.len(), 2, "{rendered}");
        for expected in [
            format!("%subscription-changed same $0 {target} {who}"),
            format!("%subscription-changed who $0 - - - : {who}"),
        ] {
            assert!(changes.contains(&expected.as_str()), "{rendered}");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn format_subscriptions_by_id_track_each_window_link() {
    let handler = Arc::new(RequestHandler::new());
    let mut control = OpenControlTestClient::spawn(handler, 42_455).await;
    control.command("new-session -s links").await;
    control.command("new-window -d -t links:1").await;
    control
        .command("link-window -d -s links:0 -t links:2")
        .await;
    control.command("new-session -d -s other").await;
    control
        .command("link-window -d -s links:0 -t other:2")
        .await;
    control
        .command("link-window -d -s links:0 -t other:3")
        .await;
    control
        .command("refresh-client -B 'win:@0:#{window_index}' -B 'pane:%0:constant'")
        .await;
    control.advance_and_drain(Duration::from_secs(1)).await;
    control.command("switch-client -t other").await;
    control.advance_and_drain(Duration::from_secs(1)).await;
    control.command("switch-client -t links").await;
    control.advance_and_drain(Duration::from_secs(1)).await;
    let rendered = control.close().await;
    assert_eq!(
        subscription_lines_named(rendered.as_bytes(), "win"),
        [
            "%subscription-changed win $0 @0 0 - : 0",
            "%subscription-changed win $0 @0 2 - : 2",
            "%subscription-changed win $1 @0 3 - : 3",
        ],
        "{rendered}"
    );
    assert_eq!(
        subscription_lines_named(rendered.as_bytes(), "pane"),
        [
            "%subscription-changed pane $0 @0 0 %0 : constant",
            "%subscription-changed pane $0 @0 2 %0 : constant",
            "%subscription-changed pane $1 @0 3 %0 : constant",
        ],
        "{rendered}"
    );
}

#[tokio::test(start_paused = true)]
async fn format_subscription_tick_reconciles_ready_removal_before_evaluating() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("format-remove-tick-race").unwrap();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await,
    );
    let mut control = OpenControlTestClient::spawn(Arc::clone(&handler), 42_454).await;
    control
        .command(format!("attach-session -t {session_name}"))
        .await;
    control
        .command("refresh-client -B 'title:%0:#{pane_title}'")
        .await;
    control.command("select-pane -T FIRST").await;
    control.yield_and_drain_available().await;
    assert_eq!(
        subscription_lines_named(&control.output, "title"),
        Vec::<String>::new(),
        "registration must arm cadence without immediate output: {:?}",
        control.rendered()
    );

    control.advance_and_drain(Duration::from_secs(1)).await;
    let expected_initial = subscription_lines_named(&control.output, "title");
    assert_eq!(expected_initial.len(), 1, "{:?}", control.rendered());
    assert!(
        expected_initial[0].ends_with(": FIRST"),
        "first cadence must publish the current title: {:?}",
        control.rendered()
    );

    run_waiting_command_after_due_format_tick(
        &handler,
        &mut control,
        "cf02-format-remove-race",
        "select-pane -T LATE ; refresh-client -B title",
    )
    .await;
    assert_eq!(
        subscription_lines_named(&control.output, "title"),
        expected_initial,
        "an acknowledged removal event already queued for the transport must beat the overdue tick: {:?}",
        control.rendered()
    );

    let rendered = control.close().await;
    assert!(
        !rendered.contains("LATE"),
        "removed subscription must not publish a stale post-ack title: {rendered:?}"
    );
    assert_eq!(
        subscription_lines_named(rendered.as_bytes(), "title"),
        expected_initial,
        "removed subscription must stay silent after close drains the transport: {rendered:?}"
    );
}

#[tokio::test]
async fn format_subscriptions_registry_overflow_is_a_bounded_command_error() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("format-registry-bound").unwrap();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await,
    );

    let mut commands = Vec::with_capacity(MAX_CONTROL_FORMAT_SUBSCRIPTIONS + 3);
    commands.push(format!("attach-session -t {session_name}"));
    for index in 0..=MAX_CONTROL_FORMAT_SUBSCRIPTIONS {
        commands.push(format!("refresh-client -B 'sub{index}::#{{pane_id}}'"));
    }
    commands.push("display-message -p after-overflow".to_owned());
    let input = format!("{}\n\n", commands.join("\n")).into_bytes();
    // The overflow ends the command-line list, which an error stops as in
    // tmux; the command after it arrives on the attached client's stdin.
    let initial_command_count = commands.len() - 1;
    let rendered = run_registered_initial_control_batch(
        Arc::clone(&handler),
        42_453,
        input,
        initial_command_count,
    )
    .await;
    let transcript = parse_strict_control_transcript(&rendered);

    assert_eq!(
        transcript.frames.len(),
        commands.len(),
        "registry overflow must not corrupt later control framing: {rendered}"
    );
    let overflow_frame = &transcript.frames[MAX_CONTROL_FORMAT_SUBSCRIPTIONS + 1];
    assert_eq!(overflow_frame.terminal, TestGuardTerminal::Error);
    let overflow_message = format!(
        "control format subscription registry exceeds {MAX_CONTROL_FORMAT_SUBSCRIPTIONS} entries"
    );
    assert!(overflow_frame
        .payload
        .iter()
        .any(|line| line == &overflow_message));
    assert!(
        transcript
            .frames
            .last()
            .is_some_and(|frame| frame.payload == ["after-overflow"]),
        "overflow must be reported as the failing command, not leak into the next frame: {rendered}"
    );
    assert_eq!(
        transcript
            .asynchronous_notifications
            .last()
            .map(String::as_str),
        Some("%exit"),
        "the stream must still terminate normally after the bounded command error: {rendered}"
    );
}

async fn run_waiting_command_after_due_format_tick(
    handler: &RequestHandler,
    control: &mut OpenControlTestClient,
    channel: &str,
    tail: &str,
) {
    let expected_frames = closed_control_frame_count(&control.output) + 1;
    control
        .client_stream
        .write_all(format!("wait-for {channel} ; {tail}\n").as_bytes())
        .await
        .expect("blocked control command writes");
    wait_for_signal_waiter_without_advancing_time(handler, channel).await;
    control.advance_and_drain(Duration::from_secs(1)).await;

    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: channel.to_owned(),
            mode: WaitForMode::Signal,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(WaitForResponse)));
    control
        .read_until_closed_frame_count(expected_frames, tail)
        .await;
    control.yield_and_drain_available().await;
}

async fn wait_for_signal_waiter_without_advancing_time(handler: &RequestHandler, channel: &str) {
    for _ in 0..2048 {
        if handler.wait_for_counts(channel).0 == 1 {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("wait-for waiter for {channel} did not register without time advancement");
}
