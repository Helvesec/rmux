#![cfg(unix)]

use rmux_os::path::socket_paths_match;
use std::path::Path;

#[path = "../../../tests/support/rerun_exact.rs"]
mod test_rerun;

#[test]
fn missing_bare_socket_matches_dot_and_absolute_spellings() {
    const CHILD: &str = "RMUX_SOCKET_PATH_PARENT_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = std::env::temp_dir().join(format!(
            "rmux-socket-parent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let rerun = std::panic::catch_unwind(|| {
            test_rerun::rerun_exact(
                "missing_bare_socket_matches_dot_and_absolute_spellings",
                |command| {
                    command.env(CHILD, "1").current_dir(&root);
                },
            )
        });
        std::fs::remove_dir_all(&root).unwrap();
        if let Err(panic) = rerun {
            std::panic::resume_unwind(panic);
        }
        return;
    }

    let absolute = std::env::current_dir().unwrap().join("review.sock");
    assert!(!absolute.exists());
    let spellings = [
        Path::new("review.sock"),
        Path::new("./review.sock"),
        &absolute,
    ];
    for left in spellings {
        for right in spellings {
            assert!(socket_paths_match(left, right), "{left:?} != {right:?}");
        }
    }
}
