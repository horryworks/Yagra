<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Browser tests (ADR-052)

Two tiers, and they answer different questions. Tier1 owns everything reachable by controlling the
data; Tier2 owns only what no mock can produce. Mixing them makes Tier2 a slow duplicate, and a slow
gate is a disabled gate.

| | What it runs against | Command | Status |
|---|---|---|---|
| **Tier1** | the built bundle in `dist/`, every API call intercepted — **no backend** | `npm run build && npm run test:ui` | shipped (Inc.1 + Inc.2) |
| **Tier2a** | a real deployment, read-only, enforced mechanically | `npm run test:e2e` | shipped (Inc.3) |
| **Tier2b** | the same, allowed to write and clean up after itself | — | not built yet (Inc.4) |

`npm run test:ui` **does not build**. It serves `dist/` and a guard aborts the run when `dist/` is
older than `src/`, because a stale bundle makes every passing test a statement about the previous
build. Both callers (`/verify` Step 18, `/flashdeploy`) build immediately beforehand.

`npm run test:e2e` builds nothing and serves nothing — it drives whatever the deployment is running.
It needs no `dist/` at all, which is why the stale-bundle guard steps aside for it.

`npm run test:e2e:upgrade` is the same suite behind a wait. It polls `GET /api/v1/version`
(unauthenticated, so the wait itself needs no credentials) until the deployment reports the version
in `package.json`, then runs. It exists because a release publishes images and **nothing is running
them** — since ADR-050 a deployment upgrades itself, when a person decides to. So the decision stays
with the person and only the waiting is automated: `/release` arms this in the background, and the
suite runs itself the moment the version flips. Override the target with `E2E_TARGET_VERSION`
(**not** an argument — `npm run … -- x` would append it to `playwright`, not to the waiter), the
cadence with `E2E_UPGRADE_POLL_MS`, and the 20-minute deadline with `E2E_UPGRADE_DEADLINE_MS`.
Exit 2 means nobody upgraded in time; that is "not verified", not "the release is broken".

## Layout

```
tests/support/openapi.ts      default responses, generated from src/api/openapi.json
tests/support/bootstrap.ts    the few responses whose generated default is schema-valid but wrong
tests/support/mockApi.ts      one route handler over /api/**; unmatched paths fail the test
tests/support/app.ts          the Tier1 fixture: seeded session, pinned locale, error capture
tests/support/globalSetup.ts  the stale-dist guard
tests/support/selection.ts    which projects the command line selected (both tiers need this)
tests/ui/screens.ts           which screens are walked, and what "rendered" means for each
tests/ui/walk.spec.ts         the walk
tests/ui/detects.spec.ts      proof the walk can fail
tests/e2e/support/live.ts     the Tier2 fixture, and the read-only guard
tests/e2e/walk.spec.ts        reachability on the deployment + the nginx delivery edge
tests/e2e/consistency.spec.ts one fact read two ways, on data nobody chose
tests/e2e/declarations.spec.ts tab rules and the permission matrix, asked of the running system
tests/e2e/auth.spec.ts        the real sign-in, and the gate
tests/e2e/harness.spec.ts     proof the read-only guard both permits and stops
```

## Tier2a is read-only, and that is mechanical

The account is Admin and could write anything. So it is not policy: every request the browser makes
is observed, and any non-GET fails the test. Two exemptions, both in `live.ts` with their reason —
`POST /api/v1/node-names` (a read whose id list will not fit a query string) and, for `auth.spec.ts`
alone, the one sign-in. `harness.spec.ts` proves the guard is connected by firing a deliberate
non-GET at a path the router does not serve.

If a screen you add makes Tier2a fail this way, that is the finding: either it writes on load, or
the test belongs in Tier2b.

## Tier2 credentials — `web/.env.e2e`

Not in the repository, and it cannot be: the root `.gitignore`'s `.env.*` rule already ignores it
(`git check-ignore -v web/.env.e2e` confirms), and the `!.env.example` exception does not reach it.
So the variable **names** are documented here and the **values** never are.

```dotenv
E2E_BASE_URL=https://<the deployment you are pointing this at>
E2E_USER=<the account created for it>
E2E_PASSWORD=<its password>
```

- One `KEY=value` per line, no `export`, no quotes, **no space around `=`**. A trailing space
  becomes part of the value — the usual way a correct password fails to log in.
- `#` at the start of a line is a comment.
- **`https://` is required.** There is deliberately no plain-HTTP listener and no redirect
  (ADR-044), so an `http://` base URL does not fail over — it simply gets nginx's 400.
- Add `:3000` only if that server's `~/yagra-deploy/.env` still pins `YAGRA_WEB_PORT=3000`.
- The account is a **write-capable Admin on the test server**, by decision (ADR-052 決定 6): a
  Viewer renders a permission notice on ~15 Settings screens, which is exactly the area that
  changes most. Its logins are audited, so Settings ▸ Audit still separates automation from people.
- **Absent file ⇒ there is no `e2e` project at all.** `playwright.config.ts` only declares it when
  `E2E_BASE_URL` is set, so `npm run test:e2e` on a machine with no credentials reports "no tests
  found" rather than a wall of connection failures. Nothing else in the repo changes.
- The certificate is self-signed (ADR-044). The browser side is the project's `ignoreHTTPSErrors`;
  the Node-side reads pass `rejectUnauthorized: false` per request, deliberately **not**
  `NODE_TLS_REJECT_UNAUTHORIZED=0`, so verification stays on everywhere else in the process.
