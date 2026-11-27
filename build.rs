fn main() {
    embed_release_commit();
    const WINDOWS_MANIFEST: &str = "resources/windows/rmux.exe.manifest";

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest::embed_manifest_file(WINDOWS_MANIFEST)
            .expect("unable to embed Windows application manifest");
    }

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={WINDOWS_MANIFEST}");
}

// Canonical payloads carry only the source commit. Paths and per-invocation
// compiler graphs belong in external receipts, never in reproducible bytes.
fn embed_release_commit() {
    use std::path::PathBuf;
    use std::process::Command;

    let source = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(args)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    if git(&["rev-parse", "--show-toplevel"]).map(PathBuf::from) != Some(source.clone()) {
        // Unpacked registry sources must not inherit an ancestor's commit.
        return;
    }
    let git_path =
        |name: &str| git(&["rev-parse", "--git-path", name]).map(|path| source.join(path));
    let common = git(&["rev-parse", "--git-common-dir"]).map(|path| source.join(path));
    let reference = git(&["symbolic-ref", "-q", "HEAD"]);
    for path in release_commit_watch_paths(&git_path, common, reference.as_deref()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let Some(commit) = git(&["rev-parse", "HEAD"]) else {
        // Registry source packages need not contain Git metadata.
        return;
    };
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return;
    }
    let marker = format!("RMUX_RELEASE_IDENTITY_V1:{commit}:END");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("build output directory"));
    let code = output.join("release-commit.rs");
    let object = output.join("release-commit.o");
    std::fs::write(&code, format!(
        "#[used]\n#[no_mangle]\npub static RMUX_RELEASE_IDENTITY_V1: [u8; {}] = *b\"{marker}\";\n", marker.len()
    )).expect("write stable release commit");
    let target = std::env::var("TARGET").expect("build target");
    let status = Command::new(std::env::var_os("RUSTC").expect("Rust compiler"))
        .args([
            "--crate-name",
            "rmux_release_commit",
            "--crate-type",
            "lib",
            "--emit=obj",
            "--target",
            &target,
            "-C",
            "debuginfo=0",
        ])
        .arg(&code)
        .arg("-o")
        .arg(&object)
        .status()
        .expect("compile stable release commit");
    assert!(status.success(), "stable release commit compilation failed");
    let keep = if target.contains("windows-msvc") {
        "/INCLUDE:RMUX_RELEASE_IDENTITY_V1"
    } else if target.contains("apple-darwin") {
        "-Wl,-u,_RMUX_RELEASE_IDENTITY_V1"
    } else if target.contains("-linux-gnu") {
        // Zig's GNU driver accepts the short spelling of the same GC root.
        "-Wl,-u,RMUX_RELEASE_IDENTITY_V1"
    } else {
        "-Wl,-u,RMUX_RELEASE_IDENTITY_V1"
    };
    for binary in ["rmux", "rmux-daemon"] {
        println!("cargo:rustc-link-arg-bin={binary}={}", object.display());
        println!("cargo:rustc-link-arg-bin={binary}={keep}");
    }
}

// Cargo treats a missing rerun-if-changed path as permanently stale, so only
// existing paths are watched. Each state still has a watched path that changes
// on the next HEAD move: a vanished loose ref is itself a change, and a ref that
// becomes loose lands in the nearest existing directory under `refs`. The whole
// Git directory is never watched because the index changes constantly.
fn release_commit_watch_paths(
    git_path: &dyn Fn(&str) -> Option<std::path::PathBuf>,
    common: Option<std::path::PathBuf>,
    reference: Option<&str>,
) -> Vec<std::path::PathBuf> {
    let existing = |name: &str| git_path(name).filter(|path| path.exists());
    // The reftable backend stores every ref, HEAD included, in these directories:
    // a linked worktree keeps its HEAD apart from the shared branch refs.
    let mut reftables: Vec<_> = existing("reftable")
        .into_iter()
        .chain(
            common
                .map(|common| common.join("reftable"))
                .filter(|path| path.is_dir()),
        )
        .collect();
    reftables.dedup();
    if !reftables.is_empty() {
        return reftables;
    }
    let head = existing("HEAD");
    let Some(loose) = reference.and_then(git_path) else {
        // A detached HEAD file holds the commit itself.
        return head.into_iter().collect();
    };
    if loose.is_file() {
        return head.into_iter().chain([loose]).collect();
    }
    let parent = git_path("refs").and_then(|refs| {
        loose
            .ancestors()
            .skip(1)
            .take_while(|directory| directory.starts_with(&refs))
            .find(|directory| directory.is_dir())
            .map(std::path::Path::to_path_buf)
    });
    head.into_iter()
        .chain(existing("packed-refs"))
        .chain(parent)
        .collect()
}
