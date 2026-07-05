# RMUX intentional divergences from tmux 3.7b

This ledger is the only allowlist source for intentional RMUX 0.9.0
compatibility divergences from the pinned product oracle, `tmux 3.7b`.

Each entry must include:

- Product reason for diverging from tmux 3.7b.
- Regression test or fixture that locks the RMUX behavior.
- Confirmation that RMUX does not advertise unsupported tmux behavior in
  command inventory, help text, parser, source-file handling, hooks, bindings,
  or docs.

Unlisted divergences found by differential tests are bugs or backlog findings,
not accepted behavior.

## Oracle decisions

### C-D1: tmux 3.7b is the sole blocking oracle

RMUX 0.9.0 accepts `tmux 3.7b` as the only blocking product oracle. tmux 3.4,
3.6, and 3.6a may be used as historical context, but they are not acceptance
criteria for 0.9.0.

Test/fixture: `tests/reference/tmux_compat/frozen_reference.yaml` and
`tests/common/tmux_compat.rs` require version `tmux 3.7b`.

Inventory impact: none. This is an oracle policy, not a product divergence.

### C-D2: oracle repins are explicit

The 0.9.0 oracle is pinned to upstream tag `3.7b`, commit
`e802909de06012a4df6209d55e86487c56223163`, and source tarball SHA-256
`87f2e99e3b685973f2ca002ffd6ed7e51a5744f7009daae5a15670b6d532db96`.
Any future tmux 3.7 patch release requires an explicit YAML/script fixture
update and fixture regeneration.

Test/fixture: `scripts/oracle/build-tmux37.sh` verifies the tarball SHA and
writes a `tmux.reference` sidecar; `FrozenTmuxBinary` verifies the source SHA,
tarball SHA, version, and binary SHA/sidecar before running oracle tests.

Inventory impact: none. This is a release process rule.

### C-D10: binary output wins over documentation

When tmux documentation and the pinned `tmux 3.7b` binary disagree, RMUX 0.9.0
uses the binary behavior as the oracle. The fixture that records the behavior
must include a short note about the documentation mismatch.

Test/fixture: future oracle fixtures must cite the exact `tmux 3.7b` command,
stdout, stderr, and exit status they encode.

Inventory impact: no feature may be advertised from documentation alone unless
the binary behavior is implemented or explicitly absent from RMUX inventory.

### C-D11: detached RPC wire is exact-versioned in 0.9.0

RMUX detached RPC is an RMUX extension rather than a tmux surface. For 0.9.0,
the frame envelope is a hard-cut compatibility boundary: `FrameDecoder` and
`decode_frame` accept only `RMUX_WIRE_VERSION`. The handshake min/max fields
are advisory after the current envelope decodes; required capabilities remain
mandatory feature gates.

Test/fixture: `crates/rmux-proto/src/codec/tests.rs` rejects unsupported
wire versions, `crates/rmux-proto/src/capabilities.rs` locks the advisory
handshake window and mandatory capabilities, and
`scripts/fuzz/fuzz_targets/detached_frame_decoder.rs` fuzzes the detached
frame envelope/decoder.

Inventory impact: none for tmux command inventory. SDK and daemon clients must
not imply cross-version wire compatibility unless the envelope range is
explicitly widened and covered by fixtures.

### C-D12: SDK armed waits use a second best-effort cancel transport

The public SDK is an RMUX extension. `Pane::wait_for_next` and
`Pane::wait_for_text_next` intentionally open a wait transport plus a separate
best-effort cancellation transport so dropping or timing out an armed wait can
request cancellation without closing the wait response stream. This double
connect is not a tmux behavior and is not part of command compatibility.

Test/fixture: `crates/rmux-sdk/tests/armed_wait.rs` asserts that armed waits
require `sdk.waits.armed`, wait for the daemon armed ACK before returning the
handle, and send best-effort cancel requests on drop/timeout. On Windows,
`crates/rmux-sdk/src/wait.rs` retains the 250ms post-ACK dispatch settle.

Inventory impact: none for tmux command inventory. The behavior may be
documented only as SDK transport policy, not as a tmux CLI feature.

## Deferred decisions

### C-D32: floating panes and new-pane remain deferred

tmux 3.7b advertises floating-pane and `new-pane` surfaces that RMUX 0.9.0 does
not implement in Lot 5. These remain backlog items until parser, command
inventory, runtime behavior, and differential fixtures can move together.

Test/fixture: Lot 5 parser and inventory gates cover only the newly accepted
command flags added in this lot; floating-pane and `new-pane` acceptance must
not be inferred from this ledger entry.

Inventory impact: RMUX must not advertise floating-pane or `new-pane` support
until implementation and oracle-backed tests land in a future scoped lot.

### C-D33: scrollbar rendering is deferred

Lot 5 accepts tmux parser compatibility for flags such as `copy-mode -S`, but
terminal scrollbar rendering and interactive scrollbar behavior remain outside
this lot.

Test/fixture: `src/cli_args_tests/pane_io.rs` covers parser acceptance only.

Inventory impact: command signatures may accept the tmux flag for compatibility,
but UI/runtime scrollbar behavior must stay unadvertised until implemented and
tested in its own scoped change.

### C-D34: copy-mode line-number gutters are deferred

tmux 3.7b exposes `copy-mode-line-numbers`,
`copy-mode-line-number-style`, and `copy-mode-current-line-number-style`.
RMUX 0.9.0 Lot 6 registers these options with tmux 3.7b defaults so existing
configuration files and `show-options` output do not fail, but the copy-mode
screen renderer does not yet draw line-number gutters for non-`off` values.

Test/fixture: `crates/rmux-core/src/options/tests/registry_metadata.rs`
locks the accepted option names and tmux 3.7b default values. Rendering tests
in Lot 6 continue to cover the default `off` behavior only.

Inventory impact: RMUX may list these options as accepted options for config
compatibility, but user-facing docs and help must not claim line-number gutter
rendering until the copy-mode renderer accepts window options and has
oracle-backed render tests.

### C-D35: invalid capture-pane bounds stay strict

tmux 3.7b accepts some non-numeric or malformed `capture-pane` bounds by
falling back to permissive parsing. RMUX keeps these as user-visible errors so
scripts do not silently capture a different range than requested.

Test/fixture: `tests/capture.rs::capture_pane_non_numeric_bounds_are_rejected`
and `tests/cli_surface.rs::capture_pane_invalid_bounds_are_rejected` lock the
direct CLI behavior.

Inventory impact: RMUX may document that invalid `capture-pane` bounds are
rejected, but command inventory and help must not claim byte-for-byte tmux
fallback parsing for malformed bounds unless the strict behavior is removed.

### C-D36: trailing bare format marker stays literal

tmux 3.7b drops a trailing bare `#` in format strings. RMUX preserves it as
literal text because the marker does not start a complete format expansion and
dropping user text is surprising in product output.

Test/fixture:
`crates/rmux-core/src/formats/tests/transformations.rs::trailing_bare_hash_is_literal_product_divergence`
locks the formatter behavior.

Inventory impact: RMUX docs may mention the literal trailing `#` behavior, but
format-token inventory must continue to describe only supported complete
format expansions.
