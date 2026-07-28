[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$TargetRoot,

    [Parameter(Mandatory = $true)]
    [string]$SidecarRoot,

    [Parameter(Mandatory = $true)]
    [string]$TargetTriple,

    [string]$ExpectedThumbprint = $env:MINIDL_WINDOWS_CERTIFICATE_THUMBPRINT
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($ExpectedThumbprint)) {
    throw "The expected Authenticode certificate thumbprint is missing."
}
$expected = ($ExpectedThumbprint -replace "\s", "").ToUpperInvariant()

$sidecarDirectory = (Resolve-Path -LiteralPath $SidecarRoot).Path
$sidecars = @(
    "minidl-native-host-$TargetTriple.exe",
    "aria2c-$TargetTriple.exe",
    "yt-dlp-$TargetTriple.exe"
) | ForEach-Object {
    $path = Join-Path $sidecarDirectory $_
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Expected signed sidecar was not produced: $path"
    }
    Get-Item -LiteralPath $path
}

$bundleRoot = Join-Path (Resolve-Path -LiteralPath $TargetRoot).Path "bundle"
$nsisRoot = Join-Path $bundleRoot "nsis"
$msiRoot = Join-Path $bundleRoot "msi"
if (-not (Test-Path -LiteralPath $nsisRoot -PathType Container)) {
    throw "The NSIS output directory is missing: $nsisRoot"
}
if (-not (Test-Path -LiteralPath $msiRoot -PathType Container)) {
    throw "The MSI output directory is missing: $msiRoot"
}

$nsisInstallers = @(Get-ChildItem -LiteralPath $nsisRoot -Filter "*.exe" -File)
$msiInstallers = @(Get-ChildItem -LiteralPath $msiRoot -Filter "*.msi" -File)
if ($nsisInstallers.Count -eq 0) {
    throw "No signed NSIS installer was produced."
}
if ($msiInstallers.Count -eq 0) {
    throw "No signed MSI installer was produced."
}

foreach ($file in @($sidecars) + @($nsisInstallers) + @($msiInstallers)) {
    $signature = Get-AuthenticodeSignature -LiteralPath $file.FullName
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Invalid Authenticode signature on $($file.FullName): $($signature.StatusMessage)"
    }
}

foreach ($installer in @($nsisInstallers) + @($msiInstallers)) {
    $signature = Get-AuthenticodeSignature -LiteralPath $installer.FullName
    $actual = ($signature.SignerCertificate.Thumbprint -replace "\s", "").ToUpperInvariant()
    if ($actual -ne $expected) {
        throw "Installer $($installer.FullName) was signed by unexpected certificate $actual."
    }
}

Write-Host (
    "Verified {0} sidecars and {1} installers with valid Authenticode signatures." -f `
        $sidecars.Count,
        ($nsisInstallers.Count + $msiInstallers.Count)
)
