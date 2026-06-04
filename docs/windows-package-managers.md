# Windows Package Managers

RMUX Windows package-manager support is generated from the GitHub Release
Windows zip. Package managers must not rebuild RMUX; they pin the published
release asset URL and SHA256.

The canonical Windows release artifact is:

```text
rmux-<semver>-windows-x86_64.zip
```

The zip contains:

```text
rmux-<semver>-windows-x86_64/
  rmux.exe
  README.md
  LICENSE-APACHE
  LICENSE-MIT
  rmux.1
  SHA256SUMS.txt
  share/rmux/artifact-metadata.json
```

GitHub Actions builds and verifies the zip with the same scripts that work under
Windows PowerShell 5.1 (`powershell.exe`) and PowerShell 7 (`pwsh`):

```powershell
./scripts/package-windows.ps1 -Configuration release -Target x86_64-pc-windows-msvc -OutputDir dist -PlatformLabel windows-x86_64
./scripts/verify-package-windows.ps1 dist/rmux-<semver>-windows-x86_64.zip -Checksums dist/SHA256SUMS.txt -RunBinary -RunDaemonSmoke
```

For a local package-manager dry run, use the `dist/SHA256SUMS.txt` produced by
`package-windows.ps1`. After the release workflow has produced `SHA256SUMS`, use
the downloaded release checksum file instead.

```sh
version=0.5.0
checksums=dist/SHA256SUMS.txt
scripts/generate-winget-manifest.sh \
  --version "$version" \
  --checksums "$checksums" \
  --output target/package-managers/winget/Helvesec.RMUX.yaml
scripts/generate-scoop-manifest.sh \
  --version "$version" \
  --checksums "$checksums" \
  --output target/package-managers/scoop/rmux.json
scripts/generate-chocolatey-package.sh \
  --version "$version" \
  --checksums "$checksums" \
  --output-dir target/package-managers/chocolatey/rmux
```

## WinGet

The generated WinGet manifest is a singleton manifest for `Helvesec.RMUX` using:

```text
InstallerType: zip
NestedInstallerType: portable
PortableCommandAlias: rmux
```

Validate and test on Windows before submission:

```powershell
pwsh ./scripts/validate-winget-manifest.ps1 `
  -Manifest target/package-managers/winget/Helvesec.RMUX.yaml `
  -Version 0.5.0 `
  -Checksums dist/SHA256SUMS.txt
winget validate target/package-managers/winget/Helvesec.RMUX.yaml
winget install --manifest target/package-managers/winget/Helvesec.RMUX.yaml
rmux -V
rmux diagnose --json
```

GitHub Actions validates the generated WinGet manifest after the release assets
are published. The release workflow performs a structural check against
`SHA256SUMS` and runs `winget validate` when WinGet is available on the Windows
runner.

WinGet publication is a pull request to `microsoft/winget-pkgs`; there is no
Chocolatey-style API key. For the first RMUX package, submit manually so the
publisher, identifier, and CLA are settled:

```powershell
winget install wingetcreate
wingetcreate submit target/package-managers/winget/Helvesec.RMUX.yaml
```

If `wingetcreate` needs a token, use its OAuth/cache flow locally or set
`WINGET_CREATE_GITHUB_TOKEN` only in a protected release secret for CI. Do not
commit or print the token. After `Helvesec.RMUX` exists in `microsoft/winget-pkgs`,
future releases can be automated with `wingetcreate update Helvesec.RMUX ...`.

## Scoop

The generated Scoop manifest is `rmux.json`. The public bucket is
`Helvesec/scoop-rmux`.

User install command:

```powershell
scoop bucket add rmux https://github.com/Helvesec/scoop-rmux
scoop install rmux
```

Validate a generated manifest locally on Windows before committing it:

```powershell
scoop install .\target\package-managers\scoop\rmux.json
rmux -V
```

## Chocolatey

The generated Chocolatey source lives in `target/package-managers/chocolatey/rmux`
and contains:

```text
rmux.nuspec
tools/chocolateyInstall.ps1
tools/chocolateyUninstall.ps1
```

Validate on Windows before pushing to Chocolatey:

```powershell
cd target/package-managers/chocolatey/rmux
choco pack
choco install rmux --source . --version <semver>
rmux -V
```

GitHub Actions publishes Chocolatey after the GitHub Release assets are public.
The release workflow expects a secret named `CHOCOLATEY_API_KEY`; keep it as a
GitHub Actions secret and never commit it to the repository. The workflow packs
the generated package, performs a local Chocolatey install smoke test, then
pushes the `.nupkg` to `https://push.chocolatey.org/` for moderation.

Never replace a published release zip silently. WinGet, Scoop, and Chocolatey
all pin SHA256 values; a bad asset requires a new version.
