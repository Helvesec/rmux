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
  libexec/rmux/rmux.exe
  rmux-daemon.exe
  README.md
  LICENSE-APACHE
  LICENSE-MIT
  rmux.1
  SHA256SUMS.txt
  share/rmux/artifact-metadata.json
```

For `0.7.0`, `rmux.exe` is the public tiny dispatcher. The full CLI helper is
private package content at `libexec/rmux/rmux.exe`; package-manager shims should
only expose `rmux.exe` and `rmux-daemon.exe`.

## Tiny CLI Contract

The Windows tiny CLI package layout is:

```text
rmux-<semver>-windows-x86_64/
  rmux.exe                 public tiny dispatcher
  libexec/rmux/rmux.exe    private full CLI helper
  rmux-daemon.exe          daemon
```

Required invariants:

- `rmux.exe` must fall back to `libexec/rmux/rmux.exe` for every command form
  outside the tiny allowlist.
- Setting `RMUX_DISABLE_TINY_CLI=1` must force `rmux.exe` to delegate to
  `libexec/rmux/rmux.exe`.
- Missing, stale, or mismatched helpers must fail closed with a clear error;
  they must not silently skip tmux-compatible behavior.
- Direct tiny paths must be enabled command-by-command. Detached list, capture,
  resize, and kill paths may be safer to enable before attach or console paths.
- The full helper remains authoritative for config loading, formats, command
  queues, attach console setup, long-lived streams, and any ambiguous parser
  surface.

Acceptance checklist for the Windows implementation:

- `package-windows.ps1` builds the full helper and tiny `rmux.exe`.
- `verify-package-windows.ps1` requires `libexec/rmux/rmux.exe`, validates
  checksums, and verifies `rmux.exe -V` with `RMUX_DISABLE_TINY_CLI=1` set.
- Native Windows named-pipe IPC smoke passes through both the tiny direct path
  and the helper fallback path.
- Native Windows attach/ConPTY smoke passes with the attach-related tiny path
  enabled.
- Native Windows benchmarks compare full CLI, tiny CLI, and the previous release
  before release notes mention Windows tiny-CLI performance.
- WinGet, Scoop, and Chocolatey dry runs install the tiny package and execute
  `rmux -V`, `rmux -V` with `RMUX_DISABLE_TINY_CLI=1` set,
  `rmux diagnose --json`, and the daemon smoke.

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
version=0.7.0
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

The generated WinGet manifest uses the current multi-file WinGet layout:

```text
target/package-managers/winget/
  Helvesec.RMUX.yaml
  Helvesec.RMUX.installer.yaml
  Helvesec.RMUX.locale.en-US.yaml
```

The installer manifest pins the GitHub Release zip with:

```text
InstallerType: zip
NestedInstallerType: portable
PortableCommandAlias: rmux
```

Validate and test on Windows before submission:

```powershell
pwsh ./scripts/validate-winget-manifest.ps1 `
  -Manifest target/package-managers/winget/Helvesec.RMUX.yaml `
  -Version 0.7.0 `
  -Checksums dist/SHA256SUMS.txt
winget validate --manifest target/package-managers/winget
winget install --manifest target/package-managers/winget
rmux -V
rmux diagnose --json
```

GitHub Actions validates the generated WinGet manifest after the release assets
are published. The release workflow performs a structural check against
`SHA256SUMS` and runs `winget validate` when WinGet is available on the Windows
runner. Singleton manifests are deprecated by `microsoft/winget-pkgs`; do not
submit or regenerate them.

WinGet publication is a pull request to `microsoft/winget-pkgs`; there is no
Chocolatey-style API key. For the first RMUX package, submit manually so the
publisher, identifier, and CLA are settled:

```powershell
winget install wingetcreate
wingetcreate submit target/package-managers/winget
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
GitHub Actions environment secret on the protected `release` environment and
never commit it to the repository. The workflow packs the generated package,
performs a local Chocolatey install smoke test, then pushes the `.nupkg` to
`https://push.chocolatey.org/` for moderation. If the same package version is
already visible on Chocolatey, the workflow skips the push.

Never replace a published release zip silently. WinGet, Scoop, and Chocolatey
all pin SHA256 values; a bad asset requires a new version.
