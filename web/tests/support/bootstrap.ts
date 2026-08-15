// SPDX-License-Identifier: AGPL-3.0-only
// The handful of responses the generator must not decide for itself, typed against the generated
// contract (ADR-052 決定 2). `satisfies` is the point: if a Rust handler changes one of these
// shapes, `tsc -p tsconfig.e2e.json` fails here instead of the walk quietly testing a fiction.

import type { components } from '../../src/api/schema';
import { defaultBodyFor, enumOf, MOCK_PREFIX, type Json } from './openapi';
import type { Override } from './mockApi';

type Schemas = components['schemas'];

/** The Troubleshoot tool whose report the walk opens. Exported so `screens.ts` builds the URL from
 *  the same constant the mocked job carries — the report shell redirects a job whose `tool` does
 *  not match the route, so these two disagreeing is a silent redirect, not an error. */
export const REPORT_TOOL = 'anomaly';

/** Bootstrap answers — the responses whose generated default is schema-valid but says something
 *  the walk did not mean. Each entry names the thing the generator could not know. */
export const BOOTSTRAP_OVERRIDES: Record<string, Override> = {
  // The generator answers a boolean with `false`, which would leave `public_dashboard: false` and
  // `auth_available: false` — a deployment that gates reads but offers no way in. It is also where
  // the two optional-subsystem switches live: with `rca_enabled`/`flow_enabled` false, the AI and
  // flow screens render their "not configured" notice and the walk would call that a pass.
  '/api/v1/config': {
    public_dashboard: false,
    auth_available: true,
    sso_enabled: false,
    rca_enabled: true,
    flow_enabled: true,
    default_poll_interval_secs: 30,
  } satisfies Schemas['ClientConfig'] as unknown as Json,

  // 🚨 `Role`'s enum is ordered least → most privileged (`["viewer","operator","admin"]`), so the
  // generator's "take the first member" rule hands the walk a **viewer** — and roughly fifteen
  // Settings screens then render a permission notice instead of their content. Every one of them
  // would still be green. This single line is the difference between walking the app and walking
  // its refusals.
  '/api/v1/auth/me': {
    username: `${'ymock-'}e2e`,
    role: 'admin',
    scope: 'All',
  } satisfies Schemas['AuthMe'] as unknown as Json,

  // 🚨 The same trap as `/auth/me`, one step further along. Since ADR-056 Inc.2 every write
  // control asks `useCan`, which reads this matrix and looks the caller's role up in it — and the
  // generator emits **one** `RoleInfo`, keyed `viewer` (the first enum member) granting `["view"]`.
  // The walk signs in as `admin`, finds no row for it, and every `+ Add`, edit and delete in the
  // app disappears — with every screen still green, because they all still render.
  //
  // Built from the document's own enums rather than transcribed: this fixture is not the RBAC
  // model (that lives in `rbac.rs` and is tested there), it is "the walk's caller may do
  // everything", which is what patching `/auth/me` to `admin` already said.
  '/api/v1/roles': (() => {
    const perms = enumOf('Permission');
    return {
      permissions: perms.map((key) => ({
        key,
        label: `${MOCK_PREFIX}${key}`,
        description: `${MOCK_PREFIX}description`,
      })),
      roles: enumOf('Role').map((key) => ({
        key,
        label: `${MOCK_PREFIX}${key}`,
        description: `${MOCK_PREFIX}description`,
        builtin: true,
        permissions: perms,
      })),
    } as unknown as Json;
  })(),

  // Schema-valid, domain-invalid: `promoted_node_id` is "the node this address became once it is
  // monitored", and the generator fills every nullable uuid. So every discovered endpoint arrived
  // already-monitored and the table — which filters to unmonitored by default, and always has —
  // rendered "0 endpoints" while the summary above it counted them. Patching the generated body
  // rather than hand-writing one keeps the fixture tied to the schema.
  '/api/v1/discovered-endpoints': (() => {
    const body = defaultBodyFor('/api/v1/discovered-endpoints') as {
      endpoints: { promoted_node_id: string | null }[];
    };
    for (const e of body.endpoints) e.promoted_node_id = null;
    return body as unknown as Json;
  })(),

  // Two things at once, both invisible to the schema:
  //
  //  - **Lifecycle enums are declared in lifecycle order**, so "the first member" is always the
  //    state a thing is in before it has done anything — `queued`. A Troubleshoot report loads its
  //    findings only once the run is `done`, so every report screen rendered its idle note. Any
  //    screen keyed off a terminal state has this trap; the enum's own ordering is the tell.
  //  - **`tool` is a bare string on the wire**, and the report shell redirects a job belonging to
  //    another tool to the report that can read it — falling back to the catalog for a tool this
  //    build has no report for. `ymock-tool` is exactly that, so the screen bounced. That is the
  //    app being right (`fix(web)`'s self-correcting links), and the mock being wrong.
  '/api/v1/analysis/jobs/{id}': (url) => {
    const body = defaultBodyFor(url.pathname) as {
      state: Schemas['AnalysisJobState'];
      tool: string;
    };
    body.state = 'done';
    body.tool = REPORT_TOOL;
    return body as unknown as Json;
  },
};
