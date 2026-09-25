#!/bin/bash
# Default-deny egress for the meta-whatsapp-rs dev container.
#
# Adapted from Anthropic's reference Claude Code devcontainer. Differences:
#
# - It fails closed. The default-DROP policies go in *first*, before anything
#   is fetched or resolved, and an EXIT trap re-asserts them on any failure.
#   The reference flushed every rule at the top and only set DROP at the end,
#   so any error in between (a GitHub rate limit, a DNS hiccup) left the
#   container with no rules and ACCEPT policies: unrestricted egress.
# - GitHub's ranges come from `api.github.com/meta`, retried, with a dated
#   snapshot baked into the image as the fallback (that endpoint allows 60
#   unauthenticated requests per hour per IP, which a few restarts behind one
#   NAT exhaust).
# - The allowlist adds what a Rust/WhatsApp workflow needs (crates.io,
#   rustup, Meta's Graph API and docs); sinkholed DNS answers are skipped;
#   IPv6 egress is dropped entirely, so the IPv4 allowlist cannot be
#   sidestepped over v6.
#
# Test hooks (never set in normal use; `sudo` resets the environment, so the
# unprivileged user cannot pass them through the sudoers rule):
#   WA_FIREWALL_GITHUB_META_URL   where to fetch GitHub's ranges
#   WA_FIREWALL_GITHUB_SNAPSHOT   the fallback snapshot file
set -Eeuo pipefail  # Exit on error (also in functions), undefined vars, pipeline failures
IFS=$'\n\t'         # Stricter word splitting

GITHUB_META_URL="${WA_FIREWALL_GITHUB_META_URL:-https://api.github.com/meta}"
GITHUB_SNAPSHOT="${WA_FIREWALL_GITHUB_SNAPSHOT:-/etc/meta-whatsapp-rs-firewall/github-meta-snapshot.json}"

have_ip6tables() {
    command -v ip6tables >/dev/null 2>&1 && ip6tables -S >/dev/null 2>&1
}

# Whatever fails from here on, egress stays denied. Rules added before the
# failure stay (they are all allowances that were meant to be there);
# anything not yet allowed stays blocked.
fail_closed() {
    local status=$?
    trap - EXIT
    if [ "$status" -ne 0 ]; then
        iptables -P INPUT DROP 2>/dev/null || true
        iptables -P FORWARD DROP 2>/dev/null || true
        iptables -P OUTPUT DROP 2>/dev/null || true
        # Keep loopback usable (language servers, local test services).
        iptables -C INPUT -i lo -j ACCEPT 2>/dev/null || iptables -I INPUT 1 -i lo -j ACCEPT 2>/dev/null || true
        iptables -C OUTPUT -o lo -j ACCEPT 2>/dev/null || iptables -I OUTPUT 1 -o lo -j ACCEPT 2>/dev/null || true
        if have_ip6tables; then
            ip6tables -P INPUT DROP 2>/dev/null || true
            ip6tables -P FORWARD DROP 2>/dev/null || true
            ip6tables -P OUTPUT DROP 2>/dev/null || true
        fi
        echo "ERROR: firewall setup failed (exit $status); egress stays DENIED by default." >&2
        echo "ERROR: only loopback, DNS, SSH, the host network and what was allowed before the failure get out." >&2
        echo "ERROR: fix the cause, then rerun: sudo /usr/local/bin/init-firewall.sh" >&2
    fi
    exit "$status"
}
trap fail_closed EXIT
trap 'echo "ERROR: line $LINENO: \`$BASH_COMMAND\` failed" >&2' ERR
trap 'exit 130' INT TERM

# 1. Extract Docker DNS info BEFORE any flushing.
DOCKER_DNS_RULES=$(iptables-save -t nat | grep "127\.0\.0\.11" || true)

# 2. Deny by default FIRST, then flush: from this line on, nothing leaves the
#    container unless a rule below allows it.
iptables -P INPUT DROP
iptables -P FORWARD DROP
iptables -P OUTPUT DROP
if have_ip6tables; then
    ip6tables -P INPUT DROP
    ip6tables -P FORWARD DROP
    ip6tables -P OUTPUT DROP
fi

iptables -F
iptables -X
iptables -t nat -F
iptables -t nat -X
iptables -t mangle -F
iptables -t mangle -X
ipset destroy allowed-domains 2>/dev/null || true

# 3. Selectively restore ONLY internal Docker DNS resolution.
if [ -n "$DOCKER_DNS_RULES" ]; then
    echo "Restoring Docker DNS rules..."
    iptables -t nat -N DOCKER_OUTPUT 2>/dev/null || true
    iptables -t nat -N DOCKER_POSTROUTING 2>/dev/null || true
    echo "$DOCKER_DNS_RULES" | xargs -L 1 iptables -t nat
else
    echo "No Docker DNS rules to restore"
fi

# 4. The base allowances: loopback, replies, DNS, SSH, the host network.
iptables -A INPUT -i lo -j ACCEPT
iptables -A OUTPUT -o lo -j ACCEPT
iptables -A INPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
iptables -A OUTPUT -p udp --dport 53 -j ACCEPT
iptables -A OUTPUT -p tcp --dport 22 -j ACCEPT

HOST_IP=$(ip route | grep default | cut -d" " -f3)
if [ -z "$HOST_IP" ]; then
    echo "ERROR: Failed to detect host IP"
    exit 1
fi
HOST_NETWORK=$(echo "$HOST_IP" | sed "s/\.[0-9]*$/.0\/24/")
echo "Host network detected as: $HOST_NETWORK"
iptables -A INPUT -s "$HOST_NETWORK" -j ACCEPT
iptables -A OUTPUT -d "$HOST_NETWORK" -j ACCEPT

# 5. The allowlist: one ipset, consulted by one rule. Addresses added to the
#    set below take effect immediately; everything else is rejected (REJECT,
#    not DROP, for immediate feedback).
ipset create allowed-domains hash:net
iptables -A OUTPUT -m set --match-set allowed-domains dst -j ACCEPT
iptables -A OUTPUT -j REJECT --reject-with icmp-admin-prohibited

# IPv6: nothing on the allowlist needs it; drop everything but loopback.
if have_ip6tables; then
    ip6tables -F
    ip6tables -X
    ip6tables -A INPUT -i lo -j ACCEPT
    ip6tables -A OUTPUT -o lo -j ACCEPT
    echo "IPv6 egress dropped"
fi
echo "Default-deny egress in place; adding allowances"

# Resolve a domain and add its addresses to the allowlist.
#
# Two changes from the reference script, both found by running it behind an
# ad-blocking resolver: sinkholed answers (0.0.0.0, 127.x) are skipped
# instead of added, and `ipset add -exist` tolerates duplicates. Telemetry
# domains are optional (warn if unresolvable); everything else is required
# (fail, which leaves egress denied).
add_domain() {
    local domain="$1" required="$2"
    echo "Resolving $domain..."
    local ips
    ips=$(dig +noall +answer A "$domain" | awk '$4 == "A" {print $5}' \
        | grep -Ev '^(0\.0\.0\.0|127\.)' || true)
    if [ -z "$ips" ]; then
        if [ "$required" = "required" ]; then
            echo "ERROR: Failed to resolve $domain"
            exit 1
        fi
        echo "WARNING: $domain did not resolve to a routable address; skipping"
        return 0
    fi
    while read -r ip; do
        if [[ ! "$ip" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]]; then
            echo "ERROR: Invalid IP from DNS for $domain: $ip"
            exit 1
        fi
        echo "Adding $ip for $domain"
        ipset add -exist allowed-domains "$ip"
    done < <(echo "$ips")
}

# The IPv4 ranges of GitHub's `web`, `api` and `git` services from a meta
# JSON document, one per line; fails on anything that is not a plausible
# IPv4 CIDR (a prefix shorter than /12 would be a suspiciously large hole).
github_ranges() {
    local json="$1"
    jq -e '(.web | type == "array") and (.api | type == "array") and (.["git"] | type == "array")' \
        "$json" >/dev/null || return 1
    local cidrs
    cidrs=$(jq -r '(.web + .api + .["git"])[] | select(contains(":") | not)' "$json") || return 1
    [ -n "$cidrs" ] || return 1
    while read -r cidr; do
        if [[ ! "$cidr" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}/([0-9]{1,2})$ ]] \
            || [ "${BASH_REMATCH[1]}" -lt 12 ] || [ "${BASH_REMATCH[1]}" -gt 32 ]; then
            echo "ERROR: Invalid CIDR range in GitHub meta: $cidr" >&2
            return 1
        fi
    done < <(echo "$cidrs")
    echo "$cidrs" | aggregate -q
}

# 6. GitHub. api.github.com itself first, so the meta request can get out.
add_domain "api.github.com" required

meta_file=$(mktemp)
ranges=""
echo "Fetching GitHub IP ranges from $GITHUB_META_URL..."
for attempt in 1 2 3; do
    status=$(curl -sS --connect-timeout 5 --max-time 15 -o "$meta_file" -w '%{http_code}' \
        "$GITHUB_META_URL" 2>/dev/null) || status="000"
    if [ "$status" = "200" ] && ranges=$(github_ranges "$meta_file"); then
        echo "GitHub ranges: live ($GITHUB_META_URL)"
        break
    fi
    ranges=""
    if [ "$status" = "403" ] || [ "$status" = "429" ]; then
        # Unauthenticated rate limit (60/hour/IP): retrying within seconds
        # cannot help.
        echo "WARNING: GitHub meta answered HTTP $status (rate limited); not retrying"
        break
    fi
    echo "WARNING: GitHub meta attempt $attempt failed (HTTP $status)"
    if [ "$attempt" -lt 3 ]; then
        sleep $((attempt * 2))
    fi
done
rm -f "$meta_file"

if [ -z "$ranges" ]; then
    if [ ! -r "$GITHUB_SNAPSHOT" ]; then
        echo "ERROR: GitHub ranges unavailable and no snapshot at $GITHUB_SNAPSHOT"
        exit 1
    fi
    snapshot_date=$(jq -r '.snapshot_date // "undated"' "$GITHUB_SNAPSHOT")
    if ! ranges=$(github_ranges "$GITHUB_SNAPSHOT"); then
        echo "ERROR: the GitHub ranges snapshot $GITHUB_SNAPSHOT is invalid"
        exit 1
    fi
    echo "WARNING: using the GitHub ranges snapshot from $snapshot_date baked into the image;"
    echo "WARNING: GitHub hosts added since then stay blocked until a live fetch succeeds."
fi

while read -r cidr; do
    echo "Adding GitHub range $cidr"
    ipset add -exist allowed-domains "$cidr"
done < <(echo "$ranges")

# 7. Everything else.
for domain in \
    "registry.npmjs.org" \
    "api.anthropic.com" \
    "marketplace.visualstudio.com" \
    "vscode.blob.core.windows.net" \
    "update.code.visualstudio.com" \
    "crates.io" \
    "index.crates.io" \
    "static.crates.io" \
    "static.rust-lang.org" \
    "docs.rs" \
    "graph.facebook.com" \
    "developers.facebook.com" \
    "lookaside.fbsbx.com"; do
    add_domain "$domain" required
done
for domain in "sentry.io" "statsig.com" "statsig.anthropic.com"; do
    add_domain "$domain" optional
done

# 8. Verify. A failure here exits non-zero; the policies stay DROP.
echo "Firewall configuration complete"
echo "Verifying firewall rules..."
if curl --connect-timeout 5 https://example.com >/dev/null 2>&1; then
    echo "ERROR: Firewall verification failed - was able to reach https://example.com"
    exit 1
else
    echo "Firewall verification passed - unable to reach https://example.com as expected"
fi

# crates.io (cargo needs it)
if ! curl --connect-timeout 5 https://index.crates.io/config.json >/dev/null 2>&1; then
    echo "ERROR: Firewall verification failed - unable to reach https://index.crates.io"
    exit 1
else
    echo "Firewall verification passed - able to reach https://index.crates.io as expected"
fi

# No check for graph.facebook.com: its DNS answer rotates between Meta edge
# addresses within seconds (TTL 30 s), so a check here fails at random and
# would leave the container locked down. See docs/dev-environment.md.

# GitHub API
if ! curl --connect-timeout 5 https://api.github.com/zen >/dev/null 2>&1; then
    echo "ERROR: Firewall verification failed - unable to reach https://api.github.com"
    exit 1
else
    echo "Firewall verification passed - able to reach https://api.github.com as expected"
fi
