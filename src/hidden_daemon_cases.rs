//! Paired caller regressions for the two hidden-daemon parser adapters.

use std::ffi::OsString;
use std::path::PathBuf;

use rmux_bootstrap::hidden_daemon::HiddenDaemonArgs;

pub(crate) fn check(
    parse: impl Fn(Vec<OsString>) -> Result<HiddenDaemonArgs, String>,
    unicode_flags: bool,
) {
    let decode = |args: &[&str]| parse(args.iter().map(OsString::from).collect());
    for (args, expected) in [
        (
            vec!["--config-default", "--config-file"],
            "--config-file requires a path",
        ),
        (
            vec!["--config-default", "--config-file", "x"],
            "--config-file conflicts with --config-default",
        ),
        (
            vec!["--config-file", "x", "--config-default"],
            "duplicate hidden daemon config selection",
        ),
        (
            vec!["--config-default", "--config-default"],
            "duplicate hidden daemon config selection",
        ),
        (
            vec!["--web-port", "0"],
            "--web-port must be between 1 and 65535",
        ),
        (
            vec!["--web-port", "65536"],
            "--web-port requires an integer port",
        ),
        (
            vec!["--startup-ready-fd", "-1"],
            "--startup-ready-fd requires a non-negative file descriptor",
        ),
        (
            vec!["--startup-ready-fd", "2147483648"],
            "--startup-ready-fd requires an integer file descriptor",
        ),
    ] {
        assert_eq!(decode(&args).unwrap_err(), expected, "{args:?}");
    }
    for (flag, operand) in [
        ("--config-file", "a path"),
        ("--config-cwd", "a path"),
        ("--web-port", "a port"),
        ("--frontend-url", "a URL"),
        ("--startup-ready-fd", "a file descriptor"),
        ("--startup-ready-event", "an event name"),
    ] {
        assert_eq!(
            decode(&[flag]).unwrap_err(),
            format!("{flag} requires {operand}")
        );
    }
    let args = decode(&[
        "",
        "--web-port",
        "1",
        "--web-port",
        "65535",
        "--frontend-url",
        "first",
        "--web-frontend",
        "last",
        "--startup-ready-fd",
        "0",
        "--startup-ready-fd",
        "2147483647",
        "--startup-ready-event",
        "first",
        "--startup-ready-event",
        "",
    ])
    .unwrap();
    assert_eq!(args.socket_path, Some(PathBuf::new()));
    assert_eq!(args.web_port, Some(65535));
    assert_eq!(args.web_frontend.as_deref(), Some("last"));
    assert_eq!(args.startup_ready_fd, Some(i32::MAX));
    assert_eq!(args.startup_ready_event, Some(OsString::new()));

    #[cfg(unix)]
    let raw = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![b'x', 255])
    };
    #[cfg(windows)]
    let raw = {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[120, 0xd800])
    };
    let args = parse(vec![
        raw.clone(),
        "--config-file".into(),
        raw.clone(),
        "--config-cwd".into(),
        raw.clone(),
        "--startup-ready-event".into(),
        raw.clone(),
    ])
    .unwrap();
    assert_eq!(args.socket_path.unwrap().as_os_str(), raw);
    assert_eq!(args.config_cwd.unwrap().as_os_str(), raw);
    assert_eq!(args.startup_ready_event.unwrap(), raw);
    assert_eq!(
        args.config_selection,
        rmux_bootstrap::hidden_daemon::ConfigSelection::Files(vec![PathBuf::from(&raw)])
    );
    for flag in ["--web-port", "--frontend-url", "--startup-ready-fd"] {
        assert_eq!(
            parse(vec![flag.into(), raw.clone()]).unwrap_err(),
            format!("invalid UTF-8 in {flag}")
        );
    }
    let mut flag = OsString::from("--");
    flag.push(&raw);
    let first = parse(vec![flag.clone()]);
    if unicode_flags {
        assert_eq!(first.unwrap().socket_path.unwrap().as_os_str(), flag);
    } else {
        assert_eq!(first.unwrap_err(), "invalid UTF-8 in hidden daemon flag");
    }
    assert_eq!(
        parse(vec!["endpoint".into(), flag]).unwrap_err(),
        if unicode_flags {
            "unexpected extra arguments for hidden daemon mode"
        } else {
            "invalid UTF-8 in hidden daemon flag"
        }
    );
}
