//! Windows command-interpreter helpers.

use std::ffi::OsString;

/// Builds the verbatim command-line tail consumed by `cmd.exe /C`.
///
/// `cmd.exe` must receive the command text inside one outer quote pair. Passing
/// the text as a normal [`std::process::Command`] argument applies MSVC argv
/// escaping first, which changes quotes that belong to the command language.
#[must_use]
pub fn cmd_c_verbatim_tail(command: &str) -> OsString {
    format!("\"{command}\"").into()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::windows::process::CommandExt as _;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::cmd_c_verbatim_tail;

    fn run_cmd(command_text: &str) -> std::process::Output {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/S", "/C"]);
        command.raw_arg(cmd_c_verbatim_tail(command_text));
        command.output().expect("cmd.exe should start")
    }

    #[test]
    fn verbatim_tail_preserves_cmd_language_syntax() {
        let cases = [
            ("echo LEFT&echo RIGHT", &["LEFT", "RIGHT"][..]),
            (r#"echo "hello world""#, &[r#""hello world""#][..]),
            ("echo left^&right", &["left&right"][..]),
            ("echo %COMSPEC%", &["cmd.exe"][..]),
            (r#"if "a"=="a" (echo yes) else (echo no)"#, &["yes"][..]),
        ];

        for (command, expected) in cases {
            let output = run_cmd(command);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(output.status.success(), "{command:?}: {output:?}");
            for needle in expected {
                assert!(stdout.contains(needle), "{command:?}: {stdout:?}");
            }
        }

        assert!(run_cmd("").status.success());
    }

    #[test]
    fn verbatim_tail_executes_a_quoted_path_with_spaces() {
        let root = TestRoot::new("quoted executable");
        let executable = root.path().join("where probe.exe");
        let system_root = std::env::var_os("SystemRoot").expect("SystemRoot is set");
        fs::copy(
            PathBuf::from(system_root).join("System32/where.exe"),
            &executable,
        )
        .expect("copy where.exe into spaced path");
        let payload = format!(r#""{}" cmd.exe"#, executable.display());

        let output = run_cmd(&payload);
        let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();

        assert!(output.status.success(), "{payload:?}: {output:?}");
        assert!(stdout.contains("cmd.exe"), "{payload:?}: {stdout:?}");
    }

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "rmux-cmd-tail-{label}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create command test root");
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
