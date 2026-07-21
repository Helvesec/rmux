use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn yaml_files(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("read YAML directory") {
        let path = entry.expect("YAML entry").path();
        if path.is_dir() {
            yaml_files(&path, files);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("yml") || extension.eq_ignore_ascii_case("yaml")
            })
        {
            files.push(path);
        }
    }
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[test]
fn exact_single_artifact_downloads_extract_into_the_requested_directory() {
    let mut files = Vec::new();
    yaml_files(&repo_root().join(".github/actions"), &mut files);
    yaml_files(&repo_root().join(".github/workflows"), &mut files);

    let mut exact_downloads = 0;
    for path in files {
        let source = fs::read_to_string(&path).expect("read YAML source");
        if !source.contains("actions/download-artifact@") {
            continue;
        }
        let lines: Vec<_> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let Some(value) = line.trim().strip_prefix("artifact-ids:") else {
                continue;
            };
            let value = value.trim();
            let key_indent = indentation(line);
            let merge = lines[index + 1..]
                .iter()
                .take_while(|next| next.trim().is_empty() || indentation(next) >= key_indent)
                .find_map(|next| next.trim().strip_prefix("merge-multiple:").map(str::trim));

            let multi_artifact_expression =
                value.ends_with(".outputs.ids }}") || value.ends_with(".outputs.artifact_ids }}");
            if multi_artifact_expression {
                assert_ne!(
                    merge,
                    Some("true"),
                    "{}:{} flattens a multi-artifact set",
                    path.display(),
                    index + 1
                );
            } else {
                exact_downloads += 1;
                assert_eq!(
                    merge,
                    Some("true"),
                    "{}:{} leaves one exact artifact in an unexpected name directory",
                    path.display(),
                    index + 1
                );
            }
        }
    }

    assert!(
        exact_downloads >= 20,
        "exact artifact coverage unexpectedly shrank"
    );
}
