//! Build-only executable identity checks at the point of child selection.
//!
//! Ordinary builds preserve the selected program. Identity builds bake in the
//! verifier and candidate context, so clearing a child's environment cannot
//! disable verification or redirect it to a different checkout.

use std::ffi::OsStr;
#[cfg(unix)]
use std::io;
use std::path::PathBuf;

mod selected_program;
pub use selected_program::resolve;
#[cfg(unix)]
use selected_program::verifier;

/// Verify Cargo's executable at each harness use. Unix identity builds return
/// a static immutable launcher; ordinary and Windows builds retain Cargo's path.
pub fn cargo_binary(program: &'static str) -> &'static str {
    let actual = resolve(OsStr::new(program)).expect("candidate executable identity");
    #[cfg(unix)]
    if option_env!("RMUX_IDENTITY_SCRIPT").is_some() {
        // The verifier selected an immutable launcher. Its exec guard checks
        // the retained compiler output again for each consumer, including
        // callers which require Cargo's static string interface.
        return Box::leak(
            actual
                .into_os_string()
                .into_string()
                .expect("candidate executable identity path is UTF-8")
                .into_boxed_str(),
        );
    }
    assert_eq!(
        actual,
        PathBuf::from(program),
        "candidate executable identity path differs"
    );
    program
}

/// Attribute a generated Unix test launcher to its verified Rust test producer.
#[cfg(unix)]
pub fn bind_test_launcher(path: &std::path::Path, kind: &str) -> io::Result<()> {
    let Some(mut verifier) = verifier()? else {
        return Ok(());
    };
    let output = verifier.arg("fixture").arg(path).arg(kind).output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            "candidate executable identity fixture rejected",
        ));
    }
    Ok(())
}
