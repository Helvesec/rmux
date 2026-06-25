# Linux Package Repositories

RMUX Linux distribution packages are generated from the same release binary as
the Linux `.tar.gz` artifact. GitHub Actions builds and verifies the `.deb` and
`.rpm` packages, then the local release orchestrator publishes signed repository
metadata through `rmux.io`.

The public package repository host is:

```text
https://packages.rmux.io
```

GitHub Releases remain the canonical immutable source for release assets and
`SHA256SUMS`. The package repositories are static metadata views over those
assets, copied into the `rmuxio` deployment tree during release.

The release workflow requires these GitHub secrets before it will generate Linux
repositories: `RMUX_APT_GPG_PRIVATE_KEY`, `RMUX_APT_GPG_KEY`,
`RMUX_RPM_GPG_PRIVATE_KEY`, `RMUX_RPM_GPG_KEY`, and optionally
`RMUX_RPM_REPO_GPG_PRIVATE_KEY` plus `RMUX_RPM_REPO_GPG_KEY` when repodata uses a
different key.

## Debian / Ubuntu

The Debian package artifacts are:

```text
rmux_<semver>_amd64.deb
rmux_<semver>_arm64.deb
```

The APT repository layout is generated under:

```text
public/packages/debian/
  rmux.asc
  pool/main/r/rmux/rmux_<semver>_amd64.deb
  pool/main/r/rmux/rmux_<semver>_arm64.deb
  dists/stable/Release
  dists/stable/InRelease
  dists/stable/Release.gpg
  dists/stable/main/binary-amd64/Packages
  dists/stable/main/binary-amd64/Packages.gz
  dists/stable/main/binary-arm64/Packages
  dists/stable/main/binary-arm64/Packages.gz
```

User install command:

```sh
curl -fsSL https://packages.rmux.io/debian/rmux.asc | sudo tee /usr/share/keyrings/rmux.asc >/dev/null
echo "deb [signed-by=/usr/share/keyrings/rmux.asc] https://packages.rmux.io/debian stable main" | sudo tee /etc/apt/sources.list.d/rmux.list
sudo apt update
sudo apt install rmux
```

Generate locally from release assets:

```sh
scripts/generate-apt-repository.sh \
  --input-dir release-assets \
  --output-dir ../rmuxio/public/packages/debian \
  --suite stable \
  --component main \
  --architecture amd64 \
  --architecture arm64 \
  --signing-key "$RMUX_APT_GPG_KEY"
gpg --armor --export "$RMUX_APT_GPG_KEY" > ../rmuxio/public/packages/debian/rmux.asc
```

## Fedora

The RPM artifact is:

```text
rmux-<semver>-1.x86_64.rpm
```

The DNF repository layout is generated under:

```text
public/packages/rpm/
  RPM-GPG-KEY-rmux
  rmux.repo
  rmux-<semver>-1.x86_64.rpm
  repodata/repomd.xml
  repodata/repomd.xml.asc
  repodata/*
```

User install command:

```sh
sudo dnf config-manager addrepo --from-repofile=https://packages.rmux.io/rpm/rmux.repo
sudo dnf install rmux
```

Generate locally from release assets:

```sh
scripts/generate-rpm-repository.sh \
  --input-dir release-assets \
  --output-dir ../rmuxio/public/packages/rpm \
  --baseurl https://packages.rmux.io/rpm \
  --rpm-signing-key "$RMUX_RPM_GPG_KEY" \
  --repo-signing-key "$RMUX_RPM_REPO_GPG_KEY"
gpg --armor --export "$RMUX_RPM_GPG_KEY" > ../rmuxio/public/packages/rpm/RPM-GPG-KEY-rmux
```

Do not replace a published `.deb` or `.rpm` silently. Package repositories pin
checksums in their metadata; a bad package requires a new version.
