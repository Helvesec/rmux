# Nix Packaging

RMUX ships a [Nix flake](../flake.nix) that builds the `rmux` and `rmux-daemon`
binaries from source, exposes a `nix run` app, and provides a development shell.
It targets `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, and
`aarch64-darwin`.

## Single source of truth for the version

The flake does **not** hardcode a version. At evaluation time it reads the
`workspace.package.version` field from [`Cargo.toml`](../Cargo.toml) with
`builtins.fromTOML` — the same number used by every workspace crate and by the
README badge.

```nix
cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
version = cargoToml.workspace.package.version;
```

Verify what the flake will use at any time:

```sh
nix eval --raw .#packages.x86_64-linux.default.version
```

This means the version lives in exactly one place. Bumping `Cargo.toml`
automatically updates the Nix package version — there is nothing version-shaped
to keep in sync inside `flake.nix`.

Dependency hashes are pinned via the committed [`Cargo.lock`](../Cargo.lock)
(`cargoLock.lockFile`), so there is **no `cargoHash` to recompute** when
dependencies change. Updating `Cargo.lock` is enough.

## Maintainer release checklist

When cutting a new release, the Nix flake needs **no manual edits** for the
common case. The release flow is:

1. **Bump the version in `Cargo.toml`** (the `[workspace.package]` `version`
   field) as you already do for a release. The flake picks this up
   automatically.
2. If dependencies changed, make sure **`Cargo.lock` is committed and current**
   (`cargo update` / `cargo build` then commit `Cargo.lock`). The flake reads
   hashes from it; no `cargoHash` edit is required.
3. Run the verification sequence below.
4. Refresh the pinned `nixpkgs` input only when you intend to
   (`nix flake update`), then commit `flake.lock`. This is optional per release;
   pin updates are a deliberate change, not a per-version chore.
5. Commit `flake.nix` / `flake.lock` changes together with the release.

> Action required only if you ever **rename the binary, change the license, move
> the repository, or restructure `[workspace.package]`** — those are reflected in
> `flake.nix` (`pname`, `meta`, the structured version lookup) and would need a
> matching edit. A plain version bump never requires touching `flake.nix`.

## Verification

```sh
nix build --no-link
nix run . -- -V                 # should print "rmux <version>" from Cargo.toml
nix flake check
nix develop -c cargo --version
```

`nix run . -- -V` (rmux uses tmux-style flags; `-V` is the version flag) is the
end-to-end check that the single-source-of-truth version flows from `Cargo.toml`
through the flake into the built binary.

The CI Nix job gates the README install path with:

- `nix flake check`
- `nix build .#packages.x86_64-linux.default --no-link`
- `nix run . -- -V`, compared against `workspace.package.version` from
  `Cargo.toml`

> The build sets `doCheck = false`, so tests do not run during `nix build`. The
> `tests/` integration suites spawn the daemon over real PTYs/sockets, shell out
> to host tools like `ps`, and some are timing-sensitive (e.g. an interactive
> choose-tree redraw race) — unreliable in the hermetic, CPU-saturated sandbox
> even though they pass under `cargo test`. Run the full suite in the project's
> CI and locally with `cargo test`.

## Updating the pinned nixpkgs

```sh
nix flake update      # refresh flake.lock to latest nixpkgs-unstable
git add flake.lock
```

Treat this as an intentional change (it can shift the toolchain and transitive
build inputs); re-run the verification sequence afterward.
