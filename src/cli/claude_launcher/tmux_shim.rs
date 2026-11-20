//! Private `tmux` shim directory: installation, target binary discovery, and PATH setup.

use std::env;
use std::ffi::OsString;
use std::fs;
#[cfg(any(unix, windows))]
use std::io;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use super::runtime_dir::validate_secure_owner_directory;
#[cfg(windows)]
use super::runtime_dir::{cleanup_stale_windows_claude_temp_dirs, create_windows_temporary_dir};
use super::ExitFailure;
#[cfg(any(unix, windows))]
use super::PUBLIC_BINARY_OVERRIDE_ENV;

pub(super) struct PrivateTmuxShim {
    dir: PathBuf,
    pub(super) cleanup_on_drop: bool,
}

impl PrivateTmuxShim {
    #[cfg(any(unix, windows))]
    fn persistent(dir: PathBuf) -> Self {
        Self {
            dir,
            cleanup_on_drop: false,
        }
    }

    #[cfg(windows)]
    fn temporary(dir: PathBuf) -> Self {
        Self {
            dir,
            cleanup_on_drop: true,
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for PrivateTmuxShim {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            remove_temporary_shim_dir(&self.dir);
        }
    }
}

#[cfg(windows)]
fn remove_temporary_shim_dir(dir: &Path) {
    for attempt in 0..10 {
        match fs::remove_dir_all(dir) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) if attempt < 9 => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
}

#[cfg(not(windows))]
fn remove_temporary_shim_dir(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

#[cfg(unix)]
pub(super) fn ensure_private_tmux_shim() -> Result<PrivateTmuxShim, ExitFailure> {
    let dir = private_shim_dir()?;
    std::fs::create_dir_all(&dir).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude: failed to create private tmux shim directory '{}': {error}",
                dir.display()
            ),
        )
    })?;
    validate_secure_owner_directory(&dir, "private tmux shim directory")?;
    let target = private_tmux_shim_target_binary()?;
    let shim = dir.join(tmux_file_name());
    match std::fs::symlink_metadata(&shim) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if !symlink_points_to(&shim, &target) {
                std::fs::remove_file(&shim).map_err(|error| {
                    ExitFailure::new(
                        1,
                        format!(
                            "rmux claude: failed to replace private tmux shim '{}': {error}",
                            shim.display()
                        ),
                    )
                })?;
                symlink(&target, &shim).map_err(|error| {
                    ExitFailure::new(
                        1,
                        format!(
                            "rmux claude: failed to create private tmux shim '{}': {error}",
                            shim.display()
                        ),
                    )
                })?;
            }
        }
        Ok(_) => {
            return Err(ExitFailure::new(
                1,
                format!(
                    "rmux claude: '{}' exists and is not a symlink; refusing to overwrite it",
                    shim.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            symlink(&target, &shim).map_err(|error| {
                ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to create private tmux shim '{}': {error}",
                        shim.display()
                    ),
                )
            })?;
        }
        Err(error) => {
            return Err(ExitFailure::new(
                1,
                format!(
                    "rmux claude: failed to inspect private tmux shim '{}': {error}",
                    shim.display()
                ),
            ));
        }
    }
    Ok(PrivateTmuxShim::persistent(dir))
}

#[cfg(windows)]
pub(super) fn ensure_private_tmux_shim() -> Result<PrivateTmuxShim, ExitFailure> {
    cleanup_stale_windows_claude_temp_dirs();
    let dir = windows_persistent_shim_dir();
    fs::create_dir_all(&dir).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude: failed to create private tmux shim directory '{}': {error}",
                dir.display()
            ),
        )
    })?;
    let source = windows_tmux_shim_source_binary()?;
    install_windows_tmux_shim_in_dir(&source, dir)
}

#[cfg(windows)]
pub(super) fn install_windows_tmux_shim_in_dir(
    source: &Path,
    dir: PathBuf,
) -> Result<PrivateTmuxShim, ExitFailure> {
    let shim = dir.join(tmux_file_name());
    match install_windows_tmux_shim(source, &shim) {
        Ok(()) => Ok(PrivateTmuxShim::persistent(dir)),
        Err(primary_error) => {
            let fallback_dir = create_windows_temporary_dir("rmux-claude-shim")?;
            let fallback_shim = fallback_dir.join(tmux_file_name());
            install_windows_tmux_shim(source, &fallback_shim).map_err(|fallback_error| {
                let _ = fs::remove_dir_all(&fallback_dir);
                ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to refresh private tmux shim '{}' ({}) and failed to create fallback shim '{}' ({})",
                        shim.display(),
                        primary_error.message(),
                        fallback_shim.display(),
                        fallback_error.message()
                    ),
                )
            })?;
            Ok(PrivateTmuxShim::temporary(fallback_dir))
        }
    }
}

#[cfg(windows)]
fn install_windows_tmux_shim(source: &Path, shim: &Path) -> Result<(), ExitFailure> {
    if shim.exists() {
        match fs::remove_file(shim) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to replace private tmux shim '{}': {error}",
                        shim.display()
                    ),
                ));
            }
        }
    }
    fs::hard_link(source, shim)
        .or_else(|_| fs::copy(source, shim).map(|_| ()))
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!(
                    "rmux claude: failed to create private tmux shim '{}' from '{}': {error}",
                    shim.display(),
                    source.display()
                ),
            )
        })
}

#[cfg(not(any(unix, windows)))]
pub(super) fn ensure_private_tmux_shim() -> Result<PrivateTmuxShim, ExitFailure> {
    Err(ExitFailure::new(
        1,
        "rmux claude is currently supported only on Unix-like systems and Windows",
    ))
}

#[cfg(windows)]
fn windows_persistent_shim_dir() -> PathBuf {
    let root = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    root.join("rmux").join("claude-tmux-shim")
}

#[cfg(windows)]
fn windows_tmux_shim_source_binary() -> Result<PathBuf, ExitFailure> {
    let current = env::current_exe().map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux claude: failed to resolve current rmux binary: {error}"),
        )
    })?;
    if let Some(path) = env::var_os(PUBLIC_BINARY_OVERRIDE_ENV) {
        let public = PathBuf::from(path);
        for candidate in windows_full_helper_candidates(&public) {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    for candidate in windows_full_helper_candidates(&current) {
        if candidate.is_file() && candidate != current {
            return Ok(candidate);
        }
    }
    Ok(current)
}

#[cfg(windows)]
pub(super) fn windows_full_helper_candidates(current: &Path) -> Vec<PathBuf> {
    let Some(parent) = current.parent() else {
        return Vec::new();
    };
    vec![
        parent.join("libexec").join("rmux").join(rmux_file_name()),
        parent
            .join("..")
            .join("libexec")
            .join("rmux")
            .join(rmux_file_name()),
    ]
}

#[cfg(unix)]
fn private_shim_dir() -> Result<PathBuf, ExitFailure> {
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ExitFailure::new(1, "rmux claude: HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("rmux")
        .join("claude-tmux-shim"))
}

#[cfg(unix)]
fn private_tmux_shim_target_binary() -> Result<PathBuf, ExitFailure> {
    let public = public_rmux_binary()?;
    Ok(private_tmux_shim_target_for_public_binary(&public))
}

#[cfg(unix)]
pub(super) fn private_tmux_shim_target_for_public_binary(public: &Path) -> PathBuf {
    unix_full_helper_candidates(public)
        .into_iter()
        .find(|candidate| candidate.is_file() && candidate.as_path() != public)
        .unwrap_or_else(|| public.to_path_buf())
}

#[cfg(unix)]
fn unix_full_helper_candidates(public: &Path) -> Vec<PathBuf> {
    let Some(parent) = public.parent() else {
        return Vec::new();
    };
    vec![
        parent.join("libexec").join("rmux").join(rmux_file_name()),
        parent
            .join("..")
            .join("libexec")
            .join("rmux")
            .join(rmux_file_name()),
        parent
            .join("..")
            .join("lib")
            .join("rmux")
            .join("libexec")
            .join(rmux_file_name()),
    ]
}

#[cfg(unix)]
fn public_rmux_binary() -> Result<PathBuf, ExitFailure> {
    if let Some(path) = env::var_os(PUBLIC_BINARY_OVERRIDE_ENV) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    env::current_exe().map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux claude: failed to resolve current rmux binary: {error}"),
        )
    })
}

pub(super) fn path_with_shim_first(shim_dir: &Path) -> Result<OsString, ExitFailure> {
    path_with_shim_first_from(shim_dir, env::var_os("PATH"))
}

pub(super) fn path_with_shim_first_from(
    shim_dir: &Path,
    original: Option<OsString>,
) -> Result<OsString, ExitFailure> {
    let original = original.unwrap_or_default();
    let mut paths = vec![shim_dir.to_path_buf()];
    if !original.is_empty() {
        paths.extend(env::split_paths(&original));
    }
    env::join_paths(paths).map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux claude: failed to build PATH with private tmux shim: {error}"),
        )
    })
}

#[cfg(unix)]
fn symlink_points_to(shim: &Path, target: &Path) -> bool {
    let Ok(link_target) = std::fs::read_link(shim) else {
        return false;
    };
    let resolved = if link_target.is_absolute() {
        link_target
    } else {
        shim.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(link_target)
    };
    paths_resolve_to_same_file(&resolved, target)
}

#[cfg(unix)]
fn paths_resolve_to_same_file(left: &Path, right: &Path) -> bool {
    let Ok(left) = std::fs::canonicalize(left) else {
        return false;
    };
    let Ok(right) = std::fs::canonicalize(right) else {
        return false;
    };
    left == right
}

fn tmux_file_name() -> OsString {
    let mut name = OsString::from("tmux");
    if !env::consts::EXE_SUFFIX.is_empty() {
        name.push(env::consts::EXE_SUFFIX);
    }
    name
}

fn rmux_file_name() -> OsString {
    let mut name = OsString::from("rmux");
    if !env::consts::EXE_SUFFIX.is_empty() {
        name.push(env::consts::EXE_SUFFIX);
    }
    name
}
