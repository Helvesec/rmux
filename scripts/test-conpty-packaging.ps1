param(
    [Parameter(Mandatory = $true)][string]$ConptyPackage,
    [Parameter(Mandatory = $true)][string]$OutputDir,
    [string]$Archive = ""
)
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "conpty/runtime.ps1")
. (Join-Path $PSScriptRoot "package-windows-archive.ps1")
$target = "x86_64-pc-windows-msvc"
$manifest = Get-ConptyManifest $target
$root = [IO.Path]::GetFullPath($OutputDir)
if (Test-Path -LiteralPath $root) { throw "test output must be fresh: $root" }
New-Item -ItemType Directory -Path $root | Out-Null
$script:results = @()
function Check([bool]$Value, [string]$Name) {
    if (-not $Value) { throw "assertion failed: $Name" }
    $script:results += $Name
    Write-Output "PASS $Name"
}
function Reject([scriptblock]$Action, [string]$Pattern, [string]$Name) {
    $message = ""
    try { & $Action } catch { $message = $_.Exception.Message }
    Check ($message -match $Pattern) "$Name ($message)"
}
function Hash([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash }
function FileMap([string]$Path) {
    $result = [ordered]@{}
    foreach ($file in Get-ChildItem -LiteralPath $Path -File -Recurse | Sort-Object FullName) {
        $result[$file.FullName.Substring($Path.Length)] = Hash $file.FullName
    }
    return ($result | ConvertTo-Json -Compress)
}

$stage = Join-Path $root "fixture"
New-Item -ItemType Directory -Path "$stage/libexec/rmux", "$stage/share/rmux" | Out-Null
$inputHash = Hash $ConptyPackage
Copy-ConptyRuntime $target $ConptyPackage $stage
# The PE fixtures test packaging only; they are never launched as RMUX.
foreach ($name in @("rmux.exe", "rmux-daemon.exe", "libexec/rmux/rmux.exe")) {
    Copy-Item -LiteralPath "$stage/OpenConsole.exe" -Destination (Join-Path $stage $name)
}
Assert-ConptyPackage $stage $target
Check ((Hash $ConptyPackage) -eq $inputHash) "official archive remains unchanged"
foreach ($name in @("conpty.dll", "OpenConsole.exe")) {
    Check ((Hash "$stage/$name") -eq (Hash "$stage/libexec/rmux/$name")) "pair copied beside both hosts: $name"
}
foreach ($bad in @("aarch64-pc-windows-msvc", "i686-pc-windows-msvc", "x86_64-pc-windows-gnu")) {
    Reject { Get-ConptyManifest $bad } "supports only" "unsupported target $bad"
}
$badArchive = Join-Path $root "corrupt.nupkg"
[IO.File]::WriteAllText($badArchive, "invalid archive")
Reject { Copy-ConptyRuntime $target $badArchive $stage } "SHA-256" "corrupt supplied archive is not replaced"
Check ((Get-Content -LiteralPath $badArchive -Raw) -eq "invalid archive") "no replacement download"
foreach ($relative in @("conpty.dll", "OpenConsole.exe", "libexec/rmux/conpty.dll",
    "libexec/rmux/OpenConsole.exe", "share/rmux/conpty-runtime.json", $manifest.license.path)) {
    $path = Join-Path $stage $relative
    $backup = Join-Path $root "held"
    Move-Item -LiteralPath $path -Destination $backup
    try { Reject { Assert-ConptyPackage $stage $target } "SHA-256" "missing $relative" }
    finally { Move-Item -LiteralPath $backup -Destination $path }
}
$dll = Join-Path $stage "conpty.dll"
$original = [IO.File]::ReadAllBytes($dll)
try {
    [IO.File]::AppendAllText($dll, "tampered")
    Reject { Assert-ConptyPackage $stage $target } "SHA-256" "tampered runtime rejected"
} finally { [IO.File]::WriteAllBytes($dll, $original) }
$badSigner = [pscustomobject]@{ signer_thumbprint = "0000" }
Reject { Assert-ConptySignature $dll $manifest $badSigner } "Authenticode" "unexpected signer rejected"
Reject { Assert-ConptyMachine "$stage/rmux-daemon.exe" 0xaa64 } "architecture mismatch" "wrong executable architecture"

$zip1 = Join-Path $root "first.zip"
$zip2 = Join-Path $root "second.zip"
Write-ReproducibleWindowsArchive $stage $zip1 "2026-09-08T12:34:57+02:00"
Get-ChildItem -LiteralPath $stage -Recurse -File | ForEach-Object { $_.LastWriteTimeUtc = [DateTime]::UtcNow }
Write-ReproducibleWindowsArchive $stage $zip2 "2026-09-08T10:34:57Z"
Check ((Hash $zip1) -eq (Hash $zip2)) "ZIP is independent of input mtimes and timestamp timezone"
$zip = [IO.Compression.ZipFile]::OpenRead($zip1)
try {
    Check (@($zip.Entries | Where-Object { $_.LastWriteTime.DateTime -ne [datetime]"2026-09-08T10:34:56" }).Count -eq 0) "ZIP UTC timestamps rounded to two seconds"
} finally { $zip.Dispose() }

if (-not [string]::IsNullOrWhiteSpace($Archive)) {
    $unpack = Join-Path $root "unpacked"
    [IO.Compression.ZipFile]::ExtractToDirectory([IO.Path]::GetFullPath($Archive), $unpack)
    $package = Join-Path $unpack ([IO.Path]::GetFileNameWithoutExtension($Archive))
    Assert-ConptyPackage $package $target
    $install = Join-Path $root "local install"
    $shell = (Get-Process -Id $PID).Path
    function Install([string]$Name, [int]$Expected) {
        $log = Join-Path $root "$Name.log"
        $savedPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = "Continue"
            & $shell -NoProfile -ExecutionPolicy Bypass -File "$package/install.ps1" -InstallDir "$install/bin" *> $log
            $code = $LASTEXITCODE
        } finally { $ErrorActionPreference = $savedPreference }
        Check (($code -eq 0) -eq ($Expected -eq 0)) "$Name exit=$code"
    }
    Install "install" 0
    foreach ($directory in @("bin", "libexec/rmux")) {
        foreach ($file in $manifest.files) {
            Check ((Hash "$install/$directory/$($file.name)") -ieq $file.sha256) "installed $directory/$($file.name)"
        }
    }
    Check ((Hash (Join-Path $install $manifest.license.path)) -ieq $manifest.license.sha256) "installed Microsoft license"
    $before = FileMap $install
    $held = Join-Path $root "held-package-dll"
    Move-Item -LiteralPath "$package/libexec/rmux/conpty.dll" -Destination $held
    try { Install "incomplete-install" 1 }
    finally { Move-Item -LiteralPath $held -Destination "$package/libexec/rmux/conpty.dll" }
    Check ((FileMap $install) -ceq $before) "incomplete pair leaves installation unchanged"
    # A real differing pre-upgrade runtime makes rollback observable.
    [IO.File]::WriteAllText("$install/bin/conpty.dll", "old runtime marker")
    $before = FileMap $install
    $savedFailure = $env:RMUX_INSTALL_TEST_FAIL_AT
    try {
        $env:RMUX_INSTALL_TEST_FAIL_AT = "after-copy-package"
        Install "rollback" 1
    } finally { $env:RMUX_INSTALL_TEST_FAIL_AT = $savedFailure }
    Check ((FileMap $install) -ceq $before) "rollback restores the entire previous runtime and package"
    Install "restore-valid-install" 0
    $metadata = Get-Content -LiteralPath "$package/share/rmux/artifact-metadata.json" -Raw | ConvertFrom-Json
    Write-ReproducibleWindowsArchive $package (Join-Path $root "repacked.zip") $metadata.generated_at_utc
    Check ((Hash $Archive) -eq (Hash (Join-Path $root "repacked.zip"))) "real archive reproduces byte for byte after extraction"
}
[ordered]@{ passed = $script:results.Count; assertions = $script:results; nupkg_sha256 = $inputHash } |
    ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $root "result.json")
