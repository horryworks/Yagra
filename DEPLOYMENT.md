# Deploying Yagra

This guide covers **how to deploy Yagra**, not how to use it. It spans the full matrix:

|                     | **Docker Compose**                        | **Native (no Docker)**       |
|---------------------|-------------------------------------------|------------------------------|
| **Single node**     | [A](#a--single-node-docker-build) · [B](#b--single-node-docker-pull) | [C](#c--single-node-native)  |
| **Distributed pollers** | [D](#d--distributed-pollers-docker)   | [E](#e--distributed-pollers-native) |

New to Yagra? Start with **[A](#a--single-node-docker-build)** (one command) or **[B](#b--single-node-docker-pull)** (production-style, pulls pre-built images). Reach for **[D](#d--distributed-pollers-docker)** once you need pollers at remote sites.

日本語版: **[DEPLOYMENT.ja.md](DEPLOYMENT.ja.md)**.

---

## Topology & backing services

Yagra is two long-running binaries plus a static WebUI, backed by five stores plus the bus:

- **Yagra-core** (`yagra-core`) — orchestration, scheduler, northbound REST API (`/api/v1`) + Prometheus `/metrics`. **Requires PostgreSQL + NATS + VictoriaMetrics; Redis is optional** (best-effort poller-liveness/assignment mirror — its absence only degrades, never blocks startup). If any of the three required URLs is unset, core drops to an in-memory **skeleton** mode instead of running live.
- **Yagra-poller** (`yagra-poller`) — stateless ICMP/SNMP/API worker. **Talks to NATS only.** Device credentials, job specs, and results all flow over the bus; the poller never touches PostgreSQL/Redis/VictoriaMetrics. This is what makes it horizontally scalable and remotely deployable.
- **Yagra-web** — a React/Vite SPA built to static files and served by nginx, which reverse-proxies `/api` to core. It is a **separate artifact** from core (core serves only the API + metrics, no static files).

| Store / bus | Role | Needed by |
|---|---|---|
| **PostgreSQL** | Metadata: nodes, config, thresholds, users, alert history | core (required) |
| **NATS** (JetStream) | core⇄poller bus: jobs, working sets, results, events | core + poller (required) |
| **VictoriaMetrics** | TSDB: metrics body store | core (required) |
| **Redis** | Ephemeral: poller liveness/assignment mirror | core (optional) |
| **VictoriaLogs** | Passive-event log store: powers passive-event search | core (optional) |
| **ClickHouse** | Traffic-flow store: powers the traffic-flow features | core (optional) |

> **Golden rule for scale-out:** distributing polling is a *config change*, not a rewrite. The single-node compositions and the distributed ones run the same images — you add remote pollers and, for a WAN bus, turn on NATS TLS+auth.

### Ports

| Port (container/bind) | Host default | Env | Purpose | Exposed? |
|---|---|---|---|---|
| `8080` | `8080` | `YAGRA_API_ADDR` (native) / `YAGRA_API_PORT` (compose) | core northbound API + `/metrics` | yes |
| `8080` (web nginx) | `3000` | `YAGRA_WEB_PORT` | WebUI | yes |
| `1514/udp` | `514` | `YAGRA_SYSLOG_BIND` / `YAGRA_SYSLOG_PORT` | syslog intake (poller) | opt-in |
| `1162/udp` | `162` | `YAGRA_TRAP_BIND` / `YAGRA_TRAP_PORT` | SNMP trap intake (poller) | opt-in |
| `2055/udp` | `2055` | `YAGRA_FLOW_BIND` / `YAGRA_FLOW_PORT` | NetFlow v5/v9 / IPFIX intake (poller) | opt-in |
| `6343/udp` | `6343` | `YAGRA_SFLOW_BIND` / `YAGRA_SFLOW_PORT` | sFlow v5 intake (poller) | opt-in |
| `9100` | — | (fixed) | poller Prometheus `/metrics` | native only |
| `4222` | — | `YAGRA_NATS_PORT` | NATS bus | internal; published **only** with TLS+auth (D) |
| `5432` / `6379` / `8428` / `9428` / `8123` | — | — | PostgreSQL / Redis / VictoriaMetrics / VictoriaLogs / ClickHouse | internal only |

> The MCP tool surface (`/mcp`, opt-in via `YAGRA_ENABLE_MCP`) is served on the API port `8080` — it does not open a separate port.

> **Outbound (forwarding).** Settings ▸ Forwarding relays received syslog / SNMP traps / flow
> exports on to external collectors, or streams them into **Google BigQuery** as queryable rows.
> **Core** does the sending — not the pollers — so only core needs egress: to the collector's
> `host:port` (UDP, TCP, or TLS), and for a BigQuery destination to **`bigquery.googleapis.com` and
> `oauth2.googleapis.com` over HTTPS** (plus the GCE metadata server at `169.254.169.254` when using
> Workload Identity instead of a stored key). Nothing is sent until you add a destination. A TLS
> destination verifies the collector's certificate against the container's system trust store; for a
> private CA, paste its PEM into the destination — there is no way to disable verification.
>
> **BigQuery destinations** need the dataset to exist already — Yagra creates the *table* (day
> partitioned, clustered) but never the dataset, because a dataset's region cannot be changed
> afterwards and choosing your data residency silently would be wrong. Grant the identity the
> **BigQuery Data Editor** role on the dataset. Rows are normalized and typed; the original bytes are
> deliberately **not** stored, so pair it with a relay destination if you also need byte-exact
> archival. Streaming inserts are billed by Google.
>
> **Bus bandwidth cost.** So that forwarding can relay what a device actually sent, pollers carry the
> original bytes to core whether or not any destination exists today: passive events gain a base64
> `raw` field (**1.45–1.64× on `yagra.events`**, measured on real device traffic), and every received
> flow datagram is relayed verbatim on `yagra.flows.raw`, on top of the aggregated `yagra.flows`
> stream. The flow cost depends on how densely your exporter packs its datagrams, and the spread is
> wide: a densely packed NetFlow v9 export (~1400 B, ~30 records) works out to **≈370 kbit/s at 1000
> flows/s**, while a device that emits small frequent datagrams — measured on a real UniFi gateway —
> costs about **~1.0 Mbit/s**. Budget **0.4–1.0 Mbit/s per 1000
> flows/s** and scale linearly (≈4–10 Mbit/s at 10 000). This carriage is deliberate — a capture toggle
> would make forwarding fidelity depend on configuration rather than being a property of the system —
> but it is real WAN traffic for a **remote-site poller**, so size the site link accordingly. Core
> itself pays nothing per message when no destination is configured.

---

## A — Single node, Docker (build from source)<a id="a--single-node-docker-build"></a>

The developer / all-in-one box. `docker-compose.yml` **builds** the images locally (tagged `:dev`) and runs the whole stack — core, poller, web, and all five stores — on one host.

```bash
git clone https://github.com/horryworks/Yagra.git
cd Yagra
docker compose up --build          # build + start the full single-node stack
```

Then open the WebUI at **http://localhost:3000** (API at http://localhost:8080).

**First login.** `YAGRA_ADMIN_PASSWORD` is unset by default, so core generates a one-time random `admin` password and prints it **once** in its logs:

```bash
docker compose logs core | grep -i password
```

Log in as `admin` with it and change it. To choose your own instead, uncomment `YAGRA_ADMIN_PASSWORD` under the `core` service in `docker-compose.yml`.

**What's running.** Web on host `:3000`, API on `:8080`; the poller listens for syslog on `:514/udp` and SNMP traps on `:162/udp`; PostgreSQL/Redis/NATS/VictoriaMetrics stay on the internal Docker network. Migrations run automatically on core startup — no manual step. Named volumes `pgdata` and `vmdata` persist data across `docker compose down`/`up`.

> This composition is fine for evaluation and dev. For anything you care about, use **B** (pinned images + a persistent KEK so stored credentials survive restarts).

---

## B — Single node, Docker (pull pre-built images)<a id="b--single-node-docker-pull"></a>

The production-style single-node deployment. `docker-compose.deploy.yml` **pulls** images from GHCR (no local build), is fully env-parameterized via `.env`, and adds a one-shot `kek-init` that writes a persistent key-encryption key so stored monitoring credentials survive redeploys.

```bash
git clone https://github.com/horryworks/Yagra.git
cd Yagra
cp .env.example .env                # then edit .env (see below)

YAGRA_IMAGE_TAG=latest docker compose -f docker-compose.deploy.yml pull
YAGRA_IMAGE_TAG=latest docker compose -f docker-compose.deploy.yml up -d
```

`YAGRA_IMAGE_TAG` selects the image tag: `latest` is the latest **stable** release (pre-releases never move it); a `v<version>` tag pins one release; the `<git-sha>` of a release is an immutable reference to exactly that build (rollback = re-run with an older tag). Only releases are published — development builds never reach the registry, so every tag you can pull is a release.

Want to know what a running container was built from? `docker exec yagra-core-1 cat /etc/yagra-source-ref` prints the commit, and `/etc/yagra-build-profile` prints the compile profile.

**Configure `.env`** (copied from `.env.example`). The essentials:

```ini
POSTGRES_PASSWORD=change-me            # change for any non-throwaway box
YAGRA_API_PORT=8080                    # host port for the API
YAGRA_WEB_PORT=3000                    # host port for the WebUI
# YAGRA_ADMIN_PASSWORD=choose-a-strong-password   # else a one-time random one is logged
# YAGRA_PUBLIC_DASHBOARD=false         # true = read-only dashboards without login
```

**Credential persistence (important).** The `kek-init` service writes a 32-byte KEK into the `kekdata` volume once and never overwrites it; core mounts it read-only at `YAGRA_KEK_FILE=/kek/key`. Without a persistent KEK, core falls back to an **ephemeral** key regenerated on every restart, and all stored credentials (SNMP communities, API tokens) become undecryptable after a redeploy. The compose file wires this up for you — just don't delete the `kekdata` volume.

**Upgrades.** Pull a newer tag and `up -d` again:

```bash
YAGRA_IMAGE_TAG=v0.1.4 docker compose -f docker-compose.deploy.yml pull
YAGRA_IMAGE_TAG=v0.1.4 docker compose -f docker-compose.deploy.yml up -d
```

Migrations are expand-contract and run automatically; `pgdata`/`vmdata`/`kekdata` are preserved. See [Upgrades & backups](#upgrades--backups).

---

## C — Single node, native (no Docker)<a id="c--single-node-native"></a>

Running the binaries directly. You provision the stores yourself, build the workspace, and run `yagra-core` + `yagra-poller` as services (e.g. systemd).

### 1. Provision the backing stores

Install and start, reachable from the host that will run core:

- **PostgreSQL 17** — create a database and role (core runs migrations itself; it does **not** create the database):
  ```sql
  CREATE ROLE yagra LOGIN PASSWORD 'yagra';
  CREATE DATABASE yagra OWNER yagra;
  ```
- **NATS 2.x with JetStream** — `nats-server -js`
- **VictoriaMetrics** — `victoria-metrics-prod --retentionPeriod=12` (12 months, single tier)
- **Redis 7** *(optional)* — only enables the poller liveness/assignment mirror

### 2. Build the workspace

Requires **Rust 1.90** and (for the WebUI) **Node 22**. Build from the repo root so the vendored `snmp2` patch (`[patch.crates-io]` in `Cargo.toml`) applies:

```bash
cargo build --release --workspace           # → target/release/yagra-core, target/release/yagra-poller
cd web && npm ci && npm run build           # → web/dist/  (static SPA bundle)
```

### 3. Provision the KEK (do this before first core start)

Envelope-encryption master key — a persistent 32-byte file. Without it, core boots with an **ephemeral dev key** and credentials won't survive a restart.

```bash
head -c 32 /dev/urandom > /etc/yagra/kek && chmod 0400 /etc/yagra/kek
```

### 4. Run core

```bash
export YAGRA_DATABASE_URL="postgres://yagra:yagra@localhost:5432/yagra"
export YAGRA_BUS_URL="nats://localhost:4222"
export YAGRA_TSDB_URL="http://localhost:8428"
export YAGRA_REDIS_URL="redis://localhost:6379"     # optional
export YAGRA_LOGS_URL="http://localhost:9428"       # optional (else events stay in PostgreSQL)
export YAGRA_KEK_FILE="/etc/yagra/kek"
export YAGRA_API_ADDR="0.0.0.0:8080"                # default
# export YAGRA_ADMIN_PASSWORD="choose-a-strong-password"   # else a one-time random one is logged
export RUST_LOG=info

./target/release/yagra-core
```

On startup core connects to the stores, **runs its embedded migrations automatically**, seeds built-in profiles/catalog, and serves `/api/v1` + `/metrics` on `YAGRA_API_ADDR`. If `YAGRA_ADMIN_PASSWORD` was unset, grep the logs for the one-time `admin` password.

### 5. Serve the WebUI

`web/dist/` is a static bundle; serve it with any web server and reverse-proxy `/api` to core. Mirror the shipped nginx config (`web/nginx.conf`): SSE needs `proxy_buffering off` and a long `proxy_read_timeout`, and the SPA needs a `try_files … /index.html` fallback. Point the proxy at `http://<core-host>:8080`.

### 6. Run the poller

The poller needs raw sockets for ICMP. Grant the capability to the binary (so it can run non-root) or run it as root:

```bash
sudo setcap cap_net_raw+ep ./target/release/yagra-poller

export YAGRA_BUS_URL="nats://localhost:4222"
export YAGRA_POLLER_ID="poller-1"          # unique per poller; defaults to hostname
export YAGRA_POLLER_POOL="default"
# Optional passive-event listeners (need root or CAP_NET_BIND_SERVICE for :514/:162):
# export YAGRA_SYSLOG_BIND="0.0.0.0:1514"
# export YAGRA_TRAP_BIND="0.0.0.0:1162"
export RUST_LOG=info

./target/release/yagra-poller
```

The poller exposes its own Prometheus `/metrics` on `0.0.0.0:9100`.

> **Optional: PDF reports.** Reports → PDF export shells out to `wkhtmltopdf` (the patched-Qt build). If it's not installed, PDF export returns HTTP 503 (`pdf_unavailable`); HTML/CSV export still work.

---

## D — Distributed pollers, Docker<a id="d--distributed-pollers-docker"></a>

Run the full stack centrally (as in **B**) and add pollers at remote sites. Each poller polls its site's devices locally and streams results back over the bus. Nodes carry a `pool` attribute; core's coordinator assigns each pool's nodes across its live pollers by consistent hashing and fails them over automatically.

> **The bus carries plaintext device credentials.** On one host that's fine (internal Docker network, nothing exposed). The moment the bus crosses a trust boundary to a remote site, it **must** be TLS-encrypted and authenticated first. Do **not** publish `:4222` plaintext.

### Step 1 — Turn on NATS TLS + auth on the central stack

This is the opt-in block already present (commented) in `docker-compose.deploy.yml` under the `nats` service. All five steps are required:

**1a. Generate a server cert** into `./certs`. The SAN **must** include the exact host/IP each poller will dial:

```bash
mkdir -p certs
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout certs/server-key.pem -out certs/server-cert.pem \
  -subj "/CN=yagra-nats" \
  -addext "subjectAltName=DNS:nats,DNS:core.example.com,IP:192.168.1.2"
```

The cert is self-signed, so it is its own CA. You hand out `server-cert.pem` (the **public cert only — never the key**) in step 5.

**1b. Set bus passwords** in `.env` (no defaults on purpose):

```ini
YAGRA_NATS_CORE_PASSWORD=a-strong-core-bus-password
YAGRA_NATS_POLLER_PASSWORD=a-strong-poller-bus-password
YAGRA_NATS_PORT=4222        # host port to publish the bus on
YAGRA_CERT_DIR=./certs
```

**1c. Load the auth/TLS config.** In `docker-compose.deploy.yml`, comment out `command: ["-js"]` on the `nats` service and uncomment the block below it (which sets `command: ["-js", "-c", "/etc/nats/nats-server.conf"]`, injects the two passwords, mounts `docker/nats/nats-server.conf` + `./certs`, and publishes `${YAGRA_NATS_PORT:-4222}:4222`).

**1d. Switch the internal clients to TLS** — server-wide TLS leaves no plaintext port, so the co-located core and poller must use `tls://` too. On `core`:
```yaml
YAGRA_BUS_URL: tls://core:${YAGRA_NATS_CORE_PASSWORD}@nats:4222
```
on the local `poller`:
```yaml
YAGRA_BUS_URL: tls://poller:${YAGRA_NATS_POLLER_PASSWORD}@nats:4222
```
and on **both** add `YAGRA_BUS_CA_FILE: /etc/nats/certs/server-cert.pem` plus a `- ${YAGRA_CERT_DIR:-./certs}:/etc/nats/certs:ro` volume mount.

**1e.** Hand `certs/server-cert.pem` (public cert only) to each remote poller operator — it becomes their `YAGRA_BUS_CA_FILE`.

Bring the central stack back up (`docker compose -f docker-compose.deploy.yml up -d`).

The `nats-server.conf` gives `core` full access and the `poller` account least privilege (publish only results/events/heartbeat; subscribe only to its jobs + working-set assignments). Note the limitation of the static accounts: there is **one shared `poller` account**, so any authenticated poller can read any pool's assignments — it is not a tenant boundary. To scope bus credentials per poller, enable the optional **NATS Auth Callout** integration (the `auth_callout` block in `docker/nats/nats-server.conf` plus the `YAGRA_NATS_CALLOUT_*` / `YAGRA_CALLOUT_SEED_DIR` variables in `.env.example`): core then mints each connecting poller a credential scoped to exactly its own pool's subjects.

### Step 2 — Register the poller in the WebUI

Go to **Settings ▸ Pollers ▸ "Register poller"**. It generates a ready-to-use `.env` (id / pool / bus URL) for the remote host. Assign the pool you want this poller to serve.

### Step 3 — Run the remote poller

On the remote-site machine, using `docker-compose.poller.yml` (runs **only** a poller):

```bash
# put the generated .env next to docker-compose.poller.yml, and the CA cert into ./certs
mkdir -p certs && cp /path/to/server-cert.pem certs/

docker compose -f docker-compose.poller.yml up -d
```

The three required vars (compose errors out if unset) — supplied by the generated `.env`:

```ini
YAGRA_BUS_URL=tls://poller:a-strong-poller-bus-password@core.example.com:4222
YAGRA_POLLER_ID=edge-tokyo-1        # stable, unique per poller
YAGRA_POLLER_POOL=tokyo             # the pool this poller serves
YAGRA_BUS_CA_FILE=/etc/yagra/certs/server-cert.pem
```

`docker-compose.poller.yml` uses `network_mode: host` (so passive syslog/trap correlation sees the real datagram source IP and raw ICMP reaches the host's interfaces) and grants `NET_RAW`.

> **Privileged-port caveat.** The remote poller runs **non-root** (file-cap `NET_RAW` only), so it cannot bind `:514`/`:162` (< 1024). Use the default high ports (`1514`/`1162`) and redirect on the host firewall (`iptables … REDIRECT 514→1514`), or point devices straight at the high ports. It will appear on the Pollers page within a few seconds of starting; core begins assigning that pool's nodes to it.

To scale a pool, run more pollers with the same `YAGRA_POLLER_POOL` (and distinct `YAGRA_POLLER_ID`s) — core rebalances the pool across them and fails over on loss. A pool with zero live pollers falls back to legacy per-job publish, so no nodes go dark during a rollout.

---

## E — Distributed pollers, native<a id="e--distributed-pollers-native"></a>

Same as **D**, but the remote poller is the native binary instead of a container. The central bus TLS+auth setup (D · Step 1) is unchanged.

On the remote host, build (or copy) the `yagra-poller` binary, drop the CA cert somewhere readable, and run:

```bash
sudo setcap cap_net_raw+ep ./yagra-poller

export YAGRA_BUS_URL="tls://poller:a-strong-poller-bus-password@core.example.com:4222"
export YAGRA_POLLER_ID="edge-tokyo-1"       # unique per poller
export YAGRA_POLLER_POOL="tokyo"
export YAGRA_BUS_CA_FILE="/etc/yagra/certs/server-cert.pem"
# Optional passive-event listeners (need root / CAP_NET_BIND_SERVICE for :514/:162):
# export YAGRA_SYSLOG_BIND="0.0.0.0:1514"
# export YAGRA_TRAP_BIND="0.0.0.0:1162"
export RUST_LOG=info

./yagra-poller
```

Run it on the host network (not a private namespace) so passive event source-IP correlation and raw ICMP work against the site's interfaces. Everything else — pools, registration, failover — behaves exactly as in **D**.

---

## Environment variable reference

### Yagra-core

| Variable | Default | Purpose |
|---|---|---|
| **Stores & bus** | | |
| `YAGRA_DATABASE_URL` | — (required for live) | PostgreSQL connection string |
| `YAGRA_BUS_URL` | — (required for live) | NATS bus URL (`nats://…` or `tls://user:pass@host:4222`) |
| `YAGRA_TSDB_URL` | — (required for live) | VictoriaMetrics base URL |
| `YAGRA_REDIS_URL` | unset ⇒ disabled | Redis URL for poller liveness/assignment mirror (best-effort) |
| `YAGRA_LOGS_URL` | unset ⇒ events stay in PostgreSQL | VictoriaLogs base URL — opt-in passive-event log store |
| `YAGRA_CLICKHOUSE_URL` | unset ⇒ flow store off | ClickHouse HTTP URL — opt-in traffic-flow store (without it the flow API returns 503) |
| `YAGRA_PG_MAX_CONNECTIONS` | `20` | PostgreSQL connection-pool ceiling (the HA leader holds +1 advisory-lock connection on top) |
| **API & security** | | |
| `YAGRA_KEK_FILE` | unset ⇒ ephemeral dev key | Path to the mounted 32-byte key-encryption key |
| `YAGRA_API_ADDR` | `0.0.0.0:8080` | API + `/metrics` bind address |
| `YAGRA_ADMIN_PASSWORD` | unset ⇒ one-time random (logged) | Bootstrap `admin` password, first boot only |
| `YAGRA_PUBLIC_DASHBOARD` | `false` | `true` = read-only dashboards without login |
| **Polling & notifications** | | |
| `YAGRA_POLL_INTERVAL_SECS` | `30` (clamp 10–3600) | Initial default poll interval (seeded on first boot; DB-authoritative after) |
| `YAGRA_SNMP_COMMUNITY` | unset | Fallback SNMP v2c community for nodes without a bound credential |
| `YAGRA_MERAKI_POOL` | `default` | Poller pool that Meraki cloud-collect jobs route to |
| `YAGRA_WEBHOOK_URL` | unset ⇒ off | Default alert webhook channel |
| `YAGRA_SMTP_HOST` / `_FROM` / `_TO` | unset ⇒ email off | Env-configured SMTP alert channel. All three are required — the channel is skipped unless every one is set and `_FROM`/`_TO` parse as mailboxes |
| `YAGRA_SMTP_PORT` | `465` (implicit TLS) | SMTP port |
| `YAGRA_SMTP_USER` / `_PASS` | unset ⇒ no auth | SMTP credentials; applied only when **both** are set |
| **Traffic flow & AS enrichment** | | |
| `YAGRA_FLOW_RETENTION_DAYS` | `30` (clamp 1–3650) | Flow retention in days (ClickHouse TTL) |
| `YAGRA_IPASN_DB` | unset ⇒ enrichment off | Path to an offline iptoasn.com TSV for flow IP→ASN enrichment |
| `YAGRA_IPASN_RELOAD_SECS` | `0` ⇒ load once at startup | Hot-reload period (seconds) for the IP→ASN file; `>0` reloads without a restart |
| **High availability** | | |
| `YAGRA_ENABLE_HA` | `false` | Opt-in active/passive leader election via a PostgreSQL advisory lock |
| `YAGRA_CORE_ID` | unset | Human-readable id of this core instance in HA logs |
| `YAGRA_SESSION_KEY_FILE` | unset ⇒ per-process tokens | Path to the mounted HMAC session-signing key (sessions valid on any core and across restarts); set but unreadable/invalid ⇒ startup fails |
| `YAGRA_PAT_OIDC_IDLE_DAYS` | `30` | Days an API token owned by an **SSO-provisioned** account survives its owner not signing in — an identity provider disabling an account is not something Yagra is told about, so the owner going quiet is the only signal. Local/service-account-owned tokens are unaffected. Clamped 1–365 |
| **MCP (AI clients)** | | |
| `YAGRA_ENABLE_MCP` | `false` | Mount the MCP tool surface at `/mcp` on the API port (auth always required) |
| `YAGRA_MCP_ALLOWED_HOSTS` | unset ⇒ any `Host` accepted | Comma-separated `Host`-header allowlist for `/mcp` (DNS-rebinding hardening) |
| **Analysis & RCA rate caps** | | |
| `YAGRA_ANALYSIS_MAX_CONCURRENT` | `4` | Max concurrently-running Troubleshoot analyses |
| `YAGRA_ANALYSIS_RATE_PER_MIN` | `30` | Max new analyses admitted per minute |
| `YAGRA_RCA_MAX_CONCURRENT` | `2` | Max simultaneous LLM root-cause generations (billed external calls) |
| `YAGRA_RCA_RATE_PER_MIN` | `10` | Max new root-cause generations per minute |
| `YAGRA_RCA_CACHE_SECS` | `900` | RCA report cache lifetime (seconds); `force` bypasses the cache but not the caps |
| **NATS Auth Callout (per-poller bus credentials)** | | |
| `YAGRA_NATS_CALLOUT_SEED_FILE` | unset ⇒ callout off | Path to the mounted NATS account nkey seed; core then mints per-poller scoped bus users |
| `YAGRA_NATS_CALLOUT_ACCOUNT` | `$G` | NATS account minted poller users are placed into (must match the server's `auth_callout` account) |
| `YAGRA_NATS_POLLER_PASSWORD` | unset ⇒ callout off | Shared poller bootstrap secret the callout validates (also consumed by the NATS server config) |
| **Observability** | | |
| `YAGRA_DISK_WATCH_PATHS` | `/=root` | Filesystems host self-metrics report capacity for (comma-separated `path` or `path=alias`); read by core **and** poller |
| `YAGRA_OTEL_ENDPOINT` | unset ⇒ logs only | OTLP/HTTP endpoint for OpenTelemetry trace export (falls back to `OTEL_EXPORTER_OTLP_ENDPOINT`) |
| `OTEL_TRACES_SAMPLER` / `_ARG` | `parentbased_always_on` | Trace sampler; use `parentbased_traceidratio` + arg (e.g. `0.01`) at scale |
| `RUST_LOG` | `info` | Log level (e.g. `info,yagra_core=debug`) |

### Yagra-poller

| Variable | Default | Purpose |
|---|---|---|
| **Identity & bus** | | |
| `YAGRA_BUS_URL` | unset ⇒ idle | NATS bus URL (only backing-service connection the poller makes) |
| `YAGRA_POLLER_ID` | hostname, else `poller-<hex>` | Stable, unique, subject-safe poller identity |
| `YAGRA_POLLER_POOL` | `default` | Pool this poller serves |
| `YAGRA_BUS_CA_FILE` | unset ⇒ plaintext | CA/server cert pinned for the `tls://` bus |
| `YAGRA_MAX_CONCURRENT_POLLS` | `64` | Max concurrent in-flight probes |
| `YAGRA_POLLER_QUEUE` | `pollers` | NATS queue-group for load-balanced job consumption |
| **Passive events (syslog / SNMP traps)** | | |
| `YAGRA_SYSLOG_BIND` | unset ⇒ off | UDP bind for syslog intake (e.g. `0.0.0.0:1514`) |
| `YAGRA_TRAP_BIND` | unset ⇒ off | UDP bind for SNMP trap intake (v1/v2c) |
| `YAGRA_TRAP_COMMUNITY` | unset ⇒ no filter | Drop traps whose community doesn't match (never logged) |
| `YAGRA_EVENT_RATE_PER_SOURCE` | `200` | Passive-event rate limit per source IP (events/sec) |
| `YAGRA_EVENT_RATE_GLOBAL` | `5000` | Passive-event rate limit across all sources (events/sec) |
| **Traffic flow (NetFlow / IPFIX / sFlow)** | | |
| `YAGRA_FLOW_BIND` | unset ⇒ off | UDP bind for NetFlow v5/v9 / IPFIX intake (e.g. `0.0.0.0:2055`) |
| `YAGRA_SFLOW_BIND` | unset ⇒ off | UDP bind for sFlow v5 intake (e.g. `0.0.0.0:6343`) |
| `YAGRA_FLOW_RATE_PER_SOURCE` | `1000` | Flow rate limit per exporter (datagrams/sec; separate budget from syslog/traps) |
| `YAGRA_FLOW_RATE_GLOBAL` | `20000` | Flow rate limit across all exporters (datagrams/sec) |
| `YAGRA_FLOW_BUCKET_SECS` | `60` | Flow aggregation bucket width (seconds) |
| `YAGRA_FLOW_TOP_N` | `500` | Top flows (by bytes) kept per bucket per exporter — the cardinality control |
| **Edge listener tuning** | | |
| `YAGRA_LISTENER_WORKERS` | CPU count, clamped 1–4 | Parallel reader sockets per UDP listener (Linux `SO_REUSEPORT`; 1 socket elsewhere) |
| `YAGRA_LISTENER_RCVBUF_BYTES` | `4194304` (4 MiB) | Socket receive-buffer size per listener |
| **Store-and-forward result buffer** | | |
| `YAGRA_STORE_FORWARD` | on (`off`/`false`/`0`/`no` disables) | Buffer poll results during bus outages and replay them when it returns |
| `YAGRA_STORE_FORWARD_DIR` | `/var/lib/yagra/buffer` | On-disk spill directory (falls back to memory-only if unwritable) |
| `YAGRA_STORE_FORWARD_MEM_MAX` | `20000` | In-memory ring size (results) before spilling to disk |
| `YAGRA_STORE_FORWARD_DISK_MAX_MB` | `512` | Max total on-disk spill (oldest segments dropped first) |
| `YAGRA_STORE_FORWARD_DISK_FREE_FLOOR_MB` | `1024` | Stop spilling when filesystem free space drops below this |
| `YAGRA_STORE_FORWARD_MAX_AGE_SECS` | `86400` | Buffered results older than this are dropped at replay |
| `YAGRA_STORE_FORWARD_SEGMENT_MB` | `16` | Spill-segment roll size (the granularity of the disk cap) |
| **Observability** | | |
| `YAGRA_OTEL_ENDPOINT` | unset ⇒ logs only | OTLP/HTTP endpoint for trace export (same collector as core) |
| `OTEL_TRACES_SAMPLER` / `_ARG` | `parentbased_always_on` | Trace sampler; sample (`parentbased_traceidratio`) at scale |
| `RUST_LOG` | `info` | Log level |

> **Compose-only vars** are consumed by Docker Compose / the NATS config, never by the Rust binaries — the binaries only ever see the final assembled `YAGRA_BUS_URL` etc. See `.env.example`:
>
> - Images & stores: `YAGRA_IMAGE_TAG`, `POSTGRES_PASSWORD`
> - Host port mappings: `YAGRA_API_PORT`, `YAGRA_WEB_PORT`, `YAGRA_SYSLOG_PORT`, `YAGRA_TRAP_PORT`, `YAGRA_FLOW_PORT`, `YAGRA_SFLOW_PORT`, `YAGRA_NATS_PORT`
> - Bus TLS + auth (D): `YAGRA_CERT_DIR`, `YAGRA_NATS_CORE_PASSWORD`, `YAGRA_NATS_POLLER_PASSWORD` (also read by core as the Auth Callout bootstrap secret), `YAGRA_NATS_CALLOUT_ISSUER` (account public key the NATS server verifies core's callout JWTs against)
> - Mounted key directories: `YAGRA_SESSION_KEY_DIR` (holds `session.key` for `YAGRA_SESSION_KEY_FILE`), `YAGRA_CALLOUT_SEED_DIR` (holds `account.seed` for `YAGRA_NATS_CALLOUT_SEED_FILE`)
> - IP→ASN updater sidecar: `YAGRA_IPASN_URL` (dataset URL), `YAGRA_IPASN_REFRESH_SECS` (fetch cadence; default `604800` = weekly)

---

## Distributed tracing (OpenTelemetry)<a id="tracing"></a>

Every binary emits structured logs and a Prometheus `/metrics` endpoint out of the box. **Distributed tracing is opt-in:** set `YAGRA_OTEL_ENDPOINT` (or the standard `OTEL_EXPORTER_OTLP_ENDPOINT`) to an OTLP/HTTP collector and core + poller export spans that stitch a single poll end to end — core's dispatch span → the poller's poll span → core's result-ingest span — plus a span per northbound API request. Unset, there is **zero tracing overhead** (logs only), so the single-node MVP needs no collector.

- **Try it locally:** `docker compose --profile tracing up` starts a bundled Jaeger (UI at http://localhost:16686), then uncomment `YAGRA_OTEL_ENDPOINT: http://jaeger:4318` on **both** `core` and `poller` in `docker-compose.yml`.
- **At scale, sample.** Tens of thousands of nodes polling on an interval would otherwise emit a trace per poll. Set `OTEL_TRACES_SAMPLER=parentbased_traceidratio` and `OTEL_TRACES_SAMPLER_ARG=0.01` (1%); `parentbased_*` keeps a whole trace's decision consistent across the core⇄poller hop. The trace context rides the bus in a `trace_context` field that is **omitted from the wire when tracing is off** and ignored by an N-1 peer (so it stays N/N-1 safe).
- **Production:** point the endpoint at an OpenTelemetry Collector that forwards to your backend (Tempo, Jaeger, Honeycomb, …). A remote-site poller needs its own reachable collector endpoint, separate from the NATS bus.

---

## Upgrades & backups<a id="upgrades--backups"></a>

Upgrades are designed to be low-effort and **never** lose or corrupt data:

- **DB migrations are expand-contract and run automatically** on core startup. N→N+1 is always supported; there is no manual migration CLI.
- **The bus is version-tolerant (N/N-1).** A new core works with old pollers during a rollout, so you can upgrade core first and pollers after.
- **Rolling upgrades.** Pollers are stateless — replace them in any order. For Docker, pull the new tag and `up -d` (see **B**). Remote pollers: pull and `up -d` per site; a pool briefly down falls back to legacy publish, so no node goes dark.
- **Back up the persistent stores before a major upgrade:** the `pgdata` (PostgreSQL), `vmdata` (VictoriaMetrics), and `kekdata` (KEK) volumes — or their native equivalents. **Redis is rebuildable**, so losing it is non-fatal.

> **Do not lose the KEK.** If the `kekdata` volume / KEK file is destroyed, every stored monitoring credential becomes permanently undecryptable. Back it up alongside the database.

---

## Security notes

- **Bus TLS is mandatory across any trust boundary.** Job messages carry plaintext device credentials — never expose NATS `:4222` plaintext to a remote site (see **D · Step 1**).
- **The KEK is a mounted file, never an env value.** Provide it via `YAGRA_KEK_FILE`; the ephemeral fallback is dev-only.
- **Images run non-root** (core uid 10001, poller uid 10002 with a `cap_net_raw+ep` file capability, web nginx uid 101). Only the poller gets `NET_RAW`.
- **Credentials are never logged** — SNMP communities, SNMPv3 auth/priv, and API tokens are encrypted at rest and redacted from logs, API responses, and metric labels.
