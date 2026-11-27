//! Cross-entry acceptance: a silent daemon cannot extend the caller's wait budget.
use std::io::{self, Read};
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use rmux_sdk::{PaneRef, RmuxBuilder, RmuxError, SessionName};

use crate::cli_args::WaitPaneArgs;

#[test]
fn cli_and_sdk_wait_deadlines_bound_a_silent_daemon() {
    let timeout = Duration::from_millis(50);
    let upper_bound = Duration::from_millis(250);
    let socket = std::env::temp_dir().join(format!("rmux-wait-budget-{}.sock", std::process::id()));
    let listener = UnixListener::bind(&socket).expect("bind fixture");
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept caller");
            let mut request = [0u8; 4096];
            assert!(stream.read(&mut request).expect("first request") > 0);
            std::thread::sleep(Duration::from_millis(400));
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let sdk_elapsed = runtime.block_on(async {
        let rmux = RmuxBuilder::new().unix_socket(&socket).default_timeout(timeout).build();
        let pane = rmux.pane(PaneRef::new(SessionName::new("alpha").unwrap(), 0, 0)).await.unwrap();
        let start = Instant::now();
        let error = pane.wait_for_text("missing").await.expect_err("silent SDK wait");
        assert!(matches!(error, RmuxError::Transport { source, .. } if source.kind() == io::ErrorKind::TimedOut));
        start.elapsed()
    });
    // Allow the first fixture peer to close before timing the CLI peer.
    std::thread::sleep(Duration::from_millis(450));
    let start = Instant::now();
    let result = super::wait::run_wait_pane(
        WaitPaneArgs {
            target: None,
            text: Some("missing".to_owned()),
            next_text: None,
            visible_text: None,
            quiet: false,
            stable_for: None,
            pane_exit: false,
            timeout: Some(timeout),
            json: false,
            get_by_text: None,
        },
        &socket,
    );
    let cli_elapsed = start.elapsed();
    server.join().expect("fixture server");
    std::fs::remove_file(socket).expect("remove owned socket");
    assert!(
        sdk_elapsed < upper_bound,
        "SDK exceeded wait budget: {sdk_elapsed:?}"
    );
    assert!(
        cli_elapsed < upper_bound,
        "CLI exceeded wait budget: {cli_elapsed:?}"
    );
    let error = result.expect_err("silent CLI wait");
    assert_eq!(error.exit_code(), 1);
    assert!(error.message().contains("timed out"), "{}", error.message());
}
