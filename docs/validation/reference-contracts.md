# Phase 0 reference contracts

This is the versioned reference for `0.contracts`: conformance, poisoned locks,
Web admission statistics, and scenario/proof accounting. It defines observable
contracts and checks their integrity. It does **not** certify runtime behavior,
Web security, performance, or a native qualification. Those gates remain open
until independently reviewed observations satisfy the applicable contract.

The source baseline is `80575f422b07e12f1399e54ebee5049dc65503e6`. The governing
v28 plan has SHA-256 `a13cdce4f403d89a285352c771828bccebcf87a998c4a44ee846aa5f56439944`.
The JSON files in this directory are versioned inputs. A changed input requires
an explicit reviewed delta; source fixtures are never silently repinned.

`reference-population.json` is the checker's separately frozen identity reference.
It binds the twelve family IDs and all 218 collection-ID/source pairs, readers,
ordered identity digests and cardinalities. The family names and historical
mapping were introduced at `6e7314438a096ce2f9e47158c0c6515a09407935`; their exact
Git blobs are recorded. Every collection source is separately bound to its
original blob at `80575f422b07e12f1399e54ebee5049dc65503e6`. This reference copies
identity bindings, not semantic expectations or product name/option tables.

The checker pins this file's bytes in its own source and loads it from its own
checkout, independently of the submitted `--root`. It never regenerates the
expected population from a submitted matrix or manifest. Removing or substituting
a family, removing a whole collection with an adjusted total, or renaming a
collection at constant cardinality all fail. A mapping transition requires an
explicit source-bound reviewed reference/checker delta; v1.1 admits exactly the
five checker-owned transitions listed under the scenario inventory.

### Frozen cited bytes (v1.1)

Every reference is cited by path and SHA-256 at the frozen source base. The
checker reads the submitted tree's copy when it still has those bytes. Otherwise
it reads the same path at `80575f4` from the tree's own Git history, and accepts
only the exact cited bytes. An evolved tree therefore no longer hides the
historical reference, and its newer bytes gain no credit. A copy that is not its
own Git top level, or a repository without the frozen base objects, fails with a
stale reference. Such a copy cannot verify this contract; that is an environment
limit, not a pass. The squashed public release history lacks those objects.

## Historical and current harness applicability

The historical baseline and both `scripts/bench/qualification.py` locators stay
at SHA-256 `01a7bbea0430dda34bb4fcef07c34d3eec2aaf171de13c97b1002cfde54466df`.
Integrating L5-024 subsequently changed that file to
`b77dfed425cd17b44ac17ed719db80a56e603fed25f84a72a41438c03c7555fb`.
The old common CLI consequently fails on the composed current tree with a stale
reference. That is a real composition failure, not a changed historical result.

`reference-applicability.json` records one explicit, bounded applicability delta.
It contains the exact archived L5-024 candidate receipt, R2-057 review and
integrator-068 consumption receipt, with original byte counts and digests. Their
candidate/tree, source pin, authority identity, receipt references and dates are
cross-checked. Local Git qualification binds the historical blob at `80575f4`,
the integrated blob at `6ab9c31b5d9510c95fbdf20c3646bd3956294f3c`, and its identical
bytes at the frozen composition base `b73256eda197e5bf7b7d28a2fdf9c5e9631aacb4`.
This source proof is a reviewed static input; the pure CLI does not run Git or
claim that an arbitrary submitted directory has that Git ancestry.

The causal source difference adds `random`/`statistics` imports and three
functions: `sample_statistics`, `qualification_plan`, and `qualification_errors`.
All ten original named definitions retain the same AST. These additions belong
to L5's bounded Darwin functional qualification. R2-057 accepted that component
with eight line PASS and four line FAIL; thermal stabilization, complete image
history, whole-criterion qualification and public performance claims remain open.
Reading its exact accepted source here grants none of those native verdicts.

The checker pins the complete applicability record from its own checkout.
Submitted policy copies cannot redefine it. The two historical semantic locators
must still exist with their original hashes. Historical bytes yield mode
`HISTORICAL`; the exact current bytes additionally require a declaration equal to
the pinned authority/date/source binding and yield mode `ACCEPTED_CURRENT`.
Changed historical hashes, unbound declarations and arbitrary harness
substitutions fail. Since v1.1, unauthorized current bytes are neither read nor
credited: the checker reports mode `FROZEN_HISTORICAL` and reads the exact
historical harness from the frozen base instead. The current allowance covers only this
canonical file and these exact bytes. Every other reference remains strict.
The twelve-family roster, 218 collections, 2865 ordered identities, independent
population digest and YAML oracle attribution are unchanged. A later harness
change requires a newly reviewed applicability delta; an input self-repin fails.

## Ownership and existing authorities

`reference-contracts.json` gives twelve family owners, entry routes, native
platforms, feature-profile rules, invariants, observables and affected neighbors.
Domain registries remain authoritative. This reference does not copy command,
option, format, wire or package semantic tables. Actual feature names and target
support come from Cargo manifests and source `cfg`, not from a new allowlist.
Every native campaign resolves that applicability before issuing challenges.
An unavailable prerequisite is a failed cell, never permission to skip it.

The existing tmux divergence ledger, conformance text fixtures, Kitty corpus,
Web protocol specification and R5/R6 benchmark population remain in their
original files. References carry hashes. L6 owns the generated options bank;
L5 owns `scripts/bench`; L3 owns scope/DAG; L2 owns runtime. This contribution
changes none of those implementations and runs none of their native harnesses.
The only executable entrypoint is a pure contract checker with two domain
modules and one receipt-applicability helper. It imports no product, collector,
benchmark driver or legacy test.

## Conformance and classifications

For each case, bind the source registry identity to an independent fixture and
its oracle version, source SHA, binary digest, case ID, symbol and captured-byte
digest. Compare stdout, stderr, exit, state, hooks, frames and PTY bytes wherever
the family promises them. Equivalent CLI/by-id/SDK/control/binding/config/queue
routes are cells of the same obligation; one route cannot silently stand in for
another. Unix and Windows native cells remain distinct, including Windows10/11.

Use exactly these classifications:

* **parity**: reproduce useful observable behavior of the exact tmux 3.7b oracle;
* **extension**: explicit RMUX behavior with its own invariant, not a parity claim;
* **corrective-divergence**: exact oracle counterexample plus a second independent
  proof of documented-contract, safety, data-conservation, progress or equivalent
  route coherence violation; a product preference is insufficient;
* **indeterminate**: retain the uncertainty and default to parity. It cannot be
  used to justify a corrective divergence or a product PASS.

A corrective case records an independently expected RMUX result, a differential
canary and the complete affected route matrix. Its test ends in
`_product_divergence`; it is never labelled `matches_tmux`. Existing C-D entries
retain their own evidence, limitations and statuses. Importing their identities
does not reclassify them as confirmed tmux bugs.

One known attribution mismatch is preserved explicitly: the error/exit matrix
names oracle source `31d77e29...` and binary `525149cd...`, while the frozen
reference names source `e802909d...` and binary `eb05f981...`. Their hashes and
source expectations stay unchanged. That historical matrix cannot establish a
new current-oracle parity PASS without an owner-reviewed attribution/repin.
The gate reads the source and binary identity scalars from both hash-pinned YAML
authorities and compares all four declared identities and the disposition. A
well-formed but false digest, or reassignment to the current oracle, fails. The
reader supports only the exact pinned quoted-scalar format and rejects missing
or ambiguous fields; it never executes YAML or an oracle binary.

## Poison policy and mechanical boundary

The default for `std::sync::Mutex`, `RwLock` and Condvar reacquisition is to fail
before using possibly incoherent protected state. Propagate an existing typed
domain error when the boundary supports one; otherwise panic at the owning
invariant boundary. A catch or restart must not convert the failed operation into
success. Poison does not notify waiters, complete shutdown or establish cleanup.
Asynchronous mutexes do not expose this poison mechanism, but cancellation still
requires their normal state invariants and cannot be an implicit exception.

Recovery requires a versioned exception naming owner, path, symbol, source,
coupled invariant, complete repair/validation, negative canary, independent
review and expiry. Merely calling `into_inner` or `clear_poison` proves no repair.
No exception is approved by this reference.
The v1 gate accepts no exception; admitting one requires a reviewed policy and
gate delta, not a syntactically valid author-supplied approval object.

`poison-policy.json` retains conservative source witnesses. The checker rejects
new, changed or silently removed witnesses. Its lexical scanner deliberately
over-approximates `into_inner`: some occurrences consume unrelated wrappers or
belong to tests. Scope hints are not Rust type/cfg analysis. Therefore the report
keeps product policy conformance **OPEN**, and owners must resolve candidates
before claiming mechanical enforcement across all product locks. This phase-0
contract does not rewrite mutex code or excuse known recoveries.

The SDK `lock_terminal_failure` visibly recovers a poisoned guard and is an open
owner review item. `AttachLockState` already documents fatal poison and contains
acquisition, Condvar wakeup and close/cleanup canaries. These tests are referenced
at their exact source, not claimed executed by the pure checker.

## Prospective Web reference profile

`web-reference-profile.json` separates two loopback topologies, three equalized
backoff states and all five invalid admission classes. Every stage from the same
client milestone (first upgrade byte) through observed close is retained. A
pre-upgrade rejection/EOF is observable and cannot be hidden by analysing only
WebSocket survivors. Pair all ten class pairs in every topology/backoff stratum:
60 comparisons. Do not pool topologies, discard failures, silently reset only
one class, retry samples or delete outliers. Warmup is exactly 50 per stratum,
recorded separately; collector readiness and backoff state require evidence.

The canonical server-to-peer projection keeps frame order, direction, opcode,
raw byte length, all deterministic fields (including unexpected fields), and
exact closure. Preserve raw frames. Decrypted deterministic fields of
authenticated records must be bound to their ciphertext by the trusted peer
collector; the pure checker does not perform cryptography. Only a missing PIN
**after verified token proof** admits `4008/pin_required`. Invalid token, origin,
proof, wrong PIN and capability target `4000/handshake_rejected`.

Nonce (16 bytes), X25519 public key (32) and ML-KEM ciphertext (1088) use canonical
base64url without padding. Their values are excluded from raw equality; format,
length and non-reuse remain checked. The native collector additionally proves
fresh generation, protocol cryptographic validity, and corresponding client
material. A synthetic repeated-byte test vector is not randomness evidence.

Timing uses an explicit equivalence bound rather than failure to reject a null:
with empirical two-sample CDF distance D, the upper confidence bound is
`D + sqrt(log(4/alpha_pair)/(2*n)) + sqrt(log(4/alpha_pair)/(2*m))`.
`alpha_pair = 0.01/60`; accept only an upper bound at most `0.05`. The floor is
50,000 observations per class/stratum. Under within-stratum IID assumptions,
DKW plus the union bound controls the whole timing family at 0.01. With this
floor, the uncertainty allowances for both family alpha 0.01 and family beta
0.01 sum below the margin under identical distributions. Unestablished IID or
stationarity makes that calibration inapplicable and leaves the native gate red.

For random fields, compare decoded bit marginals across classes with a
simultaneous Hoeffding bound over 9,088 bits and 60 pairs, floor 100,000 per class,
margin 0.05 and family alpha 0.01. Constrained X25519 bits are compared between
classes, not forced uniform. This finite projection does not prove CSPRNG quality;
cryptographic validation and freshness are separate mandatory observations. The
two statistical families have a combined union-bound error at most 0.02 under
their assumptions. These are prospective design parameters, not measured results.

Four independent negative controls must reject: deterministic reason leakage,
reused nonce/key/ciphertext, divergent close code, and shifted time distribution.
The pure tests exercise those rejections on synthetic data only. Missing classes,
stages, samples, provenance or native measurements leave product equivalence
unestablished even when the profile and pure comparison tests pass.

## Scenario inventory and historical mapping

`legacy-scenarios.json` freezes 218 source collections and 2,865 identities:
existing Rust test declarations under `tests/`, Python test functions, standalone
test programs, Kitty vectors, C-D ledger entries, error/exit cases and the R5
population. Helper-only modules are not standalone scenarios. Composite programs
retain their internal assertions at the pinned source; their execution cannot be
duplicated into invented independent observations. The manifest describes this
population, not all Rust unit tests or all dynamically expanded native cells.

The stable mapping is `collection-id/legacy-id`. Legacy IDs are copied only as
identities; observable semantics remain in the original source. Every collection
inherits explicit target, features, prerequisites, observable, risk, neighbors,
platform and oracle rules. Source digests, complete ordered IDs and exact
cardinality are checked against the frozen cited bytes. Omission, duplicate source
aliases, changed mapping and unknown collections fail. When the submitted tree has
evolved, every legacy identity must still appear there in order; additions gain
no credit. Repeated Rust function names have a frozen
occurrence suffix; moving or renaming them requires an explicit mapping delta.
The checker owns the only five v1.1 transitions: `issue8_interactive_exit.rs`
renamed `fresh_pane_title_starts_as_host_short_before_shell_updates_it#1` to
`fresh_pane_title_starts_as_host_before_shell_updates_it#1`;
`window_pane_flag_semantics.rs` renamed
`last_pane_input_flags_apply_to_the_selected_last_pane#1` to
`last_pane_input_flags_apply_to_the_last_pane#1`, because `last-pane -d/-e`
now toggles the last pane's input without selecting it, as tmux 3.7b does;
`capture.rs` renamed `capture_pane_non_numeric_bounds_are_rejected#1` to
`capture_pane_unreadable_bounds_fall_back_to_the_visible_rows#1` and
`cli_surface.rs` renamed `capture_pane_invalid_bounds_are_rejected#1` to
`capture_pane_invalid_bounds_select_the_default_bound#1`, because the
capture-pane bounds now follow tmux's strtonum fallback instead of being
rejected; and `bench_screen_adapter_spec.rs` may be absent because the
published 0.11.0 source tree does not ship it.

Static declarations are not Cargo's cfg-expanded executable population. Before
a qualification, the owner freezes the applicable scenario × target × feature ×
route × native OS matrix with expected cardinality. Legacy statuses stay
`NOT_REQUALIFIED`; an oracle or scenario identity never constitutes a new PASS.
The independent inventory references tests and fixtures, not product registries.

## Execution receipt contract

The plan is an external frozen input, not reconstructed from returned receipts.
It contains every expected qualification/cell/ID/risk and exact source, artifact,
target, OS, features, prerequisites, collector, command and input digests plus a
distinct observable ID. Q1 and Q2 must have equal populations and identical
content, including the observable. A job/run identity cannot serve both Q1 and
Q2. One expected row may declare shared scenario IDs before issuance; it
represents one execution and is counted once. Undeclared aliases fail.

Issue one unique 256-bit challenge per expected row before its start, binding the
whole plan digest and cell identity. Receipts carry run/job/attempt, actual start
and end, challenge, execution identity, stdout/stderr hashes and a canonical raw
collector envelope. The checker verifies the envelope digest and all bindings;
copies under another ID, cell or Q2, reused challenges/executions, retries, missing
cells, empty selection, skip and non-attempt cannot satisfy a qualification.
Pass prior qualification receipts too: reuse remains detectable across files.

Authenticity is an explicit trust boundary: the native collector must attest
host, artifact, challenge and actual execution. A schema checker cannot prove
that an author did not fabricate an entire envelope. Each result therefore states
that accounting PASS is not a product or native-authenticity verdict. Synthetic
fixtures are explicitly marked and rejected in native-evidence mode.

Run only the pure checks:

```sh
python3 -B scripts/validation/contracts.py
python3 -B tests/reference_contracts_spec.py -v
```

For supplied evidence, `--campaign DIR` reads `plan.json`, `challenges.json`,
`receipts.json` and `previous.json`, and never launches a collector. Synthetic
campaign fixtures additionally require `--synthetic-fixture`. Review the output
scope before consumption; native gates and release qualification stay separate.
