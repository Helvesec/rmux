use super::{OsString, TinyInvocation};

#[test]
fn delegated_capabilities_help_queue_falls_back_and_aborts_before_dispatch() {
    for command in [
        "capabilities",
        "claude",
        "doctor",
        "setup",
        "new-pane",
        "set-pane-placement",
    ] {
        let local: Vec<_> = ["rmux", command, "--help"]
            .iter()
            .map(OsString::from)
            .collect();
        assert!(matches!(
            TinyInvocation::parse(&local),
            TinyInvocation::Fallback
        ));
    }
    let invocation = [
        "rmux",
        "set-option",
        "-g",
        "@r2",
        "changed",
        ";",
        "capabilities",
        "--json",
        "--help",
    ];
    let arguments: Vec<_> = invocation.iter().map(OsString::from).collect();
    assert!(matches!(
        TinyInvocation::parse(&arguments),
        TinyInvocation::Fallback
    ));
    let error =
        crate::cli_args::parse(invocation).expect_err("full helper must abort the queued mutation");
    assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    assert_eq!(
        error.to_string(),
        "error: usage: rmux capabilities [--human|--json]\n"
    );
}
