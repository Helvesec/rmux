The sampler can require disk identity and supplied producer provenance for its
declared tools. Invoke `bench_unix.py` directly with `--tool-provenance PATH` in
addition to its existing arguments. This option requires every selected tool
to be available, an observed version for each measured tool, and a complete
producer receipt covering each declared file. When RMUX is selected,
`--rmux-daemon-binary` is required. The public and helper files, when declared,
are included too. Existing `run-unix.sh` remains in historical mode.

Without the option, artifacts explicitly say `unqualified_historical`.
Accounting completion can still be true, but missing provenance, daemon or
version stays unknown. Neither mode claims observation of transient helpers,
the executable image of a running process, or an installed RMUX package.

The provenance input is JSON schema 1 with a `tools` object. Role names are
`rmux`, `rmux_public`, `rmux_helper`, `rmux_daemon`, `tmux`, `zellij`, and `screen`.
Include the measured roles and any RMUX file roles declared on the command line.
Each maps to a pinned producer receipt and an artifact key within that receipt:

```json
{
  "schema": 1,
  "tools": {
    "rmux": {
      "receipt": {"path": "/private/build-receipt.json", "sha256": "<receipt SHA256>"},
      "artifact": "client"
    },
    "rmux_daemon": {
      "receipt": {"path": "/private/build-receipt.json", "sha256": "<same receipt SHA256>"},
      "artifact": "daemon"
    }
  }
}
```

The receipt must record `schema: 1`, `status: "success"`, integer `returncode: 0`,
a nonempty `argv` array for its producer execution, and `artifacts`. Each artifact
has `role`, `bytes` and `sha256` matching the actual declared executable. A
`source-build` receipt additionally has a nonempty `build_id` and `source` with
40-digit commit/tree hashes and `clean: true`. An `installed-package` receipt
instead has a `package` object with nonempty `name`, `version` and `installation`.
Third-party tools retain their own provenance. Every declared RMUX role must be
covered by the same pinned source-build receipt; directory adjacency and matching
version banners do not establish that link. A producer may document multiple
build commands in one receipt for one source/build set.

These are trusted external producer receipts, not self-authenticating evidence.
The sampler verifies their schema, hashes, artifact roles and links to disk
bytes. It cannot independently prove the truth of a producer's build or install
claim. Supply an actual reviewed producer receipt; do not invent one from a
directory listing. Without such a receipt use historical mode. The test receipts
are explicitly synthetic and cannot attest production source or packages.

Before any measurement, a unique `<output>.tools-<runID>.json` sidecar records the
declared spelling, selected path, resolved path, regular-file/symlink type,
stat identity, size, SHA256, version evidence and provenance. It is created
exclusively, written once and pinned by SHA256 in the main artifact. Sidecars
retain absolute paths for private review; review their contents before public
distribution. Files are hashed with before/after checks. Required mode repeats
file, receipt and sidecar identity checks before every attempt and at completion,
outside the operation's timer. A mismatch blocks subsequent calls or invalidates
completion while retaining all earlier raw attempts and metrics. Saving the
sidecar is mandatory; a failed write cannot start sampling.

The sidecar also freezes a schema-1 `path_context`: POSIX path flavor, collection
`cwd`, and the public/client/daemon/helper field-to-role bindings. The main
artifact's `rmux_binaries` records the original path spellings, including an
explicit `client`. An omitted public option binds to the measured `rmux` client;
an absent helper is explicitly null. These fields are private proof paths, not
redacted display paths. Validation interprets relative spellings lexically from
the frozen CWD and compares them with each role's selected identity path. It
never resolves executable paths on the review host. Equivalent absolute and
relative spellings are accepted; a symlink's target cannot replace the pinned
selected link path. The separately observed resolved target and disk checks
remain unchanged. RMUX path qualification applies only when RMUX is selected.

Missing or contradictory context, mappings or declarations invalidate required
mode. Earlier required artifacts without this path context and explicit client
cannot supply this proof; they must be collected again, not silently upgraded.
Historical artifacts still grant no R6 qualification. The pure path controls run
with `python3 -B benches/perf/qualification/test_tool_paths.py /private/new-evidence`.

Only the established version commands are used: RMUX/tmux `-V`, zellij
`--version`, screen `-v`, with the existing five-second timeout. No version
command is inferred for daemon/helper roles. Their versions stay unknown, while
their disk identity and shared source/build receipt remain mandatory. Versions
are queried only for selected tools with observed executable files; required
mode also checks provenance before these probes.

This is a sequence of disk observations, not an atomic binding to a later exec.
A file can change and be restored between checks. No process-image hash,
kernel hook, tracer, global process inspection or transient-helper discovery is
used. Repeated hashing and checkpoint work can affect a later workload even
outside its timer; a performance campaign must qualify that overhead separately.

Run the pure tests on a fresh directory with
`python3 -B benches/perf/qualification/test_tool_identity.py /private/new-evidence`.
They use fake calls through the real collector and never launch tools. The
optional Mac control `native_tool_identity.py` creates and executes one private
Python helper with `-V`, observes only that child's PID/birth, and verifies its
exit/reap. Its cap is four children, five seconds each and thirty seconds for
cleanup. It runs no RMUX/tmux process and provides no performance evidence.

R5-B remains a separate parent candidate. These changes add no functional
postconditions, thermal observation, statistical campaign, exhaustive R6 helper
attestation or v28 acceptance.
