#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use std::path::{Path, PathBuf};
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

#[cfg(any(unix, windows))]
pub(crate) fn command_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(unix)]
pub(crate) fn sh_quote(value: &str) -> String {
    command_quote(value)
}

#[cfg(unix)]
pub(crate) fn sh_quote_path(path: &Path) -> String {
    sh_quote(&path.display().to_string())
}

#[cfg(any(unix, windows))]
pub(crate) fn stdin_discard_command() -> String {
    platform_stdin_discard_command()
}

#[cfg(unix)]
fn platform_stdin_discard_command() -> String {
    // Environment tests may replace PATH with one that cannot resolve cat.
    // Keep the fixture alive independently of the environment under test.
    "/bin/cat >/dev/null".to_owned()
}

#[cfg(unix)]
#[test]
fn stdin_discard_command_does_not_depend_on_path() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("/bin/sh")
        .args(["-c", &stdin_discard_command()])
        .env("PATH", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn quiet fixture with an empty PATH");
    let mut input = child.stdin.take().expect("piped fixture input");
    let written = input.write_all(b"discard this input\n");
    drop(input);
    let output = child.wait_with_output().expect("fixture exits after EOF");
    assert!(output.status.success(), "quiet fixture failed: {output:?}");
    written.expect("fixture consumed its input");
    assert!(output.stdout.is_empty(), "fixture must not echo input");
    assert!(
        output.stderr.is_empty(),
        "fixture must not emit diagnostics"
    );
}

#[cfg(windows)]
fn platform_stdin_discard_command() -> String {
    powershell_encoded_command(
        "$inputStream=[Console]::OpenStandardInput(); $inputStream.CopyTo([System.IO.Stream]::Null)",
    )
}

#[cfg(windows)]
pub(crate) fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(windows)]
pub(crate) fn powershell_quote_path(path: &Path) -> String {
    powershell_quote(&path.display().to_string())
}

#[cfg(windows)]
pub(crate) fn powershell_encoded_command(script: &str) -> String {
    format!(
        "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
        encode_powershell_script(script)
    )
}

#[cfg(windows)]
fn encode_powershell_script(script: &str) -> String {
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    base64_encode(&bytes)
}

#[cfg(windows)]
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        let value = ((first as u32) << 16) | ((second as u32) << 8) | third as u32;
        encoded.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        encoded.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            encoded.push(TABLE[((value >> 6) & 0x3f) as usize] as char);
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(TABLE[(value & 0x3f) as usize] as char);
        } else {
            encoded.push('=');
        }
    }
    encoded
}

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A path a test owns outright, removed with everything under it when the
/// guard drops: at the end of a passing test and while a failing one unwinds.
///
/// Tests reach the path through `Deref<Target = Path>`, so `root.join(..)` and
/// `&root` read exactly as they did on the bare `PathBuf` this replaces.
#[derive(Debug)]
pub(crate) struct ScratchPath {
    path: PathBuf,
}

impl ScratchPath {
    /// A fresh directory under the process temp directory, created exclusively
    /// under a name no other test, process or earlier run can hold.
    pub(crate) fn directory(label: &str) -> Self {
        let scratch = Self::reserve(label, "");
        std::fs::create_dir(&scratch.path).unwrap_or_else(|error| {
            panic!(
                "scratch directory '{}' is created exclusively: {error}",
                scratch.path.display()
            )
        });
        scratch
    }

    /// A unique path under the process temp directory that does not exist yet;
    /// the test creates it as a file or a tree, and the guard removes either.
    pub(crate) fn reserve(label: &str, suffix: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self::adopt(std::env::temp_dir().join(format!(
            "rmux-{label}-{}-{sequence}-{nanos}{suffix}",
            std::process::id()
        )))
    }

    /// Takes ownership of a path a fixture names itself.
    pub(crate) fn adopt(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for ScratchPath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for ScratchPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchPath {
    fn drop(&mut self) {
        // A symlink is removed itself, never followed into what it names.
        let _ = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&self.path),
            Ok(_) => std::fs::remove_file(&self.path),
            Err(_) => Ok(()),
        };
    }
}

#[cfg(test)]
mod scratch_path_tests {
    use super::ScratchPath;

    #[test]
    fn scratch_tree_is_removed_when_the_test_passes() {
        let scratch = ScratchPath::directory("scratch-pass");
        std::fs::create_dir_all(scratch.join("nested/deeper")).expect("nested tree");
        std::fs::write(scratch.join("nested/deeper/file"), b"x").expect("nested file");
        let path = scratch.to_path_buf();

        drop(scratch);

        assert!(!path.exists(), "{} survived its guard", path.display());
    }

    #[test]
    fn scratch_tree_is_removed_while_a_failing_test_unwinds() {
        let mut kept = None;
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let scratch = ScratchPath::directory("scratch-panic");
            std::fs::write(scratch.join("file"), b"x").expect("scratch file");
            kept = Some(scratch.to_path_buf());
            panic!("the test body fails while the scratch is live");
        }));

        assert!(unwound.is_err(), "the body must have panicked");
        let path = kept.expect("the body recorded its scratch path");
        assert!(!path.exists(), "{} survived the unwind", path.display());
    }

    #[test]
    fn reserved_file_is_removed_and_a_missing_path_is_not_an_error() {
        let file = ScratchPath::reserve("scratch-file", ".txt");
        assert!(!file.exists(), "a reserved path starts absent");
        std::fs::write(&*file, b"x").expect("reserved file");
        let path = file.to_path_buf();
        drop(file);
        assert!(!path.exists(), "{} survived its guard", path.display());

        drop(ScratchPath::reserve("scratch-never-created", ""));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_removed_without_following_it() {
        let target = ScratchPath::directory("scratch-link-target");
        std::fs::write(target.join("kept"), b"x").expect("target file");
        let link = ScratchPath::reserve("scratch-link", "");
        std::os::unix::fs::symlink(&*target, &*link).expect("symlink");

        drop(link);

        assert!(target.join("kept").exists(), "the guard followed the link");
    }

    #[test]
    fn two_reservations_with_one_label_never_share_a_path() {
        let first = ScratchPath::reserve("scratch-unique", "");
        let second = ScratchPath::reserve("scratch-unique", "");
        assert_ne!(first.path(), second.path());
    }
}

/// The real-child final-sink harness.
///
/// Split out of this module because it carries a slot protocol, two child
/// scripts and their diagnostics, none of which the shell-quoting helpers
/// above need to know about.
#[cfg(any(unix, windows))]
pub(crate) mod final_sink;
