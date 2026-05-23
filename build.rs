use std::process::Command;

fn main() {
    const WINDOWS_MANIFEST: &str = "resources/windows/rmux.exe.manifest";

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest::embed_manifest_file(WINDOWS_MANIFEST)
            .expect("unable to embed Windows application manifest");
    }

    // Embed `<short-hash>[-dirty]` so `rmux -V` reports exactly which
    // commit it was built from. Falls back to "unknown" when there's
    // no git checkout (tarball builds, `cargo install` from crates.io).
    let git_hash = git_describe().unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=RMUX_GIT_HASH={git_hash}");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={WINDOWS_MANIFEST}");
    // Re-run when HEAD moves (commit, checkout, rebase). `.git/HEAD`
    // changes on branch switch; `.git/index` changes on add/commit so
    // dirtiness flips. Both are cheap stat()s for cargo.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}

fn git_describe() -> Option<String> {
    let hash = run_git(&["rev-parse", "--short=12", "HEAD"])?;
    // `diff-index --quiet HEAD` exits 1 iff any tracked file differs
    // from HEAD. Unlike `git status`, it ignores untracked files —
    // which matters because an untracked sibling dir (e.g. an editor
    // scratch folder) shouldn't make the binary report `-dirty`.
    let dirty = !Command::new("git")
        .args(["diff-index", "--quiet", "HEAD"])
        .status()
        .map(|status| status.success())
        .unwrap_or(true);
    Some(if dirty {
        format!("{hash}-dirty")
    } else {
        hash
    })
}

fn run_git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}
