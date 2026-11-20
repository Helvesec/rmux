# External instrumentation cutoff

The product no longer exports `rmux_pty::initial_trace`, reads
an `*_INITIAL_TRACE_CONFIG` variable, or publishes `initial-trace-fixture`.
The recorder, writer, clock, example, server state and call sites are removed.
Neither default builds nor `test-seams` restore them. Normal logging is unchanged.

Use the existing `bench_unix.py` custom-binary collector for external process/RPC
wall time. Its consumer is `check-perf-cutoff.py`: it checks the actual samples,
source/binary identities and completion, and exercises private real daemons.
The cutoff fixture also consumes ordinary CLI pane identities, child liveness,
captured output and resize results. No new product command, RPC or backend is
needed. These observations are intentionally distinct from internal spans.

| Removed observation | External fixture observation | Limit |
| --- | --- | --- |
| New-session commit / output activation | successful `new-session`, captured marker, positive live child PID and stable session/window/pane identity | no claim about internal event order |
| Logical geometry commit | existing `resize-window`, unchanged identities and observed 80x24 to 90x25 pane size | CLI observation; native PTY size covered separately by unit regression |
| Performance elapsed time | collector samples plus external new-session/resize RPC durations and marker observation delay | includes process/RPC/scheduling overhead, not internal latency |
| ConPTY create/resize QPC, retry HRESULT, loaded DLL/address, child birth time | not provided by the Unix fixture | requires a separately qualified external Windows observation surface; remains open (no debugger, injection or global ETW authorized) |

The old Unix configuration schema is tested with the exact owned socket, an
absolute output path and the original process-local 0..30 second clock. Fresh,
existing, missing-parent, malformed and missing configurations must leave the
same CLI results and create no trace or completion receipt. Existing files must
remain unchanged. The fixture kills only the daemon on its private endpoint.

Run from a clean checkout with identified locally built `rmux` and
`rmux-daemon` binaries:

```sh
python3 tests/instrument_cutoff_spec.py
python3 scripts/bench/check-perf-cutoff.py --source-only
python3 scripts/bench/check-perf-cutoff.py \
  --binary /absolute/path/rmux \
  --expected-sha256 RMUX_SHA256 \
  --expected-daemon-sha256 DAEMON_SHA256 \
  --expected-git-commit COMMIT \
  --output-dir /absolute/private/evidence \
  --skip-cargo-feature-probes
```

The final flag avoids the two optional Cargo missing-feature probes; it does
not skip source/manifest checks, CLI regression, environment checks or external
collection. `feature_rejections: null` records that those probes were not run.
The negative canaries reintroduce an export, environment reader, public feature
or packaged asset in disposable source fixtures and require the actual gate to
reject them. They never change the checkout under test.

## Accepted scope and remaining Windows facts

The Linux external collector and Mac external/runtime contributions are already
accepted at their recorded source identities. The Windows external fixture
supplies stable pane/child identities, positive FILETIME births, geometry and
cleanup. The common pending-output geometry correction has native regression
proof on Linux, Windows and macOS. Reusing those receipts does not require
replaying their campaigns and does not relabel their binaries as a later commit.
Windows 10 has not been observed by these Windows 11 contributions.

`windows_cutoff_consumer.py` is a pure reader, not another benchmark runner. It
reuses `qualification.completion_errors` for planned external attempts, order,
failed/missing samples and metric consistency. The native API records remain a
separate observation domain. Its successful result is deliberately only
`PASS_RECORD_CONSISTENCY_ONLY`: independent review must qualify the observer,
its raw evidence and the source/build provenance. The command also verifies
referenced binary, observer, build, qualification, raw evidence and DLL bytes.
No CLI return code or whole-process elapsed time is converted into an HRESULT
or per-call latency. A standalone ConPTY probe measures its own calls and is
rejected as a substitute for product calls.

The following table accounts for all 19 assertions in the historical
`remises/L2-007/windows-validate.py` (SHA-256
`0d4732b41e95b60b3cfbf84c2891404936ec31aef6b7807ca0471b1c5cf561fc`).
Changes of representation below are proposed for independent review. The old
consumer and its failures are preserved. No necessary fact is waived.

| Historical line / assertion | Real consumer need | Replacement / disposition |
| --- | --- | --- |
| 30: TRACE_BEGIN / TRACE_END | Complete observation boundaries | External finalization and pinned raw receipt; retired event spelling is not required. |
| 31: contiguous journal sequence | No lost or duplicate attempts | Accepted external completion checker plus contiguous native call sequences. |
| 32: complete, attempted=written, no drops/errors/cap | Loss accounting | Retained explicitly for native calls; absence fails. |
| 33: flushed closed receipt, case/producer/last sequence/bytes | Durable evidence tied to producer and case | Pinned exact raw bytes, producer FILETIME, joined cases and explicit flushed/completed state. The old sidecar filename/format is obsolete, not its integrity need. |
| 35: consistent frequency, integer QPC, producer birth | Same native clock and producer | Retained on every API pair; missing Windows fact. |
| 36: QPC inside start/end window | Calls belong to this capture | Retained on each pair; missing Windows fact. |
| 37: positive QPC frequency and producer birth | Valid native clock/process identity | Retained; child births alone do not prove producer/call timing. |
| 39: matching session/window/pane/PTY event IDs | Stable logical identities through activation | Ordinary external identities already acquired; internal event order is not claimed. PTY-to-call association remains required below. |
| 40: positive child FILETIME | Identify the actual owned child | Acquired Windows external contribution; retained. |
| 44a: begin/end/active PTY identity | Each API pair belongs to the intended PTY | Retained, currently missing externally. |
| 44b: end QPC >= begin QPC-1 | Per-call native elapsed interval | Original one-tick tolerance retained; whole CLI brackets do not substitute. |
| 45: equal begin/end dimensions | Pair measures the same request | Retained for actual create/resize arguments. |
| 46: provenance available and file version | Identify the actual selected API module | Retained with path/version/disk SHA-256; module presence alone cannot identify a call. |
| 49: four journals | Complete planned historical fixture population | Four cases retained pending independent population review; no arbitrary reduction to one successful call. |
| 50: inbox and companion kinds | Both native API implementations covered | Retained; failed companion attempts and inbox fallback stay visible. |
| 51: each same child identity no longer alive | Owned cleanup | Acquired Windows external PID+FILETIME observation; retained. |
| 53: at least one logical geometry commit | Resize changes the intended geometry | Ordinary resize plus accepted native pending/direct regression replaces removed event encoding. |
| 54: successful create HRESULT0 | Real successful native creation | Retained; CLI rc0 is insufficient. |
| 55: successful resize HRESULT0 | Real successful native resize | Retained; CLI rc0 is insufficient. |

The historical receipt filter also required rc0 and an exact source HEAD; DLL
hashes were computed even though that statement was not an `assert`. Those
provenance obligations remain. No naturally failing HRESULT/retry is invented
from the separately acquired controlled test rejection.

At the L2-031 boundary no admitted observation surface supplies successful
product ConPTY call arguments, HRESULTs, per-call QPC and selected-module
provenance. Existing public CLI responses and failure-only logging do not
expose them. The frozen Windows recipe therefore records a missing observer
qualification and binary/build binding and must fail closed. L4 may implement
only its private adapter after prospective admission of a concrete permitted
surface; L2 owns this common reader. No recorder, debugger, injection, global
ETW or previously refused operation is authorized by this document. If no such
surface is available, the migration line remains FAIL until an independently
approved, justified contract change. This is a precise dependency, not a native
proof or authorization to experiment outside the mandate.

## Exact package bytes

`cutoff_assets.py` scans native executable bytes and tar/zip member paths and
contents, recognizing nested tar/zip bytes regardless of the member filename,
including `.crate` payloads. Corrupt members with known archive suffixes fail
closed. It checks UTF-8 and both UTF-16
byte orders. Duplicate/unsafe paths, missing assets, unsupported containers and
unbounded expansion fail; deb/rpm/snap compressed bytes cannot silently pass as
raw files. This reader neither extracts nor executes a package member.

A marker finding remains in the report. In particular, the root source crate
ships two existing Python regression tests which name removed features in
negative fixtures. Those exact test bytes are source references, not a restored
product module or Cargo feature. Their disposition must name the exact members
and hashes; there is no blanket `tests/` exclusion or implicit whitelist in the
scanner. A changed test, new member or production reference remains visible.

The actual distribution inventory comes from the canonical build contract,
shared package layout, source crate producer, Snap and Nix recipes and downstream
archive references. Formats which wrap the same binary can reuse exact payload
hash bindings; independent builds cannot inherit an absence result by name.
Inventory entries are provenance obligations, not extra installation/signature
or general release qualification campaigns. Unproduced assets remain missing.

L2-031 produces Linux x86_64 release-profile tiny/full/daemon bytes and a local
archive from the unchanged builder, plus all 13 canonical source crates. The
local archive explicitly has `release_artifact=false` and native GLIBC2.39;
it is not a public GLIBC2.31 release. Accepted Mac full/full/daemon bytes retain
their 80575 source, and historical b592 packages retain b592. Neither covers a
new tiny/release or different-platform payload by relabelling.

The original consumer need is also explicit in the prospectively frozen
R1-007 contract (`initial_trace`, SHA-256
`8b85b36bfd211fb4d2d6d57303d327f7361a376dcdb181125ecd4b60d0ffb655`):
L4's initial-state qualification must distinguish arguments at creation from a
later public observation. Actual COORD, create flags, selected API, HRESULT and
all fallback attempts establish that causal history. This is why those facts
cannot simply be declared obsolete with the journal implementation. It does
not authorize replay of that historical physical campaign. Intermediate API
sizes may differ from later public geometry; the reader preserves paired
arguments and the original one-tick QPC tolerance rather than imposing final
geometry on every call. Ordinary external geometry remains checked separately.

R2-067 independently requires declared branch labels to match the observed
case, and same-PTY resize/retry intervals to follow successful creation with
the retained one-tick uncertainty. The reader checks this partial order inside
each case only; it imposes no total order across unrelated PTYs. Exact four-case
cardinality is an independently identified historical fixture artifact, so a
future prospectively frozen population may replace it with justification. The
current four-case recipe remains explicit; no cardinality is silently removed.


## Prospective schema 2: creator and handle lifetime (C06)

The frozen L2-032 schema extends each call with `api_family`, `lifetime_id`,
`creator_operation_id`, and `hpcon`. It does not reattribute historical records
or qualify an observer. API family (`inbox` or `companion`) names the selector
owned by `OwnedHpcon`; the module path/hash identifies the actual function
implementation and may differ between create and resize because of forwarding.
`module.kind` retains its legacy family attribution and must agree with the
explicit selector in schema 2.

A successful create has a fresh nonnegative lifetime ID, its own operation ID
as creator, and a positive integer handle. It replaces the active state, as
`recreate_without_passthrough` replaces the inbox state. Every resize attempt
must match that active lifetime, creator, handle and family. A reused numeric
handle is allowed only with a fresh lifetime and successful creator. Failed
creates have null lifetime/creator/handle and leave the prior state intact;
failed resizes still refer to it. Each schema-2 case owns a distinct PTY ID in
the capture, so splitting one PTY into cases cannot evade the lineage checks.

Schema 1 remains readable for single-creation record consistency, with the
creator-family join fixed. It rejects recreation and unversioned lifetime
fields: missing lineage is not silently accepted. Historical recipe v3 and
its four cases remain immutable. The bounded case list can be chosen
prospectively under pilot decision 84ceb4; any declared immediate/deferred
branch must still match. All prior COORD, flags, HRESULT, producer, module,
completeness, child cleanup and local one-tick QPC checks remain. Distinct
PTYs have no global call ordering. A pure PASS never supplies native facts.

R1-071 accepted C01-C05 at 583e60f with nine tests and 71 real CLI cases.
R2-069's exact C06 counterexample and FAIL verdict remain historical evidence;
this delta requires independent review. The available b732 Windows release
ZIP resolves payload availability only. A qualified observer and complete
raw product-call records with native handle lineage remain missing.
