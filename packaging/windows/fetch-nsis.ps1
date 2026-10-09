[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$toolsRoot = Join-Path $projectRoot '.tools'
$archivePath = Join-Path $toolsRoot 'downloads/nsis-3.11.zip'
$expectedHash = 'c7d27f780ddb6cffb4730138cd1591e841f4b7edb155856901cdf5f214394fa1'
$downloadUrl = 'https://downloads.sourceforge.net/project/nsis/NSIS%203/3.11/nsis-3.11.zip'
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $archivePath) | Out-Null
if (-not (Test-Path -LiteralPath $archivePath)) {
    Invoke-WebRequest -Uri $downloadUrl -OutFile $archivePath -TimeoutSec 120
}
$actualHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualHash -ne $expectedHash) { throw "NSIS archive checksum mismatch: $actualHash" }
$destination = Join-Path $toolsRoot 'nsis-3.11'
if (-not (Test-Path -LiteralPath (Join-Path $destination 'makensis.exe'))) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $prefix = [IO.Path]::GetFullPath($destination) + [IO.Path]::DirectorySeparatorChar
        foreach ($entry in $archive.Entries) {
            $resolved = [IO.Path]::GetFullPath((Join-Path $toolsRoot $entry.FullName))
            if (-not $resolved.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) -and $resolved.TrimEnd('\', '/') -ne $destination) {
                throw "Unexpected archive path: $($entry.FullName)"
            }
        }
    } finally {
        $archive.Dispose()
    }
    Expand-Archive -LiteralPath $archivePath -DestinationPath $toolsRoot
}
& (Join-Path $destination 'makensis.exe') /VERSION
if ($LASTEXITCODE -ne 0) { throw 'Portable NSIS could not run.' }
Write-Output "Portable NSIS ready: $destination"
