$ErrorActionPreference = 'Stop'
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
Push-Location $repoRoot
try {
    $hostLine = (rustc -vV | Select-String '^host: (.+)$')
    if (-not $hostLine) { throw 'Could not determine the Rust host target.' }
    $targetTriple = $hostLine.Matches.Groups[1].Value
    if (-not $targetTriple.EndsWith('-pc-windows-msvc')) {
        throw "The bundled wla sidecar currently supports Windows MSVC targets; found '$targetTriple'."
    }

    $previousSidecarFlag = $env:WONDERLAND_BUILD_WLA_SIDECAR
    $env:WONDERLAND_BUILD_WLA_SIDECAR = '1'
    cargo build --locked --release -p wonderland-desktop --bin wla
    if ($null -eq $previousSidecarFlag) {
        Remove-Item Env:WONDERLAND_BUILD_WLA_SIDECAR -ErrorAction SilentlyContinue
    } else {
        $env:WONDERLAND_BUILD_WLA_SIDECAR = $previousSidecarFlag
    }
    if ($LASTEXITCODE -ne 0) { throw 'The wla sidecar build failed.' }

    $targetRoot = if ($env:CARGO_TARGET_DIR) {
        [System.IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)
    } else {
        Join-Path $repoRoot 'target'
    }
    $source = Join-Path $targetRoot 'release\wla.exe'
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "The wla sidecar was not produced at '$source'."
    }
    $sidecarDirectory = Join-Path $repoRoot 'apps\desktop\src-tauri\binaries'
    New-Item -ItemType Directory -Force -Path $sidecarDirectory | Out-Null
    Copy-Item -LiteralPath $source -Destination (Join-Path $sidecarDirectory "wla-$targetTriple.exe") -Force

    pnpm --dir apps/desktop/ui run build
    if ($LASTEXITCODE -ne 0) { throw 'The desktop UI build failed.' }
} finally {
    Pop-Location
}
