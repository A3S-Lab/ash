[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$inventoryPath = Join-Path $repositoryRoot 'THIRD-PARTY-LICENSES'
$metadataJson = & cargo metadata --format-version 1 --locked
if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
$metadata = $metadataJson | ConvertFrom-Json
$expected = @(
    $metadata.packages |
        Where-Object { $null -ne $_.source -and $_.name -notlike 'a3s-ash*' } |
        ForEach-Object { '{0} {1} | {2}' -f $_.name, $_.version, $_.license } |
        Sort-Object
)
$actual = @(
    [IO.File]::ReadAllLines($inventoryPath, [Text.Encoding]::UTF8) |
        Where-Object { $_ -match '^[A-Za-z0-9_-]+ [^ ]+ \| .+$' } |
        Sort-Object
)
$difference = @(Compare-Object -ReferenceObject $expected -DifferenceObject $actual)
if ($difference.Count -ne 0) {
    $difference | ForEach-Object { [Console]::Error.WriteLine($_.ToString()) }
    throw 'THIRD-PARTY-LICENSES does not match Cargo.lock'
}

[Console]::Out.WriteLine("validated $($actual.Count) third-party package records")
