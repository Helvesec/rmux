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

### C-D13: Windows status-right defaults to host_short

tmux 3.7b defaults `status-right` to `#{=21:pane_title}`. RMUX keeps
`#{=21:host_short}` on Windows because ConPTY pane titles are noisy and often
reflect shell or host process paths rather than useful session state. Unix-like
platforms keep the tmux 3.7b `pane_title` default.

Test/fixture:
`crates/rmux-core/src/options/tests/effects_defaults.rs` locks the platform
split, and `crates/rmux-server/src/handler_attach_tests/attach_render.rs`
locks the Windows attach render path.

Inventory impact: RMUX may document the Windows default as an intentional
product divergence. Cross-platform compatibility claims must not say the
Windows `status-right` default is byte-for-byte tmux 3.7b behavior.

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

### C-D37: float-flag expression comparisons render integer booleans

RMUX keeps comparison results as integer boolean text even when the expression
uses the `f` flag. tmux 3.7b formats the same truthy comparison result as a
float string.

Test/fixture: `tests/fixtures/tmux_3_7_round4_evidence.md` records the empirical tmux 3.7b
commands. On July 7, 2026,
`tmux -L r4 -f /dev/null display-message -p '#{e|==|f:5,5}'`,
`'#{e|!=|f:5,6}'`, `'#{e|<|f:5,6}'`, `'#{e|<=|f:5,5}'`,
`'#{e|>|f:6,5}'`, and `'#{e|>=|f:5,5}'` all exited 0 with stdout `1.00`;
the same RMUX commands exited 0 with stdout `1`.

Inventory impact: this affects format rendering only. RMUX must not claim
byte-for-byte tmux float formatting for expression comparison results unless
the formatter is changed and covered by oracle tests.

### C-D38: expression operands with embedded spaces stay permissive

RMUX trims and evaluates spaced arithmetic operands such as ` 5 , 3 `. tmux
3.7b renders the expression empty. RMUX keeps the permissive behavior for now
because it accepts user-authored configuration that contains incidental spaces
without silently failing the whole format.

Test/fixture: `tests/fixtures/tmux_3_7_round4_evidence.md` records
`tmux -L r4 -f /dev/null display-message -p '#{e|+|: 5 , 3 }'` exiting 0 with
empty stdout, while RMUX exits 0 with stdout `8`.

Inventory impact: expression-format docs may describe RMUX's permissive operand
trimming, but compatibility summaries must not call this subcase byte-identical
to tmux 3.7b.

### C-D39: tmux 3.7b split-window extension flags remain deferred

tmux 3.7b accepts the newer `split-window -k`, `-m`, `-s`, `-S`, and `-R`
surfaces. RMUX 0.9.0 does not implement their runtime semantics yet, so it
rejects them instead of accepting flags that would behave incorrectly.

Test/fixture: `tests/fixtures/tmux_3_7_round4_evidence.md` records tmux 3.7b accepting
`split-window -k` with rc 0 and parsing `split-window -m`, `-s`, `-S`, and
`-R` far enough to report `expects an argument`; RMUX reports
`command split-window: unknown flag -k` and the analogous unknown-flag errors
for `-m`, `-s`, `-S`, and `-R`.

Inventory impact: RMUX must not advertise these split-window flags as supported
runtime behavior until parser, command inventory, runtime, and oracle fixtures
land together.

### C-D41: refresh-client subscription flags are unsupported and unadvertised

tmux 3.7b accepts `refresh-client -A`, `-B`, and `-r` syntactically and then
requires a current client for the measured detached invocations. RMUX rejects
the same flags at the CLI and source-file parser because their subscription and
colour-report semantics are not implemented.

Test/fixture: `tests/fixtures/tmux_3_7_round4_evidence.md` records tmux 3.7b
`refresh-client -A %0:foo`, `-B name:what:format`, and `-r pane:fmt` exiting 1
with `no current client`. RMUX keeps the corresponding wire-v5 fields reserved
and fail-closed, but does not expose the flags through help, completions, or
`list-commands`.

Inventory impact: the public command surface must not advertise these flags
until parser, runtime, and oracle-backed behavior land together.

### C-D42: respawn-pane without a command uses the default shell

tmux 3.7b respawns a dead pane with its original command when no command is
supplied. RMUX respawns with the session default shell. This preserves RMUX's
current pane creation model, which does not retain enough command provenance to
reconstruct every original argv safely.

Test/fixture: `tests/fixtures/tmux_3_7_round4_evidence.md` records a `remain-on-exit`
session with a dead `true` pane. tmux 3.7b `respawn-pane -t w:1.0` exits 0 and
`display-message -p -t w:1.0 '#{pane_current_command}'` prints `true`; RMUX
exits 0 and prints `bash`.

Inventory impact: RMUX must not claim tmux-compatible no-argument
`respawn-pane` command resurrection until command provenance is stored and
covered by tests.

### C-D43: control-mode attach replays initial pane backlog

tmux 3.7b does not replay existing pane backlog as `%output` during the
measured control-mode attach. RMUX emits the pane's current backlog as initial
`%output`, which is useful for RMUX control clients that expect an immediate
snapshot.

Test/fixture: `tests/fixtures/tmux_3_7_round4_evidence.md` records
`tmux -C -L r4 -f /dev/null attach -t w` after sending `printf old` producing
`%begin`, `%end`, `%session-changed $0 w`, `%exit` and no `%output`. The same
RMUX control attach emits `%output %0 ... printf old ... old ...` before
`%exit`.

Inventory impact: control-mode compatibility claims must exclude initial
backlog replay until RMUX either removes it or adds a negotiated mode.

### C-D44: shutdown hook run-shell delivery is best effort

tmux 3.7b drains more shutdown-time `run-shell` hook markers before server exit.
RMUX treats server shutdown as a hard boundary and does not guarantee that
asynchronous `run-shell` hook jobs complete after `kill-server` or the last
session closes.

Test/fixture: `tests/fixtures/tmux_3_7_round4_evidence.md` records a
single-session shutdown probe measured on July 8, 2026: a `session-closed`
hook running `run-shell "printf '%s\n' session-closed >> '$out'"` delivered the
marker under tmux 3.7b, while RMUX wrote no marker before daemon exit. Round4
intentionally did not change shutdown draining.

Inventory impact: hooks remain listed, but documentation must not promise
tmux-identical asynchronous `run-shell` delivery during daemon shutdown.

### C-D45: startup config messages without a current session are not surfaced

tmux 3.7b renders some startup config messages before the first client reaches
the normal pane view. RMUX applies the same final session state but does not
surface that early config status message.

Test/fixture: `tests/fixtures/tmux_3_7_round4_evidence.md` records a config containing
`display-message hello`. Under pty startup, tmux 3.7b showed
`/tmp/r4-startup-...conf:1: hello`; RMUX attached to the created session without
that config status message.

Inventory impact: source-file and startup config support remain advertised for
accepted syntax and final state, but first-client diagnostic rendering is not
byte-identical to tmux 3.7b.

### C-D46: mouse placeholder targets outside mouse events use RMUX diagnostics

tmux 3.7b reports `no mouse target` when `select-window -t=` or
`kill-window -t=` is run outside a mouse event. RMUX currently reports the
empty target through its session resolver as `invalid session: `.

Test/fixture: `tests/fixtures/tmux_3_7_round4_evidence.md` records tmux 3.7b
`select-window -t=` and `kill-window -t=` exiting 1 with `no mouse target`;
RMUX exits 1 with `invalid session: ` for both commands.

Inventory impact: this is an error-surface divergence only. Mouse binding
behavior remains covered separately; docs must not claim byte-identical
diagnostics for bare `-t=` outside mouse events.

### C-D47: kill-window last-window CLI fallback keeps SDK error semantics

tmux 3.7b emits the last-window `window-unlinked` hook before session closure.
RMUX's CLI falls back from `kill-window` to `kill-session` for the last window
so the server and SDK can keep the documented direct `window.kill()` error for
the only-window case. That fallback reverses the last-window hook order.

Test/fixture: `tests/fixtures/tmux_3_7_round4_evidence.md` records a
July 8, 2026 deterministic hook probe: with a second session keeping the
server alive, `kill-window -t victim:0` on the target session's last window
produced tmux order `window-unlinked` then `session-closed`, while RMUX
produced `session-closed` then `window-unlinked`. Round4 code deliberately
left `crates/rmux-sdk/src/handles/window.rs` only-window kill error semantics
intact.

Inventory impact: CLI `kill-window` behavior remains available, but hook-order
parity claims must exclude the last-window fallback path.

### C-D48: queued attach terminal-exit banners are omitted

tmux 3.7b writes terminal-exit banners for queued attached clients, including
`[detached (from session w)]` and `[server exited]`. RMUX completes the state
transition and returns the matching exit status, but emits no banner bytes on
the queued attach path.

Test/fixture: `tests/fixtures/tmux_3_7_round4_evidence.md` records pty probes:
`attach -t w \; detach-client` exits 0 in both tools, tmux transcript length
539 with `[detached (from session w)]`, RMUX transcript length 0; `attach -t w
\; kill-server` exits 1 and leaves no server in both tools, tmux contains
`[server exited]`, RMUX transcript length 0.

Inventory impact: attach sequencing compatibility may be advertised for exit
status and final state, but not for terminal-exit banner bytes on queued attach
until banner rendering is implemented.

### C-D49: undefined expression arithmetic is deterministic

tmux 3.7b evaluates expression arithmetic in IEEE doubles and converts
operands and integer results through C `(long long)` casts. The conversion of
non-finite or out-of-range doubles to `long long` is undefined behavior in C
and diverges by CPU (x86_64 produces the `-9223372036854775808` sentinel,
AArch64 saturates), and the sign glibc/BSD printf gives a NaN differs by
platform too. RMUX normalizes both classes to the Linux x86_64 oracle:
undefined integer results (and the comparison operands derived the same way)
become the sentinel `-9223372036854775808`, and every float NaN result
renders `-nan`, so release behavior is deterministic across CPUs. RMUX
accepts `m`, `%`, and `%%` as modulo spellings: glibc tmux evaluates both `%`
forms, while the darwin oracle's BSD strftime consumes a lone `%` before the
format parser ever sees it (a libc artifact upstream of the expression
engine, visible as `x%:y` -> `x:y`).

Test/fixture: `crates/rmux-core/src/formats/tests/operators.rs` and
`tests/display_message.rs` cover the modulo spellings, the sentinel cases for
integer divide-by-zero, modulo-by-zero, non-finite and out-of-range operands,
non-finite comparisons, and the deterministic `-nan` float renders.

Inventory impact: format rendering remains advertised, but compatibility
claims for expression arithmetic must describe these undefined cases as RMUX's
deterministic Linux-oracle behavior rather than byte-identical tmux behavior on
every CPU.

### C-D50: OSC 10/11/12 queries round-trip only application-set colours

A pane program that queries the terminal's default colours (`OSC 10;?`,
`OSC 11;?`, `OSC 12;?`) inside a detached tmux 3.7b session receives no
answer at all (probed 2026-07-11: the query times out silently), so
theme-detecting TUIs fall back to their own palette. With an attached client,
tmux forwards the query to the real outer terminal and relays its answer, if
any (probed 2026-07-11). RMUX does not currently forward this query across the
attach transport. It reports a colour only when the pane application already
set that exact OSC 10/11/12 slot; otherwise the query remains unanswered. This
avoids presenting a daemon-wide guessed dark palette as the attached client's
real theme. `OSC 110/111/112` resets the corresponding slot to unknown.

Test/fixture: `crates/rmux-core/src/input/tests/osc_dcs_misc.rs` covers the
unknown-query silence for all three slots with both terminators, the
set-then-query round trip, and reset-to-unknown behavior.

Inventory impact: OSC colour handling remains advertised, but compatibility
claims must describe attached query forwarding as unsupported. Per-pane
application-set colour round-tripping is RMUX product behavior, not a claim of
byte-identical tmux behavior.

### C-D51: Windows bracketed paste is detected from console input bursts

On Unix the outer terminal wraps a paste in bracketed-paste markers before rmux
sees it, so the daemon keeps or strips them per the pane's paste mode with no
ambiguity. The Windows attach client reads console input records
(ReadConsoleInputW), where a pasted character is byte-for-byte identical to a
typed one and a pasted newline arrives as `VK_RETURN` exactly like a typed
Enter, with no per-record injected flag (probed against conhost 2026-07-11:
pasting reaches the app as ordinary key-downs carrying their real virtual-key
codes). RMUX therefore treats a single `ReadConsoleInputW` batch carrying two or
more plain character key-downs, with no Control/Alt key, as a paste and wraps it
in bracketed-paste markers; the daemon then keeps or strips them like a Unix
paste. A native `MOUSE_EVENT` coalesced into that read is suppressed instead of
making the text live, but still updates the tracked button state so the next
move cannot become a phantom drag. Mouse-looking SGR bytes delivered as
`KEY_EVENT` records have no trustworthy mouse provenance and remain inside the
paste envelope. Markers embedded in the pasted content are stripped before
wrapping so crafted clipboard data cannot break out of the envelope. This is a
best-effort heuristic and a residual divergence: a single-character paste is
not bracketed (indistinguishable from a keystroke); two or more genuine
keystrokes — or a multi-character IME commit — that conhost happens to return
in one batch are bracketed; a paste larger than one `ReadConsoleInputW` batch
(32 records, 16 characters) is bracketed across batches using the input-buffer
drain as the end signal, so a mid-paste buffer drain can split one paste into
adjacent bracketed regions; and a host that exposes mouse input only as SGR
`KEY_EVENT` bytes loses those mouse events by deliberate fail-close policy.

Because this can wrap burst-delivered typed text, the daemon strips
bracketed-paste markers before feeding input to the command prompt on Windows
(`handle_attached_prompt_input`): the prompt treats a paste as literal text, so
without stripping the leading `ESC` of `ESC[200~` would cancel the prompt and the
body would leak to the pane's shell.

Test/fixture: `crates/rmux-client/src/attach_windows/input.rs` unit tests cover
the multi-character burst, the single-character passthrough, the cross-batch
continuation, the Control-chord exclusion, and embedded-marker stripping; the
`tests/windows_prompt_overlay_chain.rs` command-prompt chain covers the prompt
marker stripping.

Inventory impact: bracketed paste is advertised on Windows, but the detection is
a documented best-effort heuristic rather than terminal-driven bracketing.

### C-D52: Windows advertises OSC 52 clipboard; inbound clipboard queries are forwarded

On Unix tmux advertises the clipboard (Ms) capability from terminfo. Windows has
no terminfo, so rmux advertises the clipboard (OSC 52) capability for every
Windows attach: Windows Terminal sets the system clipboard from OSC 52 natively
and any other VT outer ignores the sequence. Without it the daemon has no Ms
template and a pane's OSC 52 under set-clipboard on never reaches the outer
(issue #91).

Application clipboard writes stay gated on set-clipboard on exactly as tmux gates
them (input.c input_osc_52 returns early unless set-clipboard == 2): under the
`external` default an application's inbound OSC 52 creates no paste buffer and is
not forwarded, so untrusted pane output cannot drive the system clipboard; under
`on` the write is stored in a paste buffer (paste_add), forwarded to the outer,
and fires the pane-set-clipboard hook. tmux's own selections (copy-mode yank and
`set-buffer -w`) keep forwarding under `on` or `external`, and `set-buffer -w`
forwards even under `off`, unchanged.

Residual divergence: an application's inbound OSC 52 query (a request of the form
ESC ] 52 ; c ; ? ) is handled per set-clipboard: under `on` it is forwarded to
the outer terminal rather than answered from rmux's paste buffer per
get-clipboard; under the `external` default and `off` it is dropped entirely
(neither answered nor forwarded). tmux answers the query from the top buffer
under the default get-clipboard buffer regardless of set-clipboard; honouring
get-clipboard (none / buffer / external) for inbound pane queries is not yet
implemented. Malformed OSC 52 writes and empty payloads are dropped rather than
forwarded verbatim, matching tmux's validate-then-paste_add ordering. Clipboard
writes — the subject of issue #91 — match tmux.

Test/fixture: outer-terminal gate tests in
`crates/rmux-server/src/outer_terminal/tests.rs`, client and daemon capability
tests in `src/client_terminal.rs` and
`crates/rmux-server/src/handler_client_runtime.rs`, decode and inbound-buffer
tests in `crates/rmux-server/src/pane_io/reader.rs` and
`crates/rmux-server/src/handler_alert_tests.rs`.

Inventory impact: OSC 52 clipboard writes are advertised and honored on Windows
under set-clipboard on; inbound clipboard queries remain a forward-to-outer
approximation pending get-clipboard support.

### C-D53: recognized variable-length controls use a streaming idle budget

tmux 3.7b applies the configured keyboard `escape-time` while an attached
client has supplied an unterminated variable-length control. A live probe on
2026-07-13 with `escape-time` set to 500 ms retained an incomplete OSC 52 body
at 100 ms and delivered its bytes to the pane by 600 ms. RMUX uses the same
keyboard deadline while the bytes are still ambiguous, then promotes a
recognized bracketed-paste, consumed OSC, or Kitty graphics APC opener to an
eight-second idle deadline. Each newly received fragment resets that streaming
idle deadline; unrelated output or status wakeups do not. This prevents a
valid paste or graphics transfer from being split merely because a transport
read pauses longer than keyboard `escape-time`, while still bounding abandoned
input.

Test/fixture: `crates/rmux-server/src/pane_io/pending_escape.rs` covers the
complete retained grammar and its deadline transitions;
`crates/rmux-server/src/pane_io/tests.rs` exercises split OSC/APC/paste input,
invalid CSI bodies, timer fairness under a continuously ready socket, and
keyboard-suffix replacement through the production attach path.

Inventory impact: `escape-time` remains the keyboard ambiguity budget, not a
promise that every recognized variable-length transfer is cut at that value.

### C-D54: control EOF closes transport before finite queued work completes

With tmux 3.7b, EOF closes a plain control client with a final `%exit`; an
already active finite `run-shell` can finish, but a later queued frame is
dropped. This was measured on 2026-07-13 with two marker-writing frames: the
first marker appeared and the second did not. RMUX also emits `%exit` and
closes the transport promptly, but deliberately keeps the authenticated queue
lease alive to finish finite frames already accepted from that client. It
cancels selected indefinite waits, stops on explicit exit or shutdown, and
prevents a same-PID replacement registration from overtaking the old drain.
No post-EOF reply is written to the closed transport.

Test/fixture: `crates/rmux-server/src/control/tests.rs` names the intentional
cases `..._product_divergence` and covers finite follow-on work, conditional
waits, parse failures, explicit exit, kill-server, shutdown cancellation,
permissions, and same-PID lease ordering. The oracle-backed guard and `%exit`
tuple remains covered by
`tests/tmux_compat_surface_matrix/client_control.rs`.

Inventory impact: control-mode framing and terminal `%exit` remain compatible;
automation side effects after transport EOF intentionally favor completion
over tmux's later-frame drop.

### C-D55: malformed mouse and timed-out paste fragments fail closed

tmux 3.7b accepts empty decimal fields in SGR mouse input as zero and, after
`escape-time`, forwards a partial bracketed-paste end marker to the pane as
ordinary bytes. Both behaviors were measured on 2026-07-13: an empty-button
SGR click was consumed as mouse input, while an incomplete `ESC [ 20` suffix
was delivered after the paste body. RMUX requires all three SGR decimal fields
to be present, consumes overflowed or unterminated frames at a fixed 32-byte
syntax bound, and discards an impossible completed coordinate. If a recognized
bracketed paste reaches its idle deadline, RMUX forwards the body but removes
every trailing proper prefix of either paste delimiter. These policies prevent
malformed control prefixes from being reinterpreted as prompt text, bindings,
or a second live escape sequence.

Test/fixture: `crates/rmux-server/src/input_keys/tests.rs` covers missing,
zero, overflowing, and attacker-sized SGR fields;
`crates/rmux-server/src/handler_send_keys_tests/attached_input_bounds.rs` pins
the production retention bound; and
`crates/rmux-server/src/handler_pane/attached_input/bracketed_paste.rs` covers
all partial delimiter suffixes and fixed-point neutralization.

Inventory impact: mouse and paste support remain advertised, but malformed
SGR acceptance and timed-out delimiter bytes are intentionally stricter than
tmux 3.7b.
