[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('ason_decode', 'frame_decode', 'update_metadata', 'signed_update_metadata')]
    [string]$Target,

    [ValidateRange(1, 86400)]
    [int]$MaxTotalTime = 600,

    [ValidateRange(1, 1048576)]
    [int]$MaxLength = 65536,

    [ValidateRange(128, 32768)]
    [int]$RssLimitMb = 2048,

    [ValidateRange(1, 60)]
    [int]$InputTimeoutSeconds = 5,

    [string]$CorpusSource = '',

    [string]$EvidenceRoot = (Join-Path ([System.IO.Path]::GetTempPath()) 'ash-fuzz')
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$defaultCorpus = Join-Path $repositoryRoot "fuzz/corpus/$Target"
$corpusSourcePath = if ([string]::IsNullOrWhiteSpace($CorpusSource)) {
    $defaultCorpus
} else {
    $CorpusSource
}
if (-not (Test-Path -LiteralPath $corpusSourcePath -PathType Container)) {
    throw "missing fuzz corpus: $corpusSourcePath"
}
$corpusSourcePath = (Resolve-Path -LiteralPath $corpusSourcePath).Path

$targetRoot = Join-Path ([System.IO.Path]::GetFullPath($EvidenceRoot)) $Target
if (Test-Path -LiteralPath $targetRoot) {
    throw "refusing to overwrite fuzz evidence: $targetRoot"
}

$corpusRoot = Join-Path $targetRoot 'corpus'
$artifactRoot = Join-Path $targetRoot 'artifacts'
$logPath = Join-Path $targetRoot 'libfuzzer.log'
$summaryPath = Join-Path $targetRoot 'summary.json'
New-Item -ItemType Directory -Path $targetRoot, $artifactRoot -Force | Out-Null
Copy-Item -LiteralPath $corpusSourcePath -Destination $corpusRoot -Recurse

function Get-CorpusShape {
    param([Parameter(Mandatory = $true)][string]$Root)

    $entries = @(Get-ChildItem -LiteralPath $Root -Force -Recurse)
    if (@($entries | Where-Object {
                ($_.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
            }).Count -ne 0) {
        throw 'fuzz corpus must not contain links or reparse points'
    }
    $files = @($entries | Where-Object { -not $_.PSIsContainer })
    if ($files.Count -eq 0 -or $files.Count -gt 8192) {
        throw "fuzz corpus file count is outside 1..8192: $($files.Count)"
    }
    $bytes = [uint64]0
    foreach ($file in $files) {
        if ($file.Length -gt $MaxLength) {
            throw "fuzz corpus input exceeds $MaxLength bytes: $($file.FullName)"
        }
        $bytes += [uint64]$file.Length
    }
    if ($bytes -gt 134217728) {
        throw "fuzz corpus exceeds 128 MiB: $bytes bytes"
    }
    return [ordered]@{
        files = $files.Count
        bytes = $bytes
    }
}

$initialCorpus = Get-CorpusShape -Root $corpusRoot

$toolchain = 'nightly-2026-07-31'
$cargoFuzzVersion = (& cargo fuzz --version 2>$null | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $cargoFuzzVersion -ne 'cargo-fuzz 0.13.2') {
    throw "cargo-fuzz 0.13.2 is required; found: $cargoFuzzVersion"
}

$artifactPrefix = $artifactRoot + [System.IO.Path]::DirectorySeparatorChar
$arguments = @(
    "+$toolchain",
    'fuzz',
    'run',
    '--sanitizer',
    'address',
    $Target,
    $corpusRoot,
    '--',
    "-max_total_time=$MaxTotalTime",
    "-max_len=$MaxLength",
    "-rss_limit_mb=$RssLimitMb",
    "-timeout=$InputTimeoutSeconds",
    "-artifact_prefix=$artifactPrefix",
    '-print_final_stats=1'
)

$started = [DateTimeOffset]::UtcNow
$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$previousErrorAction = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
& cargo @arguments 2>&1 | Tee-Object -FilePath $logPath
$exitCode = $LASTEXITCODE
$ErrorActionPreference = $previousErrorAction
$stopwatch.Stop()
$finished = [DateTimeOffset]::UtcNow
$null = Get-CorpusShape -Root $corpusRoot

$statistics = [ordered]@{}
$libFuzzerSeed = $null
foreach ($line in Get-Content -LiteralPath $logPath) {
    if ($line -match '^INFO: Seed: ([0-9]+)') {
        $libFuzzerSeed = [uint64]$Matches[1]
    }
    if ($line -match '^stat::([^:]+):\s+([0-9]+)') {
        $statistics[$Matches[1]] = [uint64]$Matches[2]
    }
}

function Get-FileInventory {
    param([Parameter(Mandatory = $true)][string]$Root)

    if (-not (Test-Path -LiteralPath $Root -PathType Container)) {
        return @()
    }
    $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
    $rootPrefix = $rootFull + [System.IO.Path]::DirectorySeparatorChar
    $comparison = if ($env:OS -eq 'Windows_NT') {
        [System.StringComparison]::OrdinalIgnoreCase
    } else {
        [System.StringComparison]::Ordinal
    }
    return @(
        Get-ChildItem -LiteralPath $Root -Force -File -Recurse |
            Sort-Object FullName |
            ForEach-Object {
                $fileFull = [System.IO.Path]::GetFullPath($_.FullName)
                if (-not $fileFull.StartsWith($rootPrefix, $comparison)) {
                    throw "fuzz evidence escaped its root: $fileFull"
                }
                $relative = $fileFull.Substring($rootPrefix.Length).Replace('\', '/')
                [ordered]@{
                    path = $relative
                    bytes = [uint64]$_.Length
                    sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                }
            }
    )
}

$corpus = @(Get-FileInventory -Root $corpusRoot)
$artifacts = @(Get-FileInventory -Root $artifactRoot)
$sourceCommit = (& git -C $repositoryRoot rev-parse HEAD | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $sourceCommit -notmatch '^[0-9a-f]{40}$') {
    throw 'unable to bind fuzz evidence to the source commit'
}

$status = if (
    $exitCode -eq 0 -and
    $null -ne $libFuzzerSeed -and
    $statistics.Contains('number_of_executed_units')
) {
    'passed'
} elseif ($exitCode -eq 0) {
    'invalid-evidence'
} else {
    'failed'
}
$summary = [ordered]@{
    schema = 1
    target = $Target
    status = $status
    source_commit = $sourceCommit
    workflow_run_id = if ($env:GITHUB_RUN_ID) { $env:GITHUB_RUN_ID } else { $null }
    workflow_run_attempt = if ($env:GITHUB_RUN_ATTEMPT) { $env:GITHUB_RUN_ATTEMPT } else { $null }
    started_utc = $started.ToString('O')
    finished_utc = $finished.ToString('O')
    elapsed_milliseconds = [uint64]$stopwatch.ElapsedMilliseconds
    toolchain = $toolchain
    cargo_fuzz = $cargoFuzzVersion
    engine = 'libfuzzer'
    sanitizer = 'address'
    seed = $libFuzzerSeed
    command = [ordered]@{
        max_total_time_seconds = $MaxTotalTime
        max_input_bytes = $MaxLength
        rss_limit_mb = $RssLimitMb
        input_timeout_seconds = $InputTimeoutSeconds
    }
    initial_corpus = $initialCorpus
    exit_code = $exitCode
    statistics = $statistics
    corpus = $corpus
    artifacts = $artifacts
    log_sha256 = (Get-FileHash -LiteralPath $logPath -Algorithm SHA256).Hash.ToLowerInvariant()
}
$json = $summary | ConvertTo-Json -Depth 8
[System.IO.File]::WriteAllText(
    $summaryPath,
    $json + "`n",
    [System.Text.UTF8Encoding]::new($false)
)
Write-Output "fuzz_evidence=$summaryPath"

if ($exitCode -ne 0) {
    exit $exitCode
}
if ($status -ne 'passed') {
    throw 'libFuzzer completed without a seed or final execution statistics'
}
