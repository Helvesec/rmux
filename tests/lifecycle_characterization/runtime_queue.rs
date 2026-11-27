use super::*;
use rmux_proto::{
    Response, RuntimeCommandExecutionResponse as Execution, RuntimeCommandExecutionV2Request,
    RuntimeCommandIntent,
};

fn execution(response: Response) -> Execution {
    match response {
        Response::RuntimeCommandExecution(execution) => *execution,
        other => panic!("expected runtime response, got {other:?}"),
    }
}

#[test]
fn l05_1_cli_runtime_v2_invalid_envelope() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("cli-v2-envelope")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 120"])?);
    let mut connection = rmux_client::connect(harness.socket_path())?;
    let offer = |intent| {
        RuntimeCommandExecutionV2Request::new(
        vec!["if-shell".into(), "-F".into(), "1".into(),
             "set-environment -g L05_ORDER first ; set-environment -g L05_ORDER second ; display-message -p '#{L05_ORDER}'".into()], None, intent)
    };
    for intent in [
        RuntimeCommandIntent::NewFloatingPane,
        RuntimeCommandIntent::MoveFloatingPane,
        RuntimeCommandIntent::SetPanePlacement,
    ] {
        let response = execution(connection.runtime_command_execution_v2(offer(intent))?);
        assert!(
            matches!(response, Execution::Unsupported(_)),
            "{response:?}"
        );
        let output = harness.run(&["show-environment", "-g", "L05_ORDER"])?;
        assert!(
            !output.status.success(),
            "invalid intent must not start its queue"
        );
    }
    let mut unknown = offer(RuntimeCommandIntent::GenericCli);
    unknown
        .handshake
        .required_capabilities
        .push("l05.unknown.operation".into());
    let response = execution(connection.runtime_command_execution_v2(unknown)?);
    assert!(
        matches!(response, Execution::Unsupported(_)),
        "{response:?}"
    );
    assert!(!harness
        .run(&["show-environment", "-g", "L05_ORDER"])?
        .status
        .success());
    // Generic top-level batches deliberately fall back; an admitted if-shell
    // owns its ordered nested queue and must complete without replay.
    let response = execution(
        connection.runtime_command_execution_v2(offer(RuntimeCommandIntent::GenericCli))?,
    );
    assert!(
        matches!(response, Execution::Complete { exit_status: 0, ref stdout, .. } if stdout == b"second\n"),
        "{response:?}"
    );
    assert_eq!(
        common::stdout(&harness.run(&["show-environment", "-g", "L05_ORDER"])?),
        "L05_ORDER=second\n"
    );
    let output = harness.run(&[
        "set-option",
        "-t",
        "alpha",
        "@ordered",
        "first",
        ";",
        "set-option",
        "-t",
        "alpha",
        "@ordered",
        "second",
    ])?;
    assert_success(&output);
    assert_eq!(
        common::stdout(&harness.run(&["show-options", "-v", "-t", "alpha", "@ordered"])?),
        "second\n"
    );
    Ok(())
}
