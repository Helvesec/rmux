use super::{parse_invocation, DropinInvocation};
use std::ffi::OsString;

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn dropin_prefix_projects_clusters_and_value_boundaries() {
    for (command, subcommand, expected) in [
        ("doctor", "tmux-dropin", DropinInvocation::DoctorTmuxDropin),
        ("setup", "tmux-shim", DropinInvocation::SetupTmuxShim),
    ] {
        for flag in ['L', 'S', 'c', 'f', 'T'] {
            for cluster in ["", "u", "2CDNluvv", "qNqu"] {
                let option = format!("-{cluster}{flag}");
                for value in ["name", "", "--", "--help", "-V", "doctor", "setup"] {
                    let mut prefixes = vec![args(&["-N", &option, value])];
                    if !value.is_empty() {
                        prefixes.push(args(&["-N", &format!("{option}{value}")]));
                    }
                    for prefix in prefixes {
                        for separator in [false, true] {
                            let mut actual = prefix.clone();
                            if separator {
                                actual.push("--".into());
                            }
                            actual.extend(args(&[command, subcommand]));
                            assert_eq!(
                                parse_invocation(&actual).unwrap(),
                                Some(expected),
                                "{actual:?}"
                            );
                            actual.pop();
                            actual.push("--help".into());
                            let help = parse_invocation(&actual).unwrap_err();
                            assert_eq!(help.exit_code(), 0);
                            assert_eq!(
                                help.message(),
                                format!("usage: rmux {command} {subcommand}")
                            );
                        }
                    }
                }
                // The command-looking token belongs to the value option.
                assert!(parse_invocation(&args(&[&option, command, subcommand]))
                    .unwrap()
                    .is_none());
                assert!(parse_invocation(&args(&[&option])).unwrap().is_none());
            }
        }
        for boundary in [
            "-",
            "",
            "other",
            "--",
            "-V",
            "-h",
            "-uxV",
            "--help",
            "--version",
        ] {
            let tail = if boundary == "--" { "-uLname" } else { command };
            assert!(
                parse_invocation(&args(&[boundary, tail, command, subcommand]))
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[test]
fn dropin_does_not_claim_sibling_routes_or_change_tail_parsing() {
    for command in ["diagnose", "capabilities", "claude", "list-sessions"] {
        assert!(
            parse_invocation(&args(&["-N", "-uLname", command, "--help"]))
                .unwrap()
                .is_none()
        );
    }
    for command in ["doctor", "setup"] {
        for tail in [
            vec![],
            vec!["unknown"],
            vec!["tmux-shim", "extra"],
            vec!["--"],
        ] {
            let mut plain = args(&[command]);
            plain.extend(args(&tail));
            let mut grouped = args(&["-N", "-uLname"]);
            grouped.extend(plain.clone());
            let expected = parse_invocation(&plain).unwrap_err();
            let actual = parse_invocation(&grouped).unwrap_err();
            assert_eq!(actual.exit_code(), expected.exit_code());
            assert_eq!(actual.message(), expected.message());
        }
    }
}

#[test]
fn dropin_preserves_native_values_and_command_tail() {
    #[cfg(unix)]
    let opaque = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![0xff, b'q'])
    };
    #[cfg(windows)]
    let opaque = {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[0xd800, 0x71, 0xdc00])
    };
    #[cfg(not(any(unix, windows)))]
    let opaque = OsString::from("native");
    for flag in ['L', 'S', 'c', 'f', 'T'] {
        let option = OsString::from(format!("-u{flag}"));
        let mut attached = option.clone();
        attached.push(&opaque);
        for prefix in [vec![option, opaque.clone()], vec![attached]] {
            for (command, subcommand, expected) in [
                ("doctor", "tmux-dropin", DropinInvocation::DoctorTmuxDropin),
                ("setup", "tmux-shim", DropinInvocation::SetupTmuxShim),
            ] {
                let mut actual = prefix.clone();
                actual.extend(args(&[command, subcommand]));
                let original = actual.clone();
                assert_eq!(parse_invocation(&actual).unwrap(), Some(expected));
                assert_eq!(actual, original);
                actual.pop();
                actual.push(opaque.clone());
                assert!(parse_invocation(&actual).is_err());
            }
        }
    }
}
