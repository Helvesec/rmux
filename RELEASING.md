# RMUX Releasing

This is the release checklist for RMUX. Local helper scripts may automate parts
of the process, but this file defines the order and the gates.

## Release drivers

- `.github/workflows/release.yml` is the canonical publication pipeline.
- A signed Git tag push is the normal trigger for the release workflow.
- `workflow_dispatch` is for recovery or investigation only. Do not dispatch the
  release workflow manually for the same tag after pushing that tag.
- `scripts/release-local.sh` is a local packaging and verification smoke tool. It
  does not create tags, push branches, publish releases, or contact CI.
- Local release helpers are ignored by Git and are not authoritative.

## Branch review

1. Start from an up-to-date `main`.
2. Create or update a release branch named `release/X.Y.Z`.
3. Review the branch shape:

   ```sh
   git fetch origin --prune
   git log --oneline --decorate origin/main..HEAD
   git diff --stat origin/main...HEAD
   git diff --check origin/main...HEAD
   ```

4. Keep outside contributor commits as their own commits when they should appear
   as contributors. Squash only project-maintainer cleanup commits that do not
   need separate attribution.
5. Verify every commit has the intended author, committer, and subject. Preserve
   intentional external authors. Maintainer commits use
   `Sidney Sissaoui <shideneyu@gmail.com>` and do not add `Signed-off-by:`
   trailers. `.mailmap` canonicalizes legacy local automation identity:

   ```sh
   git log --use-mailmap --format=fuller origin/main..HEAD
   git log --use-mailmap --format='%H%n%aN <%aE>%n%cN <%cE>%n%s%n%b' origin/main..HEAD
   ```

6. Review all changed files before any tag is created:

   ```sh
   git diff --name-status origin/main...HEAD
   git diff origin/main...HEAD -- . ':!target'
   ```

## Hygiene scans

Run these before merging, tagging, or publishing. Exact denylist patterns stay in
`.release-deployment/private-denylist.txt`, which is intentionally ignored by
Git. It should cover secret-like strings, local paths, machine names, private
aliases, internal planning words, stale scratch labels, and unwanted attribution
text.

If the file exists, run:

```sh
while IFS= read -r pattern; do
  [ -z "$pattern" ] && continue
  case "$pattern" in \#*) continue ;; esac
  rg -n --hidden --glob '!.git' --glob '!target' --glob '!dist' \
    --glob '!.release-deployment' --fixed-strings "$pattern" .
done < .release-deployment/private-denylist.txt
```

Any hit must be either removed or explicitly justified before release.

## Version coherence

1. Bump the workspace version and every crate/package version that must ship.
2. Update intra-workspace dependency constraints.
3. Regenerate `Cargo.lock`.
4. Update README snippets, manpage, install scripts, release notes, package
   metadata, site metadata, and package-manager manifests.
5. Verify the old version is gone from release-facing files:

   ```sh
   rg -n 'OLD_VERSION|vOLD_VERSION|old-version' \
     Cargo.toml Cargo.lock crates README.md docs scripts .github target/package-managers
   ```

6. Verify the binary reports the intended version:

   ```sh
   cargo run --locked --bin rmux -- -V
   cargo run --locked --bin rmux -- diagnose --json
   ```

## Required local gates

Run the fast gates locally before pushing the release branch:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked --no-fail-fast
scripts/unsafe-check.sh
scripts/no-network-in-runtime.sh
scripts/check-platform-neutrality.sh
scripts/no-debug-assert-side-effects.sh
RMUX_PERF_AUTO_CURRENT=1 scripts/release-review-gate.sh
```

The release workflow enforces portable absolute budgets on every runner. Owner
machines may additionally compare against their committed same-host baseline;
baselines and current runs carry a hashed `host_fingerprint` (from
`/etc/machine-id` or the hostname, never stored in clear).
Darwin uses `benches/perf/baselines/release-0.9.0.json`; Linux requires
`benches/perf/baselines/release-0.9.0-linux.json` recorded with
`scripts/perf-baseline.sh` on the Linux release machine before the release
gate can pass there, and a Linux baseline without a fingerprint fails closed.
Before tagging, the perf gate must have run green on each release machine that
owns a baseline. The Windows x86_64 release matrix requires a logged-on,
interactive self-hosted runner carrying the labels `self-hosted`, `Windows`,
`X64`, and `rmux-windows-interactive`. That job builds, Ctrl-tests, and packages
the same SHA-256-bound binaries; hosted session-0 evidence and evidence copied
from another build are not accepted.

If release packaging, Web Share, or the site changed, also run the matching
package and WASM provenance gates before tagging. For the WebSocket fuzz target,
the harness lives under `scripts/fuzz`:

```sh
cargo fuzz list --fuzz-dir scripts/fuzz
cargo fuzz check --fuzz-dir scripts/fuzz --sanitizer none websocket_client_frame
```

Run the default sanitizer-backed fuzz mode on a nightly toolchain when doing a
deeper parser/security pass.

## Release candidate

Use a disposable signed RC tag before the final tag when release workflow changes,
signing, package formats, or package-manager publication changed.

1. Create the RC tag on the release branch:

   ```sh
   git tag -s vX.Y.Z-rc.1
   git push origin vX.Y.Z-rc.1
   ```

2. Let the tag-triggered release workflow run from the tag ref.
3. Verify checksums, Sigstore, and attestations:

   ```sh
   gh attestation verify <asset> --repo Helvesec/rmux
   cosign verify-blob \
     --bundle SHA256SUMS.sigstore.json \
     --certificate-identity-regexp 'https://github.com/Helvesec/rmux/.github/workflows/release.yml@refs/tags/vX\.Y\.Z-rc\.1' \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com \
     SHA256SUMS
   ```

4. Smoke-test at least Linux locally and ask for macOS/Windows public-install
   checks when platform packaging changed.
5. Delete the disposable RC tag and GitHub release after validation.

## Final release

1. Merge the release branch to `main` only after CI is green.
2. Create the signed final tag:

   ```sh
   git tag -s vX.Y.Z
   ```

3. Push `main`.
4. Push only the final tag.
5. Let the tag-triggered release workflow publish:
   - GitHub Release archives,
   - `.deb` and `.rpm` packages,
   - package repository metadata,
   - checksums,
   - Sigstore bundle,
   - GitHub attestations.
6. Do not re-run the release workflow manually for the same tag unless recovering
   from a known infrastructure failure.

## Crates.io

Publish crates only after GitHub Release assets, checksums, signatures, and
attestations verify.

1. Publish in dependency order.
2. Wait for each crate version to become visible on crates.io before publishing
   dependents.
3. Skip crates already published at the target version.
4. The root `rmux` binary crate is intentionally `publish = false`; do not
   expect `cargo install rmux --version X.Y.Z` to work for this release line.
   Finish Rust crate publishing with the published SDK/support crates, then
   smoke the binary via the Git tag path:

   ```sh
   cargo install --git https://github.com/Helvesec/rmux --tag vX.Y.Z rmux --locked
   rmux -V
   ```

## Package managers

After the GitHub Release is verified:

- APT/RPM: verify `packages.rmux.io` metadata and install from a clean machine or
  container.
- Homebrew tap: verify `brew install helvesec/rmux/rmux` until Homebrew Core is
  current.
- Homebrew Core: update or monitor the existing PR; do not open duplicate PRs.
- Scoop: update `Helvesec/scoop-rmux` and smoke `scoop install rmux`.
- WinGet: submit or update one PR in `microsoft/winget-pkgs`; do not force-push
  unless the WinGet maintainers request it.
- Chocolatey: submit once, then use the package review comments if moderation
  asks for action. Do not repeatedly push the same version.
- Snap: publish only when the Snap package is ready and visible in the channel.
- Nix: verify the flake path tracks the release commit.

Track pending external queues until each manager reports the target version.

## rmux.io and Web Share

Update `rmux.io` only after the public install paths exist, or mark lagging
package managers with a visible rollout note.

1. Update install commands, version text, docs, SEO snippets, and package-manager
   status.
2. Keep the shell and PowerShell installers as the latest-version paths.
3. If Homebrew Core, WinGet, or Chocolatey are still under registry review, keep
   their commands visible only with a short status note such as
   `0.6.1 available; 0.7.0 rolling out`.
4. Remove each note once the public registry reports the target version.
5. Build and test the site.
6. Deploy `share.rmux.io` after verifying the Web Share WASM source-to-binary
   gate and the expected integrity hash.
7. Verify the live install scripts:

   ```sh
   curl -fsSL https://rmux.io/install.sh | sh
   irm https://rmux.io/install.ps1 | iex
   ```

## Public smoke matrix

Before closing a release, verify or request verification for:

- Linux installer: `curl -fsSL https://rmux.io/install.sh | sh`
- Linux APT: signed repository install.
- Linux DNF/RPM: signed repository install.
- macOS installer: `curl -fsSL https://rmux.io/install.sh | sh`
- macOS Homebrew: `brew install rmux` or the tap command if Core is behind.
- Windows installer: `irm https://rmux.io/install.ps1 | iex`
- Windows WinGet: `winget install rmux` or `winget install -e --id Helvesec.RMUX`
- Windows Scoop: `scoop bucket add rmux https://github.com/Helvesec/scoop-rmux && scoop install rmux`
- Windows Chocolatey: `choco install rmux`
- Rust git tag: `cargo install --git https://github.com/Helvesec/rmux --tag vX.Y.Z rmux --locked`

Every public smoke must end with:

```sh
rmux -V
```

The output must be `rmux X.Y.Z`.

## Post-release

1. Confirm `main` and the final tag point at the intended commit.
2. Confirm no stale branch, tag, package manager PR, or release draft refers to a
   superseded version.
3. Confirm no release-facing docs mention outdated package status.
4. Keep badges conservative: add Scorecard, Sigstore, SLSA, or package-manager
   badges only after public evidence exists.
5. Record blockers and fixes in the local ignored release notes, not in
   release-facing docs.

## Do not do

- Do not publish from a branch ref when signatures are expected to verify against
  a tag ref.
- Do not create both a tag push and a manual workflow dispatch for the same tag.
- Do not publish crates.io before release assets and signatures verify.
- Do not update install docs before the corresponding public path works.
- Do not store secrets, local machine names, personal aliases, or private release
  notes in Git.
