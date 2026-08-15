// SPDX-License-Identifier: AGPL-3.0-only
// Default API responses, generated from the committed OpenAPI document (ADR-052 決定 2 / Inc.1).
//
// WHY GENERATED. The API is 196 paths / 256 operations, 122 of them GET. Hand-writing a fixture per
// screen does not scale, and a hand-written fixture is exactly the artefact that drifts from the
// Rust handlers — the defect ADR-035 deleted everywhere else. The document is already generated
// from `#[utoipa::path]`, so building instances from it means the mock cannot describe a shape the
// server does not serve. The same move was already made for the MCP canary, which went from ~20
// hand-built instances to zero and proved, while doing it, that every path's 200 resolves to a
// schema.
//
// WHY THE MARKER MATTERS. Every generated string is `ymock-<property name>`, and every array gets
// exactly one element. That makes "did the data reach the screen?" answerable without writing an
// expectation per screen: if a `ymock-` string is visible, the page rendered what it was given.
// An empty array would have made a blank table indistinguishable from a broken one.
//
// ⚠️ This produces *schema-valid* data, not *domain-valid* data. A field typed `string` that the UI
// parses as an IP address gets `ymock-address`. When a screen breaks on that, the fix is a typed
// override in the caller (see `bootstrap.ts`), not a special case here — the generator must stay
// something you can read in one sitting.

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Prefix on every generated string. Also the walk's "the mock reached the screen" assertion. */
export const MOCK_PREFIX = 'ymock-';

/** Fixed instant for `date-time` fields. A constant, not `now`, so a failure reproduces. */
const FIXED_TIME = '2026-08-01T00:00:00Z';

/** Depth ceiling for instance building — a backstop, not the cycle guard. True recursion needs a
 *  `$ref`, and the per-branch ref stack below already stops that, so this only exists so a
 *  pathological document cannot hang the run.
 *
 *  🚨 It was 12, and that was **too shallow for real schemas**: a DNS resolution chain is
 *  `DnsChainCurrent → chain → hops[] → answers[] → record`, which runs out at the last hop. The
 *  failure was silent and looked legitimate — the object was built, but its **required** `kind`
 *  and `target` came back `null`, and the screen blanked on `switch (record.kind)`. Truncation
 *  that produces schema-INVALID data is worse than truncation that stops early, so the cap is now
 *  far out of reach and `auditRequiredNulls()` fails the suite if anything null lands in a
 *  required field again. */
const MAX_DEPTH = 40;

/** Nullable properties that must be generated as `null`, because a *correct* client keeps asking
 *  while they are set.
 *
 * 🚨 The one case where a schema-valid value makes a well-written client loop. `next_cursor` means
 * "there is another page"; answering it with a string told the network map to page forever, and it
 * stopped only at its own 200-page safety cap — **405 requests for one page load**, and a map
 * claiming 200 nodes when the mock had served one. Nothing failed: the screen rendered, and the
 * walk would have passed had the cap not also emptied the marker.
 *
 * The document uses TWO spellings, which is exactly why the guard in `screens.spec.ts` exists: it
 * was written expecting only `next_cursor` and failed on its first run, naming `next` — the
 * keyset cursor object on `DiscoveredEndpointPage`, `DnsChainHistory` and `NeighborHistory`. A
 * hand-maintained list of one would have covered the pages nobody happened to walk that day. */
const TERMINAL_NULL_PROPS = new Set(['next_cursor', 'next']);

/** Does this schema admit `null`? (3.1 spells it `type: [T, "null"]` or a `oneOf` with a null arm.) */
function isNullable(schema: Schema): boolean {
  if (Array.isArray(schema.type) && schema.type.includes('null')) return true;
  return (schema.oneOf ?? schema.anyOf ?? []).some((b) => b.type === 'null');
}

export type Json = string | number | boolean | null | Json[] | { [k: string]: Json };

interface Schema {
  $ref?: string;
  type?: string | string[];
  format?: string;
  enum?: Json[];
  const?: Json;
  properties?: Record<string, Schema>;
  required?: string[];
  items?: Schema;
  prefixItems?: Schema[];
  additionalProperties?: Schema | boolean;
  oneOf?: Schema[];
  anyOf?: Schema[];
  allOf?: Schema[];
}

interface Operation {
  responses?: Record<string, { content?: Record<string, { schema?: Schema }> }>;
}

interface Doc {
  paths: Record<string, Record<string, Operation>>;
  components: { schemas: Record<string, Schema> };
}

const DOC: Doc = JSON.parse(
  readFileSync(
    resolve(dirname(fileURLToPath(import.meta.url)), '../../src/api/openapi.json'),
    'utf8',
  ),
) as Doc;

// ── instance building ────────────────────────────────────────────────────────────────────────

/** Deterministic UUID from a field path, so the same field always gets the same id across runs
 *  (a random one would make a screenshot diff and a trace unreadable). v4-shaped but not random. */
function stableUuid(seed: string): string {
  // FNV-1a, twice with different offsets, to fill 12 hex digits.
  const fnv = (s: string, h: number): number => {
    for (let i = 0; i < s.length; i++) {
      h ^= s.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    return h >>> 0;
  };
  const a = fnv(seed, 0x811c9dc5).toString(16).padStart(8, '0');
  const b = fnv(seed, 0x1000193).toString(16).padStart(8, '0');
  return `00000000-0000-4000-8000-${(a + b).slice(0, 12)}`;
}

/** The last path segment — the property name a string value is standing in for. */
function leaf(path: string): string {
  const i = path.lastIndexOf('.');
  return i === -1 ? path || 'value' : path.slice(i + 1);
}

/** The first concrete type when 3.1 spells nullability as `type: ["string", "null"]`. */
function primaryType(t: string | string[] | undefined): string | undefined {
  if (typeof t === 'string') return t;
  if (Array.isArray(t)) return t.find((x) => x !== 'null') ?? 'null';
  return undefined;
}

function build(schema: Schema | undefined, path: string, depth: number, refs: string[]): Json {
  if (!schema || depth > MAX_DEPTH) return null;

  if (schema.$ref) {
    const name = schema.$ref.split('/').pop() ?? '';
    // A repeat on the SAME branch is a cycle; the same type appearing in two sibling fields is not.
    if (refs.includes(name)) return null;
    return build(DOC.components.schemas[name], path, depth + 1, [...refs, name]);
  }
  if (schema.const !== undefined) return schema.const;
  if (schema.enum?.length) return schema.enum[0];

  if (schema.allOf?.length) {
    const merged: Record<string, Json> = {};
    for (const part of schema.allOf) {
      const v = build(part, path, depth + 1, refs);
      if (v && typeof v === 'object' && !Array.isArray(v)) Object.assign(merged, v);
    }
    return merged;
  }
  // A union has no "right" arm without knowing the caller's intent, so take the first — but skip a
  // bare `null` arm. utoipa spells `Option<T>` as `oneOf: [{type: null}, {$ref: T}]`, so "first
  // arm" would have meant "absent" for every optional nested object: the TLS page showed
  // "No certificate yet", and every screen whose subject is an optional struct rendered its empty
  // state and looked fine. Preferring the arm that carries data is what makes the walk about data.
  const arms = schema.oneOf ?? schema.anyOf;
  const branch = arms?.find((b) => b.type !== 'null') ?? arms?.[0];
  if (branch) return build(branch, path, depth + 1, refs);

  const type = primaryType(schema.type) ?? (schema.properties ? 'object' : undefined);
  switch (type) {
    case 'object': {
      const out: Record<string, Json> = {};
      // Every property, not just the required ones: the UI reads optional fields, and a screen
      // that renders nothing because an optional field was absent is a false negative here.
      for (const [key, sub] of Object.entries(schema.properties ?? {})) {
        out[key] =
          TERMINAL_NULL_PROPS.has(key) && isNullable(sub)
            ? null
            : build(sub, path ? `${path}.${key}` : key, depth + 1, refs);
      }
      if (
        Object.keys(out).length === 0 &&
        schema.additionalProperties &&
        typeof schema.additionalProperties === 'object'
      ) {
        out[`${MOCK_PREFIX}key`] = build(
          schema.additionalProperties,
          `${path}.value`,
          depth + 1,
          refs,
        );
      }
      return out;
    }
    case 'array':
      if (schema.prefixItems?.length) {
        return schema.prefixItems.map((s, i) => build(s, `${path}.${i}`, depth + 1, refs));
      }
      return schema.items ? [build(schema.items, path, depth + 1, refs)] : [];
    case 'string':
      if (schema.format === 'uuid') return stableUuid(path);
      if (schema.format === 'date-time') return FIXED_TIME;
      return `${MOCK_PREFIX}${leaf(path)}`;
    // 1, not 0: a zero denominator, a zero-length window and an empty series all send the UI down
    // a different branch than the one a walk is trying to exercise.
    case 'integer':
    case 'number':
      return 1;
    case 'boolean':
      return false;
    default:
      return null;
  }
}

/** A schema-shaped instance of a named component. Exported for tests that need one directly. */
export function instanceOf(schemaName: string): Json {
  return build({ $ref: `#/components/schemas/${schemaName}` }, schemaName, 0, []);
}

// ── path matching ────────────────────────────────────────────────────────────────────────────

export interface RouteEntry {
  template: string;
  method: string;
  regex: RegExp;
  params: number;
}

const ROUTES: RouteEntry[] = Object.entries(DOC.paths)
  .flatMap(([template, ops]) =>
    Object.keys(ops).map((method) => {
      const params = (template.match(/\{[^}]+\}/g) ?? []).length;
      const pattern = template
        .replace(/[.*+?^$()|[\]\\]/g, '\\$&')
        .replace(/\{[^}]+\}/g, '[^/]+');
      return { template, method, params, regex: new RegExp(`^${pattern}$`) };
    }),
  )
  // Literal paths must be tried before templated ones, or `/nodes/search` is swallowed by
  // `/nodes/{id}` and answers with the wrong schema. Fewest parameters first, longest first.
  .sort((a, b) => a.params - b.params || b.template.length - a.template.length);

export function matchRoute(pathname: string, method: string): RouteEntry | undefined {
  const m = method.toLowerCase();
  return ROUTES.find((r) => r.method === m && r.regex.test(pathname));
}

/** Can an override keyed by this string ever be reached?
 *
 * 🚨 An override key is a path spelling repeated outside the contract, and a wrong one is a
 * **silent no-op**: the generated default is served, the screen renders something plausible, and
 * the test asserts against data it did not choose. That happened immediately — the node-detail
 * template is `/api/v1/nodes/{node_id}`, not `{id}`, and a whole spec file was quietly testing the
 * default. `installMockApi` refuses unknown keys rather than letting that happen twice. */
export function isReachableOverrideKey(key: string): boolean {
  return ROUTES.some((r) => r.template === key) || ROUTES.some((r) => r.regex.test(key));
}

/** Templates whose literal path segments match a hint — for the "did you mean…" on a bad key. */
export function templatesLike(key: string): string[] {
  const stem = key.split('/').filter((s) => s && !s.startsWith('{'))[2] ?? '';
  return stem ? ROUTES.filter((r) => r.template.includes(`/${stem}`)).map((r) => r.template) : [];
}

// ── responses ────────────────────────────────────────────────────────────────────────────────

export interface MockResponse {
  status: number;
  /** JSON body, or undefined for a body-less success. */
  body?: Json;
  /** Set when the operation answers with something other than JSON (csv, pdf, gzip, text). */
  contentType?: string;
}

/** The values a request put in a template's `{…}` slots. */
function pathParams(template: string, pathname: string): string[] {
  if (!template.includes('{')) return [];
  const rx = new RegExp(
    `^${template.replace(/[.*+?^$()|[\]\\]/g, '\\$&').replace(/\{[^}]+\}/g, '([^/]+)')}$`,
  );
  return rx.exec(pathname)?.slice(1) ?? [];
}

/** A resource fetched by id answers with **that** id.
 *
 * 🚨 The schema cannot say this and the generator cannot guess it, but clients depend on it. The
 * Troubleshoot report resolves its `?job=` with an effect guarded on `fetched?.id === jobId` and
 * listing `fetched` among its dependencies: an answer carrying a different id never satisfies the
 * guard, so each response re-triggers the effect. The mock's stable-but-unrelated uuid turned that
 * into an unthrottled request loop — **several hundred fetches in three seconds**, with the screen
 * showing its idle note the whole time. A real server cannot produce it, so this is a mock defect,
 * not a product one; it is worth knowing that the client has no retry ceiling if one ever did. */
function alignIdWithPath(body: Json, template: string, pathname: string): Json {
  const params = pathParams(template, pathname);
  if (params.length !== 1) return body;
  if (!body || typeof body !== 'object' || Array.isArray(body)) return body;
  const record = body as Record<string, Json>;
  if (typeof record.id === 'string') record.id = params[0];
  return record;
}

/** The default answer for an operation, built from its first 2xx response. */
export function defaultResponse(entry: RouteEntry, pathname: string): MockResponse {
  const op = DOC.paths[entry.template][entry.method];
  const codes = Object.keys(op.responses ?? {}).filter((c) => c.startsWith('2'));
  const code = codes.includes('200') ? '200' : (codes[0] ?? '200');
  const res = op.responses?.[code];
  const status = Number(code);

  const json = res?.content?.['application/json'];
  if (json) {
    return { status, body: alignIdWithPath(build(json.schema, '', 0, []), entry.template, pathname) };
  }

  const other = Object.keys(res?.content ?? {})[0];
  // A download or a plain-text probe: answer with the right content type and an empty payload.
  // Nothing in a route walk clicks one, so an empty body is honest rather than convenient.
  if (other) return { status, contentType: other, body: undefined };
  return { status, body: undefined };
}

/** The generated body for one path, for an override that only needs to *patch* the default.
 *  Patching keeps the override tied to the contract: a field that disappears from the schema
 *  disappears here too, instead of being re-asserted by a hand-written fixture. */
export function defaultBodyFor(pathname: string, method = 'get'): Json {
  const entry = matchRoute(pathname, method);
  if (!entry) throw new Error(`no OpenAPI path matches ${method.toUpperCase()} ${pathname}`);
  return defaultResponse(entry, pathname).body ?? null;
}

/** Every operation the document declares — used by the coverage check in `screens.spec.ts`. */
export const OPERATION_COUNT = ROUTES.length;

/** Resolve a `$ref`/`allOf`/`oneOf` wrapper down to the schema that describes an object's shape. */
function effective(schema: Schema | undefined, seen: string[] = []): Schema | undefined {
  if (!schema) return undefined;
  if (schema.$ref) {
    const name = schema.$ref.split('/').pop() ?? '';
    if (seen.includes(name)) return undefined;
    return effective(DOC.components.schemas[name], [...seen, name]);
  }
  if (schema.allOf?.length) {
    const merged: Schema = { type: 'object', properties: {}, required: [] };
    for (const part of schema.allOf) {
      const e = effective(part, seen);
      Object.assign(merged.properties!, e?.properties ?? {});
      merged.required!.push(...(e?.required ?? []));
    }
    return merged;
  }
  const arms = schema.oneOf ?? schema.anyOf;
  const branch = arms?.find((b) => b.type !== 'null');
  return branch ? effective(branch, seen) : schema;
}

/** A schema that says nothing at all — utoipa emits `{}` for a `serde_json::Value` field. The
 *  contract declines to describe it, so no instance can be wrong and no test can assert on it. */
function describesNothing(schema: Schema | undefined): boolean {
  if (!schema) return true;
  return (
    Object.keys(schema).filter((k) => k !== 'description' && k !== 'title' && k !== 'deprecated')
      .length === 0
  );
}

export interface FixtureAudit {
  /** Required properties the generator filled with `null` — always a generator bug. */
  invalid: string[];
  /** Required properties whose schema is `{}`. Not a bug: the **contract** says nothing about
   *  them, so `null` is as valid as anything else. Worth counting rather than hiding — each one
   *  is a field the generated TypeScript types as `unknown` too, i.e. an ADR-035 blind spot. */
  undescribed: string[];
}

/** Are the generated fixtures valid against the contract that generated them?
 *
 * 🚨 The whole promise of generating from the document is validity by construction. A `null` in a
 * required field breaks it quietly — the request succeeds, the shape looks right, and the screen
 * blanks on the first property access. That is precisely what a depth-capped DNS chain did:
 * `record.kind` came back null and every DNS monitor's detail page rendered white. */
export function auditFixtures(): FixtureAudit {
  const invalid: string[] = [];
  const undescribed: string[] = [];

  const walk = (schema: Schema | undefined, value: Json, where: string, depth: number): void => {
    if (depth > 12) return;
    const eff = effective(schema);
    if (!eff) return;
    if (Array.isArray(value)) {
      const items = eff.prefixItems ? undefined : eff.items;
      value.forEach((v, i) => walk(items, v, `${where}[${i}]`, depth + 1));
      return;
    }
    if (!value || typeof value !== 'object') return;
    for (const key of eff.required ?? []) {
      const sub = eff.properties?.[key];
      if (value[key] !== null || isNullable(sub ?? {})) continue;
      (describesNothing(sub) ? undescribed : invalid).push(`${where}.${key}`);
    }
    for (const [key, v] of Object.entries(value)) {
      walk(eff.properties?.[key], v, `${where}.${key}`, depth + 1);
    }
  };

  for (const entry of ROUTES) {
    const op = DOC.paths[entry.template][entry.method];
    const schema = Object.entries(op.responses ?? {}).find(([c]) => c.startsWith('2'))?.[1]
      ?.content?.['application/json']?.schema;
    if (!schema) continue;
    walk(schema, build(schema, '', 0, []), `${entry.method.toUpperCase()} ${entry.template}`, 0);
  }
  return {
    invalid: [...new Set(invalid)].sort(),
    undescribed: [...new Set(undescribed)].sort(),
  };
}

/** Nullable string properties whose name reads as "there is more" — the shape that makes a paging
 *  client loop. `screens.spec.ts` asserts this stays exactly what TERMINAL_NULL_PROPS handles. */
export function continuationProps(): string[] {
  const found = new Set<string>();
  for (const schema of Object.values(DOC.components.schemas)) {
    for (const [prop, sub] of Object.entries(schema.properties ?? {})) {
      if (!isNullable(sub)) continue;
      if (/cursor|next|page_token|more/i.test(prop)) found.add(prop);
    }
  }
  return [...found].sort();
}

/** The members of a named string enum, in document order. Lets a fixture be built from the
 *  contract instead of transcribing a backend list — see the `/api/v1/roles` override. */
export function enumOf(schemaName: string): string[] {
  const schema = DOC.components.schemas[schemaName];
  return (schema?.enum ?? []).map(String);
}
