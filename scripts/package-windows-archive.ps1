function Write-ReproducibleWindowsArchive([string]$Stage, [string]$Archive, [string]$CommitTime) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    # ZIP stores wall-clock seconds / 2, without a timezone. Use UTC explicitly.
    $utc = [DateTimeOffset]::Parse($CommitTime).UtcDateTime
    if ($utc.Year -lt 1980 -or $utc.Year -gt 2107) { throw "commit time outside ZIP range" }
    $stamp = [DateTimeOffset]::new($utc.Year, $utc.Month, $utc.Day, $utc.Hour, $utc.Minute,
        ($utc.Second - ($utc.Second % 2)), [TimeSpan]::Zero)
    $root = [IO.Path]::GetFullPath($Stage)
    $prefix = [IO.Path]::GetFileName($root)
    [string[]]$files = @(Get-ChildItem -LiteralPath $root -Recurse -File | ForEach-Object {
        $_.FullName.Substring($root.Length + 1).Replace('\', '/')
    })
    [Array]::Sort($files, [StringComparer]::Ordinal)
    $zip = [IO.Compression.ZipFile]::Open($Archive, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($file in $files) {
            $entry = $zip.CreateEntry("$prefix/$file", [IO.Compression.CompressionLevel]::Optimal)
            $entry.LastWriteTime = $stamp
            $entry.ExternalAttributes = 0
            $inputFile = [IO.File]::OpenRead((Join-Path $root $file))
            $output = $entry.Open()
            try { $inputFile.CopyTo($output) } finally { $output.Dispose(); $inputFile.Dispose() }
        }
    } finally { $zip.Dispose() }
}
