#!/bin/sh
# Create a self-signed local code-signing identity named "Writely Local" in
# the login keychain, so macOS keeps Writely's Accessibility permission
# across rebuilds (ad-hoc signatures change every build; a named cert is
# stable). Run once on each Mac; safe to re-run (exits 0 if it exists).
# Writes to the login keychain only; never prints the throwaway p12 password.
set -euo pipefail

CERT_NAME="Writely Local"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -p codesigning 2>/dev/null | grep -q "$CERT_NAME"; then
  echo "create-signing-cert: an identity named '$CERT_NAME' already exists; nothing to do."
  exit 0
fi

TMPDIR="$(mktemp -d)" || {
  echo "create-signing-cert: could not create a temp dir" >&2
  exit 1
}
trap 'rm -rf "$TMPDIR"' EXIT INT TERM

KEY="$TMPDIR/writely-local.key"
CRT="$TMPDIR/writely-local.crt"
P12="$TMPDIR/writely-local.p12"
CFG="$TMPDIR/openssl.cnf"

# Random throwaway password for the temp .p12; never printed or stored.
PASS="$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')" || {
  echo "create-signing-cert: could not generate a random password" >&2
  exit 1
}
if [ -z "${PASS:-}" ]; then
  echo "create-signing-cert: could not generate a random password" >&2
  exit 1
fi

cat > "$CFG" <<EOF || {
[req]
distinguished_name = dn
x509_extensions = v3_req
prompt = no
[dn]
CN = $CERT_NAME
[v3_req]
extendedKeyUsage = critical,codeSigning
keyUsage = critical,digitalSignature
EOF
  echo "create-signing-cert: could not write the openssl config" >&2
  exit 1
}

echo "create-signing-cert: creating a self-signed code-signing certificate '$CERT_NAME' (RSA 2048, 10 years)..."
/usr/bin/openssl req -x509 -newkey rsa:2048 -keyout "$KEY" -out "$CRT" -days 3650 -nodes -config "$CFG" || {
  echo "create-signing-cert: openssl failed to create the key and certificate" >&2
  exit 1
}

/usr/bin/openssl pkcs12 -export -out "$P12" -inkey "$KEY" -in "$CRT" -passout pass:"$PASS" || {
  echo "create-signing-cert: openssl failed to package the certificate" >&2
  exit 1
}

echo "create-signing-cert: importing '$CERT_NAME' into the login keychain..."
security import "$P12" -k "$KEYCHAIN" -T /usr/bin/codesign -P "$PASS" || {
  echo "create-signing-cert: security import failed" >&2
  exit 1
}

echo "create-signing-cert: done: identity '$CERT_NAME' is in the login keychain."
echo "The first time bundle-macos.sh signs with it, macOS may ask to let codesign use the key; click \"Always Allow\"."
