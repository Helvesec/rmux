# Preliminary Rust public API gate

Run on the native target, using already installed tools and a private, populated
offline Cargo home. Python 3.9+, Rust/rustdoc/Cargo 1.96.1 and
cargo-semver-checks 0.49.0 are pinned for this pilot. Rust 1.96.1 is **not** a
declared RMUX MSRV. Nothing is downloaded or installed by these scripts.

```sh
python3 scripts/public_api_gate.py \
  --target x86_64-unknown-linux-gnu \
  --toolchain-bin /absolute/installed/toolchain/bin \
  --cargo-home /absolute/private/cargo-home \
  --semver-checks /absolute/cargo-semver-checks \
  --output .rmux-audit/public-api-run

python3 -m unittest discover -s tests -p public_api_gate_spec.py
python3 -m unittest discover -s tests -p public_api_cfg_spec.py

python3 tests/public_api_gate_canaries.py \
  --target x86_64-unknown-linux-gnu \
  --toolchain-bin /absolute/installed/toolchain/bin \
  --cargo-home /absolute/private/cargo-home \
  --semver-checks /absolute/cargo-semver-checks \
  --output .rmux-audit/public-api-canaries
```

Each output directory must be new, ignored by Git, and owned by this run. Do not
run concurrent writers against its Cargo home. The scripts use separate target
directories, at most two Cargo jobs, and the ordinary dev profile with debug and
overflow assertions. Explicit `--target` also requires host proc-macro artifacts;
downstream compiler commands obtain their paths from Cargo's artifact messages.

The supported compiler context is deliberately **free of Cargo configuration
files**. Before every tool invocation, the runner rejects either `config` or
`config.toml` in `.cargo` at the actual working directory or any ancestor, and
in the explicit `--cargo-home`. This includes baseline extraction, candidate,
output, standalone consumer resolution/checks and direct compiler witnesses.
Both filenames are rejected even if one currently shadows the other; empty
files and unsupported TOML are also rejected. The gate does not merge, filter,
modify or parse these files. Configured linkers, flags, wrappers, `[env]`, source
replacement, profiles, includes and build-script overrides require a separately
reviewed extension to this contract. A rejection names the path without reading
or exposing its contents, records `exit: null`, and leaves the gate incomplete.

Cargo searches from its working directory, not `--manifest-path`; a member or
path dependency's configuration is inapplicable when invoked from the workspace
root. If an invocation enters that member, the same guard checks it. The runner
uses the resolved directory for both checking and execution, and rechecks before
each command. The sources, ancestry and private home must have no concurrent
writers; this check is not an OS sandbox against hostile concurrent mutation.

Nonempty ambient `CARGO`, `CARGO_*` and `RUST*` compiler overrides also reject,
case-insensitively on every host, so required linker/flag/cache settings cannot
be silently dropped. Explicit `--cargo-home` replaces ambient `CARGO_HOME`;
`RUSTUP_*` selectors are removed because installed binaries are selected
explicitly. Native PATH (including Windows `Path`), LIB, LIBPATH, INCLUDE,
SYSTEMROOT and SDK environment remain available to all tools. Prepare the native
compiler environment normally and unset unsupported overrides explicitly. The
existing private registry cache needs no Cargo config file; offline resolution,
candidate registry-pin checks and frozen compilation are unchanged. Do not
remove another user's configuration to make a gate run: use an unconfigured
owned location/home, or record the unsupported native setup as pending.

Focused native configuration regression (including the actual SDK no-default
surface, frozen removal and consumer boundaries):

```sh
python3 tests/public_api_cfg_canaries.py \
  --target x86_64-unknown-linux-gnu \
  --toolchain-bin /absolute/installed/toolchain/bin \
  --cargo-home /absolute/private/cargo-home \
  --semver-checks /absolute/cargo-semver-checks \
  --output .rmux-audit/public-api-cfg-canaries
```

This focused run cannot issue a full 13-cell gate verdict. Native Windows/macOS
correction evidence remains due on those targets.

`public_api_policy.py` owns the two-facade allowlist and the preliminary immutable
baseline: commit `654a4092c1df36d5539630fb3e29240f60e77698`, tree
`c8e34c602a6d13a455d6546b134f5f4502c6653e`. The Git object must already exist
locally. There is no registry baseline, baseline refresh command, or expectation
generated from the candidate. The gate extracts that exact source, checks its
tree, and verifies both source snapshots are unchanged after execution.

Cargo metadata must agree exactly with the reviewed feature definitions. The
SDK has `web`, `crossterm`, `regex` and default `web`. Its eight subsets under
`--no-default-features`, normal Cargo default, and `--all-features` execute
separately. Ratatui has no own features and consumes SDK defaults; its three
selections also execute separately. That is **13 physical cells**, no aliases.
Cell IDs are `rmux-sdk/no-default[/crossterm+regex+web]` (each sorted subset),
`rmux-sdk/default`, `rmux-sdk/all-features`, and the analogous three Ratatui IDs.
Optional repeated `--cell` arguments must enumerate the complete set exactly;
there is no partial run that can produce a full gate pass.

Each cell generates public rustdoc JSON from baseline and candidate with the
same explicit Cargo feature flags. `RUSTC_BOOTSTRAP=1` unlocks JSON output on the
pinned stable rustdoc; it does not qualify nightly or MSRV behavior. Comparisons
use `--current-rustdoc`, `--baseline-rustdoc`, and `--release-type minor`.
That override permits additions while retaining major-breaking lints regardless
of crate versions. The CLI's default feature heuristic never runs.

Frozen downstream consumers are mandatory alongside SemVer. The SDK reuses the
baseline's consolidated migration, identity and semantic-conversion fixtures;
small additional witnesses cover Serde identities, Tokio watch receivers,
Regex/Crossterm conversions, and Ratatui root exports/types. The new witnesses
have pinned hashes. Default/all SDK consumers explicitly enable `rmux-sdk/default`
and check Cargo's emitted feature list against the selected cell; the migration
fixture's own default forwarding of `web` alone would omit SDK's `default` cfg.
The standalone SDK lock starts from candidate pins; offline
normalization may change path/workspace entries but must not introduce a registry
package/version/checksum outside that lock. All compilation then uses `--frozen`.
These consumers compile local source artifacts; they are not installed-package
or daemon execution evidence.

Known coverage boundary: cargo-semver-checks 0.49.0 misses the demonstrated
`fn(u32) -> u32` to `fn(u64) -> u64` change. The frozen compiler consumer rejects
it. Rustdoc also records external `rmux-types` identities as paths without their
complete definitions. Changes beyond the frozen witnesses and available lints
therefore still require independent review and further qualification. This is
an enforceable pilot, not an exhaustive API-equivalence proof or stable 1.0 freeze.

The canaries compile unchanged/additive APIs and downstream callers, force major
version changes, reject removals and changed signatures, and exercise all eight
feature subsets for a break visible only with `regex+web` without `crossterm`.
They also remove `ratatui_rmux::cell_style` and bump Cargo's version to 99.0.0 in a run-owned copy of the candidate;
the unchanged control passes and both SemVer and the frozen consumer reject the
mutant. Failed compilers and missing baselines are negative controls.

`result.json` is written as incomplete unless every required SemVer and consumer
cell succeeds. `commands.json`, per-command logs and hashed rustdoc artifacts
identify what ran. Tool errors, feature drift, missing crates/libraries/artifacts,
invalid or incomplete selections, or concurrent input changes fail the gate.
Native macOS, Windows 11, Windows 10-specific contracts, exact MSRV and installed
consumers remain separate obligations. `rmux-proto` publishability does not make
it a third stable facade.
