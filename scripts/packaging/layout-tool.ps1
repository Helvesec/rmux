# One way for the PowerShell packaging consumers to reach the layout
# descriptor.
#
# Every projection is line oriented, and Invoke-LayoutTool writes those lines
# to the pipeline one by one. A caller that wants them as a list wraps the
# call in @( ), which is the reading that stays correct for one line, none or
# many; a caller that wants a single value calls Get-LayoutLine, which says so
# and checks the count. Nothing indexes the answer: indexing a single string
# yields its first character, not its first line.
#
# Returning the array as one object instead would make @( ) produce a list of
# one element holding every line, which a [string[]] parameter then joins into
# one composite name. Handing the lines over one by one is what keeps both
# readings honest.

function Resolve-LayoutPython {
    $candidates = @()
    if (-not [string]::IsNullOrWhiteSpace($env:RMUX_PYTHON)) { $candidates += $env:RMUX_PYTHON }
    $candidates += @("python3", "python")
    foreach ($candidate in $candidates) {
        $command = Get-Command $candidate -ErrorAction SilentlyContinue
        if (-not $command) { continue }
        $version = & $command.Source "--version" 2>&1
        if ($LASTEXITCODE -eq 0 -and "$version" -match "^Python 3") { return $command.Source }
    }
    throw "a working python3 is required to project the package layout descriptor"
}

function Invoke-LayoutTool([string[]]$Arguments) {
    $python = Resolve-LayoutPython
    $tool = Join-Path $PSScriptRoot "layout_dispatch.py"
    $output = & $python $tool @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "package layout: $($output -join "`n")"
    }
    # The lines go to the pipeline as lines. @( ) at the call site turns them
    # into a list; Get-LayoutLine turns one of them into a value.
    foreach ($line in $output) { [string]$line }
}

function Get-LayoutLine([string[]]$Arguments) {
    $lines = @(Invoke-LayoutTool $Arguments |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    if ($lines.Count -ne 1) {
        throw ("package layout: {0} answered with {1} lines, expected exactly one" -f
            ($Arguments -join " "), $lines.Count)
    }
    return [string]$lines[0]
}
