use std::process::Command;

#[test]
fn json_output_registers_all_framing_tests() {
    let current = std::env::current_exe().expect("current test binary");
    let binary = rmux_os::build_identity::resolve(current.as_os_str())
        .expect("candidate test executable identity");
    let output = Command::new(binary)
        .arg("--list")
        .output()
        .expect("enumerate the actual rmux unit tests");
    assert!(output.status.success(), "test enumeration failed");
    let listing = String::from_utf8(output.stdout).expect("UTF-8 test listing");
    let prefix = "cli::json_output::tests::framing::";
    let registered: std::collections::BTreeSet<_> = listing
        .lines()
        .filter_map(|line| line.strip_prefix(prefix)?.strip_suffix(": test"))
        .collect();
    assert_eq!(
        registered,
        std::collections::BTreeSet::from([
            "clients_and_sessions_keep_their_existing_framing_and_types",
            "escaped_rows_reject_bad_field_counts_invalid_utf8_and_truncation",
            "pane_json_frames_each_observation_once",
            "panes_round_trip_utf8_and_all_string_fields",
            "windows_round_trip_multiple_rows_with_embedded_delimiters",
        ]),
        "the frozen framing module must be compiled and registered exactly once",
    );
}

/// Regression for `clippy::duplicate_mod`: a source file shared by the full
/// and tiny dispatchers must have one module owner. Two out-of-line `#[path]`
/// declarations resolving to the same file compile it twice under
/// `--features tiny-cli`. The scan ignores `cfg`, so it fails in every feature
/// configuration, not only the one where both declarations are compiled.
#[test]
fn no_source_file_is_loaded_by_two_path_modules() {
    use std::collections::BTreeMap;
    use std::path::{Component, Path, PathBuf};

    fn lexical(path: PathBuf) -> PathBuf {
        let mut normal = PathBuf::new();
        for component in path.components() {
            match component {
                Component::ParentDir => assert!(normal.pop(), "{path:?} escapes"),
                Component::CurDir => {}
                other => normal.push(other),
            }
        }
        normal
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut loads: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut directories = vec![root.join("src")];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory).expect("read source directory") {
            let file = entry.expect("source directory entry").path();
            if file.is_dir() {
                directories.push(file);
                continue;
            }
            if file.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let source = std::fs::read_to_string(&file).expect("read Rust source");
            let lines: Vec<&str> = source.lines().map(str::trim).collect();
            for (index, line) in lines.iter().enumerate() {
                let Some(target) = line
                    .strip_prefix("#[path = \"")
                    .and_then(|rest| rest.strip_suffix("\"]"))
                else {
                    continue;
                };
                let item = lines[index + 1..]
                    .iter()
                    .find(|next| !next.starts_with("#["))
                    .expect("attributed item");
                if !(item.starts_with("mod ") && item.ends_with(';')) {
                    continue;
                }
                let parent = file.parent().expect("source file directory");
                let site = format!(
                    "{}:{}",
                    file.strip_prefix(root).unwrap_or(&file).display(),
                    index + 1
                );
                loads
                    .entry(lexical(parent.join(target)))
                    .or_default()
                    .push(site);
            }
        }
    }
    assert!(!loads.is_empty(), "the scan found no #[path] module");
    let duplicated: Vec<_> = loads.iter().filter(|(_, sites)| sites.len() > 1).collect();
    assert!(
        duplicated.is_empty(),
        "files loaded by several #[path] modules: {duplicated:?}"
    );
}
