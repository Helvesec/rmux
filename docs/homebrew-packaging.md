# Homebrew Packaging

RMUX Homebrew support is generated from GitHub Release assets. GitHub Actions
builds and verifies the archives; the release orchestration only updates the
tap formula after those assets and `SHA256SUMS` are public.

The Homebrew formula uses the release artifact convention produced by
`scripts/package-unix.sh`:

```text
rmux-<semver>-macos-aarch64.tar.gz
rmux-<semver>-macos-x86_64.tar.gz
```

The Homebrew tap is macOS-only. Linux users should use APT, DNF, the portable
installer, or Cargo.

Generate `Formula/rmux.rb` for the official tap:

```sh
tag=v0.6.0
version="${tag#v}"
curl -fsSL "https://github.com/Helvesec/rmux/releases/download/$tag/SHA256SUMS" -o /tmp/rmux-SHA256SUMS
scripts/generate-homebrew-formula.sh \
  --version "$version" \
  --checksums /tmp/rmux-SHA256SUMS \
  --output ../homebrew-rmux/Formula/rmux.rb
```

Validate the generated formula locally before committing it to the tap:

```sh
ruby -c ../homebrew-rmux/Formula/rmux.rb
brew style ../homebrew-rmux/Formula/rmux.rb
```

The tap repository is separate from this source repository. The expected user
install command is:

```sh
brew install helvesec/rmux/rmux
```

The release orchestrator should run the generator after `release.yml` has
published the release assets and after `SHA256SUMS` is available.

The release workflow also runs the generator against the release artifacts as a
CI guard. The tap repository is `Helvesec/homebrew-rmux`.
