#![cfg(windows)]

// C-D85 on Windows: the Unix CLI harnesses (cli_surface, socket_dropin) are
// compiled out here, so this target runs the real binary for the same contract.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOP_LEVEL_USAGE: &str = "usage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]\n";

fn rmux_binary() -> &'static Path {
    Path::new(rmux_os::build_identity::cargo_binary(env!(
        "CARGO_BIN_EXE_rmux"
    )))
}

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let path =
            std::env::temp_dir().join(format!("rmux-long-version-{label}-{}", std::process::id()));
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(program: &Path, args: &[&str], scratch: &ScratchDir, invoked_as_tmux: bool) -> Output {
    let mut command = Command::new(program);
    command
        .args(args)
        .env("RMUX_TMPDIR", &scratch.0)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    if invoked_as_tmux {
        command.env("RMUX_INTERNAL_INVOKED_AS_TMUX", "1");
    } else {
        command.env_remove("RMUX_INTERNAL_INVOKED_AS_TMUX");
    }
    command.output().expect("rmux binary must start")
}

#[test]
fn native_long_version_product_divergence_matches_short_version_windows(
) -> Result<(), Box<dyn Error>> {
    let scratch = ScratchDir::new("native")?;
    let short = run(rmux_binary(), &["-V"], &scratch, false);
    assert!(short.status.success(), "-V: {short:?}");
    assert_eq!(
        String::from_utf8(short.stdout.clone())?,
        format!("rmux {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(short.stderr.is_empty(), "-V: {short:?}");
    let check = |args: &[&str]| {
        let long = run(rmux_binary(), args, &scratch, false);
        assert_eq!(long.status, short.status, "{args:?}");
        assert_eq!(long.stdout, short.stdout, "{args:?}");
        assert_eq!(long.stderr, short.stderr, "{args:?}");
    };
    check(&["--version"]);
    check(&["-u", "--version"]);
    for later in [
        "-V",
        "-x",
        "-h",
        "--help",
        "--nope",
        "--",
        "list-sessions",
        "",
    ] {
        check(&["--version", later]);
    }
    for earlier in ["-h", "--help", "-x", "--nope"] {
        let expected = run(rmux_binary(), &[earlier, "-V"], &scratch, false);
        let actual = run(rmux_binary(), &[earlier, "--version"], &scratch, false);
        assert_eq!(actual.status, expected.status, "{earlier}");
        assert_eq!(actual.stdout, expected.stdout, "{earlier}");
        assert_eq!(actual.stderr, expected.stderr, "{earlier}");
    }
    for flag in ['L', 'S', 'f', 'T', 'c'] {
        for prefix in [format!("-{flag}"), format!("-v{flag}")] {
            check(&[&prefix, "--version", "-V"]);
            check(&[&prefix, "value", "--version"]);
            let consumed = run(
                rmux_binary(),
                &[&prefix, "--version", "-x"],
                &scratch,
                false,
            );
            assert_eq!(consumed.status.code(), Some(1), "{prefix}");
            assert_eq!(
                String::from_utf8(consumed.stderr)?,
                format!("rmux: unknown option -- x\n{TOP_LEVEL_USAGE}"),
                "{prefix}"
            );
            assert!(consumed.stdout.is_empty(), "{prefix}");
            check(&[&format!("{prefix}--version"), "--version"]);
        }
    }
    Ok(())
}

#[test]
fn tmux_personality_keeps_long_version_rejection_windows() -> Result<(), Box<dyn Error>> {
    let scratch = ScratchDir::new("personality")?;
    let shim = scratch.0.join("tmux.exe");
    fs::copy(rmux_binary(), &shim)?;
    let rejected = run(rmux_binary(), &["--nope"], &scratch, false);
    assert!(!rejected.status.success(), "--nope: {rejected:?}");
    for args in [
        &["--version"][..],
        &["--version", "-V"][..],
        &["-u", "--version"][..],
    ] {
        for (program, invoked_as_tmux) in [(shim.as_path(), false), (rmux_binary(), true)] {
            let actual = run(program, args, &scratch, invoked_as_tmux);
            assert_eq!(actual.status, rejected.status, "{program:?} {args:?}");
            assert_eq!(actual.stdout, rejected.stdout, "{program:?} {args:?}");
            assert_eq!(actual.stderr, rejected.stderr, "{program:?} {args:?}");
        }
    }
    Ok(())
}
