// SPDX-License-Identifier: AGPL-3.0-only
// The handful of responses the generator must not decide for itself, typed against the generated
// contract (ADR-052 決定 2). `satisfies` is the point: if a Rust handler changes one of these
// shapes, `tsc -p tsconfig.e2e.json` fails here instead of the walk quietly testing a fiction.

import type { components } from '../../src/api/schema';
import { defaultBodyFor, enumOf, MOCK_PREFIX, type Json } from './openapi';
import type { Override } from './mockApi';

type Schemas = components['schemas'];

/**
 * An ordinary SNMP-polled device, for the specs that open a tab only such a node has.
 *
 * One copy: this was five byte-identical local helpers, and ADR-119 gave every one of them the
 * same second line to grow. `snmp_configured` is the reason it cannot stay generated — the
 * generator answers a boolean with `false`, which is a **ping-only** device, and a ping-only
 * device has no Interfaces tab to open. Seventeen tests failed on exactly that.
 *
 * Takes the id rather than owning one: each spec already declares its own `NODE_ID` for the URLs
 * it visits, and a fixture that hardcoded a different one would answer for a node the spec never
 * asks about.
 */
export function deviceNode(nodeId: string): Json {
  const body = defaultBodyFor(`/api/v1/nodes/${nodeId}`) as {
    kind: string;
    snmp_configured: boolean;
  };
  body.kind = 'device';
  body.snmp_configured = true;
  return body as unknown as Json;
}

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

  // 🚨 The generator answers a boolean with `false`, and since ADR-119 one of this body's booleans
  // decides how many tabs the node-detail screen has: `snmp_configured: false` is a ping-only
  // device, which is offered four tabs instead of six. Left generated, the route walk would visit
  // the *narrowest* node page in the app and never render Interfaces or Neighbors — passing, while
  // silently no longer covering the screens it was written for.
  //
  // So the walk's node is SNMP-polled. `nodeTabs.spec.ts` overrides this back to `false` for the
  // ping-only half of its matrix, which is where that case belongs: there it is the subject, here
  // it would be an accident. Patch the generated body rather than hand-writing one.
  '/api/v1/nodes/{node_id}': (() => {
    const body = defaultBodyFor('/api/v1/nodes/{node_id}') as { snmp_configured: boolean };
    body.snmp_configured = true;
    return body as unknown as Json;
  })(),

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

  // 🚨 The generated body has **no poller row at all** — `ComponentKind`'s enum is ordered
  // `["core","poller"]`, so the generator's "take the first member" rule builds a components list
  // that is one core and nothing else. Every per-poller thing Settings ▸ Upgrade draws (the pool,
  // the site's own progress, and since ADR-051 Inc.7 the warning that a site has not said an
  // upgrade is safe there) is therefore unreachable, on a screen the walk otherwise passes.
  //
  // So one remote site is appended to the generated list rather than replacing it, and it carries
  // the Inc.7 warning, so the per-poller cells are rendered and measured at all rather than the
  // screen passing over a components list with no site in it. The generated core row is kept — the
  // route walk wants a visible `ymock-` string here. `sitePrep.spec.ts` is what reads the warning.
  '/api/v1/system/upgrade': (() => {
    const body = defaultBodyFor('/api/v1/system/upgrade') as unknown as Schemas['UpgradeStatusResponse'];
    body.components = [
      ...body.components,
      {
        id: 'edge-tokyo-1',
        kind: 'poller',
        pool: 'tokyo',
        version: '0.3.3',
        upgradable: true,
        reason: null,
        co_located: false,
        moves_back: false,
        live_in_pool: 2,
        needs_site_prep: true,
        progress: null,
      },
    ];
    return body as unknown as Json;
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

  // The same trap as `promoted_node_id`, on the other side: the generator fills every nullable
  // uuid, so the one group it invents claims a **parent that does not exist**. Every consumer
  // builds the folder forest by walking down from `parent_id === null` (`lib/nodeTree.ts`
  // `groupOptions`, the inventory tree), so the single group is unreachable and every folder-group
  // picker in the app renders with nothing but its placeholder — a `<select>` that is present,
  // correct, and empty. Patch the generated body rather than hand-writing one.
  // The metric picker (ADR-075 増分 4) joins this catalog against the translated meanings, and the
  // generator gives it one row with an invented name — enough to prove a list renders, and unable
  // to prove any of the three things that actually matter. So three rows, one per behaviour:
  //
  //  - `if_oper_status` — a gauge the app has a sentence for. Proves the join.
  //  - `if_hc_in_octets` — a counter. Must NOT be offered: `POST /thresholds` refuses one, and
  //    dropping it is the reason this control removes an error rather than just looking nicer.
  //  - `ymock_widget_temp` — a name nothing explains, standing in for an operator's own metric.
  //    Proves the fallback renders its OID instead of a blank line.
  // The metric picker (ADR-075 増分 4) joins this catalog against the translated meanings, and the
  // generator gives it one row with an invented name — enough to prove a list renders, and unable
  // to prove any of the three things that actually matter. So the generated row is **kept** (the
  // route walk demands a visible `ymock-` string on Settings ▸ MIB repository, and a hand-written
  // body would have removed it) and three are added, one per behaviour:
  //
  //  - `if_oper_status` — a gauge the app has a sentence for. Proves the join.
  //  - `if_hc_in_octets` — a counter. Must NOT be offered: `POST /thresholds` refuses one, and
  //    dropping it is the reason this control removes an error rather than just looking nicer.
  //  - `ymock_widget_temp` — a name nothing explains, standing in for an operator's own metric.
  //    Proves the fallback renders its OID instead of a blank line.
  //
  // ⚠️ The names are real ones on purpose. A metric name is `[a-zA-Z_:][a-zA-Z0-9_:]*`, so the
  // generator's `ymock-…` spelling is not one a device could ever carry, and the counter case in
  // particular has to be a name the product genuinely treats as a counter.
  '/api/v1/mib-catalog': [
    ...(defaultBodyFor('/api/v1/mib-catalog') as Json[]),
    {
      id: '00000000-0000-4000-8000-0000000000a1',
      metric_name: 'if_oper_status',
      oid: '1.3.6.1.2.1.2.2.1.8',
      collection: 'table',
      metric_kind: 'gauge',
      vendor: null,
      description: null,
    },
    {
      id: '00000000-0000-4000-8000-0000000000a2',
      metric_name: 'if_hc_in_octets',
      oid: '1.3.6.1.2.1.31.1.1.1.6',
      collection: 'table',
      metric_kind: 'counter',
      vendor: null,
      description: null,
    },
    {
      id: '00000000-0000-4000-8000-0000000000a3',
      metric_name: 'ymock_widget_temp',
      oid: '1.3.6.1.4.1.99999.1.1',
      collection: 'scalar',
      metric_kind: 'gauge',
      vendor: 'Ymock device health',
      description: null,
    },
  ],

  '/api/v1/node-groups': (() => {
    const body = defaultBodyFor('/api/v1/node-groups') as { parent_id: string | null }[];
    for (const g of body) g.parent_id = null;
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

  // The generator fills every `date-time` field, including the nullable ones — so every token it
  // hands the walk is already **revoked**, and `ApiTokensPage` renders no action control on a
  // revoked row. The screen was therefore green while showing a list nobody can act on, which is
  // the same shape as the `/auth/me` and `/roles` entries above: schema-valid, and not the state
  // the walk meant to be looking at. Found by ADR-088's row-menu check reporting "no ⋮ trigger".
  '/api/v1/api-tokens': (url) => {
    const body = defaultBodyFor(url.pathname) as { revoked_at: string | null }[];
    for (const row of body) row.revoked_at = null;
    return body as unknown as Json;
  },
};
