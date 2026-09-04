# Deploying Yagra

This guide covers **how to deploy Yagra**, not how to use it. It spans the full matrix:

|                     | **Docker Compose**                        | **Native (no Docker)**       |
|---------------------|-------------------------------------------|------------------------------|
| **Single node**     | [A — pre-built images](#a--single-node-docker-pull) · [B — build from source](#b--single-node-docker-build) | [C](#c--single-node-native)  |
| **Distributed pollers** | [D](#d--distributed-pollers-docker)   | [E](#e--distributed-pollers-native) |

**Start with [A](#a--single-node-docker-pull)** — it pulls the published images, needs no checkout and no build, and is the only single-node composition that can upgrade itself from the WebUI. Reach for **[D](#d--distributed-pollers-docker)** once you need pollers at remote sites.

The other three are for narrower audiences. **[B](#b--single-node-docker-build)** builds from source: the path for developing on Yagra, auditing it, or making a custom build. **[C](#c--single-node-native)** and **[E](#e--distributed-pollers-native)** run the binaries directly, for hosts where Docker is not an option. All of them are supported and all are documented here — but if you are standing up a monitoring system, A is the one you want.

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
| `8080` | `8080` | `YAGRA_API_ADDR` (native) / `YAGRA_API_PORT` + `YAGRA_API_BIND` (compose) | core northbound API + `/metrics` — **plaintext** | yes |
| `8080` (web nginx) | **`443`** | `YAGRA_WEB_PORT` | WebUI — **HTTPS** (`YAGRA_WEB_TLS`) | yes |
| `1514/udp` | `514` | `YAGRA_SYSLOG_BIND` / `YAGRA_SYSLOG_PORT` | syslog intake (poller) | opt-in |
| `1162/udp` | `162` | `YAGRA_TRAP_BIND` / `YAGRA_TRAP_PORT` | SNMP trap intake (poller) | opt-in |
| `2055/udp` | `2055` | `YAGRA_FLOW_BIND` / `YAGRA_FLOW_PORT` | NetFlow v5/v9 / IPFIX intake (poller) | opt-in |
| `6343/udp` | `6343` | `YAGRA_SFLOW_BIND` / `YAGRA_SFLOW_PORT` | sFlow v5 intake (poller) | opt-in |
| `9100` | — | (fixed) | poller Prometheus `/metrics` | native only |
| `4222` | — | `YAGRA_NATS_PORT` | NATS bus | internal; published **only** with TLS+auth (D) |
| `5432` / `6379` / `8428` / `9428` / `8123` | — | — | PostgreSQL / Redis / VictoriaMetrics / VictoriaLogs / ClickHouse | internal only |

> The MCP tool surface (`/mcp`, opt-in via `YAGRA_ENABLE_MCP`) is served on the API port `8080` — it does not open a separate port. The web container also proxies it, so it is reachable over TLS at `https://<host>/mcp`. If you have set `YAGRA_MCP_ALLOWED_HOSTS`, add the web host's name to it or that path is refused.

> ### TLS
>
> **The WebUI is HTTPS by default and there is no plain-HTTP listener** (ADR-044). Core generates a
> self-signed certificate on first start and writes it where the web container reads it, so a fresh
> stack comes up encrypted with a browser warning. Replace it at **Settings ▸ TLS** — paste or upload
> the PEM chain and key, and the new certificate is live within seconds with nothing restarted.
>
> The certificate of record is a row in PostgreSQL: the private key envelope-encrypted with the KEK,
> the chain in plaintext because a certificate is public by construction. The file on the volume is
> a materialization of that row, so deleting the volume is safe.
>
> - **A redirect from plain HTTP was considered and rejected.** Most webhook senders do not follow
>   redirects, and those that do turn a `301` on `POST` into a `GET` — inbound events would have
>   stopped arriving with nothing to show for it. Connection-refused is the honest failure.
> - **Set `YAGRA_WEB_TLS=off`** only when an external reverse proxy or load balancer already
>   terminates HTTPS in front of the container.
> - **Encrypted private keys and PKCS#12 (`.pfx`) are not accepted.** Convert first:
>   `openssl pkcs8 -topk8 -nocrypt -in key.pem -out key-plain.pem`, or
>   `openssl pkcs12 -in cert.pfx -nodes -out bundle.pem`.
> - **The NATS bus certificate is a different certificate** and Settings ▸ TLS does not manage it —
>   the NATS server reads its own at startup (section D below).

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

## A — Single node, Docker (pre-built images)<a id="a--single-node-docker-pull"></a>

**The recommended deployment, and the one the rest of this guide assumes.** `docker-compose.deploy.yml` **pulls** the published images from GHCR (no local build), is fully env-parameterized via `.env`, adds a one-shot `kek-init` that writes a persistent key-encryption key so stored monitoring credentials survive redeploys, adds a one-shot `log-init` that makes the shared log volume writable by both core and the poller (they run as different uids, and an image's ownership only gets a vote the first time Docker seeds an empty volume), and ships the `yagra-updater` sidecar that makes **Settings ▸ Upgrade** work. The two `*-init` containers are expected to sit at `Exited (0)` after startup — that is success, not a failure.

It needs no repository checkout — the composition is a single self-contained file, with a default for every variable it interpolates and no bind mounts outside `/var/run/docker.sock`:

```bash
mkdir yagra && cd yagra
curl -fsSL -o docker-compose.deploy.yml \
  https://github.com/horryworks/Yagra/releases/latest/download/docker-compose.deploy.yml
printf 'POSTGRES_PASSWORD=%s\n' "$(openssl rand -hex 16)" > .env
docker compose -f docker-compose.deploy.yml up -d
```

**Take the composition from the release, not from `main`.** The file and the images it pulls are one
artifact: a composition can require a container command, an environment variable or an init step that
only exists in an image that has not been published yet. `releases/latest/download/` resolves to the
latest **stable** release — the same thing the `:latest` image tag means — so the two always match.
The `main` copy is the *next* release's composition and may reference images nobody can pull.

`up -d` pulls on its own — the images carry `pull_policy: always`, so there is no separate `pull` step. Open **https://\<host\>/** once it is up (API on `:8080`). The certificate is self-signed until you import your own at Settings ▸ TLS.

**Keep the file where you started it, under that name.** In-place upgrades read the directory back from the `com.docker.compose.project.working_dir` label on their own container and refuse to run if it no longer holds a `docker-compose.deploy.yml`. The compose *project* name is not at risk — the file pins `name: yagra` itself — but the path is.

**Set `POSTGRES_PASSWORD` before the first start**, as the snippet does. It is baked into the database volume on initialization; changing it later needs an `ALTER ROLE` as well as an `.env` edit.

`YAGRA_IMAGE_TAG` selects the image tag and defaults to `latest`: `latest` is the latest **stable** release (pre-releases never move it); a `v<version>` tag pins one release; the `<git-sha>` of a release is an immutable reference to exactly that build (rollback = re-run with an older tag). Only releases are published — development builds never reach the registry, so every tag you can pull is a release.

Want to know what a running container was built from? `docker exec yagra-core-1 cat /etc/yagra-source-ref` prints the commit, and `/etc/yagra-build-profile` prints the compile profile.

**Configure `.env`** — optional beyond the password above. [`.env.example`](.env.example) documents every key; fetch it alongside the composition (`curl -fsSL .../.env.example -o .env`) if you want the annotated version. The essentials:

```ini
POSTGRES_PASSWORD=change-me            # change for any non-throwaway box
YAGRA_API_PORT=8080                    # host port for the API (plaintext)
YAGRA_WEB_PORT=443                     # host port for the WebUI (HTTPS)
# YAGRA_ADMIN_PASSWORD=choose-a-strong-password   # else a one-time random one is logged
# YAGRA_PUBLIC_DASHBOARD=false         # true = read-only dashboards without login
# YAGRA_WEB_TLS=off                    # only if a proxy in front already terminates HTTPS
# YAGRA_API_BIND=127.0.0.1             # close core's plaintext port to the LAN — see below
```

**Upgrading from before v0.1.22?** `YAGRA_WEB_PORT` did not change meaning, but the scheme on it did. If your `.env` still says `3000` you keep port 3000 and it becomes `https://<host>:3000` — `http://` no longer answers there. Delete the line to land on `443`.

**Closing core's API port — do this second, not first.** `YAGRA_API_BIND=127.0.0.1` takes the plaintext API off the LAN, leaving the TLS edge as the only way in. Browsers are unaffected either way (the web container proxies `/api/` and `/mcp` internally), but Prometheus scrapes, webhook senders and API scripts use that port directly. Move them to `https://<host>/api/v1` with a certificate they trust **first**; doing both at once means every machine client fails simultaneously with two overlapping causes.

**Credential persistence (important).** The `kek-init` service writes a 32-byte KEK into the `kekdata` volume once and never overwrites it; core mounts it read-only at `YAGRA_KEK_FILE=/kek/key`. Without a persistent KEK, core falls back to an **ephemeral** key regenerated on every restart, and all stored credentials (SNMP communities, API tokens) become undecryptable after a redeploy. The compose file wires this up for you — just don't delete the `kekdata` volume.

**Upgrades.** From v0.2.2 the ordinary way is **Settings ▸ Upgrade in the WebUI** — this composition ships a `yagra-updater` sidecar that does the whole thing (back up, pull, install the composition carried inside the target image, recreate, verify), and no shell is required. This is the reason to prefer this deployment over **B**: it is the only single-node composition that can upgrade itself. The command line still works and is the way back if the sidecar is switched off or cannot run:

```bash
YAGRA_IMAGE_TAG=v0.2.5 docker compose -f docker-compose.deploy.yml pull
YAGRA_IMAGE_TAG=v0.2.5 docker compose -f docker-compose.deploy.yml up -d
```

Migrations are expand-contract and run automatically; `pgdata`/`vmdata`/`kekdata` are preserved. See [Upgrades & backups](#upgrades--backups).

---

## B — Single node, Docker (build from source)<a id="b--single-node-docker-build"></a>

The developer / all-in-one box, for **working on Yagra, auditing it, or making a custom build**. `docker-compose.yml` **builds** the images locally (tagged `:dev`) and runs the whole stack — core, poller, web, and all five stores — on one host.

```bash
git clone https://github.com/horryworks/Yagra.git
cd Yagra
docker compose up --build          # build + start the full single-node stack
```

Then open the WebUI at **https://localhost:8443** (API at http://localhost:8080).

Your browser will warn: the certificate is the self-signed one core generated on first start. Accept it for now and import a real one at Settings ▸ TLS. (This developer stack publishes `8443` rather than `443` because a laptop usually has `443` taken and rootless Docker cannot bind below 1024 at all; **A** above uses `443`.)

**First login.** `YAGRA_ADMIN_PASSWORD` is unset by default, so core generates a one-time random `admin` password and prints it **once** in its logs:

```bash
docker compose logs core | grep -i password
```

Log in as `admin` with it and change it. To choose your own instead, uncomment `YAGRA_ADMIN_PASSWORD` under the `core` service in `docker-compose.yml`.

**What's running.** Web on host `:8443` (HTTPS), API on `:8080` (plaintext); the poller listens for syslog on `:514/udp` and SNMP traps on `:162/udp`; PostgreSQL/Redis/NATS/VictoriaMetrics stay on the internal Docker network. Migrations run automatically on core startup — no manual step. Named volumes `pgdata` and `vmdata` persist data across `docker compose down`/`up`.

⚠️ **Two limits make this the wrong choice for a system you depend on**, and both are deliberate rather than oversights:

- **It cannot upgrade itself.** There is no `yagra-updater` sidecar here, and the `:dev` tags it builds are never published, so there is no release for an updater to move to. Settings ▸ Upgrade says so rather than offering controls that would fail.
- **The KEK is ephemeral**, so the key that encrypts stored secrets is regenerated on every restart. A self-signed certificate is simply regenerated with it; an **imported** one cannot be decrypted afterwards, and core will say so and keep serving the last materialized certificate rather than silently replacing yours. Import real certificates only on a stack with a persistent KEK — that is **A**.

> Fine for development and evaluation. For anything you care about, use **A** (published images, a persistent KEK so stored credentials survive restarts, and in-place upgrades from the WebUI).

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
- **VictoriaMetrics** — `victoria-metrics-prod --retentionPeriod=12` (12 months, single tier; see [Data retention](#data-retention))
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

Run the full stack centrally (as in **A**) and add pollers at remote sites. Each poller polls its site's devices locally and streams results back over the bus. Nodes carry a `pool` attribute; core's coordinator assigns each pool's nodes across its live pollers by consistent hashing and fails them over automatically.

> **The bus carries plaintext device credentials.** On one host that's fine (internal Docker network, nothing exposed). The moment the bus crosses a trust boundary to a remote site, it **must** be TLS-encrypted and authenticated first. Do **not** publish `:4222` plaintext.

### Step 1 — Accept remote pollers (WebUI)

Go to **Settings ▸ Pollers ▸ Remote pollers** and press **Accept remote pollers**. Give it the
hostname or IP address the remote sites will dial — that address goes into the bus certificate, and
a site whose exact address is not on the certificate cannot connect.

That is the whole central setup. Yagra:

- reissues the bus certificate covering that address (it was generated on first start; the private
  key is envelope-encrypted in PostgreSQL like every other secret, and only ever materialized to a
  volume the bus reads);
- turns on TLS and password authentication on the bus, publishes its port, and moves the co-located
  core and poller to `tls://` in the same change;
- writes the settings into your `.env`, which **is** preserved across upgrades — unlike a hand-edited
  compose file, which is not (see below).

> **Monitoring stops for about a minute.** NATS has no way to serve TLS and plaintext at once, so the
> bus, core and the poller running beside it are all recreated. Yagra opens a fleet-wide maintenance
> window first so nothing pages, and this page disconnects while it happens — that is expected, not a
> failure. Alerts are not backfilled for the window; metrics are.

<details>
<summary>Doing it from a shell instead (no WebUI access)</summary>

Set these in `.env` beside `docker-compose.deploy.yml` and run
`docker compose -p yagra -f docker-compose.deploy.yml up -d`. They are the same keys the WebUI
writes — nothing else is needed, and **no compose edit is needed**:

```ini
YAGRA_NATS_ARGS=-js -c /etc/nats/nats-server.conf
YAGRA_NATS_BIND=0.0.0.0
YAGRA_NATS_CORE_PASSWORD=a-strong-core-bus-password
YAGRA_NATS_POLLER_PASSWORD=a-strong-poller-bus-password
YAGRA_CORE_BUS_URL=tls://core:a-strong-core-bus-password@nats:4222
YAGRA_POLLER_BUS_URL=tls://poller:a-strong-poller-bus-password@nats:4222
YAGRA_BUS_CA_FILE=/etc/nats/certs/server-cert.pem
# Extra names for the bus certificate, added to the internal defaults:
YAGRA_BUS_TLS_SANS=core.example.com,192.168.1.2
```

To turn it back off, delete those lines and bring the stack up again.
</details>

> **Why there is no `openssl` step here any more, and why that matters.** The previous procedure had
> you generate a certificate by hand and edit two blocks of `docker-compose.deploy.yml`. Both were
> worse than they looked. Settings ▸ Upgrade **replaces that file with the copy inside the target
> image**, so the edits were erased by the next upgrade — after which the central stack kept working
> and every remote poller silently stopped connecting. And the file the procedure told you to mount,
> `docker/nats/nats-server.conf`, was not inside the published images at all, so a deployment
> installed with composition [A](#a--single-node-docker-pull) had nothing there and the bus failed to
> start. Both are fixed: the configuration now ships inside the core image and is placed on a volume
> automatically, and the switch is expressed as `.env` variables, which upgrades preserve.

### Step 2 — Issue the site its token and download its bundle

On **Settings ▸ Pollers**, each poller's **Token** column says whether it has one of its own or is
using the deployment-wide bootstrap secret. Click it and press **Issue token & download**.

You get a `.tar.gz` holding everything the site needs: `.env` (its id, pool, bus token and
`COMPOSE_PROFILES`), `certs/server-cert.pem` (the certificate it pins), `docker-compose.poller.yml`
taken from this core's own image, and a README. The poller does not have to exist yet — issuing a
token registers it, which is how you prepare a site before anything is running there.

> **The dialog's "let this site install releases" box is ticked by default (v0.3.3+).** It writes
> `COMPOSE_PROFILES=self-upgrade` into that `.env`, which starts a `yagra-poller-updater` beside the
> poller — a container running as **root** with that site host's Docker socket, so Settings ▸ Upgrade
> can replace the poller there. Untick it before issuing, or empty `COMPOSE_PROFILES` in the site's
> `.env` later, and no container at that site holds a socket. Change it in `.env` and not in the
> composition: an upgrade reinstalls the composition from the release being installed and never
> touches `.env`. A site issued its bundle before v0.3.3 is unaffected until it is re-issued one.

> **The token is in that file and nowhere else.** Yagra stores only a SHA-256 digest of it. If the
> archive is lost, issue a new token — the old one stops working the moment you do.

Two things this buys beyond convenience:

- **A poller id nobody registered is refused**, whatever secret it presents. Before this, the id was
  self-asserted and checked against nothing, so one site's leaked `.env` let the holder claim *any*
  id — and the working set core then sent it carries the plaintext SNMP communities and API tokens
  of whatever nodes that id is assigned.
- **A poller with its own token can no longer be opened by the shared secret**, so issuing tokens
  narrows the blast radius one site at a time. A poller that has none still uses the shared secret,
  which is what keeps an existing fleet working across the upgrade — the Token column is how you see
  which sites are still in that state.

Use **Revoke token** to put a site back on the shared secret (for example after a leak, before
issuing a replacement). That is different from removing the poller, which also discards its anchor
and history.

### Step 3 — Run the remote poller

On the remote-site machine:

```bash
tar xzf yagra-poller-edge-tokyo-1.tar.gz
cd yagra-poller-edge-tokyo-1        # or wherever you unpacked it
docker compose -f docker-compose.poller.yml up -d
```

It appears on the Pollers page within about ten seconds, and core starts assigning that pool's nodes
to it.

`docker-compose.poller.yml` uses `network_mode: host` (so passive syslog/trap correlation sees the
real datagram source IP and raw ICMP reaches the host's interfaces) and grants `NET_RAW`.

> **Privileged-port caveat.** The remote poller runs **non-root** (file-cap `NET_RAW` only), so it cannot bind `:514`/`:162` (< 1024). Use the default high ports (`1514`/`1162`) and redirect on the host firewall (`iptables … REDIRECT 514→1514`), or point devices straight at the high ports.

To scale a pool, run more pollers with the same `YAGRA_POLLER_POOL` (and distinct `YAGRA_POLLER_ID`s) — core rebalances the pool across them and fails over on loss. A pool with zero live pollers falls back to legacy per-job publish, so no nodes go dark during a rollout.

### What Step 1 also turned on — per-poller bus credentials (Auth Callout)

The static NATS accounts give `core` full access and `poller` least privilege (publish only
results/events/heartbeat; subscribe only to its jobs and working-set assignments). There is **one
shared `poller` account**, though, so any authenticated poller could read any pool's assignments —
which is not a tenant boundary.

**NATS Auth Callout** closes that, and Step 1 enables it. Core becomes the bus's authorization
service: it mints each connecting poller a credential scoped to exactly its own subjects, and it is
what checks the per-poller tokens from Step 2. The signing key is generated on first start and kept
sealed in the database; the public half is written into the bus's own configuration by the same
one-shot that writes the rest of it. **There is nothing to generate, mount or copy.**

Two consequences worth knowing:

- **Core stops recreating a poller row from a heartbeat**, so removing a poller in the WebUI sticks.
  The other side of that: a poller registers when you issue it a token (Step 2), not by connecting.
  The pollers inside this composition are registered for you when you turn Step 1 on.
- **This deployment's own core and poller are exempt** and keep using the static accounts above.
  They present fixed names (`core`, `poller`) and a password that exists only in this host's `.env`;
  everything arriving from outside presents a poller id and is authorized by core or not at all.

> **If you set this up by hand before v0.3.2, nothing breaks.** `YAGRA_NATS_CALLOUT_SEED_FILE` still
> takes precedence over the generated key. It is worth unsetting at some point: the procedure it
> belonged to ended in an edit to `nats-server.conf`, and that file is reinstalled from the image on
> every start, so the edit only ever survived until the next one.

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
| `YAGRA_VM_WRITERS` | one per core, max 4 | How many tasks write metrics to VictoriaMetrics. Samples are sharded by node, so a series stays in order; the tier queue and spill bounds are divided among them, not repeated. Set `1` for the old single-writer behaviour |
| `YAGRA_RESULT_QUEUE_CAP` | `16384` | How many poll results the metrics tier may hold while VictoriaMetrics is slow. A large series count makes VictoriaMetrics stall periodically, and everything past this cap is dropped — by design (metrics are the best-effort tier) but still a gap in the graphs. **Read `yagra_vm_backlog_needed_high_water` on this deployment before changing it**: it is how deep an unbounded queue would have gone, so it is the figure that sizes this. At 24 ports a queued result is ~21 KB, so the memory cost is linear. Divided among `YAGRA_VM_WRITERS`; clamped (and logged) above 131072 |
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
| `YAGRA_POOL_COVERAGE_ALERT_AFTER_SECS` | `300` | How long a poller pool must hold nodes with **no live poller** before it raises a critical alert. A poller announces its own departure, so a rolling restart trips the condition instantly — this debounce is what stops that paging anyone. `0` disables the alert; the gauges are exported either way |
| **Traffic flow & AS enrichment** | | |
| `YAGRA_FLOW_RETENTION_DAYS` | `30` (clamp 1–3650) | Flow retention in days. **Seeds a brand-new deployment only** — afterwards Settings ▸ System settings ▸ Data retention is authoritative |
| `YAGRA_CLICKHOUSE_SYSTEM_LOG_RETENTION_DAYS` | `7` (clamp 0–3650) | Retention for ClickHouse's **own** `system.*_log` tables. Stock ClickHouse gives them no TTL at all, so they grow without bound and burn CPU merging themselves. Read at every start, not seeded into a settings table. **`0` leaves `system.*` untouched** — use it when `YAGRA_CLICKHOUSE_URL` points at a ClickHouse this deployment does not own |
| `YAGRA_IPASN_DB` | unset ⇒ enrichment off | Path to an offline iptoasn.com TSV for flow IP→ASN enrichment |
| `YAGRA_IPASN_RELOAD_SECS` | `0` ⇒ load once at startup | Hot-reload period (seconds) for the IP→ASN file; `>0` reloads without a restart |
| **High availability** | | |
| `YAGRA_ENABLE_HA` | `false` | Opt-in active/passive leader election via a PostgreSQL advisory lock |
| `YAGRA_CORE_ID` | unset | Human-readable id of this core instance in HA logs |
| `YAGRA_SESSION_KEY_FILE` | unset ⇒ per-process tokens | Path to the mounted HMAC session-signing key (sessions valid on any core and across restarts); set but unreadable/invalid ⇒ startup fails |
| `YAGRA_PAT_OIDC_IDLE_DAYS` | `30` | Days an API token owned by an **externally-authenticated** account — SSO **or** LDAP directory — survives its owner not signing in. An identity provider or a domain controller disabling an account is not something Yagra is told about, so the owner going quiet is the only signal. Local/service-account-owned tokens are unaffected. The variable keeps its `OIDC` name so running deployments do not break; the rule covers every external kind. Clamped 1–365 |
| **MCP (AI clients)** | | |
| `YAGRA_ENABLE_MCP` | `false` | Mount the MCP tool surface at `/mcp` on the API port (auth always required) |
| `YAGRA_MCP_ALLOWED_HOSTS` | unset ⇒ any `Host` accepted | Comma-separated `Host`-header allowlist for `/mcp` (DNS-rebinding hardening) |
| **Analysis & RCA rate caps** | | |
| `YAGRA_ANALYSIS_MAX_CONCURRENT` | `4` | Max concurrently-running Troubleshoot analyses |
| `YAGRA_ANALYSIS_RATE_PER_MIN` | `30` | Max new analyses admitted per minute |
| `YAGRA_RCA_MAX_CONCURRENT` | `2` | Max simultaneous LLM root-cause generations (billed external calls) |
| `YAGRA_RCA_RATE_PER_MIN` | `10` | Max new root-cause generations per minute |
| `YAGRA_RCA_CACHE_SECS` | `900` | RCA report cache lifetime (seconds); `force` bypasses the cache but not the caps |
| `YAGRA_RCA_MAX_TURNS` | `6` | How many tool-calling turns an LLM root-cause analysis may take before it must answer. **`1` restores the pre-v0.1.23 single-shot behaviour exactly** — no tools are offered and the provider request is byte-identical to before |
| `YAGRA_RCA_TASK_BUDGET_SECS` | `240` | Wall-clock ceiling for one root-cause analysis including its tool calls. Hitting it returns the model's last answer rather than failing the request |
| **Upgrade from the WebUI** (deployment **A**; the last six are read by the `yagra-updater` sidecar, not by core) | | |
| `YAGRA_UPGRADE_DIR` | unset ⇒ apply half off | Directory core and the sidecar hand requests through (`/data/upgrade` in `docker-compose.deploy.yml`, on a shared volume). Setting it is what tells this deployment it *has* an upgrade mechanism: unset, Settings ▸ Upgrade still answers what is running and what schema it carries, but says the deployment cannot be upgraded from the WebUI and offers no releases, no apply button and no switch — a release list can only come from the sidecar, so there is nothing to move to. Note this is distinct from the directory being set with no sidecar answering, which the page reports as a fault rather than as a property of the deployment. **Nothing in this directory is ever executed** — it carries a request file, a heartbeat and uploaded archives only |
| `YAGRA_UPGRADE_BUNDLE_MAX_BYTES` | `4294967296` (4 GiB) | Ceiling on an uploaded image archive, enforced as the bytes land. Three release images saved together come to roughly a gigabyte, so this is not a working limit — it is what catches the wrong file being dragged into the browser before it fills the filesystem PostgreSQL is on |
| `YAGRA_UPGRADE_REPO` | `ghcr.io/horryworks` | Where **releases** are looked for. Deliberately its own variable and not `YAGRA_IMAGE_REPO`: where releases live is not necessarily where this deployment's current images came from, and a box pulling SHA-tagged builds from a private mirror would otherwise point the release picker at a registry holding none. Fixed by the host either way — no API request can name a registry |
| `YAGRA_UPGRADE_CHECK_SECS` | `86400` (daily) | How often the sidecar lists the available releases. The list only fills a picker, so checking more often buys nothing. Switching the mechanism off in the WebUI stops the call entirely |
| `YAGRA_UPGRADE_ALLOW_BUNDLE` | `0` | Allow installing an uploaded `docker save` archive. **Widens the Admin-to-host-root path** from "a published tag of our three images" to "any image the archive contains", which is why it is a host setting and cannot be turned on from the WebUI. Set it only for a site with no reachable registry at all. Everything after the load is unchanged — same backup, same composition swap, same provenance check, and the archive must contain the tag the operator claimed |
| `YAGRA_UPGRADE_MIN_FREE_BYTES` | `3221225472` (3 GiB) | Free space an upgrade demands **before it writes anything**. The pre-upgrade backup is a full PostgreSQL dump plus a VictoriaMetrics snapshot, and the three release images unpack beside them — all on this host. Without the check, a host that is already full has the backup written onto it first and *then* fails to pull, which makes the situation worse rather than better. The smaller of the Docker storage and the deployment directory is what is measured; a host where neither can be measured proceeds and says so. `0` switches the check off; a value that is not a plain number of bytes (`3g`, a typo) falls back to the default rather than being honoured, so the check stays on |
| `YAGRA_UPGRADE_KEEP_RELEASES` | `1` | How many releases to keep **behind** the one being installed — this repository's three images, and the `yagra-backup-*` directories in the deployment directory. Nothing else on the host is touched, and the tidy-up runs only after the new version has been seen healthy. `1` is the default because the WebUI's "go back" is a single hop to the release you came from, and that hop must not need a re-download; going further back is already a manual operation and may re-pull. Raise it on a closed network where a re-pull is impossible. `0` keeps only the release now installed — but never fewer than one backup, because the newest one is what this very upgrade just took |
| `YAGRA_DOCKER_GID` | `0` | Group the sidecar runs as. Only matters if you move its uid off `0`; root reaches the socket whatever the gid says |
| **NATS Auth Callout (per-poller bus credentials)** | | |
| `YAGRA_NATS_POLLER_PASSWORD` | unset ⇒ callout off | **The only one of these you would ever set, and the remote-poller switch sets it for you.** Its presence is what makes core answer callout requests at all, because it is the shared bootstrap secret a poller with no token of its own presents. The NATS server config consumes the same value for its static `poller` account |
| `YAGRA_NATS_CALLOUT_SEED_FILE` | unset ⇒ use the stored key | **Legacy override.** Path to a mounted NATS account nkey seed. Since v0.3.2 core generates its own signing key on first start and keeps it sealed in the database, so there is nothing to mount; a path here still takes precedence, for a deployment that set one up before that existed |
| `YAGRA_NATS_CALLOUT_ACCOUNT` | `$G` | NATS account minted poller users are placed into. It has to match the account the bus's `callout.conf` names — and that file is written from this same value, so leave it alone unless the broker's accounts were customized |
| `YAGRA_BUS_AUTH_CALLOUT` | unset ⇒ off | Poller only. `1` or `true` makes the poller present its own `YAGRA_POLLER_ID` as the bus username, which is the name Auth Callout scopes its permissions on. Left off — the default, and what the remote-poller switch configures — it presents the username written in `YAGRA_BUS_URL`, the shared static account. Turn it on only where the callout is enabled: with the callout off, a poller announcing its own id matches no static account and the bus refuses it. |
| **Observability** | | |
| `YAGRA_DISK_WATCH_PATHS` | `/=root` | Filesystems host self-metrics report capacity for (comma-separated `path` or `path=alias`); read by core **and** poller |
| `YAGRA_OTEL_ENDPOINT` | unset ⇒ logs only | OTLP/HTTP endpoint for OpenTelemetry trace export (falls back to `OTEL_EXPORTER_OTLP_ENDPOINT`) |
| `OTEL_TRACES_SAMPLER` / `_ARG` | `parentbased_always_on` | Trace sampler; use `parentbased_traceidratio` + arg (e.g. `0.01`) at scale |
| `YAGRA_LOG_DIR` | unset ⇒ stdout only | Directory for hourly-rotated JSON-lines log files, written **in addition to** stdout. Exists for deployments where nobody can reach `docker logs`, so a panic or OOM would otherwise leave no retrievable trace; the support bundle reads these files back over HTTP. Set in `docker-compose.yml` and `docker-compose.deploy.yml` — clear it in `.env` to turn it off. Writes are non-blocking and drop rather than stall the poll loop; an unwritable directory degrades to stdout-only with a warning instead of failing startup |
| `YAGRA_LOG_RETAIN_HOURS` | `48` | Hourly log files kept in `YAGRA_LOG_DIR`, pruned automatically, so an unattended deployment cannot fill its volume with its own logs |
| `RUST_LOG` | `info` | Log level (e.g. `info,yagra_core=debug`) |

### Yagra-poller

| Variable | Default | Purpose |
|---|---|---|
| **Identity & bus** | | |
| `YAGRA_BUS_URL` | unset ⇒ idle | NATS bus URL (only backing-service connection the poller makes) |
| `YAGRA_POLLER_ID` | `local` in `docker-compose.deploy.yml`; standalone: hostname, else `poller-<hex>` | Stable, unique, subject-safe poller identity. Core is given the same value, so both halves name the same poller |
| `YAGRA_POLLER_POOL` | `default` | The pool this poller **starts** in. From v0.3.4 core owns the pool after first contact, so a move made at Settings ▸ Pollers survives a container recreate |
| `YAGRA_BUS_CA_FILE` | unset ⇒ plaintext | CA/server cert pinned for the `tls://` bus |
| `YAGRA_MAX_CONCURRENT_POLLS` | `256` | Max concurrent in-flight probes. A bound on what is *in flight*, not a rate — the polls/s it yields is this number divided by how long one probe takes. At this default, one poller was measured serving **50,000 ICMP nodes on a 30-second interval** (1,675 polls/s) at 11–14% CPU with nothing missed. Read `yagra_poll_demand_per_second` before changing it: `rate(yagra_poll_jobs_executed_total[5m])` divided by it is the fraction of the configured polling actually being done, and `yagra_poll_cycles_missed_total` only moves once a poller is so far behind that a whole interval passes unserved — it read 0 while a poller served 2% of what it owed. Read `yagra_poll_inflight` beside them, because raising this only helps when the gauge is pinned at the cap. Since v0.3.5 a permit is held only while a probe is actually running, so this really is a bound on probes rather than on jobs queueing for a busy device. The same budget also caps concurrent SNMP table walks, so lower it for a small site **Sizing it:** a poller runs `permits ÷ (how long one poll holds a permit)` polls per second, and that is an identity rather than an approximation — measured at 256, 512 and 1024 permits, with occupancy at 99.5% of the cap in all three. Both terms are on `/metrics`: the rate you owe is `yagra_poll_demand_per_second`, and the hold is `yagra_poll_phase_seconds_sum{phase="execute"}` plus `{phase="publish"}` over their `_count`. So `permits ≥ demand × hold`. ⚠️ **Take the hold from your own deployment, per `kind`, in the regime you actually run in** — it is set by how your devices answer, not by Yagra, and the fleet-wide average shifts with capacity because the mix of checks served shifts. Measured in the lab: a device that answers costs 4.3 ms for an ICMP check, 5.3 ms for an SNMP scalar and 596 ms for an interface-table walk, while a device that has stopped answering costs a full timeout on **every** check it owns — 1 s for ICMP, 4 s for most SNMP walks, 10 s for the MAU walk. Across a whole fleet that is roughly a factor of twenty, so a fleet that has gone dark wants more pollers rather than a bigger number here. |
| `YAGRA_ADOPT_RATE_PER_SEC` | `200` | Checks/sec used to size the jitter window when adopting another poller's work; `0` = jitter across the whole interval |
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
| `YAGRA_LOG_DIR` | unset ⇒ stdout only | Directory for the poller's own hourly-rotated log files, and **the switch that decides whether this poller can appear in a support bundle at all**. The bundled compose files set it two different ways on purpose: a **co-located** poller gets `/var/log/yagra/pollers`, a subdirectory of core's shared log volume that core reads back off disk, while a **remote-site** poller (`docker-compose.poller.yml`) gets `/var/log/yagra` on a volume of its own and ships a window of it over the bus on request. Unset, the poller logs to stdout only, does not advertise the `log-ship` capability, and the bundle records it as unrepresented rather than waiting on it |
| `YAGRA_LOG_RETAIN_HOURS` | `48` | Hourly log files kept in `YAGRA_LOG_DIR`, pruned automatically. Budgeted separately from core's, so a poller's log cannot displace core's own |
| `RUST_LOG` | `info` | Log level |

> **Compose-only vars** are consumed by Docker Compose / the NATS config, never by the Rust binaries — the binaries only ever see the final assembled `YAGRA_BUS_URL` etc. See `.env.example`:
>
> - Images & stores: `YAGRA_IMAGE_TAG`, `POSTGRES_PASSWORD`
> - Host port mappings: `YAGRA_API_PORT`, `YAGRA_API_BIND`, `YAGRA_WEB_PORT`, `YAGRA_SYSLOG_PORT`, `YAGRA_TRAP_PORT`, `YAGRA_FLOW_PORT`, `YAGRA_SFLOW_PORT`, `YAGRA_NATS_PORT`
> - WebUI TLS: `YAGRA_WEB_TLS` (compose), `YAGRA_TLS_DIR` (core — where the certificate is materialized)
> - Bus TLS + auth (D): `YAGRA_CERT_DIR`, `YAGRA_NATS_CORE_PASSWORD`, `YAGRA_NATS_POLLER_PASSWORD` (also read by core as the Auth Callout bootstrap secret, and what decides whether the callout runs). There is deliberately **no** `YAGRA_NATS_CALLOUT_ISSUER` since v0.3.2: the account public key is written straight into the bus's `callout.conf` by the same one-shot that writes the rest of its configuration, so the two cannot drift apart across an upgrade
> - Mounted key directories: `YAGRA_SESSION_KEY_DIR` (holds `session.key` for `YAGRA_SESSION_KEY_FILE`); `YAGRA_CALLOUT_SEED_DIR` (holds `account.seed` for `YAGRA_NATS_CALLOUT_SEED_FILE`) is legacy — nothing needs it now
> - Poller log directory: `YAGRA_POLLER_LOG_DIR` (default `/var/log/yagra/pollers`) — what `docker-compose.deploy.yml` passes to the co-located poller as its `YAGRA_LOG_DIR`. Set it empty to keep that poller on stdout only
> - IP→ASN updater sidecar: `YAGRA_IPASN_URL` (dataset URL), `YAGRA_IPASN_REFRESH_SECS` (fetch cadence; default `604800` = weekly)
> - Remote-site self-upgrade (D): `COMPOSE_PROFILES` — Docker's own variable, and at a monitored site the switch that decides whether that site can install a release. `self-upgrade` starts the `yagra-poller-updater` sidecar and makes the poller advertise the capability; an empty value leaves no container there holding a Docker socket. An issued bundle writes it

---

## Distributed tracing (OpenTelemetry)<a id="tracing"></a>

Every binary emits structured logs and a Prometheus `/metrics` endpoint out of the box. **Distributed tracing is opt-in:** set `YAGRA_OTEL_ENDPOINT` (or the standard `OTEL_EXPORTER_OTLP_ENDPOINT`) to an OTLP/HTTP collector and core + poller export spans that stitch a single poll end to end — core's dispatch span → the poller's poll span → core's result-ingest span — plus a span per northbound API request. Unset, there is **zero tracing overhead** (logs only), so the single-node MVP needs no collector.

- **Try it locally:** `docker compose --profile tracing up` starts a bundled Jaeger (UI at http://localhost:16686), then uncomment `YAGRA_OTEL_ENDPOINT: http://jaeger:4318` on **both** `core` and `poller` in `docker-compose.yml`.
- **At scale, sample.** Tens of thousands of nodes polling on an interval would otherwise emit a trace per poll. Set `OTEL_TRACES_SAMPLER=parentbased_traceidratio` and `OTEL_TRACES_SAMPLER_ARG=0.01` (1%); `parentbased_*` keeps a whole trace's decision consistent across the core⇄poller hop. The trace context rides the bus in a `trace_context` field that is **omitted from the wire when tracing is off** and ignored by an N-1 peer (so it stays N/N-1 safe).
- **Production:** point the endpoint at an OpenTelemetry Collector that forwards to your backend (Tempo, Jaeger, Honeycomb, …). A remote-site poller needs its own reachable collector endpoint, separate from the NATS bus.

---

## Upgrades & backups<a id="upgrades--backups"></a>

Upgrades are designed to be low-effort and **never** lose or corrupt data:

- **Settings ▸ Upgrade does it for you — the ordinary way to upgrade (v0.2.2+, deployment **A**).** Every other way of installing Yagra — from source, natively, or from a composition without the `yagra-updater` sidecar — has no such mechanism, and the page says so plainly instead of offering controls that would fail. The page lists the releases this deployment can move to and runs backup → pull → install the composition carried inside the target image → recreate → verify. The work is done by a `yagra-updater` sidecar, which is the only container holding the Docker socket — core never has it. Requesting an upgrade needs **manage-the-deployment**, which only an Admin holds; it is audited, and it is not on the MCP surface. A switch on the same page turns the mechanism off; the setting lives in PostgreSQL, so it survives the upgrades it governs.
  - **A release older than the running one is a downgrade, and it is offered only when it can actually boot.** A migration may declare a *compatibility floor* — the oldest version that can still run once it has been applied — and anything below the current floor is shown greyed out with the reason rather than hidden. Nothing is deleted by going back: columns the newer version added stay in place, unread.
  - **No registry reachable?** Run `docker save` on the three release images where you can reach them, and upload the archive at the same page. It needs `YAGRA_UPGRADE_ALLOW_BUNDLE=1` on the host as a second, deliberate opt-in, because `docker load` installs whatever the archive contains — see the variable reference below before turning it on.
- **DB migrations are expand-contract and run automatically** on core startup. N→N+1 is always supported; `yagra-core migrations` prints the set a binary embeds as JSON with no database and no configuration, so an upgrade can be planned by running it inside the target image first.
- **The bus is version-tolerant (N/N-1).** A new core works with old pollers during a rollout, so you can upgrade core first and pollers after.
- **Rolling upgrades.** Pollers are stateless — replace them in any order. For Docker, pull the new tag and `up -d` (see **A**). Remote pollers: pull and `up -d` per site; a pool briefly down falls back to legacy publish, so no node goes dark.
- 🚨 **A remote site brought up before v0.3.4 must be recreated once, at the site, before the next central upgrade.** Run this in that site's deployment directory:

  ```bash
  docker compose -p yagra-poller -f docker-compose.poller.yml up -d
  ```

  The site updater is a **named** service, so an apply recreates only `poller` and never the updater
  itself — a container keeps the definition it was created with. An updater created before v0.3.4
  runs `docker compose` from inside its own filesystem, so the composition's certificate bind, which
  defaults to the relative `./certs`, resolves to a directory that exists only in that container.
  Docker creates the missing host path **empty** rather than failing, and the replacement poller
  starts with nothing to trust the bus with and never reconnects — while the site reports
  `apply … succeeded` and Settings ▸ Upgrade reports the deployment aligned. A pool with other live
  pollers keeps monitoring because its nodes move; a single-poller pool goes dark until this is run
  by hand. From v0.3.4 the updater resolves that bind against the host's own directory and refuses
  to recreate a poller whose certificate directory is empty, so this is needed exactly once.
  - ⚠️ **Check `YAGRA_IMAGE_TAG` in that site's `.env` first.** The updater passes the tag on its own
    command line, so `.env` can be left pinned to something the registry does not have (a
    development build, say) without anyone noticing — and this command, run by hand, *does* read
    `.env`. Set it to the release you are on, or the recreate fails at the pull.
- **Take a backup before a major upgrade** — see [Backup & restore](#backup--restore) below.

---

## Backup & restore<a id="backup--restore"></a>

Yagra does not ship a backup product. PostgreSQL, VictoriaMetrics and ClickHouse each have mature
mechanisms of their own, and layering Yagra-specific orchestration on top would make the *restore*
procedure depend on the Yagra version that took the backup. What Yagra ships is the procedure, as a
script, plus a second script that proves a backup can actually be restored.

### What to back up

| Tier | Data | Required? |
|---|---|---|
| **1 — must not lose** | **KEK** (`kekdata` / `YAGRA_KEK_FILE`), PostgreSQL (`pgdata`), VictoriaMetrics (`vmdata`) | **Yes** |
| 2 — loss-tolerant, TTL'd | VictoriaLogs (`vldata`, 30 days), ClickHouse flow store (`chdata`, 30 days) | No — both expire on their own schedule and hold no must-preserve state |
| 3 — rebuildable | Redis | No — it mirrors state PostgreSQL already owns |

**The PostgreSQL dump is the whole configuration**, not just part of it: nodes, folders, profiles,
thresholds, classification rules, notification channels, routing rules, forwarding destinations,
URL/DNS checks, users, alert history and the audit log.

> **The KEK is item #1, and it must live somewhere the database dump does not.**
> Losing it makes every stored monitoring credential permanently undecryptable — a database that
> restores perfectly and yet cannot poll anything. Keeping both copies in one place means the single
> incident that destroys that place destroys the pair. Take `YAGRA_SESSION_KEY_FILE` and
> `YAGRA_NATS_CALLOUT_SEED_FILE` with it when they are set.

### Taking one

```bash
./scripts/yagra-backup.sh /srv/backups/yagra          # writes the tier-1 set + a manifest
```

The metrics snapshot is the one part that can be skipped without failing the run — a site with no
metrics store still needs its configuration backed up. From v0.2.4 a skip is **stated** rather than
inferred: the manifest carries `metrics_snapshot` (a snapshot name, or `null`), and the closing
summary says in words when a backup holds no metrics. Read that line before treating a backup as
complete.

### Verifying it — this is the part that matters

```bash
./scripts/yagra-restore-verify.sh /srv/backups/yagra/yagra-backup-<stamp>
```

It restores into a **throwaway** compose project (`yagra-verify`, torn down with `down -v` on exit,
and it refuses to run if you point it at the production project name) and asserts four things:

1. `/readyz` returns 200 — core starts against the restored data,
2. the node count matches the manifest — the configuration came back,
3. **every credential still decrypts** — the KEK came back *and matches the ciphertext*,
4. the `audit_log` row count matches — the "who changed what" trail survived.

Assertion 3 is the one nothing else can infer: a restore can look perfect while the key is a
different one, and nothing says so until the next poll fails. You can check it at any time with
`GET /api/v1/credentials/health`. A backup containing no credentials reports **SKIPPED**, not PASS.

**Run the verification before a destructive migration** (the built-in-catalog reseeds, migrations
`0020`–`0022`), which is what ADR-017 requires a rollback path for.

### Restoring forward only

The restore target must be **the same version as the backup, or newer** — migrations only move
forward. **Downgrade restore is unsupported**, and the verification script refuses it rather than
letting it corrupt data quietly.

---

## Configuration bundle (moving a configuration between deployments)<a id="config-bundle"></a>

A backup restores *this* deployment. A **configuration bundle** is the other job: taking the
monitoring configuration you built in one deployment and applying it to a different one — staging to
production, or an old server to a new one. **Settings ▸ Configuration bundle**, or
`GET`/`POST /api/v1/config/bundle`. Admin only, in both directions.

```bash
# Export from the source deployment. Add --cacert <file> (or -k while evaluating) if the
# deployment is still on its self-signed bootstrap certificate.
curl -sS -H "Authorization: Bearer $TOKEN" \
     https://source/api/v1/config/bundle > bundle.json

# Check what it would do on the target — the real import, rolled back
curl -sS -X POST -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     --data-binary @bundle.json \
     'https://target/api/v1/config/bundle?dry_run=true'

# Apply it
curl -sS -X POST -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     --data-binary @bundle.json https://target/api/v1/config/bundle
```

**A bundle is not a backup.** It carries no secrets, no metrics, no events and no history. Use it to
replicate a configuration; use the scripts above to survive losing a server.

### What it carries

Device profiles, metric sets and their links, classification rules, node groups, nodes, thresholds,
URL and DNS monitors, forwarding destinations, event sources and rules, report templates and
schedules, analysis schedules, and the global polling default.

### What it deliberately does not, and why

| Not carried | Why |
|---|---|
| Credentials | Sealed with this deployment's KEK. A bundle carries the *id* only, and the importer keeps the reference only if the target already holds that id. |
| Notification channels and routing rules | A channel **is** its sealed config, and the API has no way to attach a config to an existing channel id — so an imported channel could never be made to work, and a rule pointing at one would notify nobody, silently. Re-create channels on the target, then the rules. |
| Users, API tokens, OIDC providers | Identity. An import is a write path; carrying accounts would make "restore a config" the shortest route to granting yourself a role. |
| Dashboards | Widgets embed node/group references the importer cannot validate, so a carried layout would render broken with no error. |
| Retention windows | A policy of the *target* — its disks, its compliance window — and lowering one deletes data. Not something an import should change under you. |
| Meraki / LLM / poller / MIB config | Provider credentials, or properties of the target deployment rather than of the configuration being moved. |
| Metrics, events, flows, alert history | The time-series and event tiers. Out of scope — those stores have their own migration tools and are sized in gigabytes. |

Built-in profiles, metric sets, classification rules and trap rules are also left out: the target
seeds its own, under the same reserved ids.

### What the import does

**Upsert only.** A row whose id already exists is updated; a new one is created. **Nothing is ever
deleted, and there is no replace mode** — it would be one flag away, and that flag is what makes an
import unrecoverable. Everything runs in one transaction, so a failure leaves nothing behind.

The report names every row it skipped or changed:

- a row whose required reference is missing on the target is **skipped**, never widened (an event
  rule bound to a node that does not exist would otherwise silently match the whole fleet);
- an optional reference the target lacks is **cleared**, and counted;
- a forwarding destination or webhook source that needs a secret arrives **disabled** — re-enter the
  secret (or rotate the token) on the target, then enable it. One that already has a working secret
  on the target keeps it, and keeps its own enabled state;
- schedules get their next run **recomputed** on the target's clock.

`?dry_run=true` runs the whole import and rolls it back, so its report is exactly what applying
would do. **Run it first.**

### Size limit

A bundle is one JSON document, so it cannot grow with the fleet: the export refuses — rather than
truncating into a partial configuration that looks complete — when any table exceeds 10,000 rows. A
deployment that large is a disaster-recovery case, which is `pg_dump`, above.

---

## Data retention<a id="data-retention"></a>

How long each store keeps what. Most of it is editable at **Settings ▸ System settings ▸ Data
retention**; changes apply on the next sweep, with no restart.

| Data | Store | Default | Change it |
|---|---|---|---|
| Alert history, node-state snapshots, DNS chain changes, matched events | PostgreSQL | 90 days | Settings |
| Unmatched passive events | PostgreSQL | 24 hours | Settings |
| Report runs | PostgreSQL | 90 days | Settings |
| Traffic flows | ClickHouse | 30 days | Settings |
| Event log | VictoriaLogs | 30 days | **Container flag** (below) |
| Metrics | VictoriaMetrics | 12 months | **Container flag** (below) |
| Audit log | PostgreSQL | **kept indefinitely** | Not pruned, by design |

**The audit log is never pruned.** Who changed what must not be swept away as a side effect of
tidying logs.

### The two rows Yagra cannot change

VictoriaMetrics and VictoriaLogs take their retention as a **process start flag** and expose no API
to change it at runtime. Yagra therefore reports these values (read back from each store's own
`/flags` endpoint, so what you see is what the store is really enforcing) but cannot set them.
Changing one is a deployment edit:

```bash
# docker-compose.deploy.yml
#   victoriametrics: command: ["--retentionPeriod=24"]   # months
#   victorialogs:    command: ["-retentionPeriod=90d"]
docker compose -p yagra -f docker-compose.deploy.yml up -d victoriametrics
```

Only the edited container is recreated; the stack keeps running. Note that **shortening a window
does not delete data immediately** — VictoriaMetrics and VictoriaLogs drop out-of-retention data as
their storage merges proceed.

If a row shows *"Unknown — the store did not report a retention flag"*, the flag is not set at all
and the store is running its own built-in default; Yagra shows that rather than guessing a number.

`YAGRA_FLOW_RETENTION_DAYS` seeds the flow window on a **brand-new** deployment only. After first
boot the Settings value is authoritative, and the change is applied to ClickHouse immediately
(`ALTER TABLE … MODIFY TTL`) — including on an existing volume, where it previously had no effect.

---

## Uninstalling<a id="uninstalling"></a>

Yagra installs nothing on the host: no packages, no system services, no files outside the
deployment directory and Docker's own storage. Removing it is the install run backwards.

**Everything below must be run from the directory holding `docker-compose.deploy.yml`, and every
invocation must pass `-p yagra`.** Compose otherwise derives the project name from the directory
name and operates on a *different, empty* project — `down` then reports success having removed
nothing, while the real stack keeps running.

### Stop it, keep the data

```bash
docker compose -p yagra -f docker-compose.deploy.yml down
```

Containers and the network go; every named volume stays. `up -d` brings the same deployment back
with its configuration, alert history and metrics intact.

### Remove it completely

```bash
docker compose -p yagra -f docker-compose.deploy.yml down -v --remove-orphans
```

`-v` destroys the eleven named volumes, and that is not recoverable:

| Volume | What is lost |
|---|---|
| `pgdata` | Nodes, users, thresholds, alert history, acknowledgements, every setting |
| `vmdata` | All metric history |
| `vldata` | All passive events (syslog / traps / webhooks) |
| `chdata` | All traffic-flow records |
| `kekdata` | **The key-encryption key** — see below |
| `tlsdata`, `buscerts` | Materialized certificates (both are copies of PostgreSQL rows) |
| `logdata`, `pollerbuf`, `upgradedata`, `ipasndata` | Rotated logs, the store-and-forward buffer, the upgrade hand-off, the IP→ASN dataset |

**Losing `kekdata` is the one that cannot be undone by restoring a backup.** Every stored monitoring
credential — SNMP communities, SNMPv3 credentials, API tokens, the bus private key — is envelope-
encrypted with that key, and it cannot be regenerated. A database dump taken without it restores
perfectly and yet cannot poll anything.

If there is any chance of coming back, take the KEK out before `-v` removes it:

```bash
docker run --rm -v yagra_kekdata:/kek busybox cat /kek/key > yagra-kek.bin   # 32 bytes
```

⚠️ **A [configuration bundle](#config-bundle) is not a substitute** — it deliberately carries no
credentials at all, only their ids, and the importer keeps a reference only if the target already
holds that id. On a deployment rebuilt with a new KEK, those ids do not exist and the references are
dropped. See [Backup & restore](#backup--restore) for the full tier list.

### Also remove the images and the directory

```bash
docker compose -p yagra -f docker-compose.deploy.yml down -v --rmi all --remove-orphans
cd .. && rm -rf yagra          # the compose file and .env, which holds POSTGRES_PASSWORD
```

`--rmi all` removes the images used by every service, including the backing stores (`postgres`,
`redis`, `nats`, `victoria-metrics`, `victoria-logs`, `clickhouse`, `busybox`, `alpine`,
`docker:28-cli`). Docker skips any that another project is still using.

### Remote pollers are separate stacks

A poller deployed at a remote site (deployment **D**) is its own Compose project on its own host.
Nothing above touches it, and after the central stack is gone it will retry the bus forever. Remove
each one on its own host:

```bash
docker compose -f docker-compose.poller.yml down -v
```

### Checking for leftovers

If the compose file has been lost, or you just want to confirm nothing remains, Compose labels
everything it created:

```bash
docker ps -a     --filter label=com.docker.compose.project=yagra
docker volume ls --filter label=com.docker.compose.project=yagra
docker network ls --filter label=com.docker.compose.project=yagra
```

Anything listed can be removed with `docker rm -f`, `docker volume rm` and `docker network rm`.

There is no uninstall action in the WebUI — **Settings ▸ Upgrade** only moves a deployment between
releases. Uninstalling is deliberately a host-side act.

---

## Directory sign-in (LDAP / Active Directory)

Configured at **Settings ▸ Auth ▸ Directory (LDAP/AD)** (ADR-041). No environment variable turns it
on — an empty configuration is the off state, and nothing is dialled until one is saved.

**How a sign-in works.** The ordinary login form is the whole UI. Yagra looks the submitted name up
in its own `users` table first: a local account is answered locally and never touches the directory.
Otherwise a service account searches the directory for the person, and a **second, independent
connection** binds as the DN that search returned. A DN is never built from the typed name — that
breaks on any OU layout the pattern did not anticipate.

**Always keep one local administrator.** This is the rule to take away from this section. Local
accounts are tried first, so a directory that is down cannot lock you out — but that protection
exists only while a Yagra binary that knows about directories is running. If every administrator is
a directory account and the release is rolled back, nobody can sign in.

| Setting | What it is |
|---|---|
| Transport | **LDAPS** (implicit TLS, usually 636) or **StartTLS** (usually 389). There is no plaintext option, deliberately — it would put the bind password on the wire in the clear with no warning. |
| CA certificate | PEM for a private/enterprise CA. Almost always needed: an internal AD presents a certificate the container's bundle does not trust. **There is no way to skip certificate verification, and there will not be one** — configuring the CA is the supported answer. |
| Service account DN + password | Used for the search leg only. Envelope-encrypted at rest (ADR-018) and never returned by the API. It cannot be blank: a bind with a DN and an empty password is an *unauthenticated* bind, which many directories answer with success. |
| User filter | Must contain `{username}`. Without it the filter matches every entry under the base DN. AD default: `(&(objectClass=user)(sAMAccountName={username}))` |
| Username attribute | The canonical name, stored as the Yagra username. AD: `sAMAccountName`. Taken from the directory rather than from what was typed, because AD matches it case-insensitively while Yagra's own username column does not. |
| Identity attribute | The immutable per-entry id — `objectGUID` on AD, `entryUUID` on OpenLDAP. This is what lets somebody be renamed without becoming a second account. An entry that does not return it is refused rather than stored under an empty id. |
| Group membership attribute | Read from the user's own entry. AD populates `memberOf` natively; OpenLDAP needs the `memberof` overlay. |
| Group base DN + filter | Optional second lookup, for directories without that overlay — **and the only way to resolve nested groups on AD**, where `memberOf` is not transitive. Use the matching-rule OID: `(member:1.2.840.113556.1.4.1941:={user_dn})` |
| Group → role | The same mechanism the SSO provider uses. A group matches by full DN *or* by its name, case-insensitively, and the highest matching role wins. With no mapping and no default role **every login is denied**, so saving that combination while enabled is refused. |

**Use the Test button.** It exercises the *saved* configuration and reports each stage separately,
because it deliberately never binds as the user — so a green result proves the connection, the TLS
trust, the service account and the role mapping, not that a password would be accepted. Given a
username it also shows the DN, the groups, and **the role that person would receive**; a denial
there is the commonest misconfiguration and the one the login form cannot distinguish from a wrong
password.

**Account lockout.** Yagra locks an account out after 5 failed attempts, and a common AD
`lockoutThreshold` is 5–10 — so repeated typos at Yagra's login form can lock somebody out of the
domain, not merely out of Yagra. Watch `yagra_ldap_bind_total` if that is a concern.

Turning the directory off revokes the sessions of every account it provisioned. Their rows remain,
so switching it back on restores them.

## SAML

**Yagra does not implement SAML, and that is a decision rather than a gap** (ADR-041). XML signature
verification — canonicalization, XXE, signature wrapping — is a well-known source of authentication
bypasses, and the Rust service-provider implementations are not mature enough to bet an
authentication path on.

For a SAML-only identity provider, put a **SAML→OIDC bridge in front**: run
[Keycloak](https://www.keycloak.org/) or [Dex](https://dexidp.io/) as an OIDC provider that federates
to your SAML IdP, and point Settings ▸ Auth at the bridge. The operator requirement is met, the
bridge is maintained by people who specialise in exactly this, and Yagra's authentication surface
stays one protocol smaller.

---

## Security notes

- **Bus TLS is mandatory across any trust boundary.** Job messages carry plaintext device credentials — never expose NATS `:4222` plaintext to a remote site (see **D · Step 1**).
- **The KEK is a mounted file, never an env value.** Provide it via `YAGRA_KEK_FILE`; the ephemeral fallback is dev-only.
- **Images run non-root** (core uid 10001, poller uid 10002 with a `cap_net_raw+ep` file capability, web nginx uid 101). Only the poller gets `NET_RAW`.
- **Credentials are never logged** — SNMP communities, SNMPv3 auth/priv, and API tokens are encrypted at rest and redacted from logs, API responses, and metric labels.
