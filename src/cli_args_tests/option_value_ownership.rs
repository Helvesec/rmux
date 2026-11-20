use crate::cli_args::{self as cli, schema::CommandSpec};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn parse_new_window_every_entry(arguments: &[String]) -> Vec<cli::NewWindowArgs> {
    let argv = ["rmux".to_owned(), "new-window".to_owned()]
        .into_iter()
        .chain(arguments.iter().cloned())
        .collect::<Vec<_>>();
    let queue = std::iter::once("new-window".to_owned())
        .chain(arguments.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    let groups = [cli::RuntimeCommandGroup::Canonical(queue)];
    let mut results = vec![
        cli::schema::NewWindow::parse(arguments.to_vec(), String::new())
            .unwrap_or_else(|error| panic!("schema {arguments:?}: {error}")),
    ];
    for parsed in [
        cli::parse(argv.clone()),
        cli::parse_with_runtime_command_groups(argv, &groups),
    ] {
        let cli::Command::NewWindow(args) = parsed
            .unwrap_or_else(|error| panic!("full/runtime {arguments:?}: {error}"))
            .command
            .expect("command")
        else {
            panic!("expected new-window");
        };
        results.push(args);
    }
    results
}

#[test]
fn repeated_targets_preserve_other_option_values_in_every_entry() {
    for owner in ["n", "F", "c", "e"] {
        for value in ["-t", "--", "-talpha:9", "ordinary"] {
            for form in 0..3 {
                let mut arguments = match form {
                    0 => vec![format!("-{owner}"), value.to_owned()],
                    1 => vec![format!("-{owner}{value}")],
                    _ => vec![format!("-d{owner}{value}")],
                };
                arguments.extend(strings(&["-t", "alpha:1", "-t", "alpha:2"]));
                for args in parse_new_window_every_entry(&arguments) {
                    let actual = match owner {
                        "n" => args.name.as_deref(),
                        "F" => args.format.as_deref(),
                        "c" => args.start_directory.as_deref().and_then(|p| p.to_str()),
                        _ => args.environment.first().map(String::as_str),
                    };
                    assert_eq!(actual, Some(value), "{arguments:?}");
                    assert_eq!(args.target.unwrap().raw(), "alpha:2", "{arguments:?}");
                    assert!(args.command.is_empty(), "{arguments:?}");
                }
            }
        }
    }
}

#[test]
fn repeated_targets_keep_separator_values_and_shell_tails() {
    for tail in [
        &[][..],
        &["printf", "-t", "shell:1", "-t", "shell:2"][..],
        &["--", "-t", "shell:1", "-t", "shell:2"][..],
    ] {
        for targets in [
            &["-t", "alpha:1", "-t", "alpha:2"][..],
            &["-talpha:1", "-dtalpha:2"][..],
            &["-t", "--", "-t", "alpha:2"][..],
            &["-t", "-t", "-t", "alpha:2"][..],
        ] {
            let mut arguments = strings(&["-n", "-t"]);
            arguments.extend(strings(targets));
            arguments.extend(strings(tail));
            for args in parse_new_window_every_entry(&arguments) {
                assert_eq!(args.name.as_deref(), Some("-t"), "{arguments:?}");
                assert_eq!(args.target.unwrap().raw(), "alpha:2", "{arguments:?}");
                let expected = tail.strip_prefix(&["--"]).unwrap_or(tail);
                assert_eq!(args.command, strings(expected), "{arguments:?}");
            }
        }
    }
}

#[test]
fn repeated_targets_skip_long_values_and_aliases() {
    let command = clap::Command::new("new-window")
        .arg(clap::Arg::new("target").short('t'))
        .arg(
            clap::Arg::new("name")
                .short('n')
                .long("name")
                .alias("label"),
        );
    for owner in ["--name", "--label"] {
        for value in ["-t", "--", "-talpha:9"] {
            for attached in [false, true] {
                let mut arguments = if attached {
                    vec![format!("{owner}={value}")]
                } else {
                    vec![owner.to_owned(), value.to_owned()]
                };
                let mut expected = arguments.clone();
                arguments.extend(strings(&["-t", "alpha:1", "-talpha:2"]));
                expected.extend(strings(&["-t", "alpha:2"]));
                assert_eq!(
                    cli::normalize_attached_short_values(&command, arguments.clone()),
                    expected,
                    "{arguments:?}"
                );
            }
        }
    }
}

#[test]
fn repeated_targets_preserve_append_values_and_single_target_neighbors() {
    for targets in [
        &[][..],
        &["-t", "alpha:2"][..],
        &["-t", "alpha:1", "-t", "alpha:2"][..],
    ] {
        let mut arguments = strings(&["-e", "-t", "-e", "SECOND=value", "-n", "ordinary"]);
        arguments.extend(strings(targets));
        for args in parse_new_window_every_entry(&arguments) {
            assert_eq!(args.environment, strings(&["-t", "SECOND=value"]));
            assert_eq!(args.name.as_deref(), Some("ordinary"));
            assert_eq!(
                args.target.as_ref().map(|target| target.raw()),
                (!targets.is_empty()).then_some("alpha:2")
            );
        }
    }
}
