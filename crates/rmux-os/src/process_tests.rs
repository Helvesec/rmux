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

#[test]
fn utf8_environment_snapshot_skips_only_unrepresentable_entries() {
    let environment = environment_from_nul_entries(
        b"PATH=/client/bin\0BAD=foo\xffbar\0MALFORMED\0HOME=/client/home\0\0",
    )
    .expect("valid environment entries should remain available");

    assert_eq!(
        environment.get("PATH").map(String::as_str),
        Some("/client/bin")
    );
    assert_eq!(
        environment.get("HOME").map(String::as_str),
        Some("/client/home")
    );
    assert_eq!(environment.get("BAD"), None);
    assert_eq!(environment.get("MALFORMED"), None);
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

/// Expected names measured on tmux 3.7b (Linux `exec -a`, macOS executable
/// file name), except the two documented divergences at the end.
#[cfg(unix)]
#[test]
fn process_names_parse_like_tmux_window_names() {
    let measured = [
        ("my prog x", Some("my")),
        ("nginx: master process /usr/sbin/nginx", Some("nginx:")),
        ("nginx: master process nginx -g daemon off;", Some("nginx:")),
        ("gunicorn: master [app]", Some("gunicorn:")),
        ("-bash", Some("bash")),
        ("/usr/bin/my prog", Some("my")),
        ("./a.out", Some("./a.out")),
        ("bin/tool", Some("bin/tool")),
        ("/opt/x/", Some("x")),
        ("/", Some("/")),
        ("\"quoted name\" y", Some("quoted")),
        ("\"a\"b", Some("a")),
        ("x\"y z", Some("x")),
        ("\"/usr/bin/vi\"m", Some("vi")),
        ("exec foo", Some("foo")),
        ("  lead", Some("lead")),
        ("hello", Some("hello")),
        ("top\t", Some("top")),
        ("ab\u{1}\u{2}", Some("ab")),
        // Divergences: tmux shows an empty name and trims `café` to `caf`.
        ("-", None),
        ("\u{1}", None),
        ("café x", Some("café")),
    ];
    for (process_name, expected) in measured {
        assert_eq!(
            window_name_from_process_name(process_name).as_deref(),
            expected,
            "process name {process_name:?}"
        );
    }
}

/// The pane's foreground process reports `my prog x`: as argv[0] on Linux and
/// as its executable file name on macOS. tmux 3.7b names it `my` on both.
#[cfg(unix)]
#[test]
fn command_name_keeps_only_the_first_word_of_a_process_title() {
    let child = TitledSleep::spawn("rmux-os-title", "my prog x");
    let name = child.command_name();
    assert_eq!(name.as_deref(), Some("my"));
}

/// macOS names the executable file, so `proc_pidpath` must lose its
/// directory before the name is parsed: a space in a directory would
/// otherwise end the first word there and name the process `rmux-os`.
#[cfg(target_os = "macos")]
#[test]
fn command_name_ignores_a_space_in_the_executable_directory() {
    let child = TitledSleep::spawn("rmux-os spaced dir", "sleeper");
    let name = child.command_name();
    assert_eq!(name.as_deref(), Some("sleeper"));
}

/// A `sleep` child that reports `title` as its process name, killed, reaped
/// and cleaned up on drop.
///
/// Linux reports argv[0]: the system `sleep` runs under that argv[0] and is
/// never copied, so no freshly written executable can fail with ETXTBSY while
/// another test thread forks. macOS reports the executable file name, so
/// `sleep` is copied to `title` inside a directory of its own.
#[cfg(unix)]
struct TitledSleep {
    child: std::process::Child,
    copy: Option<PathBuf>,
}

#[cfg(unix)]
impl TitledSleep {
    fn spawn(dir_name: &str, title: &str) -> Self {
        use std::os::unix::process::CommandExt;

        let sleep = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .map(std::path::Path::new)
            .find(|path| path.exists())
            .expect("a sleep binary");
        let copy = cfg!(target_os = "macos").then(|| {
            let dir = std::env::temp_dir().join(format!("{dir_name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create the titled sleep directory");
            let copy = dir.join(title);
            std::fs::copy(sleep, &copy).expect("copy sleep");
            copy
        });
        let child = std::process::Command::new(copy.as_deref().unwrap_or(sleep))
            .arg0(title)
            .arg("30")
            .spawn()
            .expect("spawn the titled sleep");
        let titled = Self { child, copy };
        titled.wait_for_exec(title);
        titled
    }

    /// Waits until the child reports the new image's own record, read
    /// without the parser under test: Rust's spawn can return before Linux
    /// has published the new argv, which then reads empty, cut short (`targ`)
    /// or as the test binary that forked it.
    fn wait_for_exec(&self, title: &str) {
        let pid = self.child.id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !Self::exec_completed(pid, title) {
            assert!(
                std::time::Instant::now() < deadline,
                "the titled sleep never finished its exec"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(target_os = "linux")]
    fn exec_completed(pid: u32, title: &str) -> bool {
        std::fs::read(format!("/proc/{pid}/cmdline"))
            .is_ok_and(|cmdline| cmdline == format!("{title}\x0030\x00").as_bytes())
    }

    #[cfg(target_os = "macos")]
    fn exec_completed(pid: u32, title: &str) -> bool {
        executable_path(pid).is_some_and(|path| {
            std::path::Path::new(&path).file_name() == Some(std::ffi::OsStr::new(title))
        })
    }

    fn command_name(&self) -> Option<String> {
        command_name(self.child.id())
    }
}

#[cfg(unix)]
impl Drop for TitledSleep {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(copy) = &self.copy {
            let _ = std::fs::remove_file(copy);
            if let Some(dir) = copy.parent() {
                let _ = std::fs::remove_dir(dir);
            }
        }
    }
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
