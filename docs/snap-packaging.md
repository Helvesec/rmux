# Snap Packaging

RMUX ships a Snapcraft manifest at `snap/snapcraft.yaml`.

The snap uses `confinement: classic` because RMUX launches user shells, manages
PTYs, reads normal user configuration, and needs terminal-multiplexer behaviour
that should match the native packages. Classic confinement requires Snap Store
approval before the snap can be published.

## Store setup

Install Snapcraft on a snap-enabled Linux host:

```sh
sudo snap install snapcraft --classic
snapcraft --version
```

Then log in with the publisher Ubuntu One account and reserve the public snap
name:

```sh
snapcraft login
snapcraft whoami
snapcraft register rmux
```

Request classic confinement approval from the Snap Store for a terminal
multiplexer that launches user shells, manages PTYs, reads normal user
configuration, and needs behaviour consistent with native packages.

Export CI credentials scoped to the snap and candidate publishing:

```sh
snapcraft export-login \
  --snaps=rmux \
  --channels=latest/candidate \
  --acls package_access,package_push,package_update,package_release \
  snapcraft-login.txt
```

Store the file contents in the GitHub `release` environment secret named
`SNAPCRAFT_STORE_CREDENTIALS`, then remove the local credential file:

```sh
gh secret set SNAPCRAFT_STORE_CREDENTIALS \
  --repo Helvesec/rmux \
  --env release \
  < snapcraft-login.txt
rm -f snapcraft-login.txt
```

The release workflow intentionally publishes only to `latest/candidate`; stable
promotion stays manual after candidate testing.

## Local build and smoke

```sh
snapcraft --use-lxd
sudo snap install --dangerous --classic ./rmux_*.snap
rmux -V
rmux list-commands
rmux kill-server
sudo snap remove rmux
```

If LXD is not configured on the build host, initialize it before the first
local build:

```sh
sudo snap install lxd
sudo usermod -a -G lxd "$USER"
newgrp lxd
lxd init --auto
```

The CI and release workflows use the same smoke through:

```sh
scripts/smoke-snap-package.sh ./rmux_*.snap
```

Do not run Snapcraft with `--destructive-mode` from this workspace. The source
tree contains local build artefacts such as `target/`, and destructive builds
operate directly in the working tree instead of a clean provider environment.
Use LXD locally and the GitHub Actions Snap build for release validation.

## Release flow

The release workflow builds amd64 and arm64 snaps from source, smoke-tests them
with classic confinement, attaches the `.snap` files to the GitHub Release, and
publishes them to `latest/candidate`.

After public candidate testing, promote the tested revision to stable from the
Snapcraft dashboard or with Snapcraft CLI. Do not advertise:

```sh
sudo snap install rmux --classic
```

until the snap is accepted for classic confinement and the stable channel has a
tested revision.
