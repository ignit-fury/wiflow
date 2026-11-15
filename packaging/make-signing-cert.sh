#!/bin/sh
# One-time setup: a self-signed "Wiflow Dev" code-signing certificate in the
# login keychain. Signing every build with the same certificate keeps macOS
# permissions (Microphone, Input Monitoring, Accessibility) across rebuilds;
# an ad-hoc signature changes on every build, so macOS treats each build as
# a new app. macOS asks for your password once (to trust the certificate).
set -eu
NAME="Wiflow Dev"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
if security find-identity -v -p codesigning | grep -q "\"$NAME\""; then
  echo "$NAME already exists"
  exit 0
fi
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
# A certificate left untrusted by an earlier failed run is reused, not
# re-imported: a second cert with the same name makes "Wiflow Dev" ambiguous.
if security find-certificate -c "$NAME" "$KEYCHAIN" >/dev/null 2>&1; then
  security find-certificate -c "$NAME" -p "$KEYCHAIN" > "$TMP/cert.pem"
else
  cat > "$TMP/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF
  # macOS's own openssl (LibreSSL): its .p12 files import cleanly with
  # `security`; Homebrew OpenSSL 3 defaults do not.
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -config "$TMP/cert.cnf" -keyout "$TMP/key.pem" -out "$TMP/cert.pem"
  PASS="wiflow-$$"
  /usr/bin/openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
    -name "$NAME" -passout "pass:$PASS" -out "$TMP/id.p12"
  security import "$TMP/id.p12" -k "$KEYCHAIN" -P "$PASS" -T /usr/bin/codesign
fi
security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$TMP/cert.pem"
security find-identity -v -p codesigning | grep "\"$NAME\"" || {
  echo "Wiflow Dev is not trusted for code signing yet. If it keeps failing, delete \"Wiflow Dev\" in Keychain Access\n  (login keychain, My Certificates / Certificates) and run this script again" >&2
  exit 1
}
echo "Created $NAME. Rebuild with: sh packaging/build-app.sh"
echo "If codesign asks to use the key, click Always Allow."
