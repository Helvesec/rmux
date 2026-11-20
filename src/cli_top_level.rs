//! Early CLI decisions shared by the full and tiny entrypoints.

use std::ffi::{OsStr, OsString};

use crate::os_string::os_str_bytes;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TopLevelDecision {
    Version,
    Usage,
    LongHelp,
    InvalidLongOption,
    InvalidShortOption(u8),
    MissingArgument(u8),
}

pub(crate) fn top_level_decision(
    args: &[OsString],
    invoked_as_tmux: bool,
) -> Option<TopLevelDecision> {
    scan_top_level(args, invoked_as_tmux, |_, _| {}, |_, _| {}).err()
}

/// Project the command boundary without interpreting or rebuilding native arguments.
/// Early decisions have priority over extension dispatch in either personality.
#[cfg(any(not(feature = "tiny-cli"), debug_assertions, test))]
pub(crate) fn top_level_command_index(args: &[OsString]) -> Option<usize> {
    project_top_level_options(args, |_, _| {})
}

/// Visit options in scanner order, preserving native values and repeated occurrences.
#[cfg(any(not(feature = "tiny-cli"), debug_assertions, test))]
pub(crate) fn project_top_level_options(
    args: &[OsString],
    option: impl FnMut(u8, Option<&OsStr>),
) -> Option<usize> {
    scan_top_level(args, false, |_, _| {}, option).ok()
}

// Report ignored flags from the same walk that owns decisions and value boundaries.
fn scan_top_level(
    args: &[OsString],
    invoked_as_tmux: bool,
    mut legacy_q: impl FnMut(usize, usize),
    mut option: impl FnMut(u8, Option<&OsStr>),
) -> Result<usize, TopLevelDecision> {
    let mut index = 0;

    while let Some(argument) = args.get(index) {
        let bytes = os_str_bytes(argument);
        if bytes == b"--" {
            return Ok(index + 1);
        }
        if !bytes.starts_with(b"-") || bytes == b"-" {
            return Ok(index);
        }
        if bytes == b"--version" && !invoked_as_tmux {
            return Err(TopLevelDecision::Version);
        }
        if bytes == b"--help" {
            return Err(TopLevelDecision::LongHelp);
        }
        if bytes.starts_with(b"--") {
            return Err(TopLevelDecision::InvalidLongOption);
        }

        for (offset, flag) in bytes.iter().copied().enumerate().skip(1) {
            match flag {
                b'V' => return Err(TopLevelDecision::Version),
                b'h' => return Err(TopLevelDecision::Usage),
                b'q' => legacy_q(index, offset),
                flag if short_option_takes_argument(flag) => {
                    if offset + 1 == bytes.len() {
                        index += 1;
                        let value = args
                            .get(index)
                            .ok_or(TopLevelDecision::MissingArgument(flag))?;
                        option(flag, Some(value));
                    } else {
                        // SAFETY: the scanned prefix is ASCII, so this boundary also
                        // lies in the original native encoding, including on Windows.
                        let value = unsafe {
                            OsStr::from_encoded_bytes_unchecked(
                                &argument.as_encoded_bytes()[offset + 1..],
                            )
                        };
                        option(flag, Some(value));
                    }
                    break;
                }
                flag if short_option_takes_no_argument(flag) => option(flag, None),
                flag => {
                    return Err(TopLevelDecision::InvalidShortOption(flag));
                }
            }
        }

        index += 1;
    }

    Ok(index)
}

/// Discard only historical top-level q, before downstream command/extension parsing.
/// Registering q in RawCli would also change its ownership as a -c/-f value.
pub(crate) fn normalize_legacy_q(mut args: Vec<OsString>) -> Vec<OsString> {
    let mut ignored: Vec<(usize, usize)> = Vec::new();
    let _ = scan_top_level(
        args.get(1..).unwrap_or(&[]),
        false,
        |index, offset| {
            let index = index + 1;
            match ignored.last_mut() {
                Some((last_index, last_offset)) if *last_index == index => *last_offset = offset,
                _ => ignored.push((index, offset)),
            }
        },
        |_, _| {},
    );
    if ignored.is_empty() {
        return args;
    }
    let mut ignored = ignored.into_iter().peekable();
    let mut index = 0;
    // Retain visits original indices in order and moves each surviving slot at
    // most once, including an opaque command tail after many standalone q flags.
    args.retain_mut(|argument| {
        let original_index = index;
        index += 1;
        let Some(&(argument_index, last_q)) = ignored.peek() else {
            return true;
        };
        if argument_index != original_index {
            return true;
        }
        ignored.next();
        let mut bytes = std::mem::take(argument).into_encoded_bytes();
        let mut offset = 0;
        // Before the last reported q, every q belongs to this ASCII flag prefix.
        // The scanner cannot reach it through a value or an earlier decision.
        bytes.retain(|byte| {
            let keep = offset > last_q || *byte != b'q';
            offset += 1;
            keep
        });
        let keep = bytes != b"-";
        // SAFETY: these are this OsString's native bytes, not the scanner's
        // potentially lossy Windows view. Only ASCII q in the scanned prefix
        // was removed; the retained ASCII prefix and untouched native suffix
        // cannot split or join non-ASCII encodings (including surrogate halves).
        *argument = unsafe { OsString::from_encoded_bytes_unchecked(bytes) };
        keep
    });
    args
}

fn short_option_takes_argument(flag: u8) -> bool {
    matches!(flag, b'c' | b'f' | b'L' | b'S' | b'T')
}

fn short_option_takes_no_argument(flag: u8) -> bool {
    matches!(flag, b'2' | b'C' | b'D' | b'l' | b'N' | b'u' | b'v')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decide(args: &[&str], tmux: bool) -> Option<TopLevelDecision> {
        top_level_decision(&args.iter().map(OsString::from).collect::<Vec<_>>(), tmux)
    }

    #[test]
    fn legacy_q_preserves_first_decision_and_boundaries() {
        for tmux in [false, true] {
            for tail in [
                vec!["-V", "-x"],
                vec!["-h", "-V"],
                vec!["--version"],
                vec!["--help"],
                vec!["-x", "-V"],
                vec!["-d"],
                vec!["-U"],
                vec!["-L"],
                vec!["-f", "-q"],
                vec!["--", "-q", "-V"],
                vec!["command", "-q"],
                vec!["", "-q"],
                vec!["-", "-q"],
            ] {
                for prefix in ["-q", "-qq", "-uq", "-q2quq"] {
                    let mut args = vec![prefix];
                    args.extend(&tail);
                    assert_eq!(decide(&args, tmux), decide(&tail, tmux), "{args:?}");
                }
            }
            for (args, expected) in [
                (vec!["-qVh"], TopLevelDecision::Version),
                (vec!["-qhV"], TopLevelDecision::Usage),
                (vec!["-qxV"], TopLevelDecision::InvalidShortOption(b'x')),
                (vec!["-qdV"], TopLevelDecision::InvalidShortOption(b'd')),
                (vec!["-qUV"], TopLevelDecision::InvalidShortOption(b'U')),
            ] {
                assert_eq!(decide(&args, tmux), Some(expected));
            }
        }
    }

    #[test]
    fn legacy_q_normalization_preserves_exact_value_and_command_ownership() {
        let os = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
        for flag in ["L", "S", "f", "T", "c"] {
            for value in ["", "q", "-q", "-qq", "--", "--help", "--version", "-V"] {
                for cluster in ["", "u", "2u"] {
                    let option = format!("-{cluster}{flag}");
                    let with_q = format!("-q{cluster}q{flag}");
                    let tail = ["command", "-q", "--", "-q"];
                    let expected = os(&[&["rmux", &option, value][..], &tail].concat());
                    let actual = os(&[&["rmux", "-qq", &with_q, value, "-q"][..], &tail].concat());
                    assert_eq!(normalize_legacy_q(actual), expected);
                    if !value.is_empty() {
                        assert_eq!(
                            normalize_legacy_q(os(&[
                                "rmux",
                                &format!("{with_q}{value}"),
                                "command"
                            ])),
                            os(&["rmux", &format!("{option}{value}"), "command"])
                        );
                    }
                }
            }
        }
        for boundary in ["--", "command", "", "-"] {
            assert_eq!(
                normalize_legacy_q(os(&["rmux", "-q", boundary, "-q"])),
                os(&["rmux", boundary, "-q"])
            );
        }
    }

    #[test]
    fn legacy_q_normalization_compacts_large_clusters_without_consuming_suffixes() {
        let cluster = "qNquq".repeat(4_000);
        let retained = "Nu".repeat(4_000);
        for suffix in [
            "Lq-q", "Sq-q", "fq-q", "Tq-q", "cq-q", "Vq", "hq", "xq", "éq",
        ] {
            let expected = vec![
                OsString::from("rmux"),
                format!("-{retained}{suffix}").into(),
                "command".into(),
                "-q".into(),
            ];
            let actual = normalize_legacy_q(vec![
                "rmux".into(),
                format!("-{cluster}{suffix}").into(),
                "command".into(),
                "-q".into(),
            ]);
            assert_eq!(actual, expected);
            assert_eq!(normalize_legacy_q(actual), expected);
        }
        assert_eq!(
            normalize_legacy_q(vec![
                "rmux".into(),
                format!("-{}", "q".repeat(24_000)).into()
            ]),
            vec![OsString::from("rmux")]
        );
    }

    #[test]
    fn legacy_q_normalization_compacts_standalone_flags_before_an_opaque_tail() {
        for boundary in ["command", "--", "-", ""] {
            let mut expected = vec![OsString::from("rmux"), boundary.into()];
            expected.extend((0..8_000).map(|i| OsString::from(format!("-q-{i}-é"))));
            let mut args = vec![OsString::from("rmux")];
            args.extend((0..8_000).map(|_| OsString::from("-qq")));
            args.extend(expected[1..].iter().cloned());
            let tail_pointer = args.last().unwrap().as_encoded_bytes().as_ptr();
            let actual = normalize_legacy_q(args);
            assert_eq!(actual, expected);
            assert_eq!(
                actual.last().unwrap().as_encoded_bytes().as_ptr(),
                tail_pointer
            );
            assert_eq!(normalize_legacy_q(actual), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn legacy_q_normalization_preserves_non_utf8_values_and_tails() {
        use std::os::unix::ffi::OsStringExt;

        for flag in [b'L', b'S', b'f', b'T', b'c'] {
            for value in [vec![0xff], vec![b'q', 0xff], vec![b'-', b'q', 0xff]] {
                let value = OsString::from_vec(value);
                let option = OsString::from_vec(vec![b'-', b'q', flag]);
                let mut attached = option.clone();
                attached.push(&value);
                let mut expected = OsString::from_vec(vec![b'-', flag]);
                expected.push(&value);
                assert_eq!(
                    normalize_legacy_q(vec!["rmux".into(), attached]),
                    vec!["rmux".into(), expected]
                );
                assert_eq!(
                    normalize_legacy_q(vec!["rmux".into(), option, value.clone(), "-q".into()]),
                    vec!["rmux".into(), OsString::from_vec(vec![b'-', flag]), value]
                );
            }
        }
        let tail = OsString::from_vec(vec![0xff]);
        assert_eq!(
            normalize_legacy_q(vec!["rmux".into(), "-q".into(), tail.clone(), "-q".into()]),
            vec!["rmux".into(), tail, "-q".into()]
        );
    }

    #[cfg(windows)]
    #[test]
    fn legacy_q_normalization_preserves_unpaired_utf16_values() {
        use std::os::windows::ffi::OsStringExt;

        for flag in [b'L', b'S', b'f', b'T', b'c'] {
            for units in [
                &[0xd800, 0x71][..],
                &[0xdc00, 0x71],
                &[0xd83d, 0xde00, 0x71],
                &[0xd800, 0x71, 0xdc00],
                &[0xe9, 0x71],
            ] {
                let value = OsString::from_wide(units);
                let option = format!("-qNqq{}", char::from(flag));
                let mut attached = OsString::from(&option);
                attached.push(&value);
                let mut expected = OsString::from(format!("-N{}", char::from(flag)));
                expected.push(&value);
                assert_eq!(
                    normalize_legacy_q(vec!["rmux".into(), attached]),
                    vec!["rmux".into(), expected]
                );
                assert_eq!(
                    normalize_legacy_q(vec![
                        "rmux".into(),
                        option.into(),
                        value.clone(),
                        "-q".into()
                    ]),
                    vec![
                        "rmux".into(),
                        format!("-N{}", char::from(flag)).into(),
                        value
                    ]
                );
            }
        }
    }

    #[test]
    fn missing_argument_is_decided_only_when_no_value_token_exists() {
        for tmux in [false, true] {
            for flag in [b'L', b'S', b'f', b'T', b'c'] {
                for cluster in ["", "v", "2u", "uvv"] {
                    let option = format!("-{cluster}{}", char::from(flag));
                    for args in [vec![option.as_str()], vec![&option, "value", &option]] {
                        assert_eq!(
                            decide(&args, tmux),
                            Some(TopLevelDecision::MissingArgument(flag)),
                            "{args:?}, tmux={tmux}"
                        );
                    }
                    for value in ["", "value", "--help", "--version", "--", "-x", "-V"] {
                        assert_eq!(decide(&[&option, value], tmux), None);
                        assert_eq!(
                            decide(&[&option, value, "-V"], tmux),
                            Some(TopLevelDecision::Version)
                        );
                    }
                    for suffix in ["=", "=value", "--help", "--version", "Vh"] {
                        assert_eq!(decide(&[&format!("{option}{suffix}")], tmux), None);
                    }
                }
            }
        }
    }

    #[test]
    fn missing_argument_preserves_first_decision_and_command_boundaries() {
        for tmux in [false, true] {
            for option in ["-L", "-vS", "-2f", "-uT", "-uvc"] {
                for earlier in ["-V", "-h", "-x", "--help", "--version", "--version=x"] {
                    assert_eq!(decide(&[earlier, option], tmux), decide(&[earlier], tmux));
                }
                for boundary in ["--", "command", "-", ""] {
                    assert_eq!(decide(&[boundary, option], tmux), None);
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_values_are_present_arguments() {
        use std::os::unix::ffi::OsStringExt;

        for tmux in [false, true] {
            for flag in [b'L', b'S', b'f', b'T', b'c'] {
                let option = OsString::from_vec(vec![b'-', b'u', flag]);
                let raw = OsString::from_vec(vec![0xff]);
                assert_eq!(top_level_decision(&[option, raw], tmux), None);
                let attached = OsString::from_vec(vec![b'-', b'u', flag, 0xff]);
                assert_eq!(top_level_decision(&[attached], tmux), None);
            }
        }
    }

    #[test]
    fn native_long_version_product_divergence_is_exact_and_ordered() {
        let decide = |args: &[&str], tmux| {
            top_level_decision(&args.iter().map(OsString::from).collect::<Vec<_>>(), tmux)
        };
        assert_eq!(
            decide(&["--version"], false),
            Some(TopLevelDecision::Version)
        );
        assert_eq!(
            decide(&["--version"], true),
            Some(TopLevelDecision::InvalidLongOption)
        );
        for tmux in [false, true] {
            for boundary in ["--", "command", "-", ""] {
                assert_eq!(decide(&[boundary, "--version"], tmux), None);
            }
            for invalid in ["--vers", "--version=x", "--vesion", "---version"] {
                assert_eq!(
                    decide(&[invalid, "--version"], tmux),
                    Some(TopLevelDecision::InvalidLongOption)
                );
            }
            for flag in ['L', 'S', 'f', 'T', 'c'] {
                for prefix in [format!("-{flag}"), format!("-v{flag}")] {
                    assert_eq!(decide(&[&prefix, "--version"], tmux), None);
                    assert_eq!(
                        decide(&[&prefix, "--version", "-V"], tmux),
                        Some(TopLevelDecision::Version)
                    );
                    assert_eq!(decide(&[&format!("{prefix}--version")], tmux), None);
                }
            }
            assert_eq!(
                decide(&["-V", "--version"], tmux),
                Some(TopLevelDecision::Version)
            );
            assert_eq!(
                decide(&["-h", "--version"], tmux),
                Some(TopLevelDecision::Usage)
            );
            assert_eq!(
                decide(&["-x", "--version"], tmux),
                Some(TopLevelDecision::InvalidShortOption(b'x'))
            );
        }
    }
}
