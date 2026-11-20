use rmux_proto::{
    OptionName, PaneTarget, Request, Response, ScopeSelector, SetOptionMode, SetOptionRequest,
};
use tokio::time::{sleep, Duration, Instant};

use super::RequestHandler;

/// One liveness bound for a test that waits on the server's own signal: a
/// background shell's result, or a pane's death after its child has exited.
///
/// It is not a latency assertion. Each such wait ends as soon as the server
/// publishes the signal, which takes milliseconds once the test process runs
/// (a kept float's death: 5 to 480 ms on a Mac running the full suite up to
/// load 55). The bound only has to outlast a stretch in which the test process
/// is not scheduled at all. A cold Windows PowerShell under several CI shards
/// is one. A Mac running the full suite under nice at load 83 to 86 is
/// another: a 10 s sampler of the same priority went 11 to 14 s unscheduled.
/// On Linux, a test at nice 19 pinned to two CPUs behind busy loops at nice 0
/// saw a kept `exit 7` pane die up to 10.6 s late. A 5 or 8 s clock failed
/// those deaths as never published; a death that is really never published
/// still fails here, with the waiter's own message.
pub(crate) const SERVER_SIGNAL_LIVENESS: Duration = Duration::from_secs(30);

/// ConPTY publishes a startup preamble (DEC 1004h among it) after
/// `wait_for_pane_startup_to_finish_for_test` returns, even for a quiet child.
/// A Unix PTY running a quiet child stays silent.
const NATIVE_PANE_STARTUP_EMITS_OUTPUT: bool = cfg!(windows);

#[path = "handler_test_support/run_shell_capture.rs"]
mod run_shell_capture;

// The `#{cursor_character}` witnesses keep their file next to the provider
// they describe and are declared here so they reach the same private
// fixtures the other handler tests use. No product surface is added for
// them.
#[path = "format_runtime/cchar1_tests.rs"]
mod cchar1_tests;

// The alternate-screen saved-cursor witnesses follow the same rule: the file
// stays next to the provider, the declaration lives here so they reach the
// private fixtures.
#[path = "format_runtime/asave1_tests.rs"]
mod asave1_tests;

// The window-link witnesses keep their file next to the aggregator they
// describe. They are declared here because a window link needs real pane
// terminals: `link_window` refuses a state that has none, so the fixture has
// to drive the handler, exactly as every other link test in this crate does.
#[path = "pane_terminals/window_links/was1_tests.rs"]
mod was1_tests;

impl RequestHandler {
    /// Make every pane spawned after this call run the quiet stdin-discard
    /// child instead of the interactive default shell.
    ///
    /// A fixture that replaces or measures a live transcript calls this before
    /// it creates its panes. A shell publishes its banner and prompt whenever
    /// it is scheduled, so under load they can land after the replacement and
    /// change what the test reads. Pair it with
    /// `wait_for_pane_startup_output_for_test`: the quiet child does not
    /// silence the ConPTY startup preamble.
    ///
    /// On Unix the child also runs under `/bin/sh` instead of the host's login
    /// shell. The default command is still started as `<shell> -c`, and a
    /// login shell reads its own startup files even then (zsh reads
    /// `.zshenv`): they may print, or change the directory a fixture reads
    /// back as `pane_current_path`.
    ///
    /// `RequestHandler::new()` already starts with these options on Unix; the
    /// call still matters on Windows and after a fixture changed them.
    pub(crate) async fn pin_quiet_default_command_for_test(&self) {
        #[cfg(unix)]
        self.set_global_option_for_test(OptionName::DefaultShell, "/bin/sh".to_owned())
            .await;
        self.set_global_option_for_test(
            OptionName::DefaultCommand,
            crate::test_shell::stdin_discard_command(),
        )
        .await;
    }

    async fn set_global_option_for_test(&self, option: OptionName, value: String) {
        let response = self
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option,
                value,
                mode: SetOptionMode::Replace,
            }))
            .await;
        assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    }

    /// The pane parses `bytes` as its own output, as when it emits a query.
    pub(crate) async fn append_pane_output_for_test(&self, target: &PaneTarget, bytes: &[u8]) {
        self.state
            .lock()
            .await
            .append_bytes_to_pane_transcript_for_test(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
                bytes,
            )
            .expect("fixture pane parses its output");
    }

    /// Give byte-exact input fixtures an explicit focus-off precondition.
    /// Native ConPTY startup enables DEC 1004 even for a quiet child command.
    pub(crate) async fn disable_pane_focus_reports_for_test(&self, target: &PaneTarget) {
        // Place 1004l after the native 1004h of the ConPTY preamble.
        self.wait_for_pane_startup_output_for_test(target).await;
        let mut state = self.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
                b"\x1b[?1004l",
            )
            .expect("fixture disables pane focus reporting after startup");
        assert_eq!(
            state
                .transcript_handle(target)
                .expect("fixture transcript")
                .lock()
                .mode()
                & rmux_core::input::mode::MODE_FOCUSON,
            0,
            "fixture owns the initial focus mode"
        );
    }

    /// Wait until pane startup has finished and, where the native terminal
    /// emits a startup preamble, until the reader has published it. Fixtures
    /// that pin output sequences or exact bytes call this before their first
    /// observation, because a quiet child does not silence that preamble.
    pub(crate) async fn wait_for_pane_startup_output_for_test(&self, target: &PaneTarget) {
        self.wait_for_pane_startup_to_finish_for_test(target).await;
        if !NATIVE_PANE_STARTUP_EMITS_OUTPUT {
            return;
        }
        let output = self
            .state
            .lock()
            .await
            .pane_output_for_target(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
            .expect("fixture pane output exists");
        // ConPTY buffers its complete startup preamble before spawn returns.
        // The reader may still be parked or unscheduled: let it publish that
        // first read before the fixture observes the output sequence.
        let _demand = output.acquire_read_demand();
        let deadline = Instant::now() + Duration::from_secs(15);
        while output.capture_with_next_sequence(|| ()).0 == 0 {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for pane {target} native startup output"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    pub(crate) async fn hold_control_subscription_lookup_for_test(&self) -> impl Drop + '_ {
        self.state.lock().await
    }

    pub(crate) async fn wait_for_pane_terminal_for_test(&self, target: &PaneTarget) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let active = {
                let mut state = self.state.lock().await;
                state
                    .clone_pane_master_if_alive(
                        target.session_name(),
                        target.window_index(),
                        target.pane_index(),
                    )
                    .is_ok()
            };
            if active {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for pane {target} terminal to become active"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    pub(crate) async fn wait_for_pane_startup_to_finish_for_test(&self, target: &PaneTarget) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let ready = {
                let mut state = self.state.lock().await;
                let terminal_active = state
                    .clone_pane_master_if_alive(
                        target.session_name(),
                        target.window_index(),
                        target.pane_index(),
                    )
                    .is_ok();
                let still_starting = {
                    #[cfg(windows)]
                    {
                        state.pane_is_starting_in_window(
                            target.session_name(),
                            target.window_index(),
                            target.pane_index(),
                        )
                    }
                    #[cfg(not(windows))]
                    {
                        false
                    }
                };
                terminal_active && !still_starting
            };
            if ready {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for pane {target} startup marker to finish"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }
}
