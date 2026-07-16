use super::*;

#[test]
fn fd_path_rejects_negative_descriptors() {
    assert_eq!(fd_path(std::process::id(), -1), None);
}

#[test]
fn current_process_is_live() {
    assert_eq!(
        ProcessInspector
            .is_live(std::process::id())
            .expect("liveness query"),
        Some(true)
    );
    assert!(is_live(std::process::id()));
}

#[test]
fn current_process_path_is_available() {
    let path = current_path(std::process::id()).expect("current process cwd should be visible");
    assert!(!path.is_empty());
}

#[test]
fn linux_cwd_path_string_strips_deleted_suffix() {
    assert_eq!(
        linux_cwd_path_string(PathBuf::from("/tmp/rmux-cwd (deleted)")),
        "/tmp/rmux-cwd"
    );
    assert_eq!(
        linux_cwd_path_string(PathBuf::from("/tmp/rmux-cwd")),
        "/tmp/rmux-cwd"
    );
}

#[test]
fn current_process_command_name_is_available() {
    let name = command_name(std::process::id()).expect("current process command should be visible");
    assert!(!name.is_empty());
}

#[test]
fn current_process_executable_path_is_available() {
    let path = executable_path(std::process::id()).expect("current process exe should be visible");
    assert!(!path.is_empty());
}

#[test]
fn current_process_environment_is_available() {
    let environment =
        environment(std::process::id()).expect("current process environment should be visible");
    assert!(!environment.is_empty());
}

#[cfg(windows)]
#[test]
fn windows_reports_exited_process_as_dead_even_with_exit_code_259() {
    let mut child = std::process::Command::new("cmd.exe")
        .args(["/C", "exit", "259"])
        .spawn()
        .expect("spawn exit-code helper");
    let pid = child.id();

    loop {
        if child.try_wait().expect("poll helper").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert_eq!(
        ProcessInspector.is_live(pid).expect("liveness query"),
        Some(false)
    );
}

#[cfg(windows)]
#[test]
fn windows_reports_unavailable_fd_path_as_ok_none() {
    assert_eq!(
        ProcessInspector
            .fd_path(std::process::id(), 0)
            .expect("fd path query should not fail"),
        None
    );
}

#[cfg(windows)]
#[test]
fn windows_child_environment_is_visible() {
    let mut child = std::process::Command::new("cmd.exe")
        .args(["/C", "ping -n 6 127.0.0.1 >NUL"])
        .env("RMUX_OS_ENV_SMOKE", "visible")
        .spawn()
        .expect("spawn environment helper");
    let pid = child.id();

    for _ in 0..40 {
        let environment = ProcessInspector
            .environment(pid)
            .expect("environment query should not fail");
        if environment
            .as_ref()
            .and_then(|values| values.get("RMUX_OS_ENV_SMOKE"))
            .is_some_and(|value| value == "visible")
        {
            child.kill().ok();
            child.wait().ok();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    child.kill().ok();
    child.wait().ok();
    panic!("child environment did not become visible");
}

#[cfg(windows)]
#[test]
fn windows_child_current_path_is_visible() {
    let expected = std::env::current_dir()
        .expect("current dir")
        .to_string_lossy()
        .into_owned();
    let mut child = std::process::Command::new("cmd.exe")
        .args(["/C", "ping -n 6 127.0.0.1 >NUL"])
        .spawn()
        .expect("spawn cwd helper");
    let pid = child.id();

    for _ in 0..40 {
        let path = ProcessInspector
            .current_path(pid)
            .expect("current path query should not fail");
        if path
            .as_deref()
            .is_some_and(|path| windows_paths_match(path, &expected))
        {
            child.kill().ok();
            child.wait().ok();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    child.kill().ok();
    child.wait().ok();
    panic!("child cwd did not become visible");
}

#[cfg(windows)]
fn windows_paths_match(actual: &str, expected: &str) -> bool {
    fn normalize(path: &str) -> String {
        path.replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    }
    normalize(actual) == normalize(expected)
}

#[test]
fn parses_nul_separated_environment() {
    let environment = environment_from_nul_entries(b"A=1\0B=two\0\0").expect("environment");

    assert_eq!(environment.get("A").map(String::as_str), Some("1"));
    assert_eq!(environment.get("B").map(String::as_str), Some("two"));
}

#[cfg(unix)]
#[test]
fn parses_raw_nul_separated_environment_without_utf8_filtering() {
    use std::os::unix::ffi::OsStringExt;

    let environment = raw_environment_from_nul_entries(b"A=1\0BAD=foo\xffbar\0\0");

    assert_eq!(
        environment,
        vec![
            (
                std::ffi::OsString::from_vec(b"A".to_vec()),
                std::ffi::OsString::from_vec(b"1".to_vec())
            ),
            (
                std::ffi::OsString::from_vec(b"BAD".to_vec()),
                std::ffi::OsString::from_vec(b"foo\xffbar".to_vec())
            )
        ]
    );
}

#[cfg(all(unix, target_os = "macos"))]
#[test]
fn parses_macos_procargs_environment() {
    let mut buffer = Vec::new();
    let argc: libc::c_int = 2;
    buffer.extend_from_slice(&argc.to_ne_bytes());
    buffer.extend_from_slice(b"/bin/zsh\0");
    buffer.extend_from_slice(b"\0\0");
    buffer.extend_from_slice(b"zsh\0-l\0");
    buffer.extend_from_slice(b"RMUX_PANE=%1\0LANG=en_US.UTF-8\0\0");

    let environment = environment_from_macos_procargs(&buffer).expect("environment");

    assert_eq!(environment.get("RMUX_PANE").map(String::as_str), Some("%1"));
    assert_eq!(
        environment.get("LANG").map(String::as_str),
        Some("en_US.UTF-8")
    );
}
