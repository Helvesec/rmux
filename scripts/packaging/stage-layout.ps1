# Staging a package from the one layout descriptor, for the PowerShell
# builders. A builder passes the source roots it has and copies what the plan
# gives it, where the plan puts it, so a format keeps no second list of its
# own sources.

function Copy-LayoutStagePlan {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Plan,
        [Parameter(Mandatory = $true)][string]$Stage
    )

    $staged = @()
    foreach ($line in $Plan) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $fields = $line -split "`t"
        if ($fields.Count -lt 4) { throw "invalid package stage plan line: $line" }
        $source = $fields[0]
        $destination = $fields[1]
        $transform = $fields[3]
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            throw "package source is missing: $source"
        }
        $target = Join-Path $Stage $destination
        $parent = Split-Path -Parent $target
        if (-not [string]::IsNullOrEmpty($parent)) {
            New-Item -ItemType Directory -Force -Path $parent | Out-Null
        }
        switch ($transform) {
            "copy" { Copy-Item -LiteralPath $source -Destination $target -Force }
            default { throw "unknown package layout transform: $transform" }
        }
        $staged += $destination
    }
    if ($staged.Count -eq 0) { throw "the package stage plan is empty" }
    return $staged
}

function Copy-LayoutSourceFiles {
    <#
        The files a builder must have on hand before it stages a kind of
        source, copied between two directories under the names the descriptor
        gives them. The builder projects those names; it never spells them
        out, so renaming a source in the descriptor renames what is prepared
        here.
    #>
    param(
        [Parameter(Mandatory = $true)][string[]]$Names,
        [Parameter(Mandatory = $true)][string]$From,
        [Parameter(Mandatory = $true)][string]$To,
        [Parameter(Mandatory = $true)][string]$Missing
    )

    if ($Names.Count -eq 0) { throw "no source file to prepare" }
    New-Item -ItemType Directory -Force -Path $To | Out-Null
    foreach ($name in $Names) {
        # Each array element is one basename, including any internal spaces.
        # Reject path components without inferring array shape from the name.
        if ([string]::IsNullOrWhiteSpace($name) -or $name -in @('.', '..') -or
            $name -match '[\\/]') {
            throw "not a source file name: $name"
        }
        $source = Join-Path $From $name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            throw "${Missing}: $source"
        }
        Copy-Item -LiteralPath $source -Destination (Join-Path $To $name) -Force
    }
    return $Names
}

function Copy-LayoutGeneratedSources {
    <#
        The completion sources a builder prepares from what the generator
        really wrote.

        The generator reports one line per file: the shell it completes, a
        tab, and the path it produced. The descriptor reports one line per
        packaged completion: the same shell, a tab, and the name this package
        expects. Joining them on the shell is what connects a renamed source
        to the file the generator actually produces, without either side
        holding a copy of the other's names.
    #>
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Reported,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Expected,
        [Parameter(Mandatory = $true)][string]$To
    )

    $generated = @{}
    foreach ($line in $Reported) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $fields = $line -split "`t"
        if ($fields.Count -ne 2) {
            throw "the completion generator reported a line this packaging cannot read: $line"
        }
        if ($generated.ContainsKey($fields[0])) {
            throw "the completion generator reported $($fields[0]) twice"
        }
        $generated[$fields[0]] = $fields[1]
    }

    $prepared = @()
    foreach ($line in $Expected) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $fields = $line -split "`t"
        if ($fields.Count -ne 2) { throw "not an expected source line: $line" }
        $shell = $fields[0]
        $name = $fields[1]
        if ([string]::IsNullOrWhiteSpace($name) -or $name -in @('.', '..') -or
            $name -match '[\\/]') { throw "not a source file name: $name" }
        if (-not $generated.ContainsKey($shell)) {
            $reportedShells = (@($generated.Keys) | Sort-Object) -join ", "
            throw ("the completion generator wrote no $shell completion; it reported: $reportedShells")
        }
        $source = $generated[$shell]
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            throw "the completion generator reported a file it did not write: $source"
        }
        New-Item -ItemType Directory -Force -Path $To | Out-Null
        Copy-Item -LiteralPath $source -Destination (Join-Path $To $name) -Force
        $prepared += $name
    }
    if ($prepared.Count -eq 0) { throw "no completion source to prepare" }
    return $prepared
}
