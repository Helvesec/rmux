# Server platform-selection budget

The `rmux-server` ceiling in `scripts/cfg-check.sh` remains **9**. The scan and
all crate ceilings are unchanged; `rmux-os` remains at 30. The server has three
production selections and six test selections.

Paths below are relative to `crates/rmux-server/src/`. Each row accounts for
one direct `cfg(target_os = ...)` occurrence counted by the existing scan.

| # | File / item | Boundary and responsibility |
|---|---|---|
| 1 | `buffer_file_io/unix.rs`: `mod darwin_fifo_reader` | Darwin adapter for the server's private FIFO helper protocol, cancellation lifecycle and recovery policy for an already-opened nonblocking descriptor. |
| 2 | `buffer_file_io/unix.rs`: `use darwin_fifo_reader as fifo_reader` | Binds the Darwin adapter to the `fifo_reader` interface that the non-macOS fallback also implements. |
| 3 | `buffer_file_io/unix.rs`: `assert_read_target_is_safe` | Regression assertion for the raced Darwin read; other Unix platforms retain cancellation assertions. |
| 4 | `daemon.rs`: `with_startup_ready_fd` | Existing Linux-only server configuration API; notification transport is owned by `rmux_os::daemon::StartupReadyNotification`. |
| 5 | `handler_scripting_tests/fpc1.rs`: `native_resize` | One Darwin test adapter containing `ResizeCase`, probe arming and result extraction together. |
| 6 | `handler_scripting_tests/fpc1.rs`: `fpc1_native_resize_calls_are_exact_coalesced_and_skip_empty_panes` | Server command scope, coalescing and empty-pane native assertions. |
| 7 | `handler_scripting_tests/fpc1.rs`: `fpc1_partial_resize_uses_the_same_exact_scope_for_compensation` | Server rollback, publication and compensation scope native assertions. |
| 8 | `handler_layout_tests.rs`: `l1_033_native_snapshot_v2_delta_scope_and_rollback` | Linux PTY snapshot/resize witness with Linux process-birth recording. |
| 9 | `terminal/tests.rs`: shell-context child diagnostic | Linux process metadata for the Unix shell-context witness. |

Rows 3 and 5–9 are tests. The Darwin resize probe previously repeated its
selection on the type import and both helpers. Those three related selections
now form one platform module; both native tests still select macOS and retain
their original names, assertions and calls. Their probe bodies are unchanged.
The two Linux witnesses are retained, including process-birth diagnostics.
No test is removed, renamed or moved beyond the scanner's source population.

Rows 1 and 2 are one selection: the normal module layout spells it as a
module declaration and an alias. The raced-read recovery policy used to repeat
that selection in `unix.rs`; it now lives in the selected `fifo_reader` module,
with the Unix fallback in `fifo_reader_fallback.rs`. Behavior is unchanged.

The native resize probe is server-owned: it locks `HandlerState`, configures
resize failures and extracts server pane identities. Moving it into `rmux-os`
would invert that dependency. Its platform module is the appropriate boundary.
This regrouping changes test organization, without changing production,
Unix/Windows runtime behavior, public API, wire formats or expected outcomes.
It does not assert Linux or Windows coverage for the Darwin probe.

The count is a narrow textual guard rather than a census of platform behavior.
`cfg-check.sh` counts direct `#[cfg(target_os = ...)]` lines throughout `src`,
including tests. It does not count `cfg(not(...))`, `cfg(all(...))` or
`cfg(windows)` in that category. No selection is respelled to obtain a pass.
The independent neutral-crate scan and its exclusions remain unchanged.
