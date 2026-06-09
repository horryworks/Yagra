# Yagra-web — WebUI

React + TypeScript + Vite frontend for Yagra. Talks to the Yagra-core northbound
REST API (`/api/v1`) and receives live updates via Server-Sent Events (SSE).

Visualization:
- **uPlot** — time-series charts (fast at many series).
- Topology map (planned) — Cytoscape.js / React Flow.

## Development

```bash
npm install
npm run dev     # Vite dev server (proxies /api to Yagra-core on :8080)
npm run test    # Vitest
npm run build   # type-check + production build
```

## Structure

- `src/types/` — API types mirroring the backend shapes.
- `src/services/` — the single typed boundary to the backend (`api.ts`, `sse.ts`).
- `src/store.ts` — live alert state (Zustand).
- `src/components/` — Dashboard, AlertList, MetricChart, NodeDetail.
