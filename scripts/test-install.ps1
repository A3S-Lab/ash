[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Binary
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$installer = Join-Path $repositoryRoot 'install.ps1'
$powershell = (Get-Command powershell.exe -ErrorAction Stop).Source
$binaryPath = [IO.Path]::GetFullPath($Binary)
$unicodeSuffix = [string][char]0x7A7A + [string][char]0x683C
$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ('ash installer smoke ' + $unicodeSuffix + ' ' + [Guid]::NewGuid().ToString('N'))

function Invoke-Installer([string[]]$Arguments, [int]$ExpectedExitCode) {
    $processArguments = @(
        '-NoLogo',
        '-NoProfile',
        '-NonInteractive',
        '-ExecutionPolicy', 'Bypass',
        '-File', $installer
    ) + $Arguments
    $previousPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $output = @(& $powershell @processArguments 2>&1)
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $previousPreference
    }
    if ($exitCode -ne $ExpectedExitCode) {
        throw "installer exit $exitCode, expected $ExpectedExitCode; output=$($output -join '|')"
    }
    return $output
}

try {
    [IO.Directory]::CreateDirectory($temporaryRoot) | Out-Null
    $package = Join-Path $temporaryRoot 'package'
    [IO.Directory]::CreateDirectory($package) | Out-Null
    [IO.File]::Copy($binaryPath, (Join-Path $package 'ash.exe'))
    [IO.File]::Copy((Join-Path $repositoryRoot 'LICENSE'), (Join-Path $package 'LICENSE'))
    [IO.File]::Copy((Join-Path $repositoryRoot 'THIRD-PARTY-LICENSES'), (Join-Path $package 'THIRD-PARTY-LICENSES'))

    $metadata = @{}
    foreach ($line in @(& $binaryPath --build-info)) {
        if ($line -match '^([vtpa]):(.+)$') { $metadata[$Matches[1]] = $Matches[2] }
    }
    Assert-True ($metadata.t -match 'windows-msvc$') 'test binary is not a Windows MSVC build'
    $binaryHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $binaryPath).Hash.ToLowerInvariant()
    $release = [ordered]@{
        schema = 1
        product = 'ash'
        version = $metadata.v
        target = $metadata.t
        protocol = $metadata.p
        ason = $metadata.a
        commit = 'installer-smoke'
        build = 'local'
        binary_sha256 = $binaryHash
    }
    $releaseJson = $release | ConvertTo-Json -Compress
    [IO.File]::WriteAllText(
        (Join-Path $package 'release.json'),
        $releaseJson + "`n",
        [Text.UTF8Encoding]::new($false)
    )

    $archive = Join-Path $temporaryRoot ("ash-$($metadata.t).zip")
    Compress-Archive -LiteralPath @(
        (Join-Path $package 'ash.exe'),
        (Join-Path $package 'LICENSE'),
        (Join-Path $package 'THIRD-PARTY-LICENSES'),
        (Join-Path $package 'release.json')
    ) -DestinationPath $archive
    $archiveHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash

    $prefix = Join-Path $temporaryRoot ('prefix ' + $unicodeSuffix)
    $binDirectory = Join-Path $temporaryRoot ('bin ' + $unicodeSuffix)
    $installArguments = @(
        '-Archive', $archive,
        '-Sha256', $archiveHash,
        '-Prefix', $prefix,
        '-BinDir', $binDirectory,
        '-NoPath'
    )
    $fresh = Invoke-Installer $installArguments 0
    Assert-True (($fresh -join "`n") -match '^s:0') 'fresh install did not emit success ASON'
    $launcher = Join-Path $binDirectory 'ash.exe'
    Assert-True ([IO.File]::Exists($launcher)) 'launcher was not installed'
    Assert-True ((& $launcher --build-info) -contains "v:$($metadata.v)") 'launcher version mismatch'
    Assert-True ([IO.File]::Exists((Join-Path $prefix 'install-receipt.json'))) 'receipt was not installed'

    Invoke-Installer $installArguments 0 | Out-Null
    Invoke-Installer ($installArguments + '-Force') 0 | Out-Null
    Assert-True ((& $launcher --build-info) -contains "t:$($metadata.t)") 'forced reinstall changed target'

    $badPrefix = Join-Path $temporaryRoot 'bad checksum'
    $badBin = Join-Path $temporaryRoot 'bad checksum bin'
    $badChecksum = '0' * 64
    $rejected = Invoke-Installer @(
        '-Archive', $archive,
        '-Sha256', $badChecksum,
        '-Prefix', $badPrefix,
        '-BinDir', $badBin,
        '-NoPath'
    ) 1
    Assert-True (($rejected -join "`n") -match '29') 'checksum rejection did not expose stable code 29'
    Assert-True (-not [IO.File]::Exists((Join-Path $badBin 'ash.exe'))) 'checksum failure activated a binary'

    $lockedPrefix = Join-Path $temporaryRoot 'locked prefix'
    [IO.Directory]::CreateDirectory($lockedPrefix) | Out-Null
    $lockPath = Join-Path $lockedPrefix '.install-lock'
    $lock = [IO.File]::Open($lockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        $locked = Invoke-Installer @(
            '-Archive', $archive,
            '-Sha256', $archiveHash,
            '-Prefix', $lockedPrefix,
            '-BinDir', (Join-Path $temporaryRoot 'locked bin'),
            '-NoPath'
        ) 1
        Assert-True (($locked -join "`n") -match '14') 'lock rejection did not expose stable code 14'
    }
    finally {
        $lock.Dispose()
        if ([IO.File]::Exists($lockPath)) { [IO.File]::Delete($lockPath) }
    }

    $removed = Invoke-Installer @('-Prefix', $prefix, '-Uninstall') 0
    Assert-True (($removed -join "`n") -match '^s:0') 'uninstall did not emit success ASON'
    Assert-True (-not [IO.File]::Exists($launcher)) 'uninstall left the launcher'
    Assert-True (-not [IO.Directory]::Exists($prefix)) 'uninstall left the install root'

    [Console]::Out.WriteLine("s:0`na:installer-smoke-windows")
}
finally {
    if ([IO.Directory]::Exists($temporaryRoot)) {
        [IO.Directory]::Delete($temporaryRoot, $true)
    }
}
