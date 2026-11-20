use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

mod process;

static NEXT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn only_the_repository_task_alias_is_inert_configuration() {
    if process::isolated() {
        return;
    }
    let fixture = Fixture::new("task-alias");
    let directory = fixture.runner.root.join(".cargo");
    fs::create_dir(&directory).unwrap();
    let file = directory.join("config.toml");
    let valid = "[alias]\nxtask = 'run --locked -p xtask --'\n";
    fs::write(&file, valid).unwrap();
    let command = fixture.metadata();
    assert!(context::check(&command).is_ok());
    for invalid in [
        valid.replace("run --locked", "run"),
        format!("{valid}[build]\nrustflags = ['--cfg', 'unexpected']\n"),
        format!("{valid}extra = 'check'\n"),
    ] {
        fs::write(&file, invalid).unwrap();
        assert!(context::check(&command).is_err());
    }
}

#[test]
fn fixtures_never_reopen_an_area_left_by_a_recycled_process_id() {
    if process::isolated() {
        return;
    }
    // target/ outlives a run: CI restores it from cache, and a fresh runner
    // recycles process ids, so an earlier run's area can carry this very name.
    let label = "recycled-pid";
    let next = NEXT.load(Ordering::Relaxed);
    let stale: Vec<PathBuf> = (next..next + 16)
        .map(|n| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("target")
                .join("tmp")
                .join(format!("msrv-context-{}-{n}-{label}", std::process::id()))
        })
        .collect();
    for area in &stale {
        fs::create_dir_all(area.join("source").join(".cargo")).unwrap();
        fs::write(
            area.join("source").join(".cargo").join("config.toml"),
            "[build]\nrustflags = ['--cfg', 'stale']\n",
        )
        .unwrap();
        fs::create_dir_all(area.join("cargo-home")).unwrap();
    }
    let fixture = Fixture::new(label);
    assert!(!stale.contains(&fixture.area), "{}", fixture.area.display());
    assert!(context::check(&fixture.metadata()).is_ok());
    for area in &stale {
        assert!(area
            .join("source")
            .join(".cargo")
            .join("config.toml")
            .is_file());
        fs::remove_dir_all(area).unwrap();
    }
}

/// Claims an area that no earlier process left behind; reusing one would mix
/// its Cargo configuration into this fixture.
fn fresh_area(label: &str) -> PathBuf {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target")
        .join("tmp");
    fs::create_dir_all(&parent).unwrap();
    loop {
        let area = parent.join(format!(
            "msrv-context-{}-{}-{label}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&area) {
            Ok(()) => return area,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("{}: {error}", area.display()),
        }
    }
}

struct Fixture {
    area: PathBuf,
    home: PathBuf,
    runner: Runner,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let area = fresh_area(label);
        let root = area.join("source");
        let home = area.join("cargo-home");
        let output = area.join("output");
        fs::create_dir_all(root.join("crates/rmux-sdk/src")).unwrap();
        fs::create_dir(&home).unwrap();
        fs::create_dir(&output).unwrap();
        fs::write(root.join("Cargo.toml"), format!(
            "[workspace]\nmembers=[\"crates/rmux-sdk\"]\nresolver=\"2\"\n[workspace.package]\nrust-version=\"{FLOOR}\"\n"
        )).unwrap();
        fs::write(root.join("crates/rmux-sdk/Cargo.toml"),
            "[package]\nname=\"rmux-sdk\"\nversion=\"0.10.0\"\nedition=\"2021\"\nrust-version.workspace = true\n[features]\nregex=[]\n").unwrap();
        fs::write(root.join("crates/rmux-sdk/src/lib.rs"),
            "/// ```\n/// assert_eq!(rmux_sdk::answer(), 42);\n/// ```\npub fn answer() -> u8 { 42 }\n#[test]\nfn answer_test() { assert_eq!(answer(), 42); }\n").unwrap();
        fs::write(
            root.join("Cargo.lock"),
            "version = 4\n[[package]]\nname = \"rmux-sdk\"\nversion = \"0.10.0\"\n",
        )
        .unwrap();
        git(&root, &["init", "-q"]).unwrap();
        git(&root, &["add", "Cargo.toml", "Cargo.lock", "crates"]).unwrap();
        git(
            &root,
            &[
                "-c",
                "user.name=Sidney Sissaoui",
                "-c",
                "user.email=shideneyu@gmail.com",
                "commit",
                "--no-gpg-sign",
                "-qm",
                "MSRV context test fixture",
            ],
        )
        .unwrap();
        let cargo = PathBuf::from(env::var_os("CARGO").unwrap());
        let toolchain = cargo.parent().unwrap().parent().unwrap().to_path_buf();
        let rustc = toolchain
            .join("bin")
            .join(format!("rustc{}", env::consts::EXE_SUFFIX));
        let rustdoc = toolchain
            .join("bin")
            .join(format!("rustdoc{}", env::consts::EXE_SUFFIX));
        let version = super::output(Command::new(&rustc).arg("-vV")).unwrap();
        let host = version
            .lines()
            .find_map(|l| l.strip_prefix("host: "))
            .unwrap()
            .into();
        let receipts = File::create(output.join("receipts.jsonl")).unwrap();
        let runner = Runner {
            root,
            options: Options {
                toolchain,
                output,
                target: area.join("target"),
            },
            cargo,
            cargo_home: Some(home.clone().into_os_string()),
            rustc,
            rustdoc,
            path: env::var_os("PATH").unwrap(),
            host,
            receipts,
            count: 0,
        };
        Self { area, home, runner }
    }

    fn metadata(&self) -> Command {
        self.runner.metadata()
    }

    fn command(&self, args: &[&str]) -> Command {
        self.runner
            .cargo(&self.runner.root, &cells::words(args), &self.runner.host)
    }

    fn reject<T: std::fmt::Debug>(&self, label: &str, result: Result<T, Error>) {
        let error = result.unwrap_err().to_string();
        assert!(error.contains("Cargo configuration"), "{label}: {error}");
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.area.join("rejections.log"))
            .unwrap();
        writeln!(log, "{label}: {error}").unwrap();
        assert_eq!(self.runner.count, 0);
        assert!(fs::read(self.runner.options.output.join("receipts.jsonl"))
            .unwrap()
            .is_empty());
    }
}

#[test]
fn unconfigured_metadata_check_test_and_doc_use_real_tools() {
    if process::isolated() {
        return;
    }
    let mut fixture = Fixture::new("stable");
    let metadata = cargo_output(&mut fixture.metadata()).unwrap();
    fs::write(
        fixture.runner.options.output.join("metadata.json"),
        metadata,
    )
    .unwrap();
    for (id, args) in [
        ("check", vec!["check"]),
        ("test", vec!["test", "--lib"]),
        ("doc", vec!["test", "--doc"]),
    ] {
        let mut command = fixture.command(&args);
        // Exercise explicit inherited settings without replacing native SDK/linker inputs.
        for key in ["RUSTFLAGS", "RUSTDOCFLAGS", "INCLUDE", "LIB"] {
            if let Some(value) = env::var_os(key) {
                command.env(key, value);
            }
        }
        let before = format!("{command:?}");
        context::check(&command).unwrap();
        assert_eq!(format!("{command:?}"), before);
        fixture.runner.cell(id, &mut command, None).unwrap();
    }
    assert_eq!(fixture.runner.count, 3);
}

#[test]
fn both_config_names_reject_all_cargo_entry_paths_in_home_cwd_and_ancestor() {
    if process::isolated() {
        return;
    }
    for name in ["config", "config.toml"] {
        for location in ["home", "cwd", "ancestor"] {
            let mut fixture = Fixture::new(&format!("{location}-{name}"));
            let directory = match location {
                "home" => fixture.home.clone(),
                "cwd" => fixture.runner.root.join(".cargo"),
                _ => fixture.area.join(".cargo"),
            };
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join(name), "[env]\nRUSTC_BOOTSTRAP = \"1\"\n").unwrap();
            fixture.reject("metadata", cargo_output(&mut fixture.metadata()));
            for (id, args) in [
                ("check", vec!["check"]),
                ("test", vec!["test", "--lib"]),
                ("doc", vec!["test", "--doc"]),
            ] {
                let mut command = fixture.command(&args);
                let result = fixture.runner.cell(id, &mut command, None);
                fixture.reject(id, result);
            }
        }
    }
}

#[test]
fn witness_checks_its_own_output_ancestry_after_metadata() {
    if process::isolated() {
        return;
    }
    for name in ["config", "config.toml"] {
        let mut fixture = Fixture::new(&format!("witness-{name}"));
        let metadata = cargo_output(&mut fixture.metadata()).unwrap();
        fs::write(
            fixture.runner.options.output.join("metadata.json"),
            metadata,
        )
        .unwrap();
        let directory = fixture.runner.options.output.join(".cargo");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join(name), "").unwrap();
        let result = fixture.runner.witness();
        fixture.reject("witness-control", result);
        assert!(!fixture
            .runner
            .options
            .output
            .join("witness-control.log")
            .exists());
        assert!(fixture
            .runner
            .options
            .output
            .join("higher-requirement-source/Cargo.toml")
            .is_file());
    }
}

#[test]
fn witness_fixtures_reject_configuration_without_parent_cargo_home() {
    if process::isolated() {
        return;
    }
    let tests = [
        "msrv::context_tests::witness_checks_its_own_output_ancestry_after_metadata",
        #[cfg(windows)]
        "msrv::context_tests::windows_junction_witness_rejects_before_its_first_cell",
    ];
    for test in tests {
        let result = Command::new(env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env_remove("CARGO_HOME")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{test}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn explicit_overrides_and_missing_or_relative_home_are_rejected() {
    if process::isolated() {
        return;
    }
    let fixture = Fixture::new("environment");
    for key in [
        "RUSTC_BOOTSTRAP",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        let mut command = fixture.metadata();
        command.env(key, "unsupported");
        assert!(cargo_output(&mut command)
            .unwrap_err()
            .to_string()
            .contains(key));
    }
    for value in ["", "relative", "missing"] {
        let mut command = fixture.metadata();
        if value == "missing" {
            command.env_remove("CARGO_HOME");
        } else {
            command.env("CARGO_HOME", value);
        }
        assert!(cargo_output(&mut command)
            .unwrap_err()
            .to_string()
            .contains("absolute CARGO_HOME"));
    }
    #[cfg(windows)]
    {
        let mut command = fixture.metadata();
        command.env("rustc_bootstrap", "1");
        assert!(cargo_output(&mut command)
            .unwrap_err()
            .to_string()
            .contains("RUSTC_BOOTSTRAP"));
    }
}

#[test]
fn configuration_entries_and_invalid_directories_fail_closed() {
    if process::isolated() {
        return;
    }
    let fixture = Fixture::new("entry-types");
    for name in ["config", "config.toml"] {
        let path = fixture.home.join(name);
        fs::write(&path, "").unwrap();
        fixture.reject("empty-file", cargo_output(&mut fixture.metadata()));
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        fixture.reject("directory-entry", cargo_output(&mut fixture.metadata()));
        fs::remove_dir(&path).unwrap();
    }
    let path = fixture.runner.root.join(".cargo");
    fs::write(&path, "not a directory").unwrap();
    fixture.reject(
        "non-directory-.cargo",
        cargo_output(&mut fixture.metadata()),
    );
    fs::remove_file(&path).unwrap();
    let mut command = fixture.metadata();
    command.env("CARGO_HOME", fixture.area.join("absent-home"));
    fixture.reject("absent-home", cargo_output(&mut command));
}

#[cfg(unix)]
#[test]
fn native_symlinks_and_access_errors_cannot_hide_configuration() {
    if process::isolated() {
        return;
    }
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = Fixture::new("native paths with spaces-☃");
    let destination = fixture.area.join("ordinary-file");
    fs::write(&destination, "").unwrap();
    for name in ["config", "config.toml"] {
        let link = fixture.home.join(name);
        for target in [&destination, &fixture.area.join("absent-target"), &link] {
            symlink(target, &link).unwrap();
            fixture.reject("config-symlink", cargo_output(&mut fixture.metadata()));
            fs::remove_file(&link).unwrap();
        }
    }
    let directory = fixture.runner.root.join(".cargo");
    symlink(fixture.area.join("absent-directory"), &directory).unwrap();
    fixture.reject("dangling-.cargo", cargo_output(&mut fixture.metadata()));
    fs::remove_file(&directory).unwrap();
    symlink(&directory, &directory).unwrap();
    fixture.reject("looping-.cargo", cargo_output(&mut fixture.metadata()));
    fs::remove_file(&directory).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("config"), "").unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o0)).unwrap();
    let result = cargo_output(&mut fixture.metadata());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.reject("inaccessible-.cargo", result);
}

#[cfg(unix)]
#[test]
fn symlink_cwd_uses_physical_ancestry_like_cargo() {
    if process::isolated() {
        return;
    }
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new("cwd-alias");
    let logical = fixture.area.join("logical");
    fs::create_dir(&logical).unwrap();
    symlink(&fixture.runner.root, logical.join("alias")).unwrap();
    fs::create_dir(logical.join(".cargo")).unwrap();
    fs::write(logical.join(".cargo/config.toml"), "not TOML").unwrap();
    let mut command = fixture.metadata();
    command.current_dir(logical.join("alias"));
    cargo_output(&mut command).unwrap(); // Cargo also ignores the lexical alias parent.
    fs::create_dir(fixture.runner.root.join(".cargo")).unwrap();
    fs::write(fixture.runner.root.join(".cargo/config"), "").unwrap();
    fixture.reject("physical-cwd", cargo_output(&mut command));
}

#[test]
fn inherited_cwd_with_explicit_manifest_uses_real_cargo() {
    if process::isolated() {
        return;
    }
    let fixture = Fixture::new("inherited-cwd");
    let mut command = Command::new(&fixture.runner.cargo);
    command
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--manifest-path",
        ])
        .arg(fixture.runner.root.join("Cargo.toml"))
        .env("RUSTC", &fixture.runner.rustc)
        .env("CARGO_HOME", &fixture.home);
    assert!(command.get_current_dir().is_none());
    cargo_output(&mut command).unwrap();
}

#[cfg(windows)]
fn junction(fixture: &Fixture, destination: &Path) -> PathBuf {
    let logical = fixture.area.join("logical [cwd]");
    fs::create_dir(&logical).unwrap();
    let alias = logical.join("alias");
    let result = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&alias)
        .arg(destination)
        .output()
        .unwrap();
    assert!(result.status.success(), "junction creation: {result:?}");
    assert_eq!(
        fs::canonicalize(&alias).unwrap(),
        fs::canonicalize(destination).unwrap()
    );
    alias
}

#[cfg(windows)]
#[test]
fn windows_junction_cwd_checks_cargos_lexical_ancestry() {
    if process::isolated() {
        return;
    }
    for name in ["config", "config.toml"] {
        let mut fixture = Fixture::new(&format!("junction-cwd-{name}"));
        let alias = junction(&fixture, &fixture.runner.root);
        let mut metadata = fixture.metadata();
        metadata.current_dir(&alias);
        cargo_output(&mut metadata).unwrap();
        let directory = alias.parent().unwrap().join(".cargo");
        fs::create_dir(&directory).unwrap();
        let config = directory.join(name);
        fs::write(&config, "deliberately invalid TOML").unwrap();
        // Measure Cargo's native ancestry independently of the guard.
        let raw = metadata.output().unwrap();
        fs::write(fixture.area.join("raw-cargo.stderr"), &raw.stderr).unwrap();
        assert_eq!(raw.status.code(), Some(101));
        assert!(String::from_utf8_lossy(&raw.stderr).contains(config.to_str().unwrap()));
        let result = cargo_output(&mut metadata);
        assert!(result
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("unsupported Cargo configuration:"));
        fixture.reject("junction-metadata", result);
        for (id, args) in [
            ("check", vec!["check"]),
            ("test", vec!["test", "--lib"]),
            ("doc", vec!["test", "--doc"]),
        ] {
            let mut command = fixture.command(&args);
            command.current_dir(&alias);
            let result = fixture.runner.cell(id, &mut command, None);
            fixture.reject(id, result);
            assert!(!fixture
                .runner
                .options
                .output
                .join(format!("{id}.log"))
                .exists());
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_junction_witness_rejects_before_its_first_cell() {
    if process::isolated() {
        return;
    }
    for name in ["config", "config.toml"] {
        let mut fixture = Fixture::new(&format!("junction-witness-{name}"));
        let alias = junction(&fixture, &fixture.runner.options.output);
        let metadata = cargo_output(&mut fixture.metadata()).unwrap();
        fs::write(
            fixture.runner.options.output.join("metadata.json"),
            metadata,
        )
        .unwrap();
        fixture.runner.options.output = alias;
        let directory = fixture
            .runner
            .options
            .output
            .parent()
            .unwrap()
            .join(".cargo");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join(name), "[env]\nRUSTC_BOOTSTRAP = \"1\"\n").unwrap();
        let result = fixture.runner.witness();
        fixture.reject("junction-witness", result);
        assert!(!fixture
            .runner
            .options
            .output
            .join("witness-control.log")
            .is_file());
    }
}
