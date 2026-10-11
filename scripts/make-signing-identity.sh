#!/usr/bin/env bash
set -euo pipefail
umask 077

if [[ ${1:-} == -h || ${1:-} == --help ]]; then
    echo 'Usage: scripts/make-signing-identity.sh [output-directory]'
    exit 0
fi
if (($# > 1)); then
    echo 'Error: expected at most one output directory.' >&2
    exit 1
fi
command -v openssl >/dev/null || { echo 'Error: openssl is required.' >&2; exit 1; }
directory=${1:-$HOME/.config/tinytrac/signing}
mkdir -p "$directory"
chmod 700 "$directory"
cd "$directory"
# Never accidentally replace the identity that grants access to existing tokens.
for file in tinytrac.key.pem tinytrac.cert.pem tinytrac.p12 password.txt tinytrac.p12.base64 openssl.cnf; do
    if [[ -e $file || -L $file ]]; then
        echo "Error: $file already exists; keep the existing identity or use an empty directory." >&2
        exit 1
    fi
done
cat > openssl.cnf <<'CONFIG'
[req]
prompt = no
distinguished_name = subject
x509_extensions = signing
[subject]
CN = tinytrac
[signing]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = codeSigning
subjectKeyIdentifier = hash
CONFIG
openssl rand -base64 32 > password.txt
openssl req -new -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
    -config openssl.cnf -keyout tinytrac.key.pem -out tinytrac.cert.pem
# Explicit legacy PBE + SHA-1 MAC works with macOS security import and OpenSSL 3.
# Unlike -legacy defaults, this avoids RC2 and needs no legacy provider to read.
openssl pkcs12 -export -name tinytrac -inkey tinytrac.key.pem -in tinytrac.cert.pem \
    -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1 \
    -passout file:password.txt -out tinytrac.p12
openssl base64 -A -in tinytrac.p12 -out tinytrac.p12.base64
chmod 600 tinytrac.key.pem tinytrac.cert.pem tinytrac.p12 password.txt tinytrac.p12.base64 openssl.cnf
cat <<EOF2
Created signing identity in $directory
Paste the contents of these files into GitHub Actions repository secrets:
  tinytrac.p12.base64 -> MACOS_SIGNING_P12_BASE64
  password.txt       -> MACOS_SIGNING_P12_PASSWORD
Keep this directory private and backed up. Reuse this identity for every release.
EOF2
