use std::collections::HashMap;
#[cfg(unix)]
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

#[cfg(unix)]
const DISABLE_TMUX_SHIM_ENV: &str = "RMUX_DISABLE_TMUX_SHIM";

pub(crate) fn apply_tmux_shim_environment(
    environment: &mut HashMap<String, String>,
    socket_path: &Path,
) {
    let Some(shim) = ensure_tmux_shim(socket_path) else {
        return;
    };
    prepend_path(environment, shim.parent().expect("shim has parent"));
    set_environment_value(
        environment,
        "TMUX_PROGRAM".to_owned(),
        shim.to_string_lossy().into_owned(),
    );
}

#[cfg(unix)]
pub(crate) fn cleanup_tmux_shim(socket_path: &Path) {
    let _ = remove_private_shim_dir(&shim_dir(socket_path));
}

#[cfg(unix)]
fn ensure_tmux_shim(socket_path: &Path) -> Option<PathBuf> {
    if env_flag_enabled(DISABLE_TMUX_SHIM_ENV) {
        return None;
    }
    let rmux = public_rmux_binary()?;
    let dir = shim_dir(socket_path);
    ensure_private_dir(&dir).ok()?;
    let shim = dir.join(shim_binary_name());
    create_or_replace_shim(&shim, &rmux).ok()?;
    Some(shim)
}

#[cfg(windows)]
fn ensure_tmux_shim(socket_path: &Path) -> Option<PathBuf> {
    let _ = socket_path;
    None
}

#[cfg(unix)]
fn public_rmux_binary() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let file_stem = current.file_stem()?.to_str()?;
    if file_stem == "rmux" {
        return current.is_file().then_some(current);
    }
    if file_stem != "rmux-daemon" {
        return None;
    }
    let mut candidate = current.clone();
    let file_name = match current.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if !extension.is_empty() => format!("rmux.{extension}"),
        _ => "rmux".to_owned(),
    };
    candidate.set_file_name(file_name);
    candidate.is_file().then_some(candidate)
}

#[cfg(unix)]
fn shim_dir(socket_path: &Path) -> PathBuf {
    shim_root().join(format!("rmux-shim-{:016x}", socket_hash(socket_path)))
}

#[cfg(unix)]
fn shim_root() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(unix)]
fn socket_hash(socket_path: &Path) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    socket_path.as_os_str().hash(&mut hasher);
    hasher.finish()
}

#[cfg(unix)]
fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if let Ok(metadata) = std::fs::symlink_metadata(dir) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "tmux shim path is not a private directory",
            ));
        }
        if metadata.uid() != rustix::process::getuid().as_raw() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "tmux shim directory is owned by another user",
            ));
        }
    } else {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(unix)]
fn remove_private_shim_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
    {
        return Ok(());
    }

    let shim = dir.join(shim_binary_name());
    match std::fs::symlink_metadata(&shim) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            std::fs::remove_file(&shim)?;
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::remove_dir(dir)
}

#[cfg(unix)]
fn create_or_replace_shim(shim: &Path, rmux: &Path) -> std::io::Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(shim) {
        if metadata.file_type().is_symlink()
            && matches!(std::fs::read_link(shim), Ok(target) if target == rmux)
        {
            return Ok(());
        }
        if metadata.file_type().is_symlink() || metadata.is_file() {
            std::fs::remove_file(shim)?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "tmux shim path is not replaceable",
            ));
        }
    }
    std::os::unix::fs::symlink(rmux, shim)
}

#[cfg(unix)]
fn shim_binary_name() -> &'static str {
    "tmux"
}

fn prepend_path(environment: &mut HashMap<String, String>, dir: &Path) {
    let dir = dir.to_string_lossy();
    let path = remove_environment_value(environment, "PATH").unwrap_or_default();
    let value = if path.is_empty() {
        dir.into_owned()
    } else {
        format!("{dir}{}{path}", path_separator())
    };
    set_environment_value(environment, "PATH".to_owned(), value);
}

#[cfg(windows)]
fn path_separator() -> &'static str {
    ";"
}

#[cfg(not(windows))]
fn path_separator() -> &'static str {
    ":"
}

fn set_environment_value(environment: &mut HashMap<String, String>, name: String, value: String) {
    let _ = remove_environment_value(environment, &name);
    environment.insert(name, value);
}

fn remove_environment_value(
    environment: &mut HashMap<String, String>,
    name: &str,
) -> Option<String> {
    #[cfg(windows)]
    {
        let keys = environment
            .keys()
            .filter(|key| key.eq_ignore_ascii_case(name))
            .cloned()
            .collect::<Vec<_>>();
        let mut value = None;
        for key in keys {
            value = value.or_else(|| environment.remove(&key));
        }
        value
    }
    #[cfg(not(windows))]
    {
        environment.remove(name)
    }
}

#[cfg(unix)]
fn env_flag_enabled(name: &str) -> bool {
    let Ok(value) = std::env::var(name) else {
        return false;
    };
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    !matches!(
        value.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

#[cfg(test)]
mod tests {
    use super::apply_tmux_shim_environment;
    #[cfg(unix)]
    use super::{cleanup_tmux_shim, create_or_replace_shim, shim_dir};
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn disabled_shim_leaves_environment_untouched() {
        let _lock = crate::test_env::lock_blocking();
        let _guard = crate::test_env::EnvVarGuard::set("RMUX_DISABLE_TMUX_SHIM", Some("1"));
        let mut environment = HashMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]);

        apply_tmux_shim_environment(&mut environment, Path::new("/tmp/rmux.sock"));

        assert_eq!(
            environment.get("PATH").map(String::as_str),
            Some("/usr/bin")
        );
        assert!(!environment.contains_key("TMUX_PROGRAM"));
    }

    #[cfg(unix)]
    #[test]
    fn shim_dir_prefers_xdg_runtime_dir() {
        let _lock = crate::test_env::lock_blocking();
        let root = std::env::temp_dir().join(format!("rmux-shim-xdg-{}", std::process::id()));
        let root_value = root.to_string_lossy().into_owned();
        let _guard = crate::test_env::EnvVarGuard::set("XDG_RUNTIME_DIR", Some(&root_value));

        let dir = shim_dir(Path::new("/tmp/rmux-shim-xdg-test.sock"));

        assert!(
            dir.starts_with(&root),
            "shim dir should live under XDG_RUNTIME_DIR, got {}",
            dir.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_removes_private_shim_directory() {
        let socket_path = Path::new("/tmp/rmux-shim-cleanup-test.sock");
        let dir = shim_dir(socket_path);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create shim dir");
        std::fs::write(dir.join("tmux"), b"placeholder").expect("create shim file");

        cleanup_tmux_shim(socket_path);

        assert!(
            !dir.exists(),
            "cleanup should remove the private shim directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_or_replace_shim_keeps_existing_link_to_rmux() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("rmux-shim-existing-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");
        let rmux = root.join("rmux");
        let shim = root.join("tmux");
        std::fs::write(&rmux, b"rmux").expect("create rmux");
        symlink(&rmux, &shim).expect("create shim symlink");

        create_or_replace_shim(&shim, &rmux).expect("existing link is valid");

        assert_eq!(std::fs::read_link(&shim).expect("read shim link"), rmux);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_does_not_follow_shim_directory_symlink() {
        use std::os::unix::fs::symlink;

        let socket_path = Path::new("/tmp/rmux-shim-symlink-test.sock");
        let dir = shim_dir(socket_path);
        let target = std::env::temp_dir().join(format!("rmux-shim-target-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&target);
        std::fs::create_dir_all(&target).expect("create target dir");
        std::fs::write(target.join("sentinel"), b"keep").expect("create sentinel");
        symlink(&target, &dir).expect("create shim dir symlink");

        cleanup_tmux_shim(socket_path);

        assert!(
            target.join("sentinel").is_file(),
            "cleanup must not follow a shim directory symlink"
        );
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_dir_all(&target);
    }
}
