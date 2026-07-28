#!/usr/bin/env bash
# Upload a trusted Authenticode PFX/P12 and its password to GitHub Actions.
# The private key stays in the source file and is never written into the repo.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  ./scripts/setup-windows-signing.sh [certificate.pfx] [--repo OWNER/REPO]

If the certificate path is omitted, the script prompts for it. The PFX/P12
must contain a currently valid Authenticode code-signing certificate and its
private key. You will be prompted securely for the PFX password.
EOF
}

die() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 ||
    die "'$1' is required but was not found in PATH."
}

certificate_path=""
repository=""

while (($# > 0)); do
  case "$1" in
    --repo)
      (($# >= 2)) || die "--repo requires OWNER/REPO."
      repository="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    -*)
      die "Unknown option: $1"
      ;;
    *)
      [[ -z "$certificate_path" ]] ||
        die "Only one certificate path may be supplied."
      certificate_path="$1"
      shift
      ;;
  esac
done

require_command base64
require_command gh
require_command openssl
require_command tr

if [[ -z "$certificate_path" ]]; then
  [[ -t 0 ]] ||
    die "Pass the PFX/P12 path when running without an interactive terminal."
  read -r -p "Trusted code-signing PFX/P12 path: " certificate_path
fi

[[ -n "$certificate_path" ]] || die "The certificate path cannot be empty."
[[ -f "$certificate_path" ]] ||
  die "Certificate file not found: $certificate_path"
[[ -r "$certificate_path" ]] ||
  die "Certificate file is not readable: $certificate_path"

gh auth status --hostname github.com >/dev/null 2>&1 ||
  die "GitHub CLI is not authenticated. Run 'gh auth login' first."

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -z "$repository" ]]; then
  repository="$(
    cd "$repository_root"
    gh repo view --json nameWithOwner --jq '.nameWithOwner' 2>/dev/null
  )" || die "Could not determine the GitHub repository. Use --repo OWNER/REPO."
else
  [[ "$repository" == */* ]] ||
    die "Invalid repository '$repository'; expected OWNER/REPO."
  repository="$(
    gh repo view "$repository" \
      --json nameWithOwner \
      --jq '.nameWithOwner' 2>/dev/null
  )" || die "Could not access the GitHub repository '$repository'."
fi

[[ -t 0 ]] ||
  die "An interactive terminal is required to enter the PFX password."
read -r -s -p "PFX password: " certificate_password
printf '\n'
[[ -n "$certificate_password" ]] ||
  die "The PFX password cannot be empty."
export MINIDL_PFX_PASSWORD="$certificate_password"

temporary_directory="$(mktemp -d "${TMPDIR:-/tmp}/minidl-signing.XXXXXX")"
certificate_pem="$temporary_directory/certificate.pem"

cleanup() {
  unset MINIDL_PFX_PASSWORD certificate_password
  rm -f -- "$certificate_pem"
  rmdir -- "$temporary_directory" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

pkcs12_compatibility=()
if ! openssl pkcs12 \
  -in "$certificate_path" \
  -clcerts \
  -nokeys \
  -passin env:MINIDL_PFX_PASSWORD \
  -out "$certificate_pem" 2>/dev/null; then
  rm -f -- "$certificate_pem"
  if openssl pkcs12 \
    -legacy \
    -in "$certificate_path" \
    -clcerts \
    -nokeys \
    -passin env:MINIDL_PFX_PASSWORD \
    -out "$certificate_pem" 2>/dev/null; then
    pkcs12_compatibility=(-legacy)
  else
    die "Could not open the PFX/P12. Check the file and password."
  fi
fi

if ! openssl pkcs12 \
  "${pkcs12_compatibility[@]}" \
  -in "$certificate_path" \
  -nocerts \
  -nodes \
  -passin env:MINIDL_PFX_PASSWORD 2>/dev/null |
  openssl pkey -noout >/dev/null 2>&1; then
  die "The PFX/P12 does not contain a usable private key."
fi

extended_key_usage="$(
  openssl x509 \
    -in "$certificate_pem" \
    -noout \
    -ext extendedKeyUsage 2>/dev/null || true
)"
case "$extended_key_usage" in
  *"Code Signing"* | *"1.3.6.1.5.5.7.3.3"*) ;;
  *)
    die "The certificate is not authorized for code signing."
    ;;
esac

openssl verify \
  -partial_chain \
  -CAfile "$certificate_pem" \
  "$certificate_pem" >/dev/null 2>&1 ||
  die "The certificate is expired or not valid yet."

certificate_subject="$(
  openssl x509 -in "$certificate_pem" -noout -subject -nameopt RFC2253
)"
certificate_subject="${certificate_subject#subject=}"
certificate_issuer="$(
  openssl x509 -in "$certificate_pem" -noout -issuer -nameopt RFC2253
)"
certificate_issuer="${certificate_issuer#issuer=}"
[[ "$certificate_subject" != "$certificate_issuer" ]] ||
  die "The certificate is self-signed and will not establish a trusted publisher."

certificate_expiry="$(
  openssl x509 -in "$certificate_pem" -noout -enddate
)"
certificate_expiry="${certificate_expiry#notAfter=}"
certificate_fingerprint="$(
  openssl x509 -in "$certificate_pem" -noout -fingerprint -sha256
)"
certificate_fingerprint="${certificate_fingerprint#sha256 Fingerprint=}"

printf '\nCertificate: %s\n' "$certificate_subject"
printf 'Expires:     %s\n' "$certificate_expiry"
printf 'SHA-256:     %s\n' "$certificate_fingerprint"
printf 'Repository:  %s\n\n' "$repository"

unset MINIDL_PFX_PASSWORD
printf 'Uploading encrypted signing credentials to GitHub Actions...\n'
base64 <"$certificate_path" |
  tr -d '\r\n' |
  gh secret set WINDOWS_CERTIFICATE --repo "$repository"
printf '%s' "$certificate_password" |
  gh secret set WINDOWS_CERTIFICATE_PASSWORD --repo "$repository"

printf '\nWindows release signing is configured for %s.\n' "$repository"
printf 'You may now delete or securely archive the local PFX/P12 copy.\n'
