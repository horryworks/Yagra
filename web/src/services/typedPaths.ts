// SPDX-License-Identifier: AGPL-3.0-only
// The type machinery that binds this client to the generated OpenAPI contract (ADR-035).
//
// `api.ts` used to take a hand-built path string (`/nodes/${id}/status`) and a hand-written return
// type, both transcribed from Rust. Neither was checked by anything: a renamed endpoint kept
// compiling and 404'd at runtime, and a renamed field kept compiling and read `undefined`.
//
// These types make the *template* path — `/api/v1/nodes/{node_id}/status`, exactly as it appears in
// `api/schema.d.ts` — the argument, and derive everything else from it: which methods that path
// serves, the names and types of its path and query parameters, the request body, and the success
// response. All of it comes from the Rust handlers by way of `web/src/api/openapi.json`.
//
// The URL is still assembled here rather than by a client library, because the assembly is what
// `api.test.ts` pins: it asserts the exact `fetch(url, init)` calls, including `%2F` for a slash in
// a path segment and `+` for a space in a query value. A library that called `fetch(new Request(…))`
// would be equivalent on the wire and would invalidate every one of those assertions.

import type { paths } from '../api/schema';

/** The HTTP methods this client issues. */
export type Method = 'get' | 'put' | 'post' | 'delete';

/** Paths the contract serves with method `M`. Anything else is a compile error at the call site. */
export type PathsWith<M extends Method> = {
  [P in keyof paths]: paths[P] extends { [K in M]: { responses: unknown } } ? P : never;
}[keyof paths];

/** The operation object for `M` on `P`. */
export type Op<P extends keyof paths, M extends Method> = paths[P] extends { [K in M]: infer O }
  ? O
  : never;

type JsonOf<T> = T extends { content: { 'application/json': infer B } } ? B : never;

/** The success body: whichever of 200/201/202 the operation declares, or `void` for a bare 204.
 *  A response documented with a non-JSON content type (an SSE stream, a report export) resolves to
 *  `never`, which is correct — those are not read through this client. */
export type Ok<O> = O extends { responses: infer R }
  ? 200 extends keyof R
    ? JsonOf<R[200]>
    : 201 extends keyof R
      ? JsonOf<R[201]>
      : 202 extends keyof R
        ? JsonOf<R[202]>
        : void
  : void;

/** Path parameters, or `never` when the path has none (the generator writes `path?: never`). */
export type PathParams<O> = O extends { parameters: { path?: infer P } }
  ? Exclude<P, undefined>
  : never;

/** Query parameters, or `never` when the operation declares none. */
export type QueryParams<O> = O extends { parameters: { query?: infer Q } }
  ? Exclude<Q, undefined>
  : never;

/** The JSON request body, or `never` for operations that take none. */
export type ReqBody<O> = O extends { requestBody?: infer B } ? JsonOf<Exclude<B, undefined>> : never;

/** The options accepted for an operation: `path` and `body` are required when the contract declares
 *  them, `query` is always optional (every query parameter in this API is). */
export type Opts<O> = ([PathParams<O>] extends [never] ? object : { path: PathParams<O> }) &
  ([QueryParams<O>] extends [never] ? object : { query?: QueryParams<O> }) &
  ([ReqBody<O>] extends [never] ? object : { body: ReqBody<O> });

/** Whether an operation *must* be given options — i.e. it has a path parameter or a request body.
 *  Query-only operations stay callable as `get('/api/v1/nodes')`. */
type NeedsOpts<O> = [PathParams<O>] extends [never]
  ? [ReqBody<O>] extends [never]
    ? false
    : true
  : true;

/** The trailing argument list for an operation: required when {@link NeedsOpts}, optional otherwise.
 *  A rest tuple rather than a `?` parameter, because the requirement depends on the path. */
export type OptsArg<O> = NeedsOpts<O> extends true ? [opts: Opts<O>] : [opts?: Opts<O>];

/** Substitute `{name}` path parameters and append a query string.
 *
 *  `encodeURIComponent` for path segments and `URLSearchParams` for the query, matching what the
 *  hand-written client did — a slash inside an id must become `%2F` rather than a new path segment,
 *  while a space in a search term is `+`. `null`/`undefined` query values are dropped so an absent
 *  filter produces no parameter at all; `false` and `''` are **kept**, because deciding whether an
 *  empty filter means "unset" belongs to the endpoint's own wrapper, not here. */
export function buildUrl(
  template: string,
  opts?: { path?: Record<string, unknown>; query?: Record<string, unknown> },
): string {
  let url: string = template;
  if (opts?.path) {
    for (const [name, value] of Object.entries(opts.path)) {
      url = url.replace(`{${name}}`, encodeURIComponent(String(value)));
    }
  }
  if (opts?.query) {
    const params = new URLSearchParams();
    for (const [name, value] of Object.entries(opts.query)) {
      if (value !== undefined && value !== null) params.set(name, String(value));
    }
    const qs = params.toString();
    if (qs) url = `${url}?${qs}`;
  }
  return url;
}
