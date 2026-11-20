use std::error::Error;

use crate::common::{assert_success, stderr, stdout, CliHarness};

#[derive(Clone, Copy)]
enum Entry {
    Direct,
    List,
    Tail,
}

#[test]
fn has_session_frontend_empty_direct_single() -> Result<(), Box<dyn Error>> {
    check_empty_target(Entry::Direct, false)
}

#[test]
fn has_session_frontend_empty_direct_repeated() -> Result<(), Box<dyn Error>> {
    check_empty_target(Entry::Direct, true)
}

#[test]
fn has_session_frontend_empty_list_single() -> Result<(), Box<dyn Error>> {
    check_empty_target(Entry::List, false)
}

#[test]
fn has_session_frontend_empty_list_repeated() -> Result<(), Box<dyn Error>> {
    check_empty_target(Entry::List, true)
}

#[test]
fn has_session_frontend_empty_tail_single() -> Result<(), Box<dyn Error>> {
    check_empty_target(Entry::Tail, false)
}

#[test]
fn has_session_frontend_empty_tail_repeated() -> Result<(), Box<dyn Error>> {
    check_empty_target(Entry::Tail, true)
}

fn check_empty_target(entry: Entry, repeated: bool) -> Result<(), Box<dyn Error>> {
    // Measured with tmux 3.7b: the last -t wins, and an empty target
    // selects the current session instead of an empty literal name.
    for session in [None, Some("alpha"), Some("beta")] {
        let harness = CliHarness::new("has-frontend-empty")?;
        let _daemon = harness.start_hidden_daemon()?;
        if let Some(session) = session {
            assert_success(&harness.run(&["new-session", "-d", "-s", session])?);
        }
        for name in ["has-session", "has"] {
            for reverse in [false, true] {
                if reverse && (!repeated || session.is_none()) {
                    continue;
                }
                let mut args = Vec::new();
                if matches!(entry, Entry::List) {
                    args.extend(["start-server", ";"]);
                }
                args.push(name);
                if repeated && !reverse {
                    args.push("-talpha");
                }
                args.extend(["-t", ""]);
                if reverse {
                    args.push("-talpha");
                }
                if matches!(entry, Entry::Tail) {
                    args.extend([";", "display-message", "-p", "has-tail"]);
                }
                let expected_error = match session {
                    None => "no current target\n",
                    Some("beta") if reverse => "can't find session: alpha\n",
                    Some(_) => "",
                };
                let output = harness.run(&args)?;
                assert_eq!(stderr(&output), expected_error, "{session:?}: {args:?}");
                assert_eq!(
                    output.status.code(),
                    Some(i32::from(!expected_error.is_empty())),
                    "{session:?}: {args:?}"
                );
                assert_eq!(
                    stdout(&output),
                    if expected_error.is_empty() && matches!(entry, Entry::Tail) {
                        "has-tail\n"
                    } else {
                        ""
                    },
                    "{session:?}: {args:?}"
                );
            }
        }
        let sessions = harness.run(&["list-sessions", "-F", "#{session_name}"])?;
        assert_eq!(sessions.status.code(), Some(0));
        assert!(stderr(&sessions).is_empty());
        assert_eq!(
            stdout(&sessions),
            session.map(|name| format!("{name}\n")).unwrap_or_default()
        );
    }
    Ok(())
}

#[test]
fn has_session_empty_server_direct_named() -> Result<(), Box<dyn Error>> {
    check_empty_server(Entry::Direct, false)
}

#[test]
fn has_session_empty_server_direct_omitted() -> Result<(), Box<dyn Error>> {
    check_empty_server(Entry::Direct, true)
}

#[test]
fn has_session_empty_server_list_named() -> Result<(), Box<dyn Error>> {
    check_empty_server(Entry::List, false)
}

#[test]
fn has_session_empty_server_list_omitted() -> Result<(), Box<dyn Error>> {
    check_empty_server(Entry::List, true)
}

#[test]
fn has_session_empty_server_tail_named() -> Result<(), Box<dyn Error>> {
    check_empty_server(Entry::Tail, false)
}

#[test]
fn has_session_empty_server_tail_omitted() -> Result<(), Box<dyn Error>> {
    check_empty_server(Entry::Tail, true)
}

fn check_empty_server(entry: Entry, omitted: bool) -> Result<(), Box<dyn Error>> {
    // tmux 3.7b reports the missing current target before looking up an
    // explicit target when the already-running server has no sessions.
    for session in [None, Some("alpha"), Some("beta")] {
        let harness = CliHarness::new("has-empty-server")?;
        let _daemon = harness.start_hidden_daemon()?;
        if let Some(session) = session {
            assert_success(&harness.run(&["new-session", "-d", "-s", session])?);
        }
        let targets: &[&[&str]] = if omitted {
            &[&[]]
        } else {
            &[
                &["-t", "alpha"],
                &["-tmissing"],
                &["-t", "."],
                &["-t", ":"],
                &["-t", "="],
                &["-t", "", "-talpha"],
                &["-t", "=alpha"],
                &["-t", "$42"],
            ]
        };
        for name in ["has-session", "has"] {
            for target in targets {
                let mut args = Vec::new();
                if matches!(entry, Entry::List) {
                    args.extend(["start-server", ";"]);
                }
                args.push(name);
                args.extend_from_slice(target);
                if matches!(entry, Entry::Tail) {
                    args.extend([";", "display-message", "-p", "has-tail"]);
                }
                let error = match (session, *target) {
                    (None, _) => "no current target\n",
                    (Some(_), ["-tmissing"]) => "can't find session: missing\n",
                    // Preserve the existing nonempty-server parser diagnostic.
                    (Some(_), ["-t", "="]) => "invalid session: \n",
                    (Some(_), ["-t", "$42"]) => "can't find session: $42\n",
                    (Some("beta"), ["-t", "alpha" | "=alpha"] | ["-t", "", "-talpha"]) => {
                        "can't find session: alpha\n"
                    }
                    _ => "",
                };
                let output = harness.run(&args)?;
                assert_eq!(stderr(&output), error, "{session:?}: {args:?}");
                assert_eq!(output.status.code(), Some(i32::from(!error.is_empty())));
                assert_eq!(
                    stdout(&output),
                    if error.is_empty() && matches!(entry, Entry::Tail) {
                        "has-tail\n"
                    } else {
                        ""
                    },
                    "{session:?}: {args:?}"
                );
            }
        }
        let sessions = harness.run(&["list-sessions", "-F", "#{session_name}"])?;
        assert_eq!(sessions.status.code(), Some(0));
        assert!(stderr(&sessions).is_empty());
        assert_eq!(
            stdout(&sessions),
            session.map(|s| format!("{s}\n")).unwrap_or_default()
        );
    }
    Ok(())
}

#[test]
fn has_session_empty_server_run_shell() -> Result<(), Box<dyn Error>> {
    check_empty_server_script("run-shell")
}

#[test]
fn has_session_empty_server_if_shell() -> Result<(), Box<dyn Error>> {
    check_empty_server_script("if-shell")
}

#[test]
fn has_session_empty_server_source_file() -> Result<(), Box<dyn Error>> {
    check_empty_server_script("source-file")
}

fn check_empty_server_script(entry: &str) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("has-empty-server-script")?;
    let _daemon = harness.start_hidden_daemon()?;
    for command in [
        "has-session -t alpha",
        "has-session",
        "has -t '' -talpha",
        "has-session -t .",
        "has-session -t :",
        "has-session -t =",
    ] {
        let script = format!("{command}; display-message -p has-tail");
        let config = harness.tmpdir().join("has-empty-server.conf");
        std::fs::write(&config, &script)?;
        let output = match entry {
            "run-shell" => harness.run(&["run-shell", "-C", &script])?,
            "if-shell" => harness.run(&["if-shell", "-F", "1", &script])?,
            "source-file" => harness.run(&["source-file", config.to_str().unwrap()])?,
            _ => unreachable!(),
        };
        assert_eq!(stderr(&output), "no current target\n", "{entry}: {command}");
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            stdout(&output),
            "",
            "queue continued after {entry}: {command}"
        );
    }
    let sessions = harness.run(&["list-sessions", "-F", "#{session_name}"])?;
    assert_success(&sessions);
    assert!(stdout(&sessions).is_empty());
    Ok(())
}
