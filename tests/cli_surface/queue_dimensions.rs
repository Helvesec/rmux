use super::*;

#[test]
fn queued_dimension_error_preserves_prior_output_and_stops_tail() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("queue-dimensions")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "/bin/sh"])?);
    for (command, alias) in [
        ("new-session", "new"),
        ("resize-window", "resizew"),
        ("resize-pane", "resizep"),
    ] {
        for flag in ["-x", "-y"] {
            for value in ["65536", "9223372036854775808", "-9223372036854775809"] {
                if command == "resize-pane" && value == "65536" {
                    continue;
                }
                for spelling in [command, alias] {
                    let mut args = vec![
                        "display-message",
                        "-p",
                        "before",
                        ";",
                        spelling,
                        flag,
                        value,
                    ];
                    if command == "new-session" {
                        args.push("-d");
                    }
                    let direct = harness.run(&args[4..])?;
                    args.extend([";", "display-message", "-p", "after"]);
                    let output = harness.run(&args)?;
                    assert_eq!(output.status.code(), Some(1), "{args:?}");
                    assert_eq!(stdout(&output), "before\n", "{args:?}");
                    assert_eq!(stderr(&output), stderr(&direct), "{args:?}");
                    assert!(!stderr(&output).is_empty());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn queued_dimension_error_keeps_syntax_validation_eager() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("queue-dimension-syntax")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "/bin/sh"])?);
    for command in ["new-session", "resize-window", "resize-pane"] {
        for tail in [
            vec![command, "-x"],
            vec![command, "-Q"],
            vec![command, "-x", "65536", "-Q"],
            vec![command, "-x", "65536", ";", "display-message", "-Q"],
        ] {
            let mut args = vec!["display-message", "-p", "before", ";"];
            args.extend(tail);
            let output = harness.run(&args)?;
            assert_eq!(output.status.code(), Some(1), "{args:?}");
            assert_eq!(stdout(&output), "", "{args:?}");
        }
    }
    Ok(())
}
