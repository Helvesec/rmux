//! D392/D413 output contract, also runnable with `rustc --test` without a daemon.
use std::process::Output;

pub fn assert_startup_output(
    output: &Output,
    expected_code: i32,
    expected_stderr: &[u8],
    windows_startup: bool,
) {
    // Preserve the evidence even when the expected output or exit code fails.
    eprintln!(
        "startup stderr raw={:?}\nstartup stderr text={}",
        output.stderr,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(expected_code), "{output:?}");
    // D392 permits one exact launch warning, never arbitrary diagnostic output.
    // Existing-daemon commands and Unix callers retain their strict contract.
    const JOB_BOUND_WARNING: &[u8] =
        b"rmux: warning: daemon is job-bound; an enclosing job may terminate it\n";
    let stderr = if windows_startup {
        output
            .stderr
            .strip_prefix(JOB_BOUND_WARNING)
            .unwrap_or(&output.stderr)
    } else {
        &output.stderr
    };
    assert_eq!(stderr, expected_stderr, "{output:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    const WARNING: &[u8] =
        b"rmux: warning: daemon is job-bound; an enclosing job may terminate it\n";

    fn output(code: i32, stderr: &[u8]) -> Output {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;
        #[cfg(unix)]
        let status = std::process::ExitStatus::from_raw(code << 8);
        #[cfg(windows)]
        let status = std::process::ExitStatus::from_raw(code as u32);
        Output {
            status,
            stdout: Vec::new(),
            stderr: stderr.to_vec(),
        }
    }

    fn rejects(code: i32, stderr: &[u8], expected_code: i32, expected: &[u8], startup: bool) {
        assert!(
            std::panic::catch_unwind(|| {
                assert_startup_output(&output(code, stderr), expected_code, expected, startup);
            })
            .is_err(),
            "unexpectedly accepted stderr={stderr:?}, code={code}"
        );
    }

    #[test]
    fn absent_warning_preserves_success_and_no_sessions() {
        for startup in [false, true] {
            assert_startup_output(&output(0, b""), 0, b"", startup);
            assert_startup_output(&output(1, b"no sessions\n"), 1, b"no sessions\n", startup);
        }
    }

    #[test]
    fn exact_warning_is_allowed_for_windows_startup_success() {
        assert_startup_output(&output(0, WARNING), 0, b"", true);
    }

    #[test]
    fn exact_warning_preserves_no_sessions_and_exit_one() {
        let stderr = [WARNING, b"no sessions\n"].concat();
        assert_startup_output(&output(1, &stderr), 1, b"no sessions\n", true);
    }

    #[test]
    fn warning_is_rejected_for_preexisting_daemon_or_unix() {
        rejects(0, WARNING, 0, b"", false);
        rejects(
            1,
            &[WARNING, b"no sessions\n"].concat(),
            1,
            b"no sessions\n",
            false,
        );
    }

    #[test]
    fn unknown_malformed_duplicate_or_non_utf8_stderr_is_rejected() {
        for stderr in [
            b"rmux: warning: another warning\n".to_vec(),
            WARNING[..WARNING.len() - 1].to_vec(),
            [b" ", WARNING].concat(),
            [WARNING, b"\n"].concat(),
            [WARNING, WARNING].concat(),
            [WARNING, b"\xff"].concat(),
            [WARNING, b"error: PermissionDenied\n"].concat(),
            b"error: PermissionDenied\n".to_vec(),
        ] {
            rejects(0, &stderr, 0, b"", true);
        }
    }

    #[test]
    fn warning_does_not_hide_real_failure_or_missing_no_sessions() {
        rejects(1, WARNING, 0, b"", true);
        rejects(
            0,
            &[WARNING, b"no sessions\n"].concat(),
            1,
            b"no sessions\n",
            true,
        );
        rejects(1, WARNING, 1, b"no sessions\n", true);
        rejects(
            1,
            &[b"no sessions\n", WARNING].concat(),
            1,
            b"no sessions\n",
            true,
        );
    }
}
