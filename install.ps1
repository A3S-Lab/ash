[CmdletBinding()]
param(
    [string]$Version = $env:ASH_INSTALL_VERSION,
    [string]$Channel = $(if ($env:ASH_INSTALL_CHANNEL) { $env:ASH_INSTALL_CHANNEL } else { 'stable' }),
    [string]$Prefix = $env:ASH_INSTALL_PREFIX,
    [string]$BinDir = $env:ASH_INSTALL_BIN_DIR,
    [switch]$NoPath,
    [switch]$Force,
    [string]$Archive = $env:ASH_INSTALL_ARCHIVE,
    [string]$Sha256 = $env:ASH_INSTALL_SHA256,
    [switch]$Uninstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$Repository = 'A3S-Lab/ash'
$Stage = $null
$LockPath = $null
$LockStream = $null
$InstallSucceeded = $false
$RemovePrefixAfterUnlock = $false
$Destination = $null
$DestinationCreated = $false
$DestinationBackup = $null
$Launcher = $null
$LauncherChanged = $false
$LauncherExisted = $false
$LauncherBackup = $null
$ReceiptPath = $null
$ReceiptExisted = $false
$ReceiptBackup = $null
$PathAddedThisRun = $false

function Stop-AshInstall([int]$Code) {
    throw [InvalidOperationException]::new("ASH_INSTALL:$Code")
}

function ConvertTo-AsonString([string]$Value) {
    $escaped = $Value.Replace('\', '\\').Replace('"', '\"').Replace("`r", '\r').Replace("`n", '\n').Replace("`t", '\t')
    return '"' + $escaped + '"'
}

function Read-Utf8File([string]$Path) {
    return [IO.File]::ReadAllText($Path, [Text.UTF8Encoding]::new($false, $true))
}

function Get-NormalizedVersion([string]$Value) {
    if ([string]::IsNullOrWhiteSpace($Value)) { return '' }
    $normalized = $Value.Trim()
    if ($normalized.StartsWith('v', [StringComparison]::Ordinal)) {
        $normalized = $normalized.Substring(1)
    }
    if ($normalized -notmatch '^[0-9A-Za-z.+-]+$') { Stop-AshInstall 12 }
    return $normalized
}

function Get-TargetTriple {
    if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([Runtime.InteropServices.OSPlatform]::Windows)) {
        Stop-AshInstall 20
    }
    $architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    switch ($architecture) {
        'X64' { return 'x86_64-pc-windows-msvc' }
        'Arm64' { return 'aarch64-pc-windows-msvc' }
        default { Stop-AshInstall 21 }
    }
}

function Get-BuildInfo([string]$Executable) {
    $lines = @(& $Executable --build-info)
    if ($LASTEXITCODE -ne 0) { Stop-AshInstall 32 }
    $metadata = @{}
    foreach ($line in $lines) {
        if ($line -match '^([vtpa]):(.+)$') {
            if ($metadata.ContainsKey($Matches[1])) { Stop-AshInstall 32 }
            $metadata[$Matches[1]] = $Matches[2]
        }
    }
    foreach ($key in @('v', 't', 'p', 'a')) {
        if (-not $metadata.ContainsKey($key)) { Stop-AshInstall 32 }
    }
    if ($metadata.p -ne '1' -or $metadata.a -ne '1') { Stop-AshInstall 32 }
    if ($metadata.v -notmatch '^[0-9A-Za-z.+-]+$') { Stop-AshInstall 32 }
    return $metadata
}

function Set-AtomicFile([string]$Source, [string]$DestinationPath) {
    $parent = Split-Path -Parent $DestinationPath
    [IO.Directory]::CreateDirectory($parent) | Out-Null
    $temporary = Join-Path $parent ('.ash-' + [Guid]::NewGuid().ToString('N') + '.tmp')
    [IO.File]::Copy($Source, $temporary, $true)
    try {
        if ([IO.File]::Exists($DestinationPath)) {
            $backup = Join-Path $parent ('.ash-' + [Guid]::NewGuid().ToString('N') + '.bak')
            try {
                [IO.File]::Replace($temporary, $DestinationPath, $backup, $true)
            }
            finally {
                if ([IO.File]::Exists($backup)) { [IO.File]::Delete($backup) }
            }
        }
        else {
            [IO.File]::Move($temporary, $DestinationPath)
        }
    }
    finally {
        if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) }
    }
}

function Test-SafePrefix([string]$Path) {
    $root = [IO.Path]::GetPathRoot($Path)
    $profileRoot = if ($env:USERPROFILE) { [IO.Path]::GetFullPath($env:USERPROFILE) } else { $null }
    if ([string]::IsNullOrWhiteSpace($Path) -or
        [string]::Equals($Path.TrimEnd('\'), $root.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase) -or
        ($profileRoot -and [string]::Equals($Path.TrimEnd('\'), $profileRoot.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)))
    {
        Stop-AshInstall 13
    }
}

function Test-PathEntry([string]$Entry, [string]$Directory) {
    return [string]::Equals($Entry.TrimEnd('\'), $Directory.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
}

function Update-UserPath([string]$Directory, [bool]$Remove) {
    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($null -eq $current) { $current = '' }
    $parts = @($current.Split(';', [StringSplitOptions]::RemoveEmptyEntries))
    $filtered = @($parts | Where-Object { -not (Test-PathEntry $_ $Directory) })
    if (-not $Remove) { $filtered += $Directory }
    [Environment]::SetEnvironmentVariable('Path', ($filtered -join ';'), 'User')
    if (-not $Remove -and -not (@($env:Path.Split(';') | Where-Object { Test-PathEntry $_ $Directory }).Count -gt 0)) {
        $env:Path = $Directory + ';' + $env:Path
    }
}

function Send-EnvironmentChange {
    if (-not ('AshInstaller.NativeMethods' -as [type])) {
        Add-Type -Namespace AshInstaller -Name NativeMethods -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Auto)]
public static extern System.IntPtr SendMessageTimeout(
    System.IntPtr hWnd,
    uint Msg,
    System.UIntPtr wParam,
    string lParam,
    uint fuFlags,
    uint uTimeout,
    out System.UIntPtr lpdwResult);
'@
    }
    $result = [UIntPtr]::Zero
    [void][AshInstaller.NativeMethods]::SendMessageTimeout(
        [IntPtr]0xffff,
        0x001A,
        [UIntPtr]::Zero,
        'Environment',
        0x0002,
        5000,
        [ref]$result
    )
}

function Restore-InstallState {
    if ($PathAddedThisRun -and $BinDir) {
        try { Update-UserPath $BinDir $true } catch { }
    }
    if ($LauncherChanged -and $Launcher) {
        try {
            if ($LauncherExisted -and $LauncherBackup -and [IO.File]::Exists($LauncherBackup)) {
                Set-AtomicFile $LauncherBackup $Launcher
            }
            elseif ([IO.File]::Exists($Launcher)) {
                [IO.File]::Delete($Launcher)
            }
        }
        catch { }
    }
    if ($DestinationCreated -and $Destination -and [IO.Directory]::Exists($Destination)) {
        try { [IO.Directory]::Delete($Destination, $true) } catch { }
    }
    if ($DestinationBackup -and [IO.Directory]::Exists($DestinationBackup) -and $Destination) {
        try {
            if ([IO.Directory]::Exists($Destination)) { [IO.Directory]::Delete($Destination, $true) }
            [IO.Directory]::Move($DestinationBackup, $Destination)
        }
        catch { }
    }
    if ($ReceiptPath) {
        try {
            if ($ReceiptExisted -and $ReceiptBackup -and [IO.File]::Exists($ReceiptBackup)) {
                Set-AtomicFile $ReceiptBackup $ReceiptPath
            }
            elseif (-not $ReceiptExisted -and [IO.File]::Exists($ReceiptPath)) {
                [IO.File]::Delete($ReceiptPath)
            }
        }
        catch { }
    }
}

function Invoke-AshInstaller {
    if ($Channel -ne 'stable') { Stop-AshInstall 12 }
    $script:Version = Get-NormalizedVersion $Version
    if ([string]::IsNullOrWhiteSpace($Prefix)) {
        if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) { Stop-AshInstall 11 }
        $script:Prefix = Join-Path $env:LOCALAPPDATA 'Programs\ash'
    }
    $script:Prefix = [IO.Path]::GetFullPath($Prefix)
    if ([string]::IsNullOrWhiteSpace($BinDir)) {
        $script:BinDir = Join-Path $Prefix 'active'
    }
    $script:BinDir = [IO.Path]::GetFullPath($BinDir)
    Test-SafePrefix $Prefix
    [IO.Directory]::CreateDirectory($Prefix) | Out-Null

    $script:LockPath = Join-Path $Prefix '.install-lock'
    try {
        $script:LockStream = [IO.File]::Open($LockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    }
    catch { Stop-AshInstall 14 }

    $script:ReceiptPath = Join-Path $Prefix 'install-receipt.json'
    if ($Uninstall) {
        if (-not [IO.File]::Exists($ReceiptPath)) { Stop-AshInstall 16 }
        $receipt = Read-Utf8File $ReceiptPath | ConvertFrom-Json
        if ($receipt.schema -ne 1 -or $receipt.repository -ne $Repository -or
            -not [string]::Equals([IO.Path]::GetFullPath([string]$receipt.prefix), $Prefix, [StringComparison]::OrdinalIgnoreCase))
        {
            Stop-AshInstall 16
        }
        $launcher = [IO.Path]::GetFullPath([string]$receipt.launcher)
        $ownedBinDir = Split-Path -Parent $launcher
        if ([IO.File]::Exists($launcher)) {
            $installed = Get-BuildInfo $launcher
            if ($installed.v -ne [string]$receipt.version -or $installed.t -ne [string]$receipt.target) {
                Stop-AshInstall 16
            }
        }
        if ([bool]$receipt.path_added) { Update-UserPath $ownedBinDir $true }
        if ([IO.File]::Exists($launcher)) { [IO.File]::Delete($launcher) }
        $versions = Join-Path $Prefix 'versions'
        if ([IO.Directory]::Exists($versions)) { [IO.Directory]::Delete($versions, $true) }
        if ([IO.File]::Exists($ReceiptPath)) { [IO.File]::Delete($ReceiptPath) }
        if ([IO.Directory]::Exists($ownedBinDir) -and (Get-ChildItem -LiteralPath $ownedBinDir -Force | Measure-Object).Count -eq 0) {
            [IO.Directory]::Delete($ownedBinDir)
        }
        try { Send-EnvironmentChange } catch { }
        $script:RemovePrefixAfterUnlock = $true
        $script:InstallSucceeded = $true
        [Console]::Out.WriteLine("s:0`na:uninstalled`np:{0}" -f (ConvertTo-AsonString $Prefix))
        return
    }

    $target = Get-TargetTriple
    $asset = "ash-$target.zip"
    $script:Stage = Join-Path ([IO.Path]::GetTempPath()) ('ash-install-' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($Stage) | Out-Null
    $archivePath = Join-Path $Stage $asset
    $online = [string]::IsNullOrWhiteSpace($Archive)
    if ($online) {
        $base = if ($Version) {
            "https://github.com/$Repository/releases/download/v$Version"
        }
        else {
            "https://github.com/$Repository/releases/latest/download"
        }
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $archivePath
        $sumPath = Join-Path $Stage 'SHA256SUMS'
        Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sumPath
        $escapedAsset = [regex]::Escape($asset)
        $line = Get-Content -LiteralPath $sumPath | Where-Object { $_ -match "^[0-9A-Fa-f]{64}\s+\*?$escapedAsset$" } | Select-Object -First 1
        if (-not $line) { Stop-AshInstall 27 }
        $script:Sha256 = ($line -split '\s+')[0]
    }
    else {
        if (-not [IO.File]::Exists($Archive)) { Stop-AshInstall 25 }
        if ([string]::IsNullOrWhiteSpace($Sha256)) { Stop-AshInstall 26 }
        [IO.File]::Copy([IO.Path]::GetFullPath($Archive), $archivePath, $true)
    }
    if ($Sha256 -notmatch '^[0-9A-Fa-f]{64}$') { Stop-AshInstall 26 }
    $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash
    if (-not [string]::Equals($actualHash, $Sha256, [StringComparison]::OrdinalIgnoreCase)) { Stop-AshInstall 29 }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $names = @($zip.Entries | ForEach-Object { $_.FullName } | Sort-Object)
        $expected = @('LICENSE', 'THIRD-PARTY-LICENSES', 'ash.exe', 'release.json')
        if ($names.Count -ne 4 -or @(Compare-Object $names $expected).Count -ne 0) { Stop-AshInstall 31 }
    }
    finally { $zip.Dispose() }
    $extract = Join-Path $Stage 'extract'
    [IO.Compression.ZipFile]::ExtractToDirectory($archivePath, $extract)
    $candidateBinary = Join-Path $extract 'ash.exe'
    foreach ($name in @('ash.exe', 'LICENSE', 'THIRD-PARTY-LICENSES', 'release.json')) {
        if (-not [IO.File]::Exists((Join-Path $extract $name))) { Stop-AshInstall 31 }
    }
    if ($online) {
        $signature = Get-AuthenticodeSignature -LiteralPath $candidateBinary
        if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) { Stop-AshInstall 35 }
    }
    $metadata = Get-BuildInfo $candidateBinary
    if ($metadata.t -ne $target) { Stop-AshInstall 33 }
    if ($Version -and $metadata.v -ne $Version) { Stop-AshInstall 34 }
    $script:Version = [string]$metadata.v
    $release = Read-Utf8File (Join-Path $extract 'release.json') | ConvertFrom-Json
    $binaryHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $candidateBinary).Hash.ToLowerInvariant()
    if ($release.schema -ne 1 -or $release.product -ne 'ash' -or $release.version -ne $Version -or
        $release.target -ne $target -or $release.protocol -ne '1' -or $release.ason -ne '1' -or
        -not [string]::Equals([string]$release.binary_sha256, $binaryHash, [StringComparison]::OrdinalIgnoreCase))
    {
        Stop-AshInstall 31
    }
    if ([IO.File]::Exists($ReceiptPath)) {
        $priorReceipt = Read-Utf8File $ReceiptPath | ConvertFrom-Json
        if ($priorReceipt.schema -ne 1 -or $priorReceipt.repository -ne $Repository -or
            -not [string]::Equals([IO.Path]::GetFullPath([string]$priorReceipt.prefix), $Prefix, [StringComparison]::OrdinalIgnoreCase))
        {
            if ($env:ASH_INSTALL_DEBUG) {
                [Console]::Error.WriteLine("prior-prefix={0}" -f (ConvertTo-AsonString ([string]$priorReceipt.prefix)))
                [Console]::Error.WriteLine("current-prefix={0}" -f (ConvertTo-AsonString $Prefix))
            }
            Stop-AshInstall 16
        }
        $script:ReceiptBackup = Join-Path $Stage 'receipt.backup'
        [IO.File]::Copy($ReceiptPath, $ReceiptBackup, $true)
        $script:ReceiptExisted = $true
    }
    else {
        $priorReceipt = $null
    }

    $versions = Join-Path $Prefix 'versions'
    [IO.Directory]::CreateDirectory($versions) | Out-Null
    $script:Destination = Join-Path $versions $Version
    if ([IO.Directory]::Exists($Destination)) {
        if (-not $Force) {
            $existing = Get-BuildInfo (Join-Path $Destination 'ash.exe')
            if ($existing.v -ne $Version -or $existing.t -ne $target) { Stop-AshInstall 36 }
            $existingHash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $Destination 'ash.exe')).Hash
            if (-not [string]::Equals($existingHash, $binaryHash, [StringComparison]::OrdinalIgnoreCase)) {
                Stop-AshInstall 36
            }
        }
        else {
            $script:DestinationBackup = Join-Path $versions ('.backup-' + $Version + '-' + [Guid]::NewGuid().ToString('N'))
            [IO.Directory]::Move($Destination, $DestinationBackup)
        }
    }
    elseif ([IO.File]::Exists($Destination)) {
        Stop-AshInstall 36
    }
    if (-not [IO.Directory]::Exists($Destination)) {
        $candidateDirectory = Join-Path $versions ('.candidate-' + $Version + '-' + [Guid]::NewGuid().ToString('N'))
        [IO.Directory]::CreateDirectory($candidateDirectory) | Out-Null
        foreach ($name in @('ash.exe', 'LICENSE', 'THIRD-PARTY-LICENSES', 'release.json')) {
            [IO.File]::Copy((Join-Path $extract $name), (Join-Path $candidateDirectory $name), $false)
        }
        [IO.Directory]::Move($candidateDirectory, $Destination)
        $script:DestinationCreated = $true
    }

    [IO.Directory]::CreateDirectory($BinDir) | Out-Null
    $script:Launcher = Join-Path $BinDir 'ash.exe'
    if ([IO.Directory]::Exists($Launcher)) { Stop-AshInstall 39 }
    if ([IO.File]::Exists($Launcher)) {
        Get-BuildInfo $Launcher | Out-Null
        $script:LauncherBackup = Join-Path $Stage 'launcher.backup.exe'
        [IO.File]::Copy($Launcher, $LauncherBackup, $true)
        $script:LauncherExisted = $true
    }
    Set-AtomicFile (Join-Path $Destination 'ash.exe') $Launcher
    $script:LauncherChanged = $true
    $activeMetadata = Get-BuildInfo $Launcher
    if ($activeMetadata.v -ne $Version -or $activeMetadata.t -ne $target) { Stop-AshInstall 40 }

    $pathOwned = $false
    if ($priorReceipt -and [bool]$priorReceipt.path_added) {
        $priorBin = Split-Path -Parent ([string]$priorReceipt.launcher)
        if (-not (Test-PathEntry $priorBin $BinDir)) { Stop-AshInstall 36 }
        $pathOwned = $true
    }
    $currentUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($null -eq $currentUserPath) { $currentUserPath = '' }
    $containsPath = @($currentUserPath.Split(';', [StringSplitOptions]::RemoveEmptyEntries) | Where-Object { Test-PathEntry $_ $BinDir }).Count -gt 0
    if (-not $NoPath -and -not $containsPath) {
        Update-UserPath $BinDir $false
        $script:PathAddedThisRun = $true
        $pathOwned = $true
    }
    $receipt = [ordered]@{
        schema = 1
        repository = $Repository
        version = $Version
        target = $target
        prefix = $Prefix
        launcher = $Launcher
        path_added = $pathOwned
    }
    $receiptTemp = Join-Path $Prefix ('.install-receipt-' + [Guid]::NewGuid().ToString('N') + '.json')
    $receiptJson = $receipt | ConvertTo-Json -Compress
    [IO.File]::WriteAllText($receiptTemp, $receiptJson + "`n", [Text.UTF8Encoding]::new($false))
    Set-AtomicFile $receiptTemp $ReceiptPath
    [IO.File]::Delete($receiptTemp)

    if ($DestinationBackup -and [IO.Directory]::Exists($DestinationBackup)) {
        [IO.Directory]::Delete($DestinationBackup, $true)
        $script:DestinationBackup = $null
    }
    try { Send-EnvironmentChange } catch { }
    $script:InstallSucceeded = $true
    [Console]::Out.WriteLine("s:0`na:installed`nv:$Version`nt:$target`np:{0}" -f (ConvertTo-AsonString $Launcher))
}

try {
    Invoke-AshInstaller
}
catch {
    if (-not $InstallSucceeded -and -not $Uninstall) { Restore-InstallState }
    $code = 10
    if ($_.Exception.Message -match '^ASH_INSTALL:(\d+)$') { $code = [int]$Matches[1] }
    if ($env:ASH_INSTALL_DEBUG) {
        [Console]::Error.WriteLine($_.Exception.ToString())
        [Console]::Error.WriteLine($_.ScriptStackTrace)
    }
    [Console]::Error.WriteLine("s:1`ne{{c}}:`n$code")
    $host.SetShouldExit(1)
}
finally {
    if ($LockStream) { $LockStream.Dispose() }
    if ($LockPath -and [IO.File]::Exists($LockPath)) {
        try { [IO.File]::Delete($LockPath) } catch { }
    }
    if ($Stage -and [IO.Directory]::Exists($Stage)) {
        [IO.Directory]::Delete($Stage, $true)
    }
    if ($RemovePrefixAfterUnlock -and [IO.Directory]::Exists($Prefix) -and
        (Get-ChildItem -LiteralPath $Prefix -Force | Measure-Object).Count -eq 0)
    {
        [IO.Directory]::Delete($Prefix)
    }
}
