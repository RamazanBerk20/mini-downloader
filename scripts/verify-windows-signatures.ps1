[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ArtifactDirectory,

    [Parameter(Mandatory = $true)]
    [string]$StageDirectory,

    [string]$ExpectedThumbprint = $env:MINIDL_WINDOWS_CERTIFICATE_THUMBPRINT
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($ExpectedThumbprint)) {
    throw "The expected Authenticode certificate thumbprint is missing."
}
$expected = ($ExpectedThumbprint -replace "\s", "").ToUpperInvariant()
$stage = (Resolve-Path -LiteralPath $StageDirectory).Path
$artifacts = (Resolve-Path -LiteralPath $ArtifactDirectory).Path
$files = @(
    "mini-downloader.exe",
    "minidl-native-host.exe"
) | ForEach-Object {
    $path = Join-Path $stage $_
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Expected executable is missing: $path"
    }
    Get-Item -LiteralPath $path
}
$installers = @(Get-ChildItem -LiteralPath $artifacts -Filter "*_x64-setup.exe" -File)
if ($installers.Count -ne 1) {
    throw "Expected exactly one signed NSIS installer, found $($installers.Count)."
}

foreach ($file in @($files) + @($installers)) {
    $signature = Get-AuthenticodeSignature -LiteralPath $file.FullName
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Invalid Authenticode signature on $($file.FullName): $($signature.StatusMessage)"
    }
    $actual = ($signature.SignerCertificate.Thumbprint -replace "\s", "").ToUpperInvariant()
    if ($actual -ne $expected) {
        throw "$($file.FullName) was signed by unexpected certificate $actual."
    }
}

Write-Host "Verified $($files.Count + $installers.Count) Authenticode signatures."
