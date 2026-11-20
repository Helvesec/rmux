use super::*;

#[test]
fn nonterminal_source_file_keeps_following_commands_and_output() -> Result<(), Box<dyn Error>> {
    check_source_file_queue(false)
}

#[test]
fn aliased_nonterminal_source_file_keeps_following_commands_and_output(
) -> Result<(), Box<dyn Error>> {
    check_source_file_queue(true)
}

fn check_source_file_queue(alias: bool) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-queue")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-option",
        "-s",
        "command-alias[20]",
        "sourcequeue=source-file",
    ])?);
    let command = if alias { "sourcequeue" } else { "source-file" };
    let config = harness.tmpdir().join("queue.conf");
    let payload = b"\0\xff\x1b\n\r";

    for piped in [false, true] {
        for failing in [false, true] {
            fs::write(
                &config,
                format!(
                    "display-message -p BEFORE\n{}display-message -p AFTER\n",
                    if failing {
                        "kill-session -t definitely-missing-session\n"
                    } else {
                        ""
                    },
                ),
            )?;
            let buffer_name = format!("after-{piped}-{failing}");
            let mut child = harness
                .base_command()
                .args([
                    command,
                    config.to_str().expect("utf-8 config path"),
                    ";",
                    "set-buffer",
                    "-b",
                    &buffer_name,
                    "yes",
                ])
                .stdin(if piped { Stdio::piped() } else { Stdio::null() })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            if piped {
                child
                    .stdin
                    .take()
                    .expect("piped stdin")
                    .write_all(payload)?;
            }
            let output = child.wait_with_output()?;
            assert_eq!(output.status.code(), Some(i32::from(failing)));
            assert_eq!(stdout(&output), "BEFORE\nAFTER\n");
            assert_eq!(
                stderr(&output),
                if failing {
                    "can't find session: definitely-missing-session\n"
                } else {
                    ""
                },
            );
            let buffer = harness.run(&["show-buffer", "-b", &buffer_name])?;
            assert_eq!(buffer.status.code(), Some(0));
            assert!(stderr(&buffer).is_empty());
            assert_eq!(stdout(&buffer), "yes");
        }
    }
    Ok(())
}

#[test]
fn nonterminal_source_file_selection_and_neighbors() -> Result<(), Box<dyn Error>> {
    check_source_file_neighbors(false)
}

#[test]
fn aliased_nonterminal_source_file_selection_and_neighbors() -> Result<(), Box<dyn Error>> {
    check_source_file_neighbors(true)
}

fn check_source_file_neighbors(alias: bool) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-neighbors")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-option",
        "-s",
        "command-alias[20]",
        "sourcequeue=source-file",
    ])?);
    for (name, body) in [
        (
            "ok.conf",
            "display-message -p BEFORE\ndisplay-message -p AFTER\n",
        ),
        ("empty.conf", ""),
        (
            "runtime.conf",
            "kill-session -t definitely-missing-session\n",
        ),
        (
            "nested.conf",
            "display-message -p BEFORE\nsource-file missing.conf\ndisplay-message -p AFTER\n",
        ),
        ("unknown.conf", "not-a-command-ever\n"),
        ("syntax.conf", "%word\n"),
    ] {
        fs::write(harness.tmpdir().join(name), body)?;
    }
    fs::create_dir(harness.tmpdir().join("directory.conf"))?;
    let command = if alias { "sourcequeue" } else { "source-file" };
    // Expected queue decisions measured with tmux 3.7b. In particular a matched
    // empty/unreadable file differs from an entirely unmatched selection.
    for piped in [false, true] {
        for (name, args, status, expected_stdout, diagnostic, continues) in [
            (
                "missing",
                vec!["missing.conf"],
                1,
                "",
                "No such file or directory",
                false,
            ),
            (
                "glob",
                vec!["missing-*.conf"],
                1,
                "",
                "No such file or directory",
                false,
            ),
            ("quiet", vec!["-q", "missing.conf"], 0, "", "", true),
            ("quiet-glob", vec!["-q", "missing-*.conf"], 0, "", "", true),
            ("empty", vec!["empty.conf"], 0, "", "", true),
            (
                "runtime",
                vec!["runtime.conf"],
                1,
                "",
                "can't find session: definitely-missing-session",
                true,
            ),
            (
                "nested",
                vec!["nested.conf"],
                1,
                "BEFORE\nAFTER\n",
                "No such file or directory",
                true,
            ),
            (
                "missing-first",
                vec!["missing.conf", "ok.conf"],
                1,
                "BEFORE\nAFTER\n",
                "No such file or directory",
                true,
            ),
            (
                "missing-last",
                vec!["ok.conf", "missing.conf"],
                1,
                "BEFORE\nAFTER\n",
                "No such file or directory",
                true,
            ),
            (
                "missing-empty",
                vec!["missing.conf", "empty.conf"],
                1,
                "",
                "No such file or directory",
                true,
            ),
            (
                "read-error",
                vec!["directory.conf"],
                1,
                "",
                "Input/output error",
                true,
            ),
            ("parse-only", vec!["-n", "runtime.conf"], 0, "", "", true),
        ] {
            let buffer_name = format!("{name}-{piped}");
            let mut argv = vec![command];
            argv.extend(args);
            argv.extend([";", "set-buffer", "-b", &buffer_name, "yes"]);
            let output =
                run_source_queue(&harness, &argv, piped.then_some(&b"\0\xff\x1b\n\r"[..]))?;
            assert_eq!(
                output.status.code(),
                Some(status),
                "{command}/{name}/{piped}: {output:?}"
            );
            assert_eq!(stdout(&output), expected_stdout, "{name}/{piped}");
            if diagnostic.is_empty() {
                assert!(stderr(&output).is_empty(), "{output:?}");
            } else {
                assert!(stderr(&output).contains(diagnostic), "{output:?}");
                assert_eq!(
                    stderr(&output).lines().count(),
                    1,
                    "diagnostic duplicated: {output:?}"
                );
            }
            check_following_buffer(&harness, &buffer_name, continues)?;
        }
        for (file, diagnostic) in [
            ("unknown.conf", "unknown command"),
            ("syntax.conf", "syntax error"),
        ] {
            let buffer = format!("{file}-{piped}");
            let output = run_source_queue(
                &harness,
                &[command, file, ";", "set-buffer", "-b", &buffer, "yes"],
                piped.then_some(&b"\0\xff"[..]),
            )?;
            assert_eq!(output.status.code(), Some(1));
            assert!(stdout(&output).contains(diagnostic), "{output:?}");
            assert!(stderr(&output).is_empty(), "{output:?}");
            check_following_buffer(&harness, &buffer, true)?;
        }
        let buffer = format!("verbose-{piped}");
        let output = run_source_queue(
            &harness,
            &[
                command,
                "-v",
                "ok.conf",
                ";",
                "set-buffer",
                "-b",
                &buffer,
                "yes",
            ],
            piped.then_some(&b"\0\xff"[..]),
        )?;
        assert_eq!(output.status.code(), Some(0));
        assert!(stderr(&output).is_empty());
        assert!(stdout(&output).contains("ok.conf:1: display-message -p BEFORE"));
        assert!(stdout(&output).ends_with("BEFORE\nAFTER\n"));
        check_following_buffer(&harness, &buffer, true)?;
    }
    let output = run_source_queue(
        &harness,
        &[command, "-", ";", "set-buffer", "-b", "stdin-code", "yes"],
        Some(b"display-message -p STDIN\nkill-session -t definitely-missing-session\n"),
    )?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout(&output), "STDIN\n");
    assert_eq!(
        stderr(&output),
        "can't find session: definitely-missing-session\n"
    );
    check_following_buffer(&harness, "stdin-code", true)?;
    Ok(())
}

fn run_source_queue(
    harness: &CliHarness,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<Output, Box<dyn Error>> {
    let mut child = harness
        .base_command()
        .current_dir(harness.tmpdir())
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(input) = input {
        child.stdin.take().expect("piped stdin").write_all(input)?;
    }
    Ok(child.wait_with_output()?)
}

fn check_following_buffer(
    harness: &CliHarness,
    name: &str,
    expected: bool,
) -> Result<(), Box<dyn Error>> {
    let buffer = harness.run(&["show-buffer", "-b", name])?;
    assert_eq!(
        buffer.status.code(),
        Some(i32::from(!expected)),
        "{name}: {buffer:?}"
    );
    if expected {
        assert!(stderr(&buffer).is_empty(), "{buffer:?}");
        assert_eq!(stdout(&buffer), "yes");
    } else {
        assert!(stdout(&buffer).is_empty(), "{buffer:?}");
        assert!(stderr(&buffer).contains("no buffer"), "{buffer:?}");
    }
    Ok(())
}

#[test]
fn nonterminal_source_file_budget_keeps_queue_and_rejects_group() -> Result<(), Box<dyn Error>> {
    check_source_file_budget(false)
}

#[test]
fn aliased_nonterminal_source_file_budget_keeps_queue_and_rejects_group(
) -> Result<(), Box<dyn Error>> {
    check_source_file_budget(true)
}

fn check_source_file_budget(alias: bool) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-budget-queue")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-option",
        "-s",
        "command-alias[20]",
        "sourcequeue=source-file",
    ])?);
    let padding = format!("#{}\n", "a".repeat(1022)).repeat(11 * 1024);
    for letter in ["a", "b", "c"] {
        fs::write(
            harness.tmpdir().join(format!("large-{letter}.conf")),
            format!("set-buffer -b selected-{letter} yes\n{padding}"),
        )?;
    }
    fs::write(
        harness.tmpdir().join("tail.conf"),
        "set-buffer -b tail yes\n",
    )?;
    for (name, selection) in [
        ("glob", "large-*.conf"),
        ("paths", "large-a.conf large-b.conf large-c.conf"),
    ] {
        fs::write(
            harness.tmpdir().join(format!("nested-{name}.conf")),
            format!("source-file {selection} tail.conf\ndisplay-message -p NESTED-AFTER\n"),
        )?;
    }
    let command = if alias { "sourcequeue" } else { "source-file" };
    // The budget is RMUX-specific. tmux 3.7b supplies only the continuation
    // oracle; RMUX must still return 1 and reject the over-budget group.
    for piped in [false, true] {
        for (name, args, accepted_files, expected_stdout) in [
            ("glob", vec!["large-*.conf", "tail.conf"], 0, ""),
            (
                "paths",
                vec!["large-a.conf", "large-b.conf", "large-c.conf", "tail.conf"],
                2,
                "",
            ),
            ("quiet-glob", vec!["-q", "large-*.conf", "tail.conf"], 0, ""),
            ("parse-glob", vec!["-n", "large-*.conf", "tail.conf"], 0, ""),
            (
                "parse-paths",
                vec![
                    "-n",
                    "large-a.conf",
                    "large-b.conf",
                    "large-c.conf",
                    "tail.conf",
                ],
                0,
                "",
            ),
            ("nested-glob", vec!["nested-glob.conf"], 0, "NESTED-AFTER\n"),
            (
                "nested-paths",
                vec!["nested-paths.conf"],
                2,
                "NESTED-AFTER\n",
            ),
            (
                "path-then-glob",
                vec!["large-a.conf", "large-[bc].conf", "tail.conf"],
                1,
                "",
            ),
        ] {
            // A unique queue marker and cleared source markers prevent a prior
            // successful case from hiding a failed continuation or rejection.
            for buffer in ["selected-a", "selected-b", "selected-c", "tail"] {
                let _ = harness.run(&["delete-buffer", "-b", buffer])?;
            }
            let buffer = format!("after-{name}-{piped}");
            let mut argv = vec![command];
            argv.extend(args);
            argv.extend([";", "set-buffer", "-b", &buffer, "yes"]);
            let output = run_source_queue(&harness, &argv, piped.then_some(&b"\0\xff"[..]))?;
            assert_eq!(
                output.status.code(),
                Some(1),
                "{command}/{name}/{piped}: {output:?}"
            );
            assert_eq!(stdout(&output), expected_stdout, "{name}: {output:?}");
            assert!(stderr(&output).contains("aggregate bytes"), "{output:?}");
            assert_eq!(
                stderr(&output).lines().count(),
                1,
                "duplicated diagnostic: {output:?}"
            );
            check_following_buffer(&harness, &buffer, true)?;
            check_following_buffer(&harness, "selected-a", accepted_files >= 1)?;
            check_following_buffer(&harness, "selected-b", accepted_files >= 2)?;
            check_following_buffer(&harness, "selected-c", false)?;
            check_following_buffer(&harness, "tail", false)?;
        }
    }
    Ok(())
}
