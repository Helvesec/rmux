//! Ownership of the manifest and library/binary inputs selected by Cargo.

use super::{execute, Error};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub(super) fn verify(root: &Path, tree: &str, inputs: &[PathBuf]) -> Result<(), Error> {
    // Inspect the recorded tree, not the index or Git's ignored-file status.
    // Include directories so an ignored path prefix cannot supply discovery.
    let output = execute(Command::new("git").current_dir(root).args([
        "ls-tree",
        "-r",
        "-t",
        "-z",
        "--full-tree",
        tree,
    ]))?;
    let mut entries = BTreeMap::new();
    for record in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|r| !r.is_empty())
    {
        let text = std::str::from_utf8(record)
            .map_err(|_| Error::Invalid("non-UTF-8 Git tree path".into()))?;
        let (header, path) = text
            .split_once('\t')
            .ok_or_else(|| Error::Invalid("invalid Git tree entry".into()))?;
        let mode = header.split_whitespace().next().unwrap_or_default();
        entries.insert(PathBuf::from(path), mode.to_owned());
    }
    let committed = CommittedInputs {
        root: fs::canonicalize(root)?,
        entries,
    };
    for input in inputs {
        // Cargo may retain a symlink spelling of the workspace root. Resolve
        // that prefix only; input symlinks below it must themselves be committed.
        let path = match input.strip_prefix(root) {
            Ok(relative) => committed.root.join(relative),
            Err(_) => input.clone(),
        };
        let owned = committed.resolve(&path)?;
        if !matches!(
            committed.entries.get(&owned).map(String::as_str),
            Some("100644" | "100755")
        ) {
            return Err(uncommitted(input));
        }
    }
    Ok(())
}

fn uncommitted(path: &Path) -> Error {
    Error::Invalid(format!(
        "export/update requires committed inventory input: {}",
        path.display()
    ))
}

struct CommittedInputs {
    root: PathBuf,
    entries: BTreeMap<PathBuf, String>,
}

impl CommittedInputs {
    fn resolve(&self, path: &Path) -> Result<PathBuf, Error> {
        // This also rejects missing files and symlink cycles before following
        // committed links. A tracked link alone does not own its referent.
        let canonical = fs::canonicalize(path)?;
        if !canonical.starts_with(&self.root) {
            return Err(uncommitted(path));
        }
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| uncommitted(path))?;
        let mut owned = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir if owned.pop() => {}
                Component::Normal(name) => {
                    owned.push(name);
                    match self.entries.get(&owned).map(String::as_str) {
                        Some("040000" | "100644" | "100755") => {}
                        Some("120000") => {
                            let link = fs::read_link(self.root.join(&owned))?;
                            owned.pop();
                            owned = self.resolve(&self.root.join(&owned).join(link))?;
                        }
                        _ => return Err(uncommitted(path)),
                    }
                }
                _ => return Err(uncommitted(path)),
            }
        }
        // In particular, a native directory redirect cannot stand in for a
        // regular committed directory merely because the lexical path exists.
        if self.root.join(&owned) != canonical {
            return Err(uncommitted(path));
        }
        Ok(owned)
    }
}
