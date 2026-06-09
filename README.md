# Yagra

Yagra is a **network monitoring system (NMS)** that monitors network devices and
servers via **ICMP / SNMP / API calls**. It continuously watches liveness,
performance, and thresholds, and raises alerts on anomalies. It runs in Docker and
is architected from the start for **tens of thousands of nodes** and **distributed
polling**. Users access it through the WebUI.

> Status: **early development.** The tested core logic and a single-process "walking
> skeleton" (core → bus → poller → metric → API) are in place; runtime integration with
> the external services (NATS, VictoriaMetrics, PostgreSQL, raw-socket ICMP, SNMP) is in
> progress.

## Components

Each backend component is a workspace crate under `crates/`; the WebUI lives under `web/`.

| Component | Role | Crate / dir |
|---|---|---|
| Yagra-core | Orchestration, scheduling, northbound API | `crates/saihai` |
| Yagra-poller | ICMP/SNMP/API polling (stateless, scalable) | `crates/banshu` |
| Yagra-discovery | Device discovery & classification | `crates/monomi` |
| Yagra-alert | State machine, hysteresis, dependency suppression | `crates/noroshi` |
| Yagra-bus | Job distribution, poller fan-out | `crates/hikyaku` |
| Yagra-transport | ICMP/SNMP/HTTP abstraction | `crates/sekisho` |
| Yagra-topology | Dependency graph & map | `crates/nawabari` |
| Yagra-web | Dashboards & visualization | `web/` |

Shared libraries: `crates/yagra-common` (cross-cutting types) and `crates/yagra-secrets`
(envelope encryption for credentials).

> Crate directories still carry their original short names (`saihai`, `banshu`, …); the
> functional `Yagra-*` names above are canonical and a crate rename is planned.

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

# Full stack (skeleton)
docker compose up --build
```

## License

MIT
