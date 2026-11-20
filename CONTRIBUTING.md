# Contributing to rmux

## How to contribute

Report bugs and feature requests through GitHub issues:

https://github.com/Helvesec/rmux/issues

Submit code changes as pull requests against `main`.

## Requirements for acceptance

A change should be focused, reviewable, and include a clear commit message.

Before opening a pull request, run:

```sh
npm --prefix tests/xterm-oracle ci --ignore-scripts
cargo build --workspace
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Unix copy-mode viewport tests require Node and the project-local, lockfile-pinned
xterm oracle. `RMUX_XTERM_NODE` can select the Node executable. Missing oracle
prerequisites fail these tests and the Unix fast gate explicitly.

New behavior should include tests. Bug fixes should include a regression test when practical.

Changes that affect public behavior should update the relevant README or documentation.

Do not commit credentials, private machine paths, local logs, or generated artifacts unless they are intentionally part of the repository.

## Developer Certificate of Origin

By contributing to RMUX, you certify that you have the right to submit the work
under the project license. See [DCO](DCO).

Pull requests for code or documentation must certify this with a `Signed-off-by:`
line:

```text
Signed-off-by: Your Name <you@example.com>
```

## Platform changes

For Linux, macOS, Windows, terminal, PTY, or shell compatibility changes, mention which platform was tested in the pull request.

## Rust compiler support and toolchain bumps

The supported minimum Rust version (MSRV) is **1.88.0**, inherited from
`workspace.package.rust-version` by all 14 workspace packages. This includes
the product binaries, the stable-facade candidates `rmux-sdk` and `ratatui-rmux`,
publishable internal dependencies, and the non-publishable `xtask`.
Publication permission does not make an internal crate a stable
facade. This is a supported/tested minimum for the locked source graph, not a
claim to have demonstrated the oldest possible compiler. The locked Ratatui
proc-macro dependencies `instability` and `darling` require Rust 1.88.

From a clean committed Git root, run the exact-floor check on each required
native host (Python is not required):

```sh
# Use absolute paths to the installed 1.88.0 toolchain and private Cargo storage.
# The toolchain must include rustc, cargo, rustdoc and wasm32-unknown-unknown std.
export CARGO_HOME=/absolute/private/cargo-home
export CARGO_TARGET_DIR=/absolute/exclusive-target-directory
export RMUX_MSRV_TOOLCHAIN=/absolute/toolchain/1.88.0-host
export RUSTC="$RMUX_MSRV_TOOLCHAIN/bin/rustc"
export RUSTDOC="$RMUX_MSRV_TOOLCHAIN/bin/rustdoc"
export RUST_MIN_STACK=8388608
export RUST_TEST_THREADS=2
"$RMUX_MSRV_TOOLCHAIN/bin/cargo" run --locked --offline --jobs 2 -p xtask -- msrv \
  --toolchain-dir "$RMUX_MSRV_TOOLCHAIN" \
  --output-dir /absolute/new-receipt-directory \
  --target-dir "$CARGO_TARGET_DIR"
"$RMUX_MSRV_TOOLCHAIN/bin/cargo" run --locked --offline --jobs 2 -p xtask -- package-inventory --check
```

`msrv` verifies exact Cargo/rustc/rustdoc versions and all effective package
declarations. It checks each package separately with default, all features and
the complete powerset of its non-default features, then checks workspace
targets under no/default/all selections, including build/dev dependencies,
tests, examples and required-feature targets. The server powerset also checks
every target in isolation so workspace feature unification cannot hide missing
test feature gates. It runs focused SDK/Ratatui tests, doctests and xtask tests,
plus the browser crypto `wasm` selection on
`wasm32-unknown-unknown`. Every cell has a raw log and exit-code receipt. A copied
SDK regex cell must pass, fail specifically after raising only its compiler
requirement, and pass after restoring that declaration. A failure stops the
check; no skip option is provided. Reuse only exclusively owned targets;
compiler wrappers/bootstrap overrides are rejected and compiler flags are
retained. The check limits child Cargo builds to two jobs.

The supported gate context is free of Cargo configuration files. Before every
metadata/check/test/doc/witness Cargo invocation, both built-binary and
`cargo run` entry routes reject `config` and `config.toml` in the actual cwd's
`.cargo`, every ancestor's `.cargo`, and the explicit absolute `CARGO_HOME`.
The home must be an existing directory. Both filenames are rejected regardless
of contents or precedence, including symlinks/dangling links; invalid or
inaccessible configuration directories also fail. No configuration is parsed,
filtered or removed. Use an owned unconfigured location/home; retain required
native PATH, SDK, linker and supported flag settings in the environment.
The outer `cargo run` reads configuration before xtask starts, so prepare this
context before invoking it. These directories must remain unchanged during the
check; this is not a sandbox against concurrent filesystem mutation.

These are source compilation/test receipts, separately required per native
OS/architecture; cross compilation does not prove native behavior. They do not
replace installed-package consumers against the final daemon, immutable API
checks, full release gates, or Windows 10/macOS Intel/Linux ARM qualification.
The supported OS/architecture matrix is unchanged. Newly resolved downstream
dependencies also need exact-floor qualification; this locked check does not
promise arbitrary future resolver results or freeze the public 1.0 API.

An MSRV bump is an explicit compatibility change: review and document it in
release notes, change the inherited floor together, and obtain exact-version
receipts for the supported native matrix before release. Dependency updates
must continue to pass the existing floor until that change is intentional.
Refresh the package inventory separately from the clean declaration commit.

`rust-toolchain.toml` pins the exact workspace compiler. Bump it in its own
reviewable change or in a release stabilization change that runs the full
workspace build, test, clippy, and format gates on every supported platform.
That 1.96.1 development pin and the separately pinned WASM artifact reproduction
recipe are not MSRV declarations; the floor check does not change either recipe.

## Package/feature declaration baseline

```sh
cargo run --locked --offline -p xtask -- package-inventory --check
cargo run --locked --offline -p xtask -- package-inventory --export
cargo run --locked --offline -p xtask -- package-inventory --update
cargo test --locked --offline -p xtask
```

Run inside the workspace using the pinned toolchain and locally available locked
dependencies. `--check` reads `xtask/spec/package-inventory-v1.json` and fails on
missing, changed or noncanonical declarations; it never writes the baseline.
`--export` prints JSON. `--update` explicitly replaces the expected file. Export
and update require a clean committed workspace at the Git root: commit source
changes first, then review and commit the baseline separately. `source_base`
records that input commit/tree, not the later commit containing the baseline;
checking retains this historical source and does not compare it to current HEAD.

Cargo metadata format 1 (`--no-deps --locked --offline`) owns workspace discovery,
manifest parsing and inheritance. Every workspace member is recorded by package
name, including non-publishable members and publishable internal dependencies.
`publish` retains Cargo's effective policy: `null` permits all registries, `[]`
disables publication, and a nonempty array restricts registries. Equivalent
explicit/inherited settings normalize alike; omission does not imply inheritance.
`rust_version: null` records absent declared MSRV. Features retain Cargo's mapping,
including implicit optional-dependency features, expansion strings and the
presence/absence of `default`. Libraries (including proc-macros) and binaries
retain names, kinds, crate types and required features. Package/target IDs contain
names, never paths; object keys and set-like lists have deterministic ordering.
Examples, tests, benches, build scripts and resolved dependency graphs are outside
this slice. The separate fixture baseline exercises real virtual-workspace Cargo
semantics; it is not a product baseline.

This is the Phase0 package/feature inventory only. Rust public API enumeration,
compiler SemVer checks, target/cfg and feature-combination qualification, a
supported terminal matrix, the exact-floor check above and installed consumer
proof remain separate obligations. This baseline does not declare stable 1.0.

## Security issues

Do not open a public issue for security problems. See [SECURITY.md](SECURITY.md).

## License

By contributing, you agree that your contribution is licensed under `MIT OR Apache-2.0`, matching the project license.
