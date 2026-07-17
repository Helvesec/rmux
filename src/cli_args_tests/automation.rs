use super::*;

#[test]
fn wait_pane_accepts_one_full_word_condition() {
    let mut queue = parse_args(&[
        "wait-pane",
        "-t",
        "%1",
        "--visible-text",
        "Ready",
        "--timeout",
        "30s",
    ])
    .expect("wait-pane parses")
    .into_command_queue();
    let super::super::Command::WaitPane(args) = queue.remove(0) else {
        panic!("expected wait-pane command");
    };
    assert_eq!(target_text(&args.target), "%1");
    assert_eq!(args.visible_text.as_deref(), Some("Ready"));
    assert_eq!(args.timeout.expect("timeout").as_secs(), 30);
}

#[test]
fn rmux_extension_commands_are_exact_only_cli_commands() {
    let mut queue = parse_args(&["wait-pane", "--pane-exit"])
        .expect("exact RMUX extension command parses")
        .into_command_queue();
    let super::super::Command::WaitPane(_) = queue.remove(0) else {
        panic!("expected wait-pane command");
    };

    let prefix_error =
        parse_args(&["wait-p", "--pane-exit"]).expect_err("RMUX extensions have no prefix aliases");
    assert_eq!(
        prefix_error.kind(),
        clap::error::ErrorKind::InvalidSubcommand
    );
}

#[test]
fn with_session_accepts_options_after_session_before_child_separator() {
    let mut queue = parse_args(&[
        "with-session",
        "owned",
        "--kill-on-owner-exit",
        "--ttl",
        "45s",
        "--",
        "sh",
        "-c",
        "true",
    ])
    .expect("with-session options after the session name must parse")
    .into_command_queue();
    let super::super::Command::WithSession(args) = queue.remove(0) else {
        panic!("expected with-session command");
    };
    assert_eq!(args.session_name.as_str(), "owned");
    assert!(args.kill_on_owner_exit);
    assert_eq!(args.ttl.as_secs(), 45);
    assert_eq!(args.command, ["sh", "-c", "true"]);
}

#[test]
fn with_session_keeps_supporting_options_before_session() {
    let mut queue = parse_args(&[
        "with-session",
        "--ttl",
        "2m",
        "--kill-on-owner-exit",
        "owned",
        "--",
        "sh",
        "-c",
        "true",
    ])
    .expect("with-session options before the session name must parse")
    .into_command_queue();
    let super::super::Command::WithSession(args) = queue.remove(0) else {
        panic!("expected with-session command");
    };
    assert_eq!(args.session_name.as_str(), "owned");
    assert!(args.kill_on_owner_exit);
    assert_eq!(args.ttl.as_secs(), 120);
    assert_eq!(args.command, ["sh", "-c", "true"]);
}

#[test]
fn with_session_separator_scopes_child_flags_and_invalid_outer_options_still_fail() {
    let mut queue = parse_args(&[
        "with-session",
        "owned",
        "--",
        "sh",
        "--kill-on-owner-exit",
        "--ttl",
        "forever",
    ])
    .expect("child flags after the separator must stay in the child command")
    .into_command_queue();
    let super::super::Command::WithSession(args) = queue.remove(0) else {
        panic!("expected with-session command");
    };
    assert!(!args.kill_on_owner_exit);
    assert_eq!(args.ttl.as_secs(), 30);
    assert_eq!(
        args.command,
        ["sh", "--kill-on-owner-exit", "--ttl", "forever"]
    );

    let unknown = parse_args(&["with-session", "--unknown", "owned", "--", "sh"])
        .expect_err("unknown outer options must remain invalid");
    assert_eq!(unknown.kind(), clap::error::ErrorKind::UnknownArgument);

    let invalid_ttl = parse_args(&["with-session", "owned", "--ttl", "forever", "--", "sh"])
        .expect_err("invalid with-session options after the session name must fail");
    assert_eq!(invalid_ttl.kind(), clap::error::ErrorKind::ValueValidation);

    let missing_child = parse_args(&["with-session", "owned", "--kill-on-owner-exit", "--"])
        .expect_err("with-session still requires a child command");
    assert_eq!(
        missing_child.kind(),
        clap::error::ErrorKind::ValueValidation
    );
}

#[test]
fn wait_pane_rejects_multiple_conditions_and_short_extension_flags() {
    let error = parse_args(&["wait-pane", "--text", "Done", "--quiet"])
        .expect_err("multiple wait conditions should fail");
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);

    let short_error =
        parse_args(&["wait-pane", "-q"]).expect_err("RMUX-only quiet has no short flag");
    assert_eq!(short_error.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn send_keys_wait_requires_payload_separator() {
    let error = parse_args(&[
        "send-keys",
        "-t",
        "%1",
        "--wait",
        "quiet",
        "make test",
        "Enter",
    ])
    .expect_err("send-keys wait requires -- before payload");
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn send_keys_wait_keeps_payload_after_separator() {
    let mut queue = parse_args(&[
        "send-keys",
        "-t",
        "%1",
        "--wait-next-text",
        "__DONE__",
        "--timeout",
        "2m",
        "--",
        "make test",
        "Enter",
    ])
    .expect("send-keys wait parses")
    .into_command_queue();
    let super::super::Command::SendKeys(args) = queue.remove(0) else {
        panic!("expected send-keys command");
    };
    assert_eq!(target_text(&args.target), "%1");
    assert_eq!(args.wait_next_text.as_deref(), Some("__DONE__"));
    assert_eq!(args.timeout.expect("timeout").as_secs(), 120);
    assert_eq!(args.keys, ["make test", "Enter"]);
}

#[test]
fn send_keys_timeout_requires_wait_condition() {
    let error = parse_args(&["send-keys", "-t", "%1", "--timeout", "2s", "--", "Enter"])
        .expect_err("send-keys timeout without wait should fail");
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn send_keys_wait_accepts_normal_send_keys_options() {
    let mut queue = parse_args(&[
        "send-keys",
        "-t",
        "%1",
        "-F",
        "--wait-next-text",
        "DONE",
        "--",
        "#{pane_id}",
        "Enter",
    ])
    .expect("send-keys wait should keep normal send-keys options")
    .into_command_queue();
    let super::super::Command::SendKeys(args) = queue.remove(0) else {
        panic!("expected send-keys command");
    };
    assert!(args.expand_formats);
    assert_eq!(args.wait_next_text.as_deref(), Some("DONE"));
    assert_eq!(args.keys, ["#{pane_id}", "Enter"]);
}

#[test]
fn collect_pane_output_requires_bounded_until_exit_collection() {
    let missing_until = parse_args(&["collect-pane-output", "-t", "%1", "--max-bytes", "1024"])
        .expect_err("until-pane-exit is required");
    assert_eq!(
        missing_until.kind(),
        clap::error::ErrorKind::ValueValidation
    );

    let zero_cap = parse_args(&[
        "collect-pane-output",
        "-t",
        "%1",
        "--until-pane-exit",
        "--max-bytes",
        "0",
    ])
    .expect_err("max bytes must be positive");
    assert_eq!(zero_cap.kind(), clap::error::ErrorKind::ValueValidation);
}
