use std::error::Error;

use crate::common::{assert_success, stderr, stdout, CliHarness};

#[test]
fn attach_session_frontend_final_empty_direct_single() -> Result<(), Box<dyn Error>> {
    check_final_empty_target(false, false)
}

#[test]
fn attach_session_frontend_final_empty_direct_repeated() -> Result<(), Box<dyn Error>> {
    check_final_empty_target(false, true)
}

#[test]
fn attach_session_frontend_final_empty_list_single() -> Result<(), Box<dyn Error>> {
    check_final_empty_target(true, false)
}

#[test]
fn attach_session_frontend_final_empty_list_repeated() -> Result<(), Box<dyn Error>> {
    check_final_empty_target(true, true)
}

fn check_final_empty_target(queued: bool, repeated: bool) -> Result<(), Box<dyn Error>> {
    // tmux 3.7b treats the final empty -t as the current session, even when
    // an earlier -t names a session which does not exist.
    for session in [None, Some("alpha"), Some("beta")] {
        let harness = CliHarness::new("attach-frontend-final-empty")?;
        let _daemon = harness.start_hidden_daemon()?;
        if let Some(session) = session {
            assert_success(&harness.run(&["new-session", "-d", "-s", session])?);
        }
        for (name, target_flag) in [
            ("attach-session", "-t"),
            ("attach", "-t"),
            ("attach-session", "-dt"),
        ] {
            let mut args = Vec::new();
            if queued {
                args.extend(["start-server", ";"]);
            }
            args.push(name);
            if repeated {
                args.extend(["-t", "alpha"]);
            }
            args.extend([target_flag, ""]);
            check_attach_failure(&harness, &args, session)?;
            if queued {
                // This tail selects run_attach_session_queued instead of the
                // ordinary attach path used when attach is last in the list.
                args.extend([";", "detach-client"]);
                check_attach_failure(&harness, &args, session)?;
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

fn check_attach_failure(
    harness: &CliHarness,
    args: &[&str],
    session: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let output = harness.run(args)?;
    assert_eq!(output.status.code(), Some(1), "{session:?}: {args:?}");
    assert!(stdout(&output).is_empty(), "{session:?}: {args:?}");
    assert_eq!(
        stderr(&output),
        if session.is_none() {
            "no sessions\n"
        } else {
            "open terminal failed: not a terminal\n"
        },
        "{session:?}: {args:?}"
    );
    Ok(())
}
