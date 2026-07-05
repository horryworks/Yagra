# Yagra

Yagra is a **network monitoring system (NMS)** that monitors network devices and
servers via **ICMP / SNMP / API calls**. It continuously watches liveness,
performance, and thresholds, and raises alerts on anomalies. It runs in Docker and
is architected from the start for **tens of thousands of nodes** and **distributed
polling**. Users access it through the WebUI.

> Status: **v0.1.2.** A functional single-node stack (ICMP / SNMP v2c+v3 /
> URL monitoring / Cisco Meraki via the read-only Dashboard API, discovery & classification,
> alerting, dashboards, and reports) over PostgreSQL, Redis, NATS, and VictoriaMetrics via Docker
> Compose. Architected to scale out (distributed pollers, HA stores) by configuration, not rewrite.

## Components

Each backend component is a workspace crate under `crates/`; the WebUI lives under `web/`.

| Component | Role | Crate / dir |
|---|---|---|
| Yagra-core | Orchestration, scheduling, northbound API | `crates/yagra-core` |
| Yagra-poller | ICMP/SNMP/API polling (stateless, scalable) | `crates/yagra-poller` |
| Yagra-discovery | Device discovery & classification | `crates/yagra-discovery` |
| Yagra-alert | State machine, hysteresis, dependency suppression | `crates/yagra-alert` |
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

## Getting Started

```bash
# Backend (Rust workspace)
cargo build
cargo test

# Frontend (web/)
cd web && npm install && npm run dev

# Full stack (single-node Docker Compose)
docker compose up --build
```

## License

MIT
