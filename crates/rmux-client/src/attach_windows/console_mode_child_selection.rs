//! Test-only child selection and completion evidence, independent of console I/O.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) const RECEIPT_ENV: &str = "RMUX_INTERNAL_CONSOLE_MODE_TEST_RECEIPT";

pub(super) fn exact_test_name(module: &str, short_name: &str) -> String {
    let (_, module) = module
        .split_once("::")
        .expect("test module has crate prefix");
    format!("{module}::{short_name}")
}

pub(super) struct ChildReceipt {
    path: PathBuf,
}

impl ChildReceipt {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "rmux-console-test-{}-{}.receipt",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Self { path },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create child completion receipt: {error}"),
            }
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn verify(&self, child_succeeded: bool, test_name: &str) -> io::Result<()> {
        if !child_succeeded {
            return Err(io::Error::other("child did not exit successfully"));
        }
        if fs::read(&self.path)? != format!("{test_name}\n").as_bytes() {
            return Err(io::Error::other(format!(
                "expected exactly one completed child test: {test_name}"
            )));
        }
        eprintln!("verified child completion exactly once: {test_name}");
        Ok(())
    }
}

impl Drop for ChildReceipt {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(super) fn record_completion(test_name: &str) {
    let path = std::env::var_os(RECEIPT_ENV).expect("child completion receipt path");
    append_completion(Path::new(&path), test_name).expect("write child completion receipt");
}

fn append_completion(path: &Path, test_name: &str) -> io::Result<()> {
    // Append, never truncate: a second execution must fail the parent's check.
    let mut file = OpenOptions::new().append(true).open(path)?;
    writeln!(file, "{test_name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_omit_only_the_crate_prefix() {
        assert_eq!(
            exact_test_name("renamed_crate::attach::windows::owner_tests", "child"),
            "attach::windows::owner_tests::child"
        );
    }

    #[test]
    fn receipt_requires_success_and_one_exact_completion() {
        let receipt = ChildReceipt::new();
        let name = "attach::windows::owner_tests::child";
        assert!(receipt.verify(true, name).is_err(), "zero/ignored child");
        append_completion(receipt.path(), name).unwrap();
        assert!(receipt.verify(false, name).is_err(), "failed child");
        assert!(receipt.verify(true, "other::child").is_err());
        receipt.verify(true, name).unwrap();
        append_completion(receipt.path(), name).unwrap();
        assert!(receipt.verify(true, name).is_err(), "duplicate execution");
    }

    #[test]
    fn receipts_are_isolated_and_removed_on_drop() {
        let first = ChildReceipt::new();
        let second = ChildReceipt::new();
        let path = first.path().to_owned();
        assert_ne!(path, second.path());
        append_completion(first.path(), "child").unwrap();
        assert!(second.verify(true, "child").is_err());
        drop(first);
        assert!(!path.exists());
        assert!(second.path().exists());
    }
}
