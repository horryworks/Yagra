# Yagra

Yagra is a **network monitoring system (NMS)** that monitors network devices and
servers via **ICMP / SNMP / API calls**. It continuously watches liveness,
performance, and thresholds, and raises alerts on anomalies. It runs in Docker and
is architected from the start for **tens of thousands of nodes** and **distributed
polling**. Users access it through the WebUI.

> Status: **v0.1.17.** A functional stack (ICMP / SNMP v2c+v3 / URL monitoring / DNS monitoring / Cisco Meraki via
> the read-only Dashboard API, passive event monitoring, discovery & classification, alerting,
> dashboards, and reports) over PostgreSQL, Redis, NATS, and VictoriaMetrics via Docker Compose.
> Single-node by default, it now scales out with **distributed poller pools** — remote pollers at
> branch sites, assigned by location affinity and failed over automatically. A remote poller now
> **rides out a network partition**: it keeps polling locally, buffers results, and **backfills the
> metrics** for the outage when the link returns (alerts resume from "now", never replayed).
> Yagra now **collects traffic-flow records** (NetFlow v5/v9, IPFIX, and sFlow) into a dedicated
> ClickHouse store and surfaces per-node **top talkers, ports, protocols, and AS-level conversations**,
> with offline **IP→ASN enrichment** naming the autonomous systems behind the traffic; incoming
> **SNMP traps are decoded to human-readable names** with built-in trap rules. Every
> binary exports **OpenTelemetry traces** (opt-in) and now reports **host-resource trends** (CPU /
> load / memory / disk) for the core and each poller in System Health, and shuts down gracefully on
> restart. The WebUI switches between **English and 日本語** on the fly across most screens, and
> **SNMPv3** nodes collect per-interface metrics via a GETBULK table walk. Users can now sign in with
> **single sign-on (OpenID Connect)** alongside local accounts, and the **core runs highly
> available** — multiple instances with automatic leader election and failover (opt-in). In an HA
> set, **user sessions can be shared across cores** (opt-in) so a failover no longer forces re-login,
> and **remote pollers on an exposed bus can be scoped to their own pool's credentials** (opt-in),
> narrowing what a compromised poller can reach. **AI assistants can now query Yagra through a built-in,
> opt-in MCP tool surface** at `/mcp` — mostly read-only status, metrics, flow, and event queries plus
> on-demand Troubleshoot analyses, alongside a few audited write actions (acknowledge an alert, open a
> maintenance window, poll now); it is authenticated by API token and cannot change device
> configuration. HA stores remain a configuration step away, not a rewrite.

## Components

Each backend component is a workspace crate under `crates/`; the WebUI lives under `web/`.

| Component | Role | Crate / dir |
|---|---|---|
| Yagra-core | Orchestration, scheduling, northbound API | `crates/yagra-core` |
| Yagra-poller | ICMP/SNMP/API polling (stateless, scalable) | `crates/yagra-poller` |
| Yagra-discovery | Device discovery & classification | `crates/yagra-discovery` |
| Yagra-alert | State machine, hysteresis, dependency suppression | `crates/yagra-alert` |
| Yagra-ingest | Passive-event parsing (syslog / SNMP traps) + rate limiting | `crates/yagra-ingest` |
| Yagra-bus | Job distribution, poller fan-out | `crates/yagra-bus` |
| Yagra-transport | ICMP/SNMP/HTTP abstraction | `crates/yagra-transport` |
| Yagra-topology | Dependency graph & map | `crates/yagra-topology` |
| Yagra-web | Dashboards & visualization | `web/` |

Shared libraries: `crates/yagra-common` (cross-cutting types) and `crates/yagra-secrets`
(envelope encryption for credentials).

> Crate directory names match the functional `Yagra-*` names above (e.g. `crates/yagra-core`).

## Tech Stack

- **Backend:** Rust — Tokio, Axum, sqlx (PostgreSQL). Cargo workspace under `crates/`.
- **Frontend:** React + TypeScript + Vite, uPlot for time-series. Under `web/`.
- **Stores:** PostgreSQL (metadata), Redis (cache/locks/poller-assignment), VictoriaMetrics (TSDB — metrics).
- **Bus:** NATS (core⇄poller).
- **Northbound API:** REST (`/api/v1`).
- **Deploy:** Docker / Docker Compose (MVP) → Kubernetes (scale-out / HA).

## Passive event monitoring (syslog / SNMP traps / webhooks)

Besides active polling, Yagra receives events and matches them against operator-defined
rules (substring / regex) to raise alerts, which can be forwarded to PagerDuty (Events
API v2) and Jira Service Management (Alerts API) with native fire/resolve lifecycle:

- **syslog** (UDP 514) and **SNMP traps** (UDP 162, v1/v2c + informs) are received by the
  poller and forwarded to core over the bus. Enable per site via `YAGRA_SYSLOG_BIND` /
  `YAGRA_TRAP_BIND` (see `docker-compose.yml`). **SNMPv3 traps are not supported yet.**
- **Webhooks** are received on the core API: `POST /api/v1/ingest/webhook/<source-id>`
  with a per-source bearer token (create sources under *Alerts ▸ Event sources*).
- Rules (*Alerts ▸ Event rules*) assign severity, an auto-close TTL, an optional
  clear-pattern (e.g. link-up clears link-down), and a fire threshold (N events in M
  seconds). Received events are browsable under *Alerts ▸ Events*.

> **Deployment notes:** event→node correlation uses the datagram **source IP** — if
> Docker's bridge networking rewrites it on your host, run the poller with
> `network_mode: host`. If the host already runs a syslog daemon on port 514, remap the
> published port. Poller-side rate limits (per-source and global) bound event floods.

## Connecting an AI client (MCP)

Yagra can expose a **read-only [MCP](https://modelcontextprotocol.io) tool surface** (ADR-028) so an
AI client — Claude Code, Claude Desktop, or another MCP-capable assistant — can query live monitoring
state in natural language: *"which nodes are down?"*, *"summarize the active alerts"*, *"show CPU on
edge-router-1 for the last hour"*, *"run anomaly detection and tell me what looks wrong"*. Most tools
are **read-only** — the AI sees the same data the WebUI does — and it can launch the same on-demand
**Troubleshoot** analyses (which only read metric history and return findings). A few **write** tools
can act on the *monitoring* system — acknowledge an alert, open a maintenance window, or trigger an
immediate poll — but only for a token whose role permits it (a **Viewer** token is read-only), and
every write is recorded in the audit log. There are still **no** tools that configure or change
network devices.

Read tools: `get_fleet_summary`, `list_nodes`, `get_node_status`, `get_active_alerts`,
`get_alert_history`, `query_metrics`, `get_topology`, `top_flows`, `search_events` (syslog / traps /
webhooks), plus the Troubleshoot trio `run_analysis`, `get_analysis_findings`, `list_analyses`
(on-demand anomaly / correlation / capacity / flap analysis).
Write tools (need an Operator/Admin token; every call is audited): `ack_alert`, `open_maintenance`,
`poll_now`.

### 1. Enable the server

Off by default. Set `YAGRA_ENABLE_MCP=true` for core (uncomment it in `docker-compose.yml`, or add it
to your `.env` for the deploy compose) and restart. The endpoint is then served **on the API port** at:

```
http://<yagra-host>:8080/mcp          # Streamable HTTP transport
```

Point clients at the **core API port** (`8080`), **not** the WebUI port (`3000`) — the WebUI's reverse
proxy does not route `/mcp`, so `:3000/mcp` returns 405. When disabled the path is not mounted (404),
byte-identical to before. MCP always requires authentication, even if `YAGRA_PUBLIC_DASHBOARD` is on.

### 2. Create an API token

Sign in to the WebUI as an admin → **Settings ▸ API tokens ▸ New token** → choose **Viewer** for a
read-only assistant (all the read/Troubleshoot tools work), or **Operator/Admin** if you want it to
also acknowledge alerts, open maintenance windows, or poll on demand → copy the `yat_…` value shown
once. This is the bearer token the AI client sends. (A regular login session token works too, but it
expires; an API token is meant for an unattended client and is revocable from the same page.)

> **Reachability:** the AI client makes the HTTP call from *your* machine, not from Anthropic's cloud —
> so the client only needs network access to `<yagra-host>:8080` (same LAN, or over a VPN). No public
> inbound exposure is required unless you want to connect from the claude.ai web app (see below).

### 3. Register it with your client

**Claude Code (CLI or VS Code extension)** — use **`--scope user`** so the server is available in every
project and directory:

```bash
claude mcp add --scope user --transport http yagra http://<yagra-host>:8080/mcp \
  --header "Authorization: Bearer yat_your_token"
```

Without `--scope user`, `claude mcp add` defaults to *local* scope — the CLI sees it, but the **VS Code
extension does not load local-scope servers** (it reads user-scope and project `.mcp.json` servers only),
so `/mcp` in the extension won't show it. MCP servers are also loaded at session start, so **reload the
window / start a new session** after adding. Then `/mcp` should list `yagra` as connected; ask it to
list nodes or summarize alerts.

**Claude Desktop** — Desktop bridges to a remote HTTP server via the `mcp-remote` helper. Add this to
`claude_desktop_config.json` (Settings ▸ Developer ▸ Edit config), then restart Desktop:

```json
{
  "mcpServers": {
    "yagra": {
      "command": "npx",
      "args": [
        "-y", "mcp-remote", "http://<yagra-host>:8080/mcp",
        "--header", "Authorization: Bearer yat_your_token"
      ]
    }
  }
}
```

**Gemini CLI** — add the server to `~/.gemini/settings.json` (or a project-local
`.gemini/settings.json`). The `httpUrl` key selects the Streamable HTTP transport:

```json
{
  "mcpServers": {
    "yagra": {
      "httpUrl": "http://<yagra-host>:8080/mcp",
      "headers": { "Authorization": "Bearer yat_your_token" }
    }
  }
}
```

Restart `gemini`, then `/mcp` lists the Yagra tools. Gemini Code Assist (VS Code) reads the same
`settings.json`.

**claude.ai (web) / Team / Enterprise** — add Yagra as a **Custom Connector** (Settings ▸ Connectors).
This requires the `/mcp` endpoint to be reachable from Anthropic's servers, i.e. a **public HTTPS URL**
(e.g. front it with a reverse proxy or a Cloudflare Tunnel) — a LAN/VPN-only address won't work here.
The same public-HTTPS requirement applies to the Gemini web app / Vertex AI agent connectors.

**Any MCP client / quick check with `curl`** — the transport is plain JSON-RPC over HTTP, so you can
smoke-test without a client:

```bash
curl -sN http://<yagra-host>:8080/mcp \
  -H "Authorization: Bearer yat_your_token" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

> **Note:** because the AI reads live monitoring data (node names, addresses, alerts) and, for a
> cloud-hosted assistant, that data is sent to the model provider as conversation context, treat the
> tool output as you would any data leaving your boundary. Device credentials are **never** included in
> any tool result. Keep the token least-privileged (Viewer) and revoke it from Settings ▸ API tokens
> when a client no longer needs it.

## Deployment

Bring up a full single-node stack in one command:

```bash
docker compose up --build   # core + poller + web + PostgreSQL/Redis/NATS/VictoriaMetrics
```

WebUI on **http://localhost:3000**, API on **http://localhost:8080**. On first start core prints a
one-time `admin` password in its logs (`docker compose logs core`).

For everything else — production images, running **natively** without Docker, and **distributed
pollers** across remote sites — see **[DEPLOYMENT.md](DEPLOYMENT.md)** (日本語:
[DEPLOYMENT.ja.md](DEPLOYMENT.ja.md)). It covers all four combinations: single-node / distributed ×
Docker / native, plus the full environment-variable reference and upgrade/backup guidance.

Local development:

```bash
cargo build && cargo test              # backend (Rust workspace)
cd web && npm install && npm run dev   # frontend (Vite dev server)
```

## License

Yagra is licensed under the **GNU Affero General Public License v3.0 only**
(`AGPL-3.0-only`) — see [LICENSE](LICENSE). Because Yagra is typically operated as a
network service, note AGPL **§13**: if you run a **modified** version and let users
interact with it over a network, you must offer those users the Corresponding Source of
your modified version.

For use under terms other than the AGPL (e.g. embedding Yagra in a proprietary product,
or operating a modified version without the source-disclosure obligation), a separate
**commercial license** may be available — contact horryworks@gmail.com.
