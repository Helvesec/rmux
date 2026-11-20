# Source inventory v1

This standalone internal tool reads exact Git commits without checking them out.
It leaves `xtask feature-inventory check-file-sizes` and runtime dependencies alone.
Python >=3.11, Git and the locked Rust parser are required; the parser supports
Rust 1.88. Its separate Cargo workspace uses syn 2.0.118 (full AST + Visit) and
proc-macro2 1.0.106 (UTF-8 byte spans). `Cargo.lock` pins the entire tool graph.
The binary embeds its source/manifest/lock inputs; scans reject a mismatched binary.

From the repository root, using an explicitly selected Rust toolchain:

```sh
export CARGO_HOME="$PWD/.rmux-audit/source-cargo"
export CARGO_TARGET_DIR="$PWD/.rmux-audit/source-target"
cargo build --locked --manifest-path scripts/source-inventory/Cargo.toml --jobs 2
export SOURCE_SYNTAX="$CARGO_TARGET_DIR/debug/rmux-source-syntax"
python3 -m unittest discover -s scripts/source-inventory -p test_inventory.py -v
python3 scripts/source-inventory/inventory.py --repo . \
  --revision 1f4571e74f36be0c033c6294d1616c7d3a6fbda1 \
  --parser "$SOURCE_SYNTAX" --output .rmux-audit/b0.json
python3 scripts/source-inventory/inventory.py --repo . \
  --revision EXACT_40_DIGIT_CURRENT_COMMIT \
  --parser "$SOURCE_SYNTAX" --output .rmux-audit/current.json
python3 scripts/source-inventory/compare.py --repo . \
  --before .rmux-audit/b0.json --after .rmux-audit/current.json \
  --output .rmux-audit/delta.json
```

`inventory.py` always rejects unknown paths, unconsumed Rust without an explicit
root declaration, unsupported syntax, missing provenance and stale/unknown
reference documents. `--check-size` additionally rejects any file with **>=600**
handwritten production lines (599 is the maximum). Existing violations are
reported even without that flag. `compare.py --check-ratchets` rejects growth
in the three literal populations. These are explicit checks, not an assertion
that the existing repository meets the final size limit. No G is set.

## Attribution and output

Git `ls-tree -rz --full-tree EXACT_COMMIT` supplies tracked regular blobs and
`cat-file --batch` supplies bytes; build/audit/untracked files cannot enter the
population. Symlinks, submodules, non-UTF-8 paths and non-UTF-8 Rust fail. No
working-tree path, timestamp, host or run ID enters the canonical JSON report.
Reports identify source commit/tree and SHA-256 of maintained tool/declaration
inputs. Identical inputs produce identical bytes across checkout paths.

Rust Cargo targets and explicit standalone roots seed a module/include graph.
Crate-root, normal module and explicit `#[path]` directory contexts are distinct.
An integration test's helper can also be production: production consumption
wins. Build scripts and tool crates are tooling; their test-only regions are
tests. Examples in production crates retain production classification. Explicit
fixture crates/roots are maintained declarations, never a filename heuristic.
Unreachable cfg expressions retain enclosing source classification.

Every attribute-bearing syn AST owner contributes a byte region; nested cfg and
cfg_attr are combined symbolically with its consumers. A region is test-only
only when its condition cannot hold with `test=false` but can with `test=true`.
`all`, `any`, `not`, flags and key/value atoms are supported. Build/platform/feature
atoms remain independent Boolean variables, conservatively retaining production
in logically impossible platform combinations. SAT is bounded at 16 distinct
non-test atoms per formula; exceeding it fails. Conditional `cfg_attr(path)` and
unknown predicate grammar fail. No expansion or selected host build is used.

The counting unit is LF-delimited physical lines, including blank/comment lines
and a final unterminated line. CRLF counts once. Empty files have zero lines.
UTF-8 byte regions are separately reported. A cfg owner's immediately following
comma belongs to that owner. An expression statement owns its AST semicolon and
intervening trivia; a sibling empty statement remains in the enclosing region.
Unattached trivia belongs to its enclosing region.
For mixed lines, horizontal whitespace does not introduce a second class;
production takes precedence and the line is counted once. Mixed-line numbers
and disjoint contiguous line ranges make reconciliation inspectable. Binary
assets have byte counts and zero text lines. Documentation/assets/non-code remain
explicit categories alongside production/tests/specifications/tooling/generated/
vendored. Crate attribution is the longest tracked Cargo-manifest directory prefix.

Macros are counted as maintained source in their enclosing region, without
inventing expanded LOC. Conditional syntax inside an opaque macro fails.
The narrow `tokio::select!` adapter parses top-level brace groups as syn Rust
blocks; conditional syntax outside those blocks fails. Unsupported macro DSLs
need a bounded adapter before their conditional regions can be accepted.
Nonblocking diagnostics report source token counts inside opaque macros and
function spans >=80 LF lines, including tests; these are not complexity or
duplication gates. Manifest dependencies/features are published, without claiming
a resolved all-target dependency graph.

The frozen literal populations are byte-substring occurrence counts of
`#[path =` and `#[allow(dead_code)]` in **all tracked regular files** under `src/`
and `crates/`, including comments/strings, plus all tracked paths ending `.md`
(the Git `*.md` population). Per-occurrence locations and per-crate counts/files/
size violations are published. Their deliberately literal definition differs
from AST condition recognition.

## Maintained declarations and limits

`declarations.json` is an exact allowlist for non-Rust files, explicit standalone
Rust roots, tool/fixture crate roles, unresolved nonliteral includes, document metadata and
actual generated/vendored provenance. Adding a reference document requires its
role, source or generator, consumers, owner, freshness command and source-blob
binding. Rows marked `maintained declaration` assign a maintainer review duty;
they do not assert an unobserved runtime consumer. Source-blob freshness detects
changed document bytes; semantic/document-to-runtime freshness still requires
human review. Recorded missing historical generator inputs/versions remain
visible provenance limitations, not reconstructed evidence. Only
`include!(concat!(env!("CARGO_MANIFEST_DIR"), "/path"))` resolves without a
declaration, against the crate compiling a Cargo target; any other nonliteral
form, and that form under a standalone root, is rejected unless declared.

`compare.py` publishes raw added/removed lines by class and all maintained-source
additions outside production, including declarations, generators, adapters,
tests, docs and assets. Raw line removals are **never credited reductions**.
Reclassification of unchanged source is listed separately. Moving, splitting,
reformatting, minification and generation cannot earn credit: credited reduction
is always zero and any nonzero `--claim-reduction` fails. This measurement tool
does not certify semantic reductions. Disjoint proven lower bounds, generated
outputs/tokens/expanded bytes per feature/target, a comparable-core scope,
complexity/duplication metrics, dated G decision and final v28 acceptance remain
separate requirements. No 170K target or completion claim is implied.
