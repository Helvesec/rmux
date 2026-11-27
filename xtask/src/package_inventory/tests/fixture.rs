//! Dependency-free real Cargo workspace, including automatically discovered members.

use super::super::{metadata, Package};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub(super) struct Fixture {
    pub(super) root: PathBuf,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/tmp/package-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::create_dir(&root).unwrap();
        let fixture = Self { root };
        for (path, text) in FILES {
            fixture.write(path, text);
        }
        fixture.git(&["init", "--quiet"]);
        fixture.git(&["config", "user.name", "Package inventory fixture"]);
        fixture.git(&["config", "user.email", "fixture@example.invalid"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "--quiet", "-m", "Cargo declaration fixture"]);
        fixture
    }

    pub(super) fn write(&self, path: &str, text: &str) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    pub(super) fn replace(&self, path: &str, before: &str, after: &str) {
        let text = fs::read_to_string(self.root.join(path)).unwrap();
        assert!(
            text.contains(before),
            "fixture mutation must take effect: {before}"
        );
        self.write(path, &text.replace(before, after));
    }

    pub(super) fn git(&self, args: &[&str]) -> String {
        let result = Command::new("git")
            .current_dir(&self.root)
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    }

    pub(super) fn packages(&self) -> Vec<Package> {
        metadata::packages(metadata::read(&self.root).unwrap())
            .unwrap()
            .packages
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
}

pub(super) const FILES: &[(&str, &str)] = &[
    (
        ".gitignore",
        "/xtask/spec/package-inventory-v1.json\n/Cargo.lock\n",
    ),
    ("xtask/spec/.gitkeep", ""),
    (
        "Cargo.toml",
        r#"[workspace]
members = ["registry", "internal", "alpha"]
resolver = "2"
[workspace.package]
version = "1.2.3"
edition = "2021"
publish = false
rust-version = "1.70"
"#,
    ),
    (
        "alpha/Cargo.toml",
        r#"[package]
name = "surface-a"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
publish = true
[lib]
name = "renamed_core"
crate-type = ["rlib", "cdylib"]
[[bin]]
name = "alpha-tool"
path = "src/main.rs"
required-features = ["zeta", "alpha"]
[dependencies]
alias = { package = "helper-pkg", path = "../helper", optional = true, default-features = false }
implicit = { package = "internal-pkg", path = "../internal", optional = true }
[features]
default = ["zeta", "alpha"]
zeta = []
spare = []
alpha = ["dep:alias", "alias?/std"]
"#,
    ),
    ("alpha/src/lib.rs", "pub fn example() {}\n"),
    ("alpha/src/main.rs", "fn main() {}\n"),
    ("alpha/examples/sample.rs", "fn main() {}\n"),
    ("alpha/tests/sample.rs", "#[test] fn sample() {}\n"),
    (
        "internal/Cargo.toml",
        r#"[package]
name = "internal-pkg"
version.workspace = true
edition.workspace = true
publish.workspace = true
[lib]
proc-macro = true
"#,
    ),
    ("internal/src/lib.rs", ""),
    (
        "registry/Cargo.toml",
        r#"[package]
name = "registry-pkg"
version = "1.0.0"
edition = "2021"
publish = ["private-b", "private-a"]
[features]
default = []
"#,
    ),
    ("registry/src/main.rs", "fn main() {}\n"),
    (
        "helper/Cargo.toml",
        r#"[package]
name = "helper-pkg"
version = "1.0.0"
edition = "2021"
[features]
default = ["std"]
std = []
"#,
    ),
    ("helper/src/lib.rs", ""),
];
