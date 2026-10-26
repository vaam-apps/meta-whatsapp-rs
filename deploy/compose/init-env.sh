#!/usr/bin/env bash
# Writes deploy/compose/.env once: the domain and the Meta app's secret you
# type in, and random values for the rest. The file is readable by you
# alone; it is gitignored (the repository's `.env` rule). Never commit it.
set -euo pipefail
cd "$(dirname "$0")"

if [ -e .env ]; then
    echo ".env exists: delete it first to write a new one (its vault key encrypts the stored tokens)." >&2
    exit 1
fi
command -v openssl >/dev/null || { echo "openssl is required" >&2; exit 1; }

read -rp "Domain pointing at this VM (e.g. wa.example.com): " domain
[ -n "$domain" ] || { echo "a domain is required" >&2; exit 1; }
read -rsp "Meta app secret (App Dashboard > App settings > Basic): " app_secret
echo
[ -n "$app_secret" ] || { echo "the app secret is required" >&2; exit 1; }

umask 077
cat > .env <<EOF
WA_DOMAIN=${domain}
WA_APP_SECRET=${app_secret}
# Enter this value as the webhook's verify token in the App Dashboard.
WA_VERIFY_TOKEN=$(openssl rand -hex 24)
# Encrypts the WABA tokens the service stores: back it up; without it they are lost.
WA_VAULT_KEY=$(openssl rand -base64 32)
WA_OTP_PEPPER=$(openssl rand -base64 48)
POSTGRES_PASSWORD=$(openssl rand -hex 24)
EOF
echo "Wrote $(pwd)/.env. The verify token is its WA_VERIFY_TOKEN line."
