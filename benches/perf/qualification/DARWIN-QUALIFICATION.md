# Common Darwin qualification

`scripts/bench/qualify_darwin.py` exercises the common driver and qualification
gate with the installed RMUX package and tmux 3.7b. It requires an externally
pinned manifest before launching anything. The manifest records both products'
argument templates, cold/warm preparation, reset, timing window, independent
postcondition, exact sample population/order, timeouts, sources and executables.

This is a **functional qualification population**, not a shortened public
benchmark. It has cold and warm `new-session` cells. The existing 33-cell/10-public
population remains unchanged. The public 20-window and 80-pane recipes are not
silently reduced to fit a smaller qualification fixture. Comparative publication
still requires the full applicable population, a qualified quiet and thermal
window, and three independent paired/interleaved runs.

Both products use `darwin_driver.run`: environment/descriptors are prepared
before timing; the primary clock spans `posix_spawn` through actual `waitpid`.
Optional own-child image observations are inside that clock and their overhead
is explicitly retained. Floor measurements are separate diagnostics; they are
never subtracted from primary values. The collector publishes every attempt,
first failure, unattempted identities, raw values, linear p50/p95/p99 and a
seeded percentile-bootstrap interval for the median. Small qualification samples
do not establish tail precision, confidence in a performance ratio, or a win.

A cold fixture starts with an absent private endpoint. A warm fixture starts a
daemon outside the timer and requires the same PID/birth after the operation.
Each sample creates one 80x24 session/pane, then checks public state and has its
shell execute a fresh challenge into the existing private FIFO/kqueue validator.
Neither cached capture output nor elapsed waiting is a readiness receipt.
Held daemon/pane exit events and subsequent birth absence close the fixture.
An indirect exit event does not become an invented numeric wait status. Unknown
closure retains the process's owned kqueues and blocks admission of another lot.

The shared `qualification.qualification_errors` gate checks immutable population,
attempt order/count, no retry, actual wait and clock window, exact command,
regime, fresh independent work, closure, artifact checks and recomputed statistics.
The same function validates the saved native artifacts and intentional canaries.
Pure controls are explicitly synthetic and cannot establish a native line.

`tool_identity.darwin_process_image` queries only an owned PID. It brackets
`proc_pidpath` and `csops(CS_OPS_CDHASH)` with PID/birth observations. The kernel's
code-directory hash belongs to the process text vnode at observation; the
expected value is obtained from the pinned, verified signed disk artifact.
This does **not** observe every transient helper, fork, exec, mapping or loaded
library. Darwin no longer supports kqueue `NOTE_TRACK`. A short client may exit
before an image observation. These unknowns remain explicit and make the strict
R6 gate red; disk hashes or two snapshots cannot upgrade that result. Setting
optimistic result flags cannot overcome the frozen observer's capability.

NSProcessInfo thermal state is observed before/after each sample without host
changes. Those snapshots alone do not prove stabilization or a quiet window.
The strict R5/R6 result remains red until the missing evidence exists. A
functional command exit of zero only means the bounded functional population
passed; `qualified_r5_r6`, its error list and independent review remain separate.

Run pure controls with `python3 -B benches/perf/qualification/test_qualification.py`.
Run native qualification with `--manifest PATH --manifest-sha256 DIGEST --out NEW`
after the source/tools/effects/resource freeze. The optional `--intentional-canary`
labels a residual-daemon, omitted-work, fixed-cache or wrong-artifact injection;
it never turns a failing
qualification into a successful product run.

The paired entry (`--paired`) accepts an externally frozen envelope containing
both manifests and an append-only reservation receipt. It runs four cold and
four warm samples per product, interleaved tmux then RMUX for each index. Both
consume one preceding 60-second window: aggregate CPU mean at most20%, every
interval at most30%, nominal NSProcessInfo thermal state, and observation gaps
at most5 seconds. The collector samples every2 seconds and keeps all raw Mach
host CPU counters. It never enumerates other processes. The gate recomputes the
window, checks its cohort and clock binding to each sample, and ignores any
optimistic `thermal_stabilization_confirmed` flag. A failed window is retained
without retry; functional observations may continue with thermal/strict results
red. This is the defined operational condition, not a claim about unmeasured
physical temperature sensors or public performance comparability.

New manifests may carry a prospective `source_role_graph`, derived from pinned
source, original package provenance and actual installed layout before execution.
For example, a tiny entry with `-f` may exec a full helper whose ordinary client
autostart selects that same helper when no sibling daemon exists. The graph
declares client images and the daemon role separately; it cannot be chosen from
an observed digest. Old manifests keep their old role expectation and findings.
Source role reasoning and any changed policy require independent review.

The implemented Darwin observer remains incapable of full exec/fork history.
Its capability is fixed in code; changing a manifest label or result boolean
cannot promote it. The finite `native_history_canary.py` uses freely executing
owned helpers to expose fast execs and fork/delegation notification coalescing.
It uses no suspended target, debugger, product instrumentation or global scan.
`NOTE_FORK` does not supply a child PID, `NOTE_TRACK` is unsupported, and
`NOTE_EXEC` supplies neither an image history nor a loss counter. Consequently,
identity, residual-history and strict baseline remain red even when the new
role binding, thermal contract and functional population pass.

Changing `qualification.py` requires a separate applicability review for any
checker-owned contracts policy pinned to its earlier bytes. This candidate does
not alter that accepted policy or silently move its source receipt.
