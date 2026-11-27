use super::*;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::time::Duration;
use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

#[test]
fn windows_job_argv_rejects_nul_and_windows_length_overflow() {
    for program in ["", "bad\0.exe", "bad\".exe"] {
        assert_eq!(
            launch_hidden_daemon(OsStr::new(program), ["unused"])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    let exe = std::env::current_exe().unwrap();
    for arg in ["bad\0argument".to_owned(), "x".repeat(32_768)] {
        assert_eq!(
            launch_hidden_daemon(exe.as_os_str(), [arg])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn windows_job_missing_executable_preserves_native_error() {
    let missing = std::env::current_exe()
        .unwrap()
        .with_file_name("rmux-d392-does-not-exist.exe");
    let error = launch_hidden_daemon(missing.as_os_str(), ["unused"]).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(2));
}

#[test]
fn windows_job_executable_search_preserves_bare_names_and_optional_exe() {
    let exe = std::env::current_exe().unwrap();
    let expected = executable::resolve(exe.as_os_str()).unwrap();
    assert_eq!(
        executable::resolve(exe.file_name().unwrap()).unwrap(),
        expected
    );
    assert_eq!(
        executable::resolve(exe.file_stem().unwrap()).unwrap(),
        expected
    );
    assert_eq!(
        executable::resolve(exe.with_extension("").as_os_str()).unwrap(),
        expected
    );
    assert_eq!(
        launch_hidden_daemon(OsStr::new("C:\\"), ["unused"])
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn windows_job_argv_preserves_utf16_and_crt_quote_boundaries() {
    let raw = OsString::from_wide(&[120, 0xd800, 92]);
    let result = command_line(OsStr::new(r"C:\a b\daemon.exe"), [&raw]).unwrap();
    let mut expected: Vec<u16> = r#""C:\a b\daemon.exe" ""#.encode_utf16().collect();
    expected.extend([120, 0xd800, 92, 92, 34, 0]);
    assert_eq!(result, expected);
    let result = command_line(OsStr::new("daemon.exe"), ["", "a\"b", r"end\"]).unwrap();
    assert_eq!(
        String::from_utf16(&result[..result.len() - 1]).unwrap(),
        r#""daemon.exe" "" "a\"b" "end\\""#
    );
}

const ARG_READY: &str = "RMUX_D392_ARG_READY";
fn argument_samples() -> [&'static str; 6] {
    [
        "",
        "tab\tspace é",
        r#"embedded"quote"#,
        r"trailing\",
        r#"slash\"quote"#,
        "a&b",
    ]
}

#[test]
fn windows_job_argv_round_trip_child() {
    let Some(ready) = std::env::var_os(ARG_READY) else {
        return;
    };
    let args: Vec<_> = std::env::args_os().collect();
    let received: Vec<_> = args
        .windows(2)
        .filter(|pair| pair[0] == "--skip")
        .map(|pair| OsString::from(pair[1].to_str().unwrap().strip_prefix("!probe:").unwrap()))
        .collect();
    assert_eq!(received, argument_samples().map(OsString::from));
    super::super::signal_startup_ready_event(&ready).unwrap();
    std::process::exit(0);
}

#[test]
fn windows_job_argv_round_trips_through_native_process_creation() {
    const LAUNCHER: &str = "RMUX_D392_ARG_LAUNCHER";
    if std::env::var_os(LAUNCHER).is_some() {
        let mut args: Vec<OsString> = [
            "daemon::windows_launch::tests::windows_job_argv_round_trip_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ]
        .map(Into::into)
        .to_vec();
        for sample in argument_samples() {
            args.extend(["--skip".into(), format!("!probe:{sample}").into()]);
        }
        let child =
            launch_hidden_daemon(std::env::current_exe().unwrap().as_os_str(), args).unwrap();
        // SAFETY: this child owns the process handle for the whole wait.
        assert_eq!(
            unsafe { WaitForSingleObject(child._process.as_raw_handle(), 5_000) },
            WAIT_OBJECT_0
        );
        let mut code = 0;
        assert_ne!(
            unsafe { GetExitCodeProcess(child._process.as_raw_handle(), &mut code) },
            0
        );
        assert_eq!(code, 0);
        return;
    }
    let ready = super::super::StartupReadyEvent::new().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["daemon::windows_launch::tests::windows_job_argv_round_trips_through_native_process_creation", "--exact", "--nocapture", "--test-threads=1"])
        .env(LAUNCHER, "1").env(ARG_READY, ready.name()).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(ready.wait(Duration::from_secs(1)).unwrap());
}

#[test]
fn windows_job_breakaway_child() {
    let Some(ready) = std::env::var_os("RMUX_D392_BREAK_READY") else {
        return;
    };
    super::super::signal_startup_ready_event(&ready).unwrap();
    let name =
        super::super::wide_event_name(&std::env::var_os("RMUX_D392_BREAK_STOP").unwrap()).unwrap();
    // SAFETY: valid named event, owned handle with synchronization access.
    let stop = unsafe {
        windows_sys::Win32::System::Threading::OpenEventW(
            windows_sys::Win32::System::Threading::SYNCHRONIZATION_SYNCHRONIZE,
            0,
            name.as_ptr(),
        )
    };
    assert!(!stop.is_null());
    let stop = unsafe { OwnedHandle::from_raw_handle(stop) };
    unsafe { WaitForSingleObject(stop.as_raw_handle(), 10_000) };
    std::process::exit(0);
}

#[test]
fn windows_job_breakaway_removes_only_jobs_that_permit_it() {
    use std::process::{Command, Stdio};
    use windows_sys::Win32::System::JobObjects::*;
    const TEST: &str =
        "daemon::windows_launch::tests::windows_job_breakaway_removes_only_jobs_that_permit_it";
    if std::env::var_os("RMUX_D392_BREAK_LAUNCHER").is_some() {
        let mut line = String::new();
        io::stdin().read_line(&mut line).unwrap();
        let args = [
            "daemon::windows_launch::tests::windows_job_breakaway_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ];
        let child =
            launch_hidden_daemon(std::env::current_exe().unwrap().as_os_str(), args).unwrap();
        println!("DAEMON_PID={}", child.id());
        return;
    }
    for nested in [false, true] {
        let ready = super::super::StartupReadyEvent::new().unwrap();
        let stop = super::super::StartupReadyEvent::new().unwrap();
        let make_job = |flags| {
            // SAFETY: create and own an unnamed job and initialize its limits.
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            assert!(!raw.is_null());
            let job = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = flags;
            assert_ne!(
                unsafe {
                    SetInformationJobObject(
                        raw,
                        JobObjectExtendedLimitInformation,
                        (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        std::mem::size_of_val(&info) as u32,
                    )
                },
                0
            );
            job
        };
        let outer = nested.then(|| make_job(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE));
        let inner = make_job(JOB_OBJECT_LIMIT_BREAKAWAY_OK);
        let mut launcher = Command::new(std::env::current_exe().unwrap())
            .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
            .env("RMUX_D392_BREAK_LAUNCHER", "1")
            .env("RMUX_D392_BREAK_READY", ready.name())
            .env("RMUX_D392_BREAK_STOP", stop.name())
            // The production policy is identical in debug and release, including
            // when a legacy hosted-runner override is present.
            .env("RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        for job in outer.iter().chain(std::iter::once(&inner)) {
            assert_ne!(
                unsafe { AssignProcessToJobObject(job.as_raw_handle(), launcher.as_raw_handle()) },
                0
            );
        }
        launcher.stdin.take().unwrap().write_all(b"\n").unwrap();
        let output = launcher.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(ready.wait(Duration::from_secs(2)).unwrap());
        let text = String::from_utf8_lossy(&output.stdout);
        let pid = text
            .lines()
            .find_map(|line| {
                line.split_once("DAEMON_PID=")
                    .map(|(_, value)| value.parse::<u32>().unwrap())
            })
            .unwrap();
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, SYNCHRONIZATION_SYNCHRONIZE,
        };
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZATION_SYNCHRONIZE,
                0,
                pid,
            )
        };
        assert!(!raw.is_null());
        let daemon = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut belongs = 0;
        assert_ne!(
            unsafe { IsProcessInJob(raw, inner.as_raw_handle(), &mut belongs) },
            0
        );
        assert_eq!(belongs, 0, "breakaway must be attempted before fallback");
        if let Some(outer) = &outer {
            assert_ne!(
                unsafe { IsProcessInJob(raw, outer.as_raw_handle(), &mut belongs) },
                0
            );
            assert_ne!(
                belongs, 0,
                "must remain in the ancestor forbidding breakaway"
            );
        }
        let bound = process_is_job_bound(raw).unwrap();
        assert_eq!(ready.is_job_bound().unwrap(), bound);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.lines().count(), usize::from(bound), "{stderr}");
        if bound {
            assert!(stderr.contains("job-bound"));
        }
        assert_eq!(unsafe { WaitForSingleObject(raw, 0) }, WAIT_TIMEOUT);
        super::super::signal_startup_ready_event(stop.name()).unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(daemon.as_raw_handle(), 2_000) },
            WAIT_OBJECT_0
        );
    }
}
