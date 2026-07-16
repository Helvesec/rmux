use std::env;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use crate::ChildCommand;

pub(super) fn resolve_application_path(command: &ChildCommand) -> io::Result<PathBuf> {
    if command.program.is_absolute() || has_path_component(&command.program) {
        let base = command.current_dir.clone().unwrap_or(env::current_dir()?);
        let candidate = if command.program.is_absolute() {
            command.program.clone()
        } else {
            base.join(&command.program)
        };
        let pathext = effective_env_value(command, "PATHEXT");
        if let Some(resolved) = resolve_application_candidate(&candidate, pathext.as_deref())? {
            return Ok(resolved);
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "ConPTY executable not found: {}",
                candidate.to_string_lossy()
            ),
        ));
    }

    search_application_path(command).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "ConPTY executable not found on PATH: {}",
                command.program.to_string_lossy()
            ),
        )
    })
}

fn has_path_component(path: &Path) -> bool {
    path.parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty())
}

fn search_application_path(command: &ChildCommand) -> Option<PathBuf> {
    let path_value = effective_env_value(command, "PATH")?;
    let pathext = effective_env_value(command, "PATHEXT");
    let extensions = executable_extensions(&command.program, pathext.as_deref());
    let current_dir = command
        .current_dir
        .clone()
        .or_else(|| env::current_dir().ok());
    for directory in env::split_paths(&path_value) {
        let directory = if directory.is_absolute() {
            directory
        } else if let Some(current_dir) = &current_dir {
            current_dir.join(directory)
        } else {
            directory
        };
        for extension in &extensions {
            let candidate = append_extension(&directory.join(&command.program), extension);
            match resolve_exact_application_candidate(&candidate) {
                Ok(Some(path)) => return Some(path),
                Ok(None) => {}
                Err(_) => continue,
            }
        }
    }
    None
}

fn executable_extensions(program: &Path, pathext: Option<&OsStr>) -> Vec<OsString> {
    if program.extension().is_some() {
        return vec![OsString::new()];
    }

    let mut extensions = vec![OsString::new()];
    extensions.extend(
        pathext
            .map(|value| {
                value
                    .to_string_lossy()
                    .split(';')
                    .filter(|extension| !extension.is_empty())
                    .map(|extension| {
                        if extension.starts_with('.') {
                            OsString::from(extension)
                        } else {
                            OsString::from(format!(".{extension}"))
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|extensions| !extensions.is_empty())
            .unwrap_or_else(|| [".COM", ".EXE"].into_iter().map(OsString::from).collect())
            .into_iter()
            .filter(|extension| extension.is_empty() || is_direct_application_extension(extension)),
    );
    extensions
}

fn resolve_application_candidate(
    path: &Path,
    pathext: Option<&OsStr>,
) -> io::Result<Option<PathBuf>> {
    for extension in executable_extensions(path, pathext) {
        if let Some(candidate) =
            resolve_exact_application_candidate(&append_extension(path, &extension))?
        {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn resolve_exact_application_candidate(path: &Path) -> io::Result<Option<PathBuf>> {
    if !path.is_file() {
        return Ok(None);
    }
    if !is_direct_application_path(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "ConPTY executable must be .exe or .com: {}",
                path.to_string_lossy()
            ),
        ));
    }
    absolutize_existing_path(path.to_path_buf()).map(Some)
}

fn append_extension(path: &Path, extension: &OsStr) -> PathBuf {
    let mut candidate = path.as_os_str().to_owned();
    candidate.push(extension);
    PathBuf::from(candidate)
}

fn is_direct_application_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|extension| {
            extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("com")
        })
        .unwrap_or(true)
}

fn is_direct_application_extension(extension: &OsStr) -> bool {
    extension
        .to_str()
        .map(|extension| {
            matches!(
                extension
                    .trim_start_matches('.')
                    .to_ascii_lowercase()
                    .as_str(),
                "exe" | "com"
            )
        })
        .unwrap_or(false)
}

fn effective_env_value(command: &ChildCommand, name: &str) -> Option<OsString> {
    command
        .env
        .iter()
        .rev()
        .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .or_else(|| (!command.clear_env).then(|| env::var_os(name)).flatten())
}

fn absolutize_existing_path(path: PathBuf) -> io::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }

    Ok(env::current_dir()?.join(path))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn resolves_bare_application_from_explicit_path_env() {
        let root = temp_root("path");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("create bin");
        let executable = bin.join("tool.EXE");
        fs::write(&executable, b"").expect("create executable placeholder");

        let command = ChildCommand::new("tool")
            .clear_env()
            .env("PATH", bin.as_os_str().to_owned())
            .env("PATHEXT", ".EXE");
        let resolved = resolve_application_path(&command).expect("program resolves");

        assert!(
            resolved
                .to_string_lossy()
                .eq_ignore_ascii_case(&executable.to_string_lossy()),
            "expected {resolved:?} to resolve to {executable:?}"
        );
        assert!(
            !resolved.to_string_lossy().starts_with(r"\\?\"),
            "ConPTY application paths should avoid verbatim prefixes unless the caller supplied one"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_relative_path_entry_against_child_current_dir() {
        let root = temp_root("relative-path-entry");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("create bin");
        let executable = bin.join("tool.EXE");
        fs::write(&executable, b"").expect("create executable placeholder");

        let command = ChildCommand::new("tool")
            .current_dir(root.clone())
            .clear_env()
            .env("PATH", "bin")
            .env("PATHEXT", ".EXE");
        let resolved = resolve_application_path(&command).expect("program resolves");

        assert!(
            resolved
                .to_string_lossy()
                .eq_ignore_ascii_case(&executable.to_string_lossy()),
            "expected {resolved:?} to resolve to {executable:?}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_relative_application_against_child_current_dir() {
        let root = temp_root("relative");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("create bin");
        let executable = bin.join("tool.exe");
        fs::write(&executable, b"").expect("create executable placeholder");

        let command =
            ChildCommand::new(PathBuf::from("bin").join("tool.exe")).current_dir(root.clone());
        let resolved = resolve_application_path(&command).expect("program resolves");

        assert!(
            resolved
                .to_string_lossy()
                .eq_ignore_ascii_case(&executable.to_string_lossy()),
            "expected {resolved:?} to resolve to {executable:?}"
        );
        assert!(
            !resolved.to_string_lossy().starts_with(r"\\?\"),
            "ConPTY application paths should avoid verbatim prefixes unless the caller supplied one"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_relative_application_without_extension_via_pathext() {
        let root = temp_root("relative-pathext");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("create bin");
        let executable = bin.join("tool.exe");
        fs::write(&executable, b"").expect("create executable placeholder");

        let command = ChildCommand::new(PathBuf::from("bin").join("tool"))
            .current_dir(root.clone())
            .clear_env()
            .env("PATHEXT", ".CMD;.EXE");
        let resolved = resolve_application_path(&command).expect("program resolves");

        assert!(
            resolved
                .to_string_lossy()
                .eq_ignore_ascii_case(&executable.to_string_lossy()),
            "expected {resolved:?} to resolve to {executable:?}"
        );
        assert!(
            !resolved.to_string_lossy().starts_with(r"\\?\"),
            "ConPTY application paths should avoid verbatim prefixes unless the caller supplied one"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn skips_batch_wrappers_when_resolving_bare_applications() {
        let root = temp_root("batch-wrapper");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("create bin");
        fs::write(bin.join("tool.cmd"), b"").expect("create batch placeholder");

        let command = ChildCommand::new("tool")
            .clear_env()
            .env("PATH", bin.as_os_str().to_owned())
            .env("PATHEXT", ".CMD");
        let error = resolve_application_path(&command).expect_err("batch wrapper is skipped");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unresolved_relative_application_is_rejected_before_create_process() {
        let command = ChildCommand::new("not-present").clear_env();

        let error = resolve_application_path(&command).expect_err("program is absent");

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    fn temp_root(label: &str) -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        env::temp_dir().join(format!("rmux-conpty-{label}-{id}"))
    }
}
