#![cfg(windows)]

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmux_core::command_inventory::RMUX_EXTENSION_COMMANDS;
use rmux_core::command_parser::COMMAND_TABLE;

const SHORT_USAGE: &str = "usage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]\n";

type CliResult = (i32, String, String);

// Windows has no argv0 override, so the tmux personality is reached through a
// copy of the candidate executable named tmux.exe, as an installed shim would be.
struct HelpCli {
    root: PathBuf,
    label: String,
}

impl HelpCli {
    fn new(prefix: &str) -> Result<Self, Box<dyn Error>> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let label = format!("{prefix}-{}-{nanos}", std::process::id());
        let root = std::env::temp_dir().join(&label);
        fs::create_dir_all(&root)?;
        Ok(Self { root, label })
    }

    fn rmux_binary() -> &'static Path {
        Path::new(rmux_os::build_identity::cargo_binary(env!(
            "CARGO_BIN_EXE_rmux"
        )))
    }

    fn tmux_shim(&self) -> Result<PathBuf, Box<dyn Error>> {
        let shim = self.root.join("tmux.exe");
        fs::copy(Self::rmux_binary(), &shim)?;
        Ok(shim)
    }

    fn run(&self, program: &Path, args: &[&str]) -> Result<CliResult, Box<dyn Error>> {
        let absent_daemon = self.root.join("absent-daemon.exe");
        let mut child = Command::new(program)
            .arg("-L")
            .arg(&self.label)
            .args(args)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("RMUX")
            .env_remove("RMUX_INTERNAL_INVOKED_AS_TMUX")
            .env("RMUX_DISABLE_TMUX_FALLBACK", "1")
            .env("RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE", "1")
            .env("RMUX_INTERNAL_BINARY_PATH", &absent_daemon)
            .env("RMUX_DAEMON_BINARY_PATH", &absent_daemon)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                return Err(format!("help CLI timed out: {program:?} {args:?}").into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output()?;
        let code = output
            .status
            .code()
            .ok_or("help CLI exited without a code")?;
        Ok((
            code,
            String::from_utf8(output.stdout)?,
            String::from_utf8(output.stderr)?,
        ))
    }
}

impl Drop for HelpCli {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn section_roots(section: &str) -> Vec<&str> {
    section
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect()
}

#[test]
fn windows_native_long_help_lists_commands_on_stdout() -> Result<(), Box<dyn Error>> {
    let cli = HelpCli::new("gh195-native-help")?;
    let (code, out, err) = cli.run(HelpCli::rmux_binary(), &["--help"])?;
    assert_eq!(code, 0, "native --help is a successful CLI request");
    assert!(err.is_empty(), "stderr={err:?}");
    let (preamble, commands) = out
        .split_once("\nCommands:\n")
        .ok_or("native --help has no Commands section")?;
    let (usage, extensions) = preamble
        .split_once("\nRMUX extensions:\n")
        .ok_or("native --help has no RMUX extensions section")?;
    assert_eq!(usage, SHORT_USAGE);

    let command_roots = section_roots(commands);
    assert_eq!(command_roots.len(), COMMAND_TABLE.len());
    assert_eq!(
        command_roots.into_iter().collect::<BTreeSet<_>>(),
        COMMAND_TABLE.iter().map(|entry| entry.name).collect()
    );
    let extension_roots = section_roots(extensions);
    let expected_extensions = RMUX_EXTENSION_COMMANDS
        .iter()
        .map(|entry| entry.name)
        .chain(["diagnose"])
        .collect::<BTreeSet<_>>();
    assert_eq!(extension_roots.len(), expected_extensions.len());
    assert_eq!(
        extension_roots.into_iter().collect::<BTreeSet<_>>(),
        expected_extensions
    );

    let list = cli.run(HelpCli::rmux_binary(), &["list-commands"])?;
    assert_eq!(list, (0, commands.to_owned(), String::new()));
    Ok(())
}

#[test]
fn windows_tmux_shim_long_help_uses_stderr_and_exit_one() -> Result<(), Box<dyn Error>> {
    let cli = HelpCli::new("gh195-tmux-help")?;
    let native = cli.run(HelpCli::rmux_binary(), &["--help"])?;
    assert_eq!(native.0, 0);
    let shim = cli.tmux_shim()?;
    assert_eq!(
        cli.run(&shim, &["--help"])?,
        (1, String::new(), native.1),
        "tmux personality keeps tmux's failing exit (accepted divergence C-D86)"
    );
    Ok(())
}
