# meta-whatsapp-server on one VM (Docker Compose)

A test deployment: the service, Postgres, and Caddy for HTTPS, so that Meta
can deliver webhooks (message statuses, inbound messages, template reviews)
and you can read them back with `GET /v1/events`. It is not the published
image (roadmap M4) and not a production setup: read
[Before production](#before-production). The service itself is described in
[docs/guides/server.md](../../docs/guides/server.md).

```text
Meta ─ HTTPS ─► Caddy :443 ─► server :8080  /webhooks/meta (public listener)
you (on the VM) ──────────► server :8081  /v1, /v1/events, /v1/admin  (127.0.0.1 only)
                               │
                               └─► Postgres (volume `db`)
```

## 1. The VM

- Docker with the Compose plugin, `git` and `openssl`.
- A domain (or subdomain) whose DNS `A`/`AAAA` record points at the VM, and
  ports 80 and 443 open to the internet: Caddy obtains the certificate from
  Let's Encrypt on first start and renews it.

Without ports 80 and 443, use Cloudflare's proxy instead:
[Behind Cloudflare](#behind-cloudflare-no-ports-80-or-443), then continue
at step 2.

## 2. Start it

```bash
git clone https://github.com/vaam-apps/meta-whatsapp-rs.git
cd meta-whatsapp-rs/deploy/compose
./init-env.sh                 # asks for the domain and the Meta app secret; writes .env
docker compose up -d --build  # the first build compiles the service: ~10–20 minutes
docker compose ps             # server and db `healthy`, caddy `running`
```

`init-env.sh` generates the verify token, the vault key, the OTP pepper and
the database password. `.env` is readable by you alone and gitignored;
**back it up**: the vault key encrypts the access tokens the service stores,
and without it they cannot be read.

Check that Meta will be able to verify the callback (the service answers the
challenge only with the right verify token):

```bash
set -a; . ./.env; set +a
curl -sS "https://$WA_DOMAIN/webhooks/meta?hub.mode=subscribe&hub.verify_token=$WA_VERIFY_TOKEN&hub.challenge=ok"
# → ok
```

## 3. A tenant, its key, and the WhatsApp account

On the VM (the admin API listens on its loopback only):

```bash
ADMIN_KEY=$(docker compose exec -T server meta-whatsapp-server admin create-admin-key --name ops)

curl -sS -X POST http://127.0.0.1:8081/v1/admin/tenants \
  -H "Authorization: Bearer $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"id": "sshop", "name": "SShop"}'

curl -sS -X POST http://127.0.0.1:8081/v1/admin/tenants/sshop/keys \
  -H "Authorization: Bearer $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"scopes": ["numbers", "send", "events"], "name": "tests"}'
# → {"key": "wak_…", …}: shown once; keep it in a secret store
```

Attach the WhatsApp Business Account with a system user token that can reach
it. The service lists its numbers with that token, binds them to the tenant,
stores the token encrypted, and subscribes the app to the account's
webhooks. Pass the body from a file so the token stays out of your shell
history, and delete the file afterwards:

```bash
# attach.json: {"waba_id": "<WABA id>", "token": "EAAG…"}
curl -sS -X POST http://127.0.0.1:8081/v1/admin/tenants/sshop/wabas \
  -H "Authorization: Bearer $ADMIN_KEY" -H "Content-Type: application/json" \
  -d @attach.json && shred -u attach.json
```

Under Meta's account model evolution the id to attach is the WABA id your
templates and billing live under (now named the Messaging Account; it kept
the WABA's id).

## 4. Point the Meta app at it

In the App Dashboard (WhatsApp, Configuration), or with the Meta connector:

- **Callback URL**: `https://<WA_DOMAIN>/webhooks/meta`
- **Verify token**: the `WA_VERIFY_TOKEN` line of `.env`
- **Webhook fields**: `messages` at least (statuses arrive under it), and
  `message_template_status_update` for template reviews.

Meta may not send real webhooks to an app in development mode: publish the
app (App Dashboard, Publish) once its settings let you.

## 5. Read the statuses

The `/v1` API is on the internal listener, which the service keeps off the
internet: read it on the VM, or from your machine through an SSH tunnel
(`ssh -N -L 8081:127.0.0.1:8081 <vm>`), with the tenant key:

```bash
curl -sS "http://127.0.0.1:8081/v1/events?types=status_updated" \
  -H "Authorization: Bearer $TENANT_KEY"
```

Each `status_updated` event's `data.status` carries the `wamid` a send
returned (`id`), the state (`sent`, `delivered`, `read`, `failed`) and, when
it failed, Meta's `errors`. Pass the answer's `next_after` as `after=` to
read on. `docker compose logs -f server` shows each delivery as it arrives. A
status for a number no tenant holds (before step 3's attach) is kept for
operators only and never appears in a tenant's events.

## Behind Cloudflare (no ports 80 or 443)

Cloudflare serves the domain on 443 with its own certificate and forwards
to the VM on a port you choose (8443 here). Caddy presents a Cloudflare
Origin CA certificate there, so nothing asks Let's Encrypt for one and
nothing listens on 80 or 443. Meta sees an ordinary
`https://<WA_DOMAIN>/webhooks/meta`.

In the Cloudflare dashboard, for the zone:

1. **DNS**: an `A` (or `AAAA`) record for the subdomain, pointing at the
   VM, **proxied** (orange cloud).
2. **SSL/TLS → Overview**: encryption mode **Full (strict)**.
3. **SSL/TLS → Origin Server → Create certificate**: for the subdomain, in
   PEM. Save the certificate as `deploy/compose/certs/origin.pem` and the
   private key as `deploy/compose/certs/origin-key.pem` on the VM
   (`chmod 600` the key; `certs/` is gitignored). Cloudflare shows the key
   once.
4. **Rules → Origin Rules → Create rule**: when the hostname equals the
   subdomain, rewrite the **destination port** to 8443.
5. **Security → WAF → Custom rules**, recommended: block requests to
   `/webhooks/meta` that do not come from Meta's network, with the
   expression
   `(http.host eq "<subdomain>" and http.request.uri.path eq "/webhooks/meta" and ip.src.asnum ne 32934)`
   and the action **Block**. This is the "admit only Meta" filter the
   service guide asks for; the origin only ever sees Cloudflare's
   addresses, so it has to live here.

On the VM, admit the origin port from
[Cloudflare's IP ranges](https://www.cloudflare.com/ips/) only (the VM's
firewall or the provider's security group), then answer **y** to
`init-env.sh`'s Cloudflare question, which writes
`COMPOSE_FILE=compose.yaml:compose.cloudflare.yaml` and `WA_ORIGIN_PORT`
into `.env`: every `docker compose` command then uses the override, and
step 2 is unchanged. The challenge check of step 2 goes through
Cloudflare, so it also proves the DNS record, the Origin Rule and the
certificate.

## Before production

- **Admit `/webhooks/meta` only from Meta.** Every delivery's signature is
  checked, but only after its body is read; the service guide asks the
  ingress to admit only Meta's webhook IP ranges or mutual TLS with Meta's
  client certificate. The default Caddyfile does neither; behind Cloudflare,
  step 5 of [Behind Cloudflare](#behind-cloudflare-no-ports-80-or-443) does
  it by Meta's network (AS32934).
- Secrets travel as environment variables, which `docker inspect` shows to
  anyone with access to the Docker daemon. The service also reads
  `<NAME>_FILE` for each secret: move them to files (Docker secrets) there.
- Back up the `db` volume, and keep `.env`'s vault key with the backup: a
  backup without it holds unreadable tokens.
- Run `meta-whatsapp-server migrate` as a job and set
  `WA_SERVER_MIGRATE=skip` once more than one replica runs.

## Updating and stopping

```bash
git pull && docker compose up -d --build   # rebuild and restart the service
docker compose down                        # stop; volumes (data, certificates) stay
```
