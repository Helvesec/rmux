# Build/package tooling only. The accepted runtime loader never calls this file.
$script:ConptyManifestPath = Join-Path $PSScriptRoot "runtime.json"
$script:ConptyLicensePath = Join-Path $PSScriptRoot "LICENSE"

function Get-ConptyManifest([string]$Target) {
    $manifest = Get-Content -LiteralPath $script:ConptyManifestPath -Raw | ConvertFrom-Json
    if ($Target -cne $manifest.target) {
        throw "ConPTY packaging supports only $($manifest.target); target '$Target' is not qualified"
    }
    return $manifest
}

function Get-ConptySha256([string]$Path) {
    # Candidate child shells may not expose the Get-FileHash module command.
    # Use the same SHA-256 primitive without depending on module discovery.
    $stream = [IO.File]::OpenRead([IO.Path]::GetFullPath($Path))
    try {
        $sha256 = [Security.Cryptography.SHA256]::Create()
        try {
            return ([BitConverter]::ToString($sha256.ComputeHash($stream)) -replace '-', '').ToLowerInvariant()
        } finally { $sha256.Dispose() }
    } finally { $stream.Dispose() }
}

function Assert-ConptyHash([string]$Path, [string]$Expected) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf) -or
        (Get-ConptySha256 $Path) -ine $Expected) {
        throw "ConPTY SHA-256 mismatch or missing file: $Path"
    }
}

function Assert-ConptyMachine([string]$Path, [int]$Machine) {
    $reader = [IO.BinaryReader]::new([IO.File]::OpenRead($Path))
    try {
        if ($reader.ReadUInt16() -ne 0x5a4d) { throw "not a PE executable: $Path" }
        $reader.BaseStream.Position = 0x3c
        $offset = $reader.ReadUInt32()
        $reader.BaseStream.Position = $offset
        if ($reader.ReadUInt32() -ne 0x4550 -or $reader.ReadUInt16() -ne $Machine) {
            throw "ConPTY package PE architecture mismatch: $Path"
        }
    } finally { $reader.Dispose() }
}

function Assert-ConptySignature([string]$Path, [object]$Manifest, [object]$File) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne "Valid" -or
        $null -eq $signature.SignerCertificate -or
        $signature.SignerCertificate.Subject -cne $Manifest.publisher -or
        $signature.SignerCertificate.Thumbprint -ine $File.signer_thumbprint) {
        throw "ConPTY Microsoft Authenticode verification failed: $Path ($($signature.Status))"
    }
    $v = [Diagnostics.FileVersionInfo]::GetVersionInfo($Path)
    $version = "$($v.FileMajorPart).$($v.FileMinorPart).$($v.FileBuildPart).$($v.FilePrivatePart)"
    if ($version -cne $Manifest.file_version) { throw "ConPTY file version mismatch: $Path" }
}

function Copy-ConptyRuntime([string]$Target, [string]$Package, [string]$Stage) {
    $manifest = Get-ConptyManifest $Target
    Assert-ConptyHash $script:ConptyLicensePath $manifest.license.sha256
    # Never download or replace a caller-supplied archive, even on digest failure.
    # Otherwise acquire only the immutable, digest-pinned NuGet package at build time.
    $scratch = Join-Path (Split-Path -Parent $Stage) (".conpty-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $scratch | Out-Null
    try {
        if ([string]::IsNullOrWhiteSpace($Package)) {
            $Package = Join-Path $scratch "runtime.nupkg"
            $tls = [Net.ServicePointManager]::SecurityProtocol
            try {
                [Net.ServicePointManager]::SecurityProtocol = $tls -bor [Net.SecurityProtocolType]::Tls12
                Invoke-WebRequest -UseBasicParsing -Uri $manifest.url -OutFile $Package
            } finally { [Net.ServicePointManager]::SecurityProtocol = $tls }
        }
        Assert-ConptyHash $Package $manifest.sha256
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [IO.Compression.ZipFile]::OpenRead([IO.Path]::GetFullPath($Package))
        try {
            foreach ($file in $manifest.files) {
                # Extract only the two pinned entries, never arbitrary archive paths.
                $entries = @($zip.Entries | Where-Object { $_.FullName -ceq $file.entry })
                if ($entries.Count -ne 1) { throw "ConPTY archive entry missing or duplicate: $($file.entry)" }
                $path = Join-Path $scratch $file.name
                [IO.Compression.ZipFileExtensions]::ExtractToFile($entries[0], $path)
                Assert-ConptyHash $path $file.sha256
                Assert-ConptyMachine $path $manifest.pe_machine
                Assert-ConptySignature $path $manifest $file
            }
        } finally { $zip.Dispose() }
        foreach ($directory in @($Stage, (Join-Path $Stage "libexec/rmux"))) {
            foreach ($file in $manifest.files) {
                Copy-Item -LiteralPath (Join-Path $scratch $file.name) -Destination $directory
            }
        }
        $license = Join-Path $Stage $manifest.license.path
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $license) | Out-Null
        Copy-Item -LiteralPath $script:ConptyLicensePath -Destination $license
        Copy-Item -LiteralPath $script:ConptyManifestPath -Destination (Join-Path $Stage "share/rmux/conpty-runtime.json")
    } finally { Remove-Item -LiteralPath $scratch -Recurse -Force }
}

function Assert-ConptyPackage([string]$Root, [string]$Target) {
    $manifest = Get-ConptyManifest $Target
    Assert-ConptyHash (Join-Path $Root "share/rmux/conpty-runtime.json") `
        (Get-ConptySha256 $script:ConptyManifestPath)
    Assert-ConptyHash (Join-Path $Root $manifest.license.path) $manifest.license.sha256
    foreach ($directory in @($Root, (Join-Path $Root "libexec/rmux"))) {
        foreach ($file in $manifest.files) {
            $path = Join-Path $directory $file.name
            Assert-ConptyHash $path $file.sha256
            Assert-ConptyMachine $path $manifest.pe_machine
        }
    }
    # A label or a matching DLL cannot turn an unrelated executable into x64.
    foreach ($binary in @("rmux.exe", "rmux-daemon.exe", "libexec/rmux/rmux.exe")) {
        Assert-ConptyMachine (Join-Path $Root $binary) $manifest.pe_machine
    }
}
