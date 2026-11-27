#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static NEXT_PROJECT_ID: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
#[path = "native_consumer_output.rs"]
mod native_consumer_output;

// One boundary for the three Rust-owned consumer launches.
pub(crate) fn consumer_output(command: &mut Command, case: &str) -> std::io::Result<Output> {
    #[cfg(windows)]
    if native_consumer_output::enabled(command) {
        return native_consumer_output::output(command, case);
    }
    #[cfg(not(windows))]
    let _ = case;
    command.output()
}

pub(crate) struct CompileFailCase {
    name: &'static str,
    source: String,
    error_code: &'static str,
    diagnostic: &'static str,
}

impl CompileFailCase {
    #[allow(dead_code)]
    pub(crate) fn enum_match(name: &'static str, source: impl Into<String>) -> Self {
        Self {
            name,
            source: source.into(),
            error_code: "error[E0004]",
            diagnostic: "is marked as non-exhaustive",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn struct_literal(name: &'static str, source: impl Into<String>) -> Self {
        Self {
            name,
            source: source.into(),
            error_code: "error[E0639]",
            diagnostic: "non-exhaustive struct",
        }
    }
}

pub(crate) fn assert_compile_fail_cases(
    dependency: &str,
    manifest_dir: &Path,
    cases: &[CompileFailCase],
) {
    let project = ConsumerProject::new(dependency, manifest_dir, cases);
    let cargo_executable = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let identity_session = project.qualify(dependency, manifest_dir);

    for case in cases {
        let mut command = Command::new(&cargo_executable);
        command
            .args([
                "check",
                "--offline",
                "--quiet",
                "--color=never",
                "--bin",
                case.name,
            ])
            .current_dir(&project.root)
            .env("CARGO_TARGET_DIR", project.root.join("target"))
            .env("CARGO_NET_OFFLINE", "true");
        if let Some(session) = &identity_session {
            project.check(session);
            command
                .arg("--locked")
                .env("RMUX_IDENTITY_DOC_SESSION", session);
        }
        let output = consumer_output(&mut command, case.name).unwrap_or_else(|error| {
            panic!(
                "failed to launch cargo for compile-fail case `{}`: {error}",
                case.name
            )
        });
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            !output.status.success(),
            "consumer case `{}` compiled; its #[non_exhaustive] contract may have disappeared",
            case.name
        );
        assert!(
            stderr.contains(case.error_code) && stderr.contains(case.diagnostic),
            "consumer case `{}` failed for the wrong reason:\n{stderr}",
            case.name
        );
    }
}

struct ConsumerProject {
    root: PathBuf,
}

impl ConsumerProject {
    fn new(dependency: &str, manifest_dir: &Path, cases: &[CompileFailCase]) -> Self {
        let project_id = NEXT_PROJECT_ID.fetch_add(1, Ordering::Relaxed);
        let base = std::env::var_os("RMUX_IDENTITY_TARGET")
            .map(|target| PathBuf::from(target).join("consumer-inputs"))
            .unwrap_or_else(std::env::temp_dir);
        fs::create_dir_all(&base).expect("create compile-fail project parent");
        let root = base.join(format!(
            "{dependency}-public-api-compile-fail-{}-{project_id}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create fresh compile-fail project");
        let bin_dir = root.join("src").join("bin");
        fs::create_dir_all(&bin_dir).unwrap_or_else(|error| {
            panic!(
                "failed to create compile-fail project at {}: {error}",
                root.display()
            )
        });

        let dependency_path = toml_string(manifest_dir);
        let manifest = format!(
            "[package]\n\
             name = \"{dependency}-public-api-compile-fail\"\n\
             version = \"0.0.0\"\n\
             edition = \"2021\"\n\
             publish = false\n\
             \n\
             [workspace]\n\
             \n\
             [dependencies]\n\
             {dependency} = {{ path = \"{dependency_path}\" }}\n"
        );
        fs::write(root.join("Cargo.toml"), manifest).unwrap_or_else(|error| {
            panic!(
                "failed to write compile-fail manifest at {}: {error}",
                root.display()
            )
        });
        for case in cases {
            fs::write(bin_dir.join(format!("{}.rs", case.name)), &case.source).unwrap_or_else(
                |error| {
                    panic!(
                        "failed to write compile-fail case `{}` at {}: {error}",
                        case.name,
                        root.display()
                    )
                },
            );
        }

        Self { root }
    }

    fn identity_command(&self) -> Option<Command> {
        let python = std::env::var_os("RMUX_IDENTITY_PYTHON")?;
        let script = std::env::var_os("RMUX_IDENTITY_SCRIPT")
            .expect("qualified consumer requires identity script");
        let mut command = Command::new(python);
        if cfg!(windows) && std::env::var_os("RMUX_NATIVE_INPUT_MANIFEST").is_some() {
            command.args(["-I", "-S", "-B"]);
        }
        command.arg(script).arg("consumer");
        Some(command)
    }

    fn qualify(&self, dependency: &str, manifest_dir: &Path) -> Option<String> {
        let mut command = self.identity_command()?;
        command
            .args(["prepare"])
            .arg(&self.root)
            .arg(manifest_dir)
            .arg(dependency);
        let output =
            consumer_output(&mut command, "consumer-prepare").expect("launch consumer attribution");
        assert!(
            output.status.success(),
            "consumer attribution failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(
            String::from_utf8(output.stdout)
                .expect("consumer session encoding")
                .trim()
                .to_owned(),
        )
    }

    fn check(&self, session: &str) {
        let mut command = self.identity_command().expect("qualified consumer checker");
        command
            .arg("check")
            .env("RMUX_IDENTITY_DOC_SESSION", session)
            .env("CARGO_TARGET_DIR", self.root.join("target"));
        let output =
            consumer_output(&mut command, "consumer-check").expect("launch consumer check");
        assert!(
            output.status.success(),
            "consumer check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for ConsumerProject {
    fn drop(&mut self) {
        if std::env::var_os("RMUX_IDENTITY_TARGET").is_some() {
            return;
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn toml_string(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}
