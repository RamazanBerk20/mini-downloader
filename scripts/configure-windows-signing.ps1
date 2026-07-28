[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ConfigPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Test-CodeSigningCertificate {
    param(
        [Parameter(Mandatory = $true)]
        [System.Security.Cryptography.X509Certificates.X509Certificate2]$Certificate
    )

    $codeSigningOid = "1.3.6.1.5.5.7.3.3"
    foreach ($extension in $Certificate.Extensions) {
        if ($extension -is [System.Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]) {
            foreach ($usage in $extension.EnhancedKeyUsages) {
                if ($usage.Value -eq $codeSigningOid) {
                    return $true
                }
            }
        }
    }
    return $false
}

$certificateBase64 = $env:WINDOWS_CERTIFICATE
$certificatePassword = $env:WINDOWS_CERTIFICATE_PASSWORD
if ([string]::IsNullOrWhiteSpace($certificateBase64)) {
    throw "WINDOWS_CERTIFICATE is required; refusing to create an unsigned Windows release."
}
if ([string]::IsNullOrWhiteSpace($certificatePassword)) {
    throw "WINDOWS_CERTIFICATE_PASSWORD is required; refusing to create an unsigned Windows release."
}

$configFile = (Resolve-Path -LiteralPath $ConfigPath).Path
$tempRoot = if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    [System.IO.Path]::GetTempPath()
} else {
    $env:RUNNER_TEMP
}
$pfxPath = Join-Path $tempRoot "mini-downloader-signing-$([guid]::NewGuid()).pfx"

try {
    try {
        $certificateBytes = [Convert]::FromBase64String($certificateBase64)
    } catch {
        throw "WINDOWS_CERTIFICATE is not a valid base64-encoded PFX: $($_.Exception.Message)"
    }
    [System.IO.File]::WriteAllBytes($pfxPath, $certificateBytes)

    $securePassword = ConvertTo-SecureString `
        -String $certificatePassword `
        -AsPlainText `
        -Force
    $imported = @(
        Import-PfxCertificate `
            -FilePath $pfxPath `
            -CertStoreLocation "Cert:\CurrentUser\My" `
            -Password $securePassword
    )
} finally {
    Remove-Item -LiteralPath $pfxPath -Force -ErrorAction SilentlyContinue
}

$certificate = $imported |
    Where-Object {
        $_.HasPrivateKey -and (Test-CodeSigningCertificate -Certificate $_)
    } |
    Select-Object -First 1

if ($null -eq $certificate) {
    throw "The PFX does not contain a code-signing certificate with a private key."
}

$now = Get-Date
if ($certificate.NotBefore -gt $now -or $certificate.NotAfter -le $now) {
    throw "The code-signing certificate is not currently valid."
}

$config = Get-Content -LiteralPath $configFile -Raw | ConvertFrom-Json
if ($null -eq $config.bundle) {
    throw "The Tauri configuration has no bundle object: $configFile"
}
if ($null -eq $config.bundle.windows) {
    $config.bundle |
        Add-Member -MemberType NoteProperty -Name "windows" -Value ([pscustomobject]@{})
}

$config.bundle.windows |
    Add-Member `
        -MemberType NoteProperty `
        -Name "certificateThumbprint" `
        -Value $certificate.Thumbprint `
        -Force
$config.bundle.windows |
    Add-Member -MemberType NoteProperty -Name "digestAlgorithm" -Value "sha256" -Force
$config.bundle.windows |
    Add-Member `
        -MemberType NoteProperty `
        -Name "timestampUrl" `
        -Value "http://timestamp.digicert.com" `
        -Force
$config.bundle.windows |
    Add-Member -MemberType NoteProperty -Name "tsp" -Value $true -Force

$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
$json = $config | ConvertTo-Json -Depth 100
[System.IO.File]::WriteAllText(
    $configFile,
    $json + [Environment]::NewLine,
    $utf8WithoutBom
)

if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_ENV)) {
    Add-Content `
        -LiteralPath $env:GITHUB_ENV `
        -Value "MINIDL_WINDOWS_CERTIFICATE_THUMBPRINT=$($certificate.Thumbprint)" `
        -Encoding utf8
}

Write-Host "Configured Tauri Authenticode signing with certificate $($certificate.Thumbprint)."
