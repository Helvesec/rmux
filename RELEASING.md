# RMUX Releasing

This file is the versioned release checklist for RMUX. It is the canonical source for release ordering. Local helper scripts may automate parts of the process, but they must follow this checklist.

## Release drivers

- `.github/workflows/release.yml` is the canonical publication pipeline.
- A signed Git tag push is the normal trigger for the release workflow.
- `workflow_dispatch` is reserved for manual recovery or dry-run investigation. Do not dispatch the release workflow manually for the same tag after pushing that tag.
- `scripts/release-local.sh` is a local packaging and verification smoke tool. It does not create tags, push branches, publish releases, or contact CI.
- A local `rmux-release.sh`, if present, is ignored by Git and is not authoritative. Before using any local release helper, verify that it does not both push a tag and dispatch `.github/workflows/release.yml`.

## Before a release candidate

1. Start from an up-to-date `main`.
2. Create or update a release branch.
3. Ensure the workspace version, Cargo manifests, `Cargo.lock`, manpage, changelog, README assets, and package metadata all match the intended version.
4. Run the release gates:

   ```sh
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo build --workspace --locked
   cargo test --workspace --locked --no-fail-fast
   scripts/unsafe-check.sh
   scripts/no-network-in-runtime.sh
   scripts/check-platform-neutrality.sh
   scripts/no-debug-assert-side-effects.sh
   scripts/release-review-gate.sh
   ```

5. Verify the committed performance baseline and comparator:

   ```sh
   python3 scripts/perf-diff.py benches/perf/baselines/release-0.7.0.json \
     benches/perf/baselines/release-0.7.0.json --fail-on-regression
   ```

6. Verify modified GitHub Actions use pinned actions and valid YAML.
7. Verify no modified release file contains automation attribution or internal tool names.

## Release candidate

1. Create a disposable signed RC tag.
2. Push only the RC tag.
3. Let the `push.tags` release workflow run from the tag ref.
4. Verify the uploaded `SHA256SUMS` and `SHA256SUMS.sigstore.json` with the exact `cosign verify-blob` command from `SECURITY.md`.
5. Verify GitHub artifact attestations with:

   ```sh
   gh attestation verify <asset> --repo Helvesec/rmux
   ```

6. Delete the disposable RC tag after validation.

## Final release

1. Merge the release branch back to `main`.
2. Create a signed final tag:

   ```sh
   git tag -s vX.Y.Z
   ```

3. Push `main`.
4. Push only the final tag.
5. Let the tag-triggered release workflow publish archives, package repositories, checksums, Sigstore bundle, and GitHub attestations.
6. Verify:

   ```sh
   gh attestation verify <asset> --repo Helvesec/rmux
   cosign verify-blob --bundle SHA256SUMS.sigstore.json --certificate-identity-regexp 'https://github.com/Helvesec/rmux/.github/workflows/release.yml@refs/tags/vX\.Y\.Z' --certificate-oidc-issuer https://token.actions.githubusercontent.com SHA256SUMS
   ```

7. Sanity-check package managers and crates:

   ```sh
   cargo install rmux@X.Y.Z --locked
   rmux -V
   ```

## Do not do

- Do not create both a tag push and a manual `gh workflow run release.yml` for the same release.
- Do not publish from a branch ref when release signatures are expected to verify against a tag ref.
- Do not publish crates.io or package-manager metadata before the release artifacts and signatures verify.
- Do not add Scorecard, Sigstore, or SLSA badges before the corresponding public evidence exists.
