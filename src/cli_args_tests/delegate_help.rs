use super::super::{parse, parse_with_runtime_command_groups, RuntimeCommandGroup};

#[test]
fn delegated_capabilities_help_aborts_queue_before_dispatch() {
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
    let groups = [RuntimeCommandGroup::Canonical(
        "set-option -g @r2 changed ; capabilities --json --help".to_owned(),
    )];
    let results = [
        parse(invocation),
        parse_with_runtime_command_groups(invocation, &groups),
    ];
    for result in results {
        // Err returns no Cli or command queue that could dispatch SetOption.
        let error = result.expect_err("help must abort the entire queue before dispatch");
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        assert_eq!(
            error.to_string(),
            "error: usage: rmux capabilities [--human|--json]\n"
        );
    }
}
