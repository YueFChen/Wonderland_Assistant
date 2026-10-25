$ErrorActionPreference = 'Stop'
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
Push-Location $repoRoot
try {
    $hostLine = (rustc -vV | Select-String '^host: (.+)$')
    if ($LASTEXITCODE -ne 0) { throw 'Could not query the Rust host target.' }
    if (-not $hostLine) { throw 'Could not determine the Rust host target.' }
    $targetTriple = $hostLine.Matches.Groups[1].Value
    if (-not $targetTriple.EndsWith('-pc-windows-msvc')) {
        throw "The bundled wla sidecar currently supports Windows MSVC targets; found '$targetTriple'."
    }

    $metadataJson = cargo metadata --no-deps --format-version 1 --locked
    if ($LASTEXITCODE -ne 0) { throw 'Could not query the Cargo target directory.' }
    $targetRoot = ($metadataJson | ConvertFrom-Json).target_directory

    $previousSidecarFlag = $env:WONDERLAND_BUILD_WLA_SIDECAR
    try {
        $env:WONDERLAND_BUILD_WLA_SIDECAR = '1'
        pnpm run build:cli
        if ($LASTEXITCODE -ne 0) { throw 'The wla sidecar build failed.' }
    } finally {
        if ($null -eq $previousSidecarFlag) {
            Remove-Item Env:WONDERLAND_BUILD_WLA_SIDECAR -ErrorAction SilentlyContinue
        } else {
            $env:WONDERLAND_BUILD_WLA_SIDECAR = $previousSidecarFlag
        }
    }
    $source = Join-Path $targetRoot 'release\wla.exe'
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "The wla sidecar was not produced at '$source'."
    }
    $sidecarDirectory = Join-Path $repoRoot 'apps\desktop\src-tauri\binaries'
    New-Item -ItemType Directory -Force -Path $sidecarDirectory | Out-Null
    Copy-Item -LiteralPath $source -Destination (Join-Path $sidecarDirectory "wla-$targetTriple.exe") -Force

    pnpm run build:ui
    if ($LASTEXITCODE -ne 0) { throw 'The desktop UI build failed.' }
} finally {
    Pop-Location
}
