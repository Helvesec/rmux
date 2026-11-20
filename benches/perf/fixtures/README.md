# RMUX Perf Fixtures

Fixtures in this directory are intentionally small and deterministic. Larger
stress inputs should be generated from the seed and parameters recorded in the
benchmark JSON instead of committed as large blobs.

Each fixture listed in `MANIFEST.sha256` must be updated deliberately when the
fixture content changes.

## Pre-baseline instrumentation migration

The published `perf-instrument` Cargo feature and product `RMUX_PERF_TRACE`
environment control are retired. Remove the feature from downstream Cargo
invocations; the environment variable no longer creates or appends product logs.
The internal `state_lock_hold`, `render_compose`, `snapshot`, `attach_refresh`
and `web_writer` span timings, and `queue_backpressure` trace events, are no longer
supported metrics. External process/RPC wall time does not measure those spans.

Execute the contract fixture on Unix with Python 3.11+, Cargo and an exact,
locally built full binary from the clean checkout:

```bash
scripts/bench/check-perf-cutoff.py --binary /absolute/path/to/rmux \
  --expected-sha256 <build-receipt-binary-sha256> \
  --expected-git-commit <build-receipt-source-commit> \
  --expected-daemon-sha256 <build-receipt-sibling-daemon-sha256> \
  --output-dir /absolute/path/to/perf-cutoff-output
```

Omit `--expected-daemon-sha256` only when no sibling `rmux-daemon` exists;
in that layout the full binary re-executes itself for the daemon. The fixture
clears internal binary overrides and verifies both artifacts when present.

The fixture rejects the removed feature for both published packages, compares
CLI results with the legacy variable unset/set, checks absent and existing trace
paths, and executes two operations through `scripts/bench/bench_unix.py`. It uses
`scripts/perf-provenance.sh` for source and artifact identity. A debug binary is
appropriate for this correctness smoke; its single samples are not release
performance evidence and must not feed the published benchmark tables.

For release measurements, retain the existing `scripts/perf-bench.sh` and
`scripts/perf-baseline.sh` release-artifact/provenance checks, or the documented
package layout in `scripts/bench/run-unix.sh`. Existing scenario, fairness,
thermal-state, statistical and provenance acceptance contracts still apply.
This cutoff carries no reduction credit and makes no performance-gain claim.
