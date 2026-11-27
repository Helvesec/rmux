//! The public executable belongs to one launcher-to-helper invocation.

#[cfg(unix)]
pub(crate) const FULL_HELPER: &str = "__rmux-internal-full-helper";

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
mod receiver {
    use std::{env, path::PathBuf};

    #[cfg(unix)]
    static LAUNCHER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

    #[cfg(unix)]
    pub(crate) fn invocation_args() -> Vec<std::ffi::OsString> {
        let mut args: Vec<_> = env::args_os().collect();
        // exec supplies the public executable as argv[0], followed by the
        // private marker and the original argument vector (including aliases).
        if args.len() >= 3
            && args[1] == super::FULL_HELPER
            && std::path::Path::new(&args[0]).is_absolute()
        {
            LAUNCHER
                .set(PathBuf::from(&args[0]))
                .expect("single helper invocation");
            return args.split_off(2);
        }
        args
    }

    #[cfg(unix)]
    pub(crate) fn public_exe() -> std::io::Result<PathBuf> {
        LAUNCHER.get().cloned().map_or_else(env::current_exe, Ok)
    }

    #[cfg(windows)]
    pub(crate) fn launcher_path() -> Option<PathBuf> {
        let hint = env::var_os("RMUX_INTERNAL_PUBLIC_BINARY_PATH")?;
        let pid = rmux_os::process::parent_pid(std::process::id())?;
        let parent = rmux_os::process::executable_path(pid)?;
        verified_launcher(std::path::Path::new(&hint), std::path::Path::new(&parent))
    }

    #[cfg(windows)]
    fn verified_launcher(hint: &std::path::Path, parent: &std::path::Path) -> Option<PathBuf> {
        let parent = std::fs::canonicalize(parent).ok()?;
        (std::fs::canonicalize(hint).ok()? == parent).then_some(parent)
    }

    #[cfg(all(test, windows))]
    #[test]
    fn windows_handoff_requires_the_launching_process_image() {
        let current = env::current_exe().unwrap();
        let other = PathBuf::from(env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
        assert!(other.is_file());
        assert_eq!(
            verified_launcher(&current, &current),
            Some(std::fs::canonicalize(&current).unwrap())
        );
        assert!(verified_launcher(&current, &other).is_none());
    }
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
pub(crate) use receiver::*;
