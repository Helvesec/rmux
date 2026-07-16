param(
    [Parameter(Mandatory = $true)]
    [string]$Binary,
    [Parameter(Mandatory = $true)]
    [string]$HelperBinary,
    [Parameter(Mandatory = $true)]
    [string]$DaemonBinary
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Error "error: $Message"
    exit 1
}

function Find-Dumpbin {
    $inPath = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
    if ($inPath) {
        return $inPath.Source
    }

    $programFilesX86 = [Environment]::GetFolderPath("ProgramFilesX86")
    $vswhere = Join-Path $programFilesX86 "Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
        Fail "dumpbin.exe is not in PATH and vswhere.exe was not found"
    }

    $installation = (& $vswhere `
        -latest `
        -products "*" `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
        -property installationPath | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($installation)) {
        Fail "unable to locate Visual C++ tools with vswhere.exe"
    }

    $toolsRoot = Join-Path $installation "VC\Tools\MSVC"
    $toolsets = @(Get-ChildItem -LiteralPath $toolsRoot -Directory -ErrorAction SilentlyContinue |
        Sort-Object -Property Name -Descending)
    foreach ($toolset in $toolsets) {
        foreach ($relative in @(
            "bin\HostX64\x64\dumpbin.exe",
            "bin\HostX86\x64\dumpbin.exe",
            "bin\HostX86\x86\dumpbin.exe"
        )) {
            $candidate = Join-Path $toolset.FullName $relative
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                return $candidate
            }
        }
    }

    Fail "Visual C++ tools were found, but dumpbin.exe was not"
}

function Imported-Dlls([string]$Dumpbin, [string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Fail "Windows release binary was not found: $Path"
    }

    $output = & $Dumpbin /NOLOGO /DEPENDENTS $Path 2>&1
    if ($LASTEXITCODE -ne 0) {
        Fail "dumpbin.exe failed for $Path"
    }

    $imports = @($output |
        ForEach-Object { $_.ToString().Trim() } |
        Where-Object { $_ -match '^[A-Za-z0-9_.+-]+\.dll$' })
    if ($imports.Count -eq 0) {
        Fail "dumpbin.exe reported no DLL imports for $Path"
    }
    $imports
}

$dumpbin = Find-Dumpbin
$forbiddenRuntime = '^(VCRUNTIME|MSVCP|MSVCR|CONCRT).*\.dll$'
$binaries = @($Binary, $HelperBinary, $DaemonBinary)

foreach ($path in $binaries) {
    $imports = @(Imported-Dlls $dumpbin $path)
    $forbidden = @($imports | Where-Object { $_ -match $forbiddenRuntime })
    if ($forbidden.Count -gt 0) {
        Fail "$path imports redistributable MSVC runtime DLLs: $($forbidden -join ', ')"
    }
    Write-Output "windows-static-crt=$path imports=$($imports.Count)"
}

Write-Output "windows-static-crt=ok"
