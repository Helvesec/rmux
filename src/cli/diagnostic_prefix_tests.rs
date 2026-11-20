use super::{capabilities, diagnose};
use std::ffi::OsString;

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn help(arguments: &[OsString], command: &str) -> Option<(i32, String)> {
    let error = match command {
        "diagnose" => diagnose::parse_invocation(arguments).err(),
        _ => capabilities::parse_invocation(arguments).err(),
    }?;
    Some((error.exit_code(), error.message().to_owned()))
}

#[test]
fn diagnostic_prefix_projects_value_boundaries_and_early_decisions() {
    for command in ["diagnose", "capabilities"] {
        let expected = help(&args(&[command, "--help"]), command).unwrap();
        assert_eq!(expected.0, 0);
        for flag in ['L', 'S', 'f', 'T', 'c'] {
            for cluster in ["", "u", "2CDNluvv", "qNqu"] {
                let option = format!("-{cluster}{flag}");
                for value in ["name", "", "--", "--help", "-V", "diagnose", "capabilities"] {
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
                            actual.extend(args(&[command, "--help"]));
                            assert_eq!(
                                help(&actual, command),
                                Some(expected.clone()),
                                "{actual:?}"
                            );
                        }
                    }
                }
                assert_eq!(help(&args(&[&option, command, "--help"]), command), None);
                assert_eq!(help(&args(&[&option]), command), None);
            }
        }
        for boundary in ["-", "", "other", "-V", "-h", "-uxV", "--help", "--version"] {
            assert_eq!(help(&args(&[boundary, command, "--help"]), command), None);
        }
        for sibling in ["doctor", "setup", "claude", "list-sessions"] {
            assert_eq!(help(&args(&["-uLname", sibling, "--help"]), command), None);
        }
        for tail in [
            vec!["--json", "--human"],
            vec!["--bad", "--help"],
            vec!["--help", "--bad"],
            vec!["--"],
        ] {
            let mut plain = args(&[command]);
            plain.extend(args(&tail));
            let mut grouped = args(&["-N", "-uLname"]);
            grouped.extend(plain.clone());
            assert_eq!(help(&grouped, command), help(&plain, command));
        }
    }
}

#[test]
fn diagnostic_prefix_preserves_native_values_and_tail() {
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
    for flag in ['L', 'S', 'f', 'T', 'c'] {
        let option = OsString::from(format!("-u{flag}"));
        let mut attached = option.clone();
        attached.push(&opaque);
        for prefix in [vec![option, opaque.clone()], vec![attached]] {
            for command in ["diagnose", "capabilities"] {
                let mut actual = prefix.clone();
                actual.extend(args(&[command, "--help"]));
                let original = actual.clone();
                assert_eq!(help(&actual, command).unwrap().0, 0);
                assert_eq!(actual, original);
                actual.pop();
                actual.push(opaque.clone());
                assert_eq!(help(&actual, command).unwrap().0, 1);
            }
        }
    }
}
