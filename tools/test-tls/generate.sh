#!/usr/bin/env bash
# TEST-ONLY TLS material for the `rediss://` live tests (roadmap L21c): a CA,
# a server certificate (localhost, 127.0.0.1, redis-tls) and a client
# certificate, both signed by that CA, valid for 30 days. Nothing here is
# committed: the files go to $1, by default .test-tls/ at the repository
# root (gitignored).
#
# Used by `just test-live` before it starts compose.test.yaml, and by the
# devcontainer's `initializeCommand` before its sidecars start: the
# redis-tls service reads the server files, the tests the CA and the client
# files. The CA key is deleted once both certificates are signed, so nothing
# else can be signed with it; the other keys are left world-readable, so the
# Redis container's own user can read them. Never use these anywhere else.
#
# Does nothing while every file is there and the certificates are good for
# another day; otherwise regenerates all of them. Plain `openssl req` and
# `openssl x509` options only, so LibreSSL (macOS) works too.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
dir=${1:-$root/.test-tls}
files=(ca.crt server.crt server.key client.crt client.key)

fresh=1
for f in "${files[@]}"; do
    [ -s "$dir/$f" ] || fresh=0
done
if [ "$fresh" = 1 ] \
    && openssl x509 -checkend 86400 -noout -in "$dir/server.crt" >/dev/null 2>&1 \
    && openssl x509 -checkend 86400 -noout -in "$dir/client.crt" >/dev/null 2>&1; then
    exit 0
fi

mkdir -p "$dir"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

serial() { echo "0x$(openssl rand -hex 16)"; }

cat >"$work/ca.ext" <<'EOF'
basicConstraints = critical, CA:TRUE, pathlen:0
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash
EOF
cat >"$work/server.ext" <<'EOF'
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost, DNS:redis-tls, IP:127.0.0.1
authorityKeyIdentifier = keyid
EOF
cat >"$work/client.ext" <<'EOF'
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = clientAuth
authorityKeyIdentifier = keyid
EOF

# The CA: self-signed.
openssl req -new -newkey rsa:2048 -nodes -keyout "$work/ca.key" \
    -subj "/CN=meta-whatsapp-rs TEST-ONLY CA" -out "$work/ca.csr" 2>/dev/null
openssl x509 -req -in "$work/ca.csr" -signkey "$work/ca.key" -days 30 \
    -set_serial "$(serial)" -extfile "$work/ca.ext" -out "$work/ca.crt" 2>/dev/null

# The server and the client certificates, signed by the CA.
for who in server client; do
    openssl req -new -newkey rsa:2048 -nodes -keyout "$work/$who.key" \
        -subj "/CN=meta-whatsapp-rs TEST-ONLY $who" -out "$work/$who.csr" 2>/dev/null
    openssl x509 -req -in "$work/$who.csr" -CA "$work/ca.crt" -CAkey "$work/ca.key" \
        -days 30 -set_serial "$(serial)" -extfile "$work/$who.ext" \
        -out "$work/$who.crt" 2>/dev/null
done

for f in "${files[@]}"; do
    cp "$work/$f" "$dir/$f"
    chmod 0644 "$dir/$f"
done
chmod 0755 "$dir"
echo "test-tls: wrote ${files[*]} to $dir (test-only, 30 days)"
