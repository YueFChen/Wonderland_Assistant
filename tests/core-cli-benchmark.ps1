param(
    [string]$Cli = (Join-Path $PSScriptRoot '../target/w18-current-artifacts/wla.exe'),
    [string]$BaselineCli = (Join-Path $PSScriptRoot '../target/w18-baseline-artifacts/wla.exe'),
    [ValidateRange(1, 51)][int]$Iterations = 7
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$Cli = [IO.Path]::GetFullPath($Cli)
$BaselineCli = [IO.Path]::GetFullPath($BaselineCli)
$utf8 = [Text.UTF8Encoding]::new($false)
$pollIntervalMs = 5
$timeoutMs = 120000

function Binary-Info([string]$path) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "CLI executable is missing: $path" }
    $file = Get-Item -LiteralPath $path
    return [ordered]@{
        path = $path
        sizeBytes = $file.Length
        sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        productVersion = $file.VersionInfo.ProductVersion
    }
}

function Median([double[]]$values) {
    if ($values.Count -eq 0) { return $null }
    $sorted = @($values | Sort-Object)
    $middle = [int][Math]::Floor($sorted.Count / 2)
    if ($sorted.Count % 2 -eq 1) { return $sorted[$middle] }
    return ($sorted[$middle - 1] + $sorted[$middle]) / 2
}

# Hash reads also warm executable file caches; this is deliberately not a cold-cache benchmark.
$currentInfo = Binary-Info $Cli
$baselineInfo = Binary-Info $BaselineCli
$head = (& git -C $repoRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify the repository HEAD.' }
$baselineMetadataPath = Join-Path ([IO.Path]::GetDirectoryName($BaselineCli)) 'baseline.json'
$baselineMetadata = Get-Content -LiteralPath $baselineMetadataPath -Raw | ConvertFrom-Json
if ($baselineMetadata.commit -notmatch '^[a-fA-F0-9]{40}$') {
    throw 'Baseline metadata must identify a full 40-character commit SHA.'
}
& git -C $repoRoot merge-base --is-ancestor $baselineMetadata.commit $head
if ($LASTEXITCODE -ne 0) {
    throw 'Baseline commit must be an ancestor of the current HEAD.'
}
if ($baselineMetadata.sha256 -ne $baselineInfo.sha256) {
    throw 'Baseline metadata does not identify this executable.'
}
if ($baselineMetadata.profile -ne 'debug' -or $baselineMetadata.features -ne 'default' -or $baselineMetadata.coreVersion -ne '0.1.9') {
    throw 'Expected a debug/default Core 0.1.9 baseline.'
}
foreach ($info in @($currentInfo, $baselineInfo)) {
    if ($info.productVersion -ne '0.1.9') { throw "Expected Core 0.1.9: $($info.path)" }
}

$benchmarkRoot = Join-Path $repoRoot ('target/w18-cli-benchmark-' + [Guid]::NewGuid().ToString('N'))
if (Test-Path -LiteralPath $benchmarkRoot) { throw "Fresh benchmark directory already exists: $benchmarkRoot" }
[IO.Directory]::CreateDirectory($benchmarkRoot) | Out-Null

function Run-Status([string]$label, [string]$exe, [int]$iteration) {
    $runRoot = Join-Path $benchmarkRoot ("{0:D2}-{1}" -f $iteration, $label)
    $profile = Join-Path $runRoot 'profile'
    $processTemp = Join-Path $runRoot 'process-temp'
    [IO.Directory]::CreateDirectory($profile) | Out-Null
    [IO.Directory]::CreateDirectory($processTemp) | Out-Null
    if (@(Get-ChildItem -LiteralPath $profile -Force).Count -ne 0 -or @(Get-ChildItem -LiteralPath $processTemp -Force).Count -ne 0) {
        throw "Run directories are not empty: $runRoot"
    }
    $start = [Diagnostics.ProcessStartInfo]::new($exe)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = $runRoot
    # Both current and legacy endpoint discovery use the child's isolated temp directory.
    $start.Environment['TEMP'] = $processTemp
    $start.Environment['TMP'] = $processTemp
    foreach ($argument in @('--data-dir', $profile, 'core', 'status')) { $start.ArgumentList.Add($argument) }

    $peak = $null
    $liveSamples = 0
    $sampleErrors = 0
    $firstSampleError = $null
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $process = [Diagnostics.Process]::Start($start)
    try {
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        while ($true) {
            $process.Refresh()
            if ($process.HasExited) { break }
            try {
                # Never query memory after exit: some runtimes then return no useful value.
                $sample = $process.PeakWorkingSet64
                if ($null -ne $sample -and $sample -gt 0) {
                    $liveSamples += 1
                    if ($null -eq $peak -or $sample -gt $peak) { $peak = $sample }
                }
            } catch {
                $sampleErrors += 1
                if ($null -eq $firstSampleError) { $firstSampleError = $_.Exception.Message }
            }
            if ($process.WaitForExit($pollIntervalMs)) { break }
            if ($timer.Elapsed.TotalMilliseconds -ge $timeoutMs) {
                $process.Kill($true)
                $process.WaitForExit()
                throw "CLI did not finish within $timeoutMs ms: $label iteration $iteration"
            }
        }
        $timer.Stop()
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        $exitCode = $process.ExitCode
        [IO.File]::WriteAllText((Join-Path $runRoot 'stdout.json'), $stdout, $utf8)
        [IO.File]::WriteAllText((Join-Path $runRoot 'stderr.txt'), $stderr, $utf8)
        if ($exitCode -ne 0) { throw "CLI returned $exitCode in $runRoot. $stderr" }
        $response = $stdout | ConvertFrom-Json
        if ($response.ok -ne $true -or $response.data.desktopRunning -ne $false -or $response.data.version -ne '0.1.9') {
            throw "Isolated Core status contract failed: $runRoot"
        }
        return [ordered]@{
            label = $label
            iteration = $iteration
            command = @('--data-dir', $profile, 'core', 'status')
            profileDirectory = $profile
            childTempDirectory = $processTemp
            elapsedMs = [Math]::Round($timer.Elapsed.TotalMilliseconds, 3)
            peakWorkingSetBytes = $peak
            peakMemoryStatus = if ($null -eq $peak) { 'unavailable' } else { 'observed_while_running' }
            liveMemorySamples = $liveSamples
            memorySampleErrors = $sampleErrors
            firstMemorySampleError = $firstSampleError
            exitCode = $exitCode
            ok = $response.ok
            desktopRunning = $response.data.desktopRunning
            version = $response.data.version
        }
    } finally {
        $timer.Stop()
        $process.Dispose()
    }
}

function Summarize([object[]]$runs) {
    $elapsed = @($runs | ForEach-Object { $_.elapsedMs })
    $peaks = @($runs | ForEach-Object { $_.peakWorkingSetBytes })
    $availablePeaks = @($runs | Where-Object { $null -ne $_.peakWorkingSetBytes } | ForEach-Object { $_.peakWorkingSetBytes })
    return [ordered]@{
        runs = $runs.Count
        elapsedMs = $elapsed
        medianElapsedMs = Median $elapsed
        peakWorkingSetBytes = $peaks
        availablePeakSamples = $availablePeaks.Count
        medianObservedPeakWorkingSetBytes = Median $availablePeaks
        peakMemoryStatus = if ($availablePeaks.Count -eq 0) { 'unavailable' } elseif ($availablePeaks.Count -eq $runs.Count) { 'observed_while_running' } else { 'partially_available' }
    }
}

$runs = @()
try {
    for ($iteration = 1; $iteration -le $Iterations; $iteration += 1) {
        $runs += Run-Status 'current' $Cli $iteration
        $runs += Run-Status 'baseline' $BaselineCli $iteration
    }
    $result = [ordered]@{
        schemaVersion = 1
        measuredAtUtc = [DateTimeOffset]::UtcNow.ToString('o')
        artifactDirectory = $benchmarkRoot
        sourceHead = $head
        baselineMetadataPath = $baselineMetadataPath
        baselineMetadata = $baselineMetadata
        binaries = @{ current = $currentInfo; baseline = $baselineInfo }
        method = @{
            command = 'core status'
            buildProfile = 'debug'
            features = 'default'
            coreVersion = '0.1.9'
            repetitionsPerBinary = $Iterations
            order = 'current, baseline repeated alternately'
            elapsed = 'Stopwatch immediately before Process.Start through observed process exit; stdout parsing excluded'
            memory = 'Process.Refresh + PeakWorkingSet64 while running, with WaitForExit(5 ms) between polls; unavailable values remain null'
            pollIntervalMs = $pollIntervalMs
            profiles = 'A separate fresh empty --data-dir for every child'
            endpoints = 'A separate child TEMP/TMP for every run; actual desktop endpoint is never read'
        }
        summary = @{
            current = Summarize @($runs | Where-Object { $_.label -eq 'current' })
            baseline = Summarize @($runs | Where-Object { $_.label -eq 'baseline' })
        }
        runs = $runs
        limitations = @(
            'Debug process startup-to-exit with warm OS executable caches, not desktop cold startup or idle performance.',
            'A small alternating sample on one machine; scheduler, antivirus and cache variation are not controlled.',
            'Live polling reports an observed process peak; exit races or sub-poll transients can be missed.',
            'Executable bytes and SHA256 are single-binary measurements, not installer or installed-product size.',
            'No actual desktop endpoint, user profile, installer, release artifact or old benchmark directory is changed.'
        )
    }
    $resultPath = Join-Path $benchmarkRoot 'benchmark.json'
    [IO.File]::WriteAllText($resultPath, ($result | ConvertTo-Json -Depth 12), $utf8)
    [ordered]@{ artifactPath = $resultPath; summary = $result.summary; binaries = $result.binaries } | ConvertTo-Json -Depth 6
} catch {
    [IO.File]::WriteAllText((Join-Path $benchmarkRoot 'failure.json'), (@{ error = $_.Exception.Message; completedRuns = $runs } | ConvertTo-Json -Depth 10), $utf8)
    throw
}
