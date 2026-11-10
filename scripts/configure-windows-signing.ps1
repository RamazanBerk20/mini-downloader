[CmdletBinding()]
param(
    # Publish the certificate thumbprint, then delete the certificate and its
    # private key again. The release build compiles the thumbprint in and must
    # run on a machine that never holds the key.
    [switch]$ThumbprintOnly
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
                if ($usage.Value -eq $codeSigningOid) { return $true }
            }
        }
    }
    return $false
}

$certificateBase64 = $env:WINDOWS_CERTIFICATE
$certificatePassword = $env:WINDOWS_CERTIFICATE_PASSWORD
if ([string]::IsNullOrWhiteSpace($certificateBase64)) {
    throw "WINDOWS_CERTIFICATE is required."
}
if ([string]::IsNullOrWhiteSpace($certificatePassword)) {
    throw "WINDOWS_CERTIFICATE_PASSWORD is required."
}

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
        throw "WINDOWS_CERTIFICATE is not a valid base64 PFX: $($_.Exception.Message)"
    }
    [System.IO.File]::WriteAllBytes($pfxPath, $certificateBytes)
    $securePassword = ConvertTo-SecureString $certificatePassword -AsPlainText -Force
    $imported = @(Import-PfxCertificate `
        -FilePath $pfxPath `
        -CertStoreLocation "Cert:\CurrentUser\My" `
        -Password $securePassword)
} finally {
    Remove-Item -LiteralPath $pfxPath -Force -ErrorAction SilentlyContinue
}

$certificate = $imported |
    Where-Object { $_.HasPrivateKey -and (Test-CodeSigningCertificate $_) } |
    Select-Object -First 1
if ($null -eq $certificate) {
    throw "The PFX has no code-signing certificate with a private key."
}
$now = Get-Date
if ($certificate.NotBefore -gt $now -or $certificate.NotAfter -le $now) {
    throw "The code-signing certificate is not currently valid."
}

if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_ENV)) {
    Add-Content $env:GITHUB_ENV `
        "MINIDL_WINDOWS_CERTIFICATE_THUMBPRINT=$($certificate.Thumbprint)" `
        -Encoding utf8
}
$env:MINIDL_WINDOWS_CERTIFICATE_THUMBPRINT = $certificate.Thumbprint
if ($ThumbprintOnly) {
    if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_OUTPUT)) {
        Add-Content $env:GITHUB_OUTPUT `
            "thumbprint=$($certificate.Thumbprint)" `
            -Encoding utf8
    }
    # -DeleteKey: without it the key container outlives the certificate and
    # stays usable by any process of this user.
    Remove-Item -LiteralPath "Cert:\CurrentUser\My\$($certificate.Thumbprint)" -DeleteKey -Force
    Write-Host "Read Authenticode certificate $($certificate.Thumbprint); the key was not kept."
    return
}
Write-Host "Imported Authenticode certificate $($certificate.Thumbprint)."
