use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

type Evidence = Vec<(PathBuf, Vec<u8>)>;

fn retained_path(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?.join("target"));
    let relative = path.strip_prefix(root).map_err(io::Error::other)?;
    Ok(target
        .join("rmux-sdk-config-fixtures")
        .join(root.file_name().expect("smoke root has a name"))
        .join(relative))
}

pub(super) fn preserve(root: &Path, paths: &[&Path]) -> io::Result<()> {
    for path in paths {
        let contents = fs::read(path)?;
        let retained = retained_path(root, path)?;
        fs::create_dir_all(retained.parent().expect("retained config has a parent"))?;
        let mut copy = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(retained)?;
        copy.write_all(&contents)?;
        copy.sync_all()?;
    }
    Ok(())
}

pub(super) fn assert_retained(root: &Path, paths: &[&Path]) -> io::Result<Evidence> {
    paths
        .iter()
        .map(|path| {
            let original = fs::read(path)?;
            let retained = retained_path(root, path)?;
            assert_eq!(
                fs::read(&retained).expect("SDK config retained before first daemon read"),
                original
            );
            Ok((retained, original))
        })
        .collect()
}

pub(super) fn assert_survived(evidence: &Evidence) -> io::Result<()> {
    for (retained, original) in evidence {
        assert_eq!(
            &fs::read(retained)?,
            original,
            "SDK cleanup must preserve the consumed config bytes"
        );
    }
    Ok(())
}
