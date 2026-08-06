[CmdletBinding()]
param(
    [string]$ArtifactDirectory = "dist-native",
    [string]$SidecarDirectory = "dist-native\windows-stage"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-SignTool {
    $command = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) { return $command.Source }
    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $candidate = Get-ChildItem -LiteralPath $kits -Filter signtool.exe -File -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if ($null -eq $candidate) { throw "signtool.exe was not found." }
    return $candidate.FullName
}

function Sign-Executable {
    param([Parameter(Mandatory = $true)][string]$Path)
    if ([string]::IsNullOrWhiteSpace($env:MINIDL_WINDOWS_CERTIFICATE_THUMBPRINT)) { return }
    $signTool = Get-SignTool
    & $signTool sign /sha1 $env:MINIDL_WINDOWS_CERTIFICATE_THUMBPRINT `
        /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 $Path
    if ($LASTEXITCODE -ne 0) { throw "Authenticode signing failed: $Path" }
}

$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $projectRoot
$manifest = Get-Content -LiteralPath "Cargo.toml" -Raw
$match = [regex]::Match($manifest, '(?m)^version = "([0-9]+\.[0-9]+\.[0-9]+)"')
if (-not $match.Success) {
    throw "Could not read workspace version."
}
$version = $match.Groups[1].Value
$artifacts = [IO.Path]::GetFullPath((Join-Path $projectRoot $ArtifactDirectory))
$stage = [IO.Path]::GetFullPath((Join-Path $projectRoot $SidecarDirectory))

$cargoHome = if ([string]::IsNullOrWhiteSpace($env:CARGO_HOME)) {
    Join-Path $HOME ".cargo"
} else {
    $env:CARGO_HOME
}
$encodedSeparator = [char]0x1f
$releaseRustFlags = $env:CARGO_ENCODED_RUSTFLAGS
foreach ($flag in @(
    "--remap-path-prefix=$projectRoot=.",
    "--remap-path-prefix=$cargoHome=/cargo"
)) {
    if (-not [string]::IsNullOrEmpty($releaseRustFlags)) {
        $releaseRustFlags += $encodedSeparator
    }
    $releaseRustFlags += $flag
}
$env:CARGO_ENCODED_RUSTFLAGS = $releaseRustFlags

cargo build --locked --release -p minidl-native-desktop -p minidl-native-host
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

New-Item -ItemType Directory -Force $artifacts, $stage | Out-Null
Copy-Item target\release\mini-downloader.exe $stage\mini-downloader.exe -Force
Copy-Item target\release\minidl-native-host.exe $stage\minidl-native-host.exe -Force
foreach ($sidecar in @("aria2c.exe", "yt-dlp.exe")) {
    if (-not (Test-Path -LiteralPath (Join-Path $stage $sidecar) -PathType Leaf)) {
        throw "$sidecar must be staged in $stage before packaging."
    }
}
Copy-Item LICENSE $stage\LICENSE.txt -Force
Copy-Item PRIVACY.md $stage\PRIVACY.txt -Force
Copy-Item SECURITY.md $stage\SECURITY.txt -Force
Copy-Item THIRD_PARTY_NOTICES.md $stage\THIRD_PARTY_NOTICES.txt -Force
Copy-Item THIRD_PARTY_LICENSES.txt $stage\THIRD_PARTY_LICENSES.txt -Force
foreach ($notice in @(
    "LICENSE.aria2.txt",
    "NOTICE.aria2-openssl.txt",
    "LICENSE.yt-dlp.txt"
)) {
    if (-not (Test-Path -LiteralPath (Join-Path $stage $notice) -PathType Leaf)) {
        throw "$notice must be staged in $stage before packaging."
    }
}
foreach ($executable in @(
    "$stage\mini-downloader.exe",
    "$stage\minidl-native-host.exe"
)) {
    Sign-Executable $executable
}

$portableStage = Join-Path $artifacts "windows-portable"
if (Test-Path -LiteralPath $portableStage) {
    Remove-Item -LiteralPath $portableStage -Recurse -Force
}
New-Item -ItemType Directory -Force $portableStage | Out-Null
foreach ($name in @(
    "mini-downloader.exe",
    "minidl-native-host.exe",
    "aria2c.exe",
    "yt-dlp.exe",
    "LICENSE.txt",
    "PRIVACY.txt",
    "SECURITY.txt",
    "THIRD_PARTY_NOTICES.txt",
    "THIRD_PARTY_LICENSES.txt",
    "LICENSE.aria2.txt",
    "NOTICE.aria2-openssl.txt",
    "LICENSE.yt-dlp.txt"
)) {
    Copy-Item (Join-Path $stage $name) (Join-Path $portableStage $name) -Force
}
$portable = Join-Path $artifacts "Mini.Downloader_${version}_windows-x86_64.zip"
if (Test-Path -LiteralPath $portable) { Remove-Item -LiteralPath $portable -Force }
Compress-Archive -Path "$portableStage\*" -DestinationPath $portable -CompressionLevel Optimal
Remove-Item -LiteralPath $portableStage -Recurse -Force

$makensis = Get-Command makensis.exe -ErrorAction SilentlyContinue
if ($null -eq $makensis) { $makensis = Get-Command makensis -ErrorAction SilentlyContinue }
if ($null -eq $makensis) { throw "NSIS (makensis) is required." }
& $makensis.Source "/DVERSION=$version" "/DSTAGE_DIR=$stage" "/DOUT_DIR=$artifacts" "/DPROJECT_DIR=$projectRoot" packaging\windows\installer.nsi
if ($LASTEXITCODE -ne 0) { throw "NSIS packaging failed" }
$installer = Get-ChildItem -LiteralPath $artifacts -Filter "*_x64-setup.exe" -File |
    Select-Object -First 1
if ($null -eq $installer) { throw "NSIS installer was not produced." }
Sign-Executable $installer.FullName

Write-Host "Native Windows artifacts: $artifacts"
