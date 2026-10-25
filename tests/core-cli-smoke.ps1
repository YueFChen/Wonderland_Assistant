param(
    [string]$Cli = (Join-Path $PSScriptRoot '../target/debug/deps/wla.exe'),
    [string]$OldCli = (Join-Path $PSScriptRoot '../target/w18-baseline-artifacts/wla.exe'),
    [string]$Packages = (Join-Path $PSScriptRoot '../target/w18-maintained-plugins'),
    [ValidatePattern('^[a-z0-9-]+$')]
    [string]$DrillPrefix = 'w18'
)
$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$drillRoot = Join-Path $repoRoot ("target/$DrillPrefix-cli-smoke-" + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($drillRoot) | Out-Null
$Cli = [IO.Path]::GetFullPath($Cli)
if (-not (Test-Path -LiteralPath $Cli -PathType Leaf)) { throw "Build the current wla first: $Cli" }
$expectedVersion = (Select-String -LiteralPath (Join-Path $repoRoot 'Cargo.toml') -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value
$cliVersion = (& $Cli --version).Trim()
if ($LASTEXITCODE -ne 0 -or $cliVersion -ne "wla $expectedVersion") { throw "CLI version mismatch: $cliVersion, expected wla $expectedVersion. Use the newly compiled CLI, not the copied historical sidecar." }
$utf8 = [Text.UTF8Encoding]::new($false)

function Write-Fixture([string]$path, [string]$text) {
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
    [IO.File]::WriteAllText($path, $text, $utf8)
}
function Run-Cli([string]$exe, [string[]]$arguments, [int]$expected = 0) {
    $start = [Diagnostics.ProcessStartInfo]::new($exe)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $isolatedTemp = Join-Path $drillRoot 'process-temp'
    [IO.Directory]::CreateDirectory($isolatedTemp) | Out-Null
    $start.Environment['TEMP'] = $isolatedTemp
    $start.Environment['TMP'] = $isolatedTemp
    foreach ($argument in $arguments) { $start.ArgumentList.Add($argument) }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $process = [Diagnostics.Process]::Start($start)
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(120000)) {
        $process.Kill($true)
        throw "CLI did not finish within 120 seconds."
    }
    $timer.Stop()
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()
    $peak = $process.PeakWorkingSet64
    $exitCode = $process.ExitCode
    $process.Dispose()
    if ($exitCode -ne $expected) { throw "CLI returned $exitCode, expected $expected. $stderr" }
    $json = if ($expected -eq 0) { $stdout | ConvertFrom-Json } else { $stderr | ConvertFrom-Json }
    return @{ response = $json; elapsedMs = $timer.Elapsed.TotalMilliseconds; peakWorkingSetBytes = $peak }
}
function Hash-Files([string]$root) {
    $hashes = @{}
    Get-ChildItem -LiteralPath $root -File -Recurse | ForEach-Object {
        $relative = [IO.Path]::GetRelativePath($root, $_.FullName)
        $hashes[$relative] = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
    }
    return $hashes
}

$defaultRoot = Join-Path $drillRoot 'default'
$activeRoot = Join-Path $drillRoot 'active'
[IO.Directory]::CreateDirectory($defaultRoot) | Out-Null
[IO.Directory]::CreateDirectory($activeRoot) | Out-Null
$locator = Join-Path $defaultRoot '.user-data-location.json'
Write-Fixture $locator (@{ active_path = $activeRoot; pending_path = $null } | ConvertTo-Json)
$oldManager = '{"schemaVersion":3,"enabledPlugins":{"comment_collector":true},"grantedCapabilities":{"comment_collector":[]},"activeVersions":{},"installationSources":{}}'
Write-Fixture (Join-Path $activeRoot 'plugin-manager.json') $oldManager
Write-Fixture (Join-Path $activeRoot 'settings.json') '{}'
Write-Fixture (Join-Path $activeRoot 'account/accounts.json') '{"accounts":[],"current_account":null}'
Write-Fixture (Join-Path $activeRoot 'plugin-data/comment_collector/opaque-fixture.bin') 'plugin-defined data retained verbatim'
Write-Fixture (Join-Path $activeRoot 'plugin-secrets/comment_collector/opaque-fixture.dpapi') 'synthetic opaque bytes; not a credential'
Write-Fixture (Join-Path $activeRoot 'account/credentials/opaque-fixture.bin') 'synthetic opaque bytes; not a credential'
Write-Fixture (Join-Path $activeRoot 'theme/fixture.png') 'synthetic image bytes'
Write-Fixture (Join-Path $activeRoot 'Wonderland Assistant/comment_collector/Exports/keep.txt') 'existing user export'

$oldPackage = Join-Path $repoRoot 'plugins/comment_collector/target/comment_collector-0.1.3-windows-x86_64.wplug'
if (-not (Test-Path -LiteralPath $oldPackage -PathType Leaf)) { throw "Old v2 package is required for preservation test." }
$packageRoot = Join-Path $activeRoot 'installed-plugins/comment_collector/0.1.3'
[IO.Directory]::CreateDirectory($packageRoot) | Out-Null
[IO.Compression.ZipFile]::ExtractToDirectory($oldPackage, $packageRoot)
$before = Hash-Files $activeRoot
$locatorBefore = (Get-FileHash -LiteralPath $locator -Algorithm SHA256).Hash
$backupRoot = Join-Path $drillRoot 'backup'
Copy-Item -LiteralPath $activeRoot -Destination $backupRoot -Recurse
Copy-Item -LiteralPath $locator -Destination (Join-Path $drillRoot 'locator.backup')
$status = Run-Cli $Cli @('--data-dir', $defaultRoot, 'core', 'status')
if (-not $status.response.ok -or $status.response.data.desktopRunning) { throw 'Headless status contract failed.' }
$plugins = Run-Cli $Cli @('--data-dir', $defaultRoot, 'plugins', 'list')
$legacy = @($plugins.response.data.plugins | Where-Object { $_.manifest.id -eq 'comment_collector' })
if ($legacy.Count -ne 1 -or $legacy[0].installation -ne 'incompatible') { throw 'Old v2 package was not retained as incompatible.' }
$configPath = Join-Path $activeRoot 'plugin-manager.json'
$config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
if ($config.schemaVersion -ne 4 -or $config.PSObject.Properties.Name -contains 'grantedCapabilities') { throw 'Core config upgrade failed.' }
$archives = @(Get-ChildItem -LiteralPath $activeRoot -Filter 'plugin-manager.json.pre-v4-*.bak')
if ($archives.Count -ne 1 -or [IO.File]::ReadAllText($archives[0].FullName) -ne $oldManager) { throw 'Original config backup was not exact.' }
foreach ($relative in $before.Keys) {
    if ($relative -eq 'plugin-manager.json') { continue }
    if ((Get-FileHash -LiteralPath (Join-Path $activeRoot $relative) -Algorithm SHA256).Hash -ne $before[$relative]) {
        throw "Persistent file changed during config upgrade: $relative"
    }
}
if ((Get-FileHash -LiteralPath $locator -Algorithm SHA256).Hash -ne $locatorBefore) { throw 'Custom locator changed.' }

$invalid = Join-Path $drillRoot 'invalid-config'
Write-Fixture (Join-Path $invalid 'plugin-manager.json') '{broken'
$invalidResult = Run-Cli $Cli @('--data-dir', $invalid, 'core', 'status') 1
if ($invalidResult.response.error.code -ne 'INVALID_CONFIG' -or [IO.File]::ReadAllText((Join-Path $invalid 'plugin-manager.json')) -ne '{broken') { throw 'Invalid config was rewritten.' }
$missing = Join-Path $drillRoot 'missing-active'
$missingLocator = Join-Path $missing '.user-data-location.json'
Write-Fixture $missingLocator (@{ active_path = (Join-Path $drillRoot 'absent'); pending_path = $null } | ConvertTo-Json)
$missingHash = (Get-FileHash -LiteralPath $missingLocator -Algorithm SHA256).Hash
$missingResult = Run-Cli $Cli @('--data-dir', $missing, 'core', 'status') 1
if ($missingResult.response.error.code -ne 'DATA_DIR_UNAVAILABLE' -or (Get-FileHash -LiteralPath $missingLocator -Algorithm SHA256).Hash -ne $missingHash -or (Test-Path -LiteralPath (Join-Path $drillRoot 'absent'))) { throw 'Unavailable custom root fell back or was created.' }
$uiResult = Run-Cli $Cli @('--data-dir', $defaultRoot, 'ui', 'state') 1
if ($uiResult.response.error.code -ne 'DESKTOP_NOT_RUNNING') { throw 'CLI launched an implicit UI.' }
$unknown = Run-Cli $Cli @('--data-dir', (Join-Path $drillRoot 'unknown'), 'plugins', 'permissions', 'list', 'x') 2
if ($unknown.response.error.code -ne 'INVALID_REQUEST') { throw 'Removed grant command remained available.' }

$maintained = @()
$readMethods = @{
    comment_collector = 'archives'; genshin_ugc = 'get_info'; knowledge_library = 'status'
    my_wonderland = 'account_snapshot'; rich_editor = 'load'; template_plugin = 'get_info'
    translator = 'provider_profiles'; wa_bookmarks = 'get_info'
}
if (-not (Test-Path -LiteralPath $Packages -PathType Container)) { throw 'Build all eight maintained plugin packages before this drill.' }
if (@(Get-ChildItem -LiteralPath $Packages -Directory).Count -ne 8) { throw 'Expected exactly eight maintained packages.' }
    foreach ($source in Get-ChildItem -LiteralPath $Packages -Directory) {
        $packageFile = Join-Path $source.FullName 'candidate.wplug'
        $sourcePath = if (Test-Path -LiteralPath $packageFile -PathType Leaf) { $packageFile } else { $source.FullName }
        $profile = Join-Path $drillRoot ("plugin-" + $source.Name)
        $installed = Run-Cli $Cli @('--data-dir', $profile, 'plugins', 'install', $sourcePath, '--yes')
        $checked = Run-Cli $Cli @('--data-dir', $profile, 'plugins', 'ui-check', $source.Name)
        if (-not $checked.response.data.ok) { throw "UI check failed: $($source.Name)" }
        $called = Run-Cli $Cli @('--data-dir', $profile, 'plugins', 'call', $source.Name, $readMethods[$source.Name], '--input-json', '{}', '--yes')
        if (-not $called.response.ok) { throw "Headless plugin call failed: $($source.Name)" }
        if ($source.Name -eq 'wa_bookmarks') {
            $exported = Run-Cli $Cli @('--data-dir', $profile, 'plugins', 'call', $source.Name, 'export_json', '--input-json', '{}', '--yes')
            $receipt = $exported.response.data.result
            if ($receipt.kind -ne 'local' -or $receipt.state -ne 'saved' -or -not (Test-Path -LiteralPath $receipt.path -PathType Leaf)) { throw 'Headless export did not return a saved real local file.' }
            if ((Get-Item -LiteralPath $receipt.path).Length -ne $receipt.size) { throw 'Export receipt size mismatch.' }
        }
        if ($source.Name -eq 'translator') {
            $interactive = Run-Cli $Cli @('--data-dir', $profile, 'plugins', 'call', $source.Name, 'choose_file', '--input-json', '{}', '--yes') 1
            if ($interactive.response.error.code -ne 'INTERACTION_UNAVAILABLE') { throw 'Headless interaction did not fail explicitly.' }
        }
        $data = Join-Path $profile ("plugin-data/" + $source.Name + "/preserve.bin")
        $secret = Join-Path $profile ("plugin-secrets/" + $source.Name + "/preserve.dpapi")
        Write-Fixture $data 'persisted plugin data'
        Write-Fixture $secret 'opaque plugin secret'
        $removed = Run-Cli $Cli @('--data-dir', $profile, 'plugins', 'uninstall', $source.Name, '--yes')
        if (-not (Test-Path -LiteralPath $data) -or -not (Test-Path -LiteralPath $secret)) { throw 'Default uninstall lost plugin data or secrets.' }
        $reinstalled = Run-Cli $Cli @('--data-dir', $profile, 'plugins', 'install', $sourcePath, '--yes')
        $deleted = Run-Cli $Cli @('--data-dir', $profile, 'plugins', 'uninstall', $source.Name, '--yes', '--remove-data')
        if ((Test-Path -LiteralPath $data) -or (Test-Path -LiteralPath $secret)) { throw 'Explicit data removal did not remove both stores.' }
        $maintained += @{ id = $source.Name; installed = $true; uiCheck = $true; headlessCall = $readMethods[$source.Name]; keptByDefault = $true; explicitDataAndSecretsRemoval = $true }
    }

# Restore only Core control records. Plugin data/export files and post-upgrade changes stay in place.
Copy-Item -LiteralPath (Join-Path $backupRoot 'plugin-manager.json') -Destination $configPath -Force
Copy-Item -LiteralPath (Join-Path $drillRoot 'locator.backup') -Destination $locator -Force
foreach ($relative in $before.Keys) {
    if ((Get-FileHash -LiteralPath (Join-Path $activeRoot $relative) -Algorithm SHA256).Hash -ne $before[$relative]) { throw "Rollback hash mismatch: $relative" }
}
$oldRun = $null
if (-not [string]::IsNullOrWhiteSpace($OldCli) -and (Test-Path -LiteralPath $OldCli -PathType Leaf)) {
    $savedOldCli = Join-Path $drillRoot 'old-wla.exe'
    Copy-Item -LiteralPath $OldCli -Destination $savedOldCli
    $oldRun = Run-Cli $savedOldCli @('--data-dir', $defaultRoot, 'core', 'status')
    if (-not $oldRun.response.ok) { throw 'Old Core could not reopen restored controls.' }
    $oldPlugins = Run-Cli $savedOldCli @('--data-dir', $defaultRoot, 'plugins', 'list')
    $oldLegacy = @($oldPlugins.response.data.plugins | Where-Object { $_.manifest.id -eq 'comment_collector' })
    if ($oldLegacy.Count -ne 1 -or $oldLegacy[0].installation -ne 'installed') { throw 'Restored old Core could not recognize its original v2 package.' }
    foreach ($relative in $before.Keys) {
        if ($relative -eq 'plugin-manager.json') { continue }
        if ((Get-FileHash -LiteralPath (Join-Path $activeRoot $relative) -Algorithm SHA256).Hash -ne $before[$relative]) { throw "Old Core rollback changed persistent data: $relative" }
    }
}
$result = @{
    currentCli = $Cli; currentVersion = $cliVersion; fixtureRoot = $drillRoot; persistedFiles = $before.Count
    dataHashesUnchanged = $true; configBackupExact = $true; customLocatorPreserved = $true
    oldPackageIncompatible = $true; corruptConfigRetained = $true; unavailableCustomRootRetained = $true
    headlessLocalFileSaved = $true; headlessInteractionRejected = $true
    noImplicitUi = $true; removedGrantsUnavailable = $true; rollbackHashesMatch = $true
    maintainedPlugins = $maintained; oldCoreReopened = ($null -ne $oldRun)
    oldVersion = if ($null -ne $oldRun) { (& $savedOldCli --version).Trim() } else { $null }
    currentHeadlessStatus = @{ elapsedMs = $status.elapsedMs; peakWorkingSetBytes = $status.peakWorkingSetBytes }
    oldHeadlessStatus = if ($null -ne $oldRun) { @{ elapsedMs = $oldRun.elapsedMs; peakWorkingSetBytes = $oldRun.peakWorkingSetBytes } } else { $null }
    limitations = @('Single CLI launch is not desktop startup/idle performance.', 'Synthetic opaque credential files are not a DPAPI decryption test.', 'Browser byte delivery is not confirmed user disk saving.', 'No installer, signed release, or real user profile is changed.')
}
$result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $drillRoot 'verification.json') -Encoding utf8NoBOM
$result | ConvertTo-Json -Depth 10
