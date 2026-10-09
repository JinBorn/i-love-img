[CmdletBinding()]
param(
    [string]$Target = 'x86_64-pc-windows-msvc',
    [string]$OutputDirectory = 'dist',
    [string]$RuntimeDirectory = '',
    [string]$NsisDirectory = '',
    [switch]$SkipBuild,
    [switch]$Installer,
    # Retained for callers of the earlier ZIP-only option; portable ZIP is now the default.
    [switch]$NoInstaller
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$projectRoot = Split-Path -Parent $PSScriptRoot
$previousRustFlags = $env:RUSTFLAGS
Push-Location $projectRoot
try {
    if ($Installer -and $NoInstaller) { throw 'Use either -Installer or -NoInstaller, not both.' }
    if ($Target -notmatch '^x86_64-pc-windows-(msvc|gnullvm)$') {
        throw 'The Windows package script supports x64 MSVC and GNU LLVM targets.'
    }
    if ($Target -eq 'x86_64-pc-windows-msvc') {
        $env:RUSTFLAGS = "$previousRustFlags -C target-feature=+crt-static".Trim()
    }
    if (-not $SkipBuild) {
        & cargo build --locked --release --workspace --target $Target
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    }
    $metadataText = & cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed.' }
    $metadata = $metadataText | ConvertFrom-Json
    $desktop = @($metadata.packages | Where-Object { $_.targets.name -contains 'local-image-tools' })
    if ($desktop.Count -ne 1) { throw 'Could not identify the local-image-tools package.' }
    $version = $desktop[0].version
    $requestedOutput = if ([IO.Path]::IsPathRooted($OutputDirectory)) { $OutputDirectory } else { Join-Path $projectRoot $OutputDirectory }
    $outputRoot = [IO.Path]::GetFullPath($requestedOutput)
    New-Item -ItemType Directory -Force -Path $outputRoot | Out-Null
    $suffix = if ($Target -eq 'x86_64-pc-windows-msvc') { 'windows-x64' } else { "windows-x64-$($Target.Split('-')[-1])" }
    $portableBase = "local-image-tools-$version-$suffix-portable"
    $zipPath = Join-Path $outputRoot "$portableBase.zip"
    $manifestPath = Join-Path $outputRoot "$portableBase-files.json"
    $checksumPath = Join-Path $outputRoot "local-image-tools-$version-$suffix-SHA256SUMS.txt"
    $installerPath = Join-Path $outputRoot "local-image-tools-$version-$suffix-setup.exe"
    $requestedArtifacts = @($zipPath, $manifestPath, $checksumPath)
    if ($Installer) { $requestedArtifacts += $installerPath }
    foreach ($path in $requestedArtifacts) {
        if (Test-Path -LiteralPath $path) { throw "Package already exists: $path" }
    }
    $stageRoot = Join-Path $outputRoot ('.stage-windows-' + [Guid]::NewGuid().ToString('N'))
    # Common payload stays marker-free for the optional installed application.
    $stage = Join-Path $stageRoot 'common'
    New-Item -ItemType Directory -Path $stage | Out-Null
    $releaseRoot = Join-Path $metadata.target_directory "$Target/release"
    foreach ($binary in @('local-image-tools.exe', 'lit.exe')) {
        Copy-Item -LiteralPath (Join-Path $releaseRoot $binary) -Destination $stage
    }
    if ($Target -eq 'x86_64-pc-windows-gnullvm') {
        if (-not $RuntimeDirectory) {
            $RuntimeDirectory = Join-Path $projectRoot '.tools/llvm-mingw-20261006-ucrt-x86_64/bin'
        }
        $runtimeRoot = (Resolve-Path -LiteralPath $RuntimeDirectory).Path
        $runtimeDll = Join-Path $runtimeRoot 'libunwind.dll'
        $runtimeLicense = Join-Path (Split-Path -Parent $runtimeRoot) 'LICENSE.TXT'
        if (-not (Test-Path -LiteralPath $runtimeDll) -or -not (Test-Path -LiteralPath $runtimeLicense)) {
            throw 'GNU LLVM packaging requires libunwind.dll and the original LLVM distribution LICENSE.TXT.'
        }
        Copy-Item -LiteralPath $runtimeDll -Destination $stage
        $runtimeNotices = Join-Path $stage 'dependency-licenses/llvm-runtime'
        New-Item -ItemType Directory -Path $runtimeNotices -Force | Out-Null
        Copy-Item -LiteralPath $runtimeLicense -Destination $runtimeNotices
        $runtimeRecord = [ordered]@{
            component = 'LLVM libunwind'
            source = 'https://github.com/mstorsjo/llvm-mingw'
            distribution = Split-Path -Leaf (Split-Path -Parent $runtimeRoot)
            binary = 'libunwind.dll'
            sha256 = (Get-FileHash -LiteralPath $runtimeDll -Algorithm SHA256).Hash.ToLowerInvariant()
            license_sha256 = (Get-FileHash -LiteralPath $runtimeLicense -Algorithm SHA256).Hash.ToLowerInvariant()
            license = 'Apache-2.0 WITH LLVM-exception; legacy notices retained in LICENSE.TXT'
        }
        [IO.File]::WriteAllText((Join-Path $runtimeNotices 'runtime.json'), ($runtimeRecord | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
    }
    foreach ($file in @('LICENSE', 'THIRD_PARTY_NOTICES.md', 'README.md', 'CONTRIBUTING.md', 'CHANGELOG.md', 'CONTEXT.md')) {
        Copy-Item -LiteralPath (Join-Path $projectRoot $file) -Destination $stage
    }
    Copy-Item -LiteralPath (Join-Path $projectRoot 'docs') -Destination $stage -Recurse
    & python (Join-Path $projectRoot 'packaging/copy-assets.py') (Join-Path $projectRoot 'assets') (Join-Path $stage 'assets')
    if ($LASTEXITCODE -ne 0) { throw 'Asset collection failed.' }
    & python (Join-Path $projectRoot 'packaging/collect-licenses.py') (Join-Path $stage 'dependency-licenses') --target $Target
    if ($LASTEXITCODE -ne 0) { throw 'Dependency license collection failed.' }
    $portableRoot = Join-Path $stageRoot 'portable'
    New-Item -ItemType Directory -Path $portableRoot | Out-Null
    $portableStage = Join-Path $portableRoot 'Local Image Tools'
    Copy-Item -LiteralPath $stage -Destination $portableStage -Recurse
    foreach ($file in @('portable.flag', 'START-HERE.txt')) {
        Copy-Item -LiteralPath (Join-Path $projectRoot "packaging/windows/$file") -Destination $portableStage
    }
    $nsisPath = $null
    if ($Installer) {
        $nsis = Get-Command makensis -ErrorAction SilentlyContinue
        $nsisCandidates = @()
        if ($NsisDirectory) { $nsisCandidates += (Join-Path $NsisDirectory 'makensis.exe') }
        $nsisCandidates += (Join-Path $projectRoot '.tools/nsis-3.11/makensis.exe')
        if ($nsis) { $nsisCandidates += $nsis.Source }
        if (${env:ProgramFiles(x86)}) { $nsisCandidates += (Join-Path ${env:ProgramFiles(x86)} 'NSIS/makensis.exe') }
        if ($env:ProgramFiles) { $nsisCandidates += (Join-Path $env:ProgramFiles 'NSIS/makensis.exe') }
        foreach ($candidate in $nsisCandidates) {
            if ((Test-Path -LiteralPath $candidate) -and (Test-Path -LiteralPath (Join-Path (Split-Path -Parent $candidate) 'COPYING'))) {
                $nsisPath = $candidate
                break
            }
        }
        if (-not $nsisPath) { throw 'The optional -Installer package requires a full NSIS installation with COPYING. Omit -Installer to create the portable ZIP only.' }
        $nsisLicense = Join-Path (Split-Path -Parent $nsisPath) 'COPYING'
        if (-not (Test-Path -LiteralPath $nsisLicense)) { throw "Cannot find NSIS license notices: $nsisLicense" }
        $nsisNotices = Join-Path $stage 'dependency-licenses/nsis'
        New-Item -ItemType Directory -Path $nsisNotices | Out-Null
        Copy-Item -LiteralPath $nsisLicense -Destination $nsisNotices
        $uninstallManifest = Join-Path $stageRoot 'uninstall-files.nsh'
        & python (Join-Path $projectRoot 'packaging/windows/write-uninstall-manifest.py') $stage $uninstallManifest
        if ($LASTEXITCODE -ne 0) { throw 'Uninstall manifest generation failed.' }
    }
    $manifest = @(Get-ChildItem -LiteralPath $portableStage -File -Recurse | Sort-Object FullName | ForEach-Object {
        [ordered]@{
            path = $_.FullName.Substring($portableStage.Length + 1).Replace('\', '/')
            bytes = $_.Length
            sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    })
    [IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 3), [Text.UTF8Encoding]::new($false))
    Compress-Archive -LiteralPath $portableStage -DestinationPath $zipPath -CompressionLevel Optimal
    $artifacts = @($zipPath, $manifestPath)
    if ($Installer) {
        if (Test-Path -LiteralPath (Join-Path $stage 'portable.flag')) { throw 'Portable marker must never enter the installer payload.' }
        $nsisArgs = @("/DSTAGE_DIR=$stage", "/DOUTPUT_FILE=$installerPath", "/DBUILD_VERSION=$version", "/DUNINSTALL_MANIFEST=$uninstallManifest")
        $iconPath = Join-Path $projectRoot 'assets/icons/app.ico'
        if (Test-Path -LiteralPath $iconPath) { $nsisArgs += "/DAPP_ICON=$iconPath" }
        $nsisArgs += (Join-Path $projectRoot 'packaging/windows/installer.nsi')
        & $nsisPath @nsisArgs
        if ($LASTEXITCODE -ne 0) { throw 'NSIS installer creation failed.' }
        $artifacts += $installerPath
    }
    $checksums = foreach ($artifact in $artifacts) {
        $hash = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $([IO.Path]::GetFileName($artifact))"
    }
    [IO.File]::WriteAllLines($checksumPath, $checksums, [Text.UTF8Encoding]::new($false))
    # Delete only our newly created, resolved staging directory beneath outputRoot.
    $resolvedStage = [IO.Path]::GetFullPath($stageRoot)
    $expectedPrefix = $outputRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolvedStage.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe staging cleanup path.' }
    Remove-Item -LiteralPath $resolvedStage -Recurse -Force
    $artifacts | ForEach-Object { Write-Output $_ }
    Write-Output $checksumPath
} finally {
    $env:RUSTFLAGS = $previousRustFlags
    Pop-Location
}
