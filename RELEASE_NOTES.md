# Release Notes

<!-- ## Unreleased is where a shipped behaviour change waits for a version.
     Yagra deploys to the test server on every push to main, so changes go live long before a
     release is tagged. Add a bullet here in the SAME commit that ships the change — anything an
     operator or an API client could notice: response shapes, status codes, defaults, the meaning of
     a query, removed behaviour. At release time `/docs` folds this section into the new `## v<x>`
     heading and leaves an empty one behind. An empty Unreleased is the normal resting state; a
     missing one means someone deleted the mechanism. -->

## Unreleased

### Breaking changes
- **Core no longer starts when `YAGRA_KEK_FILE` points at a file it cannot read.** It used to log an
  error and boot on a freshly generated random key — which meant the deployment looked healthy while
  every stored monitoring credential had become permanently undecryptable, and nothing said so until
  the next poll failed. That silent data-loss path is now a loud startup failure naming the path
  (`load KEK from <path>`). **Unset stays unchanged**: no `YAGRA_KEK_FILE` still means the ephemeral
  dev key, so the dev compose is byte-identical. Only a deployment that was *already* broken is
  affected; point the variable at the real key file, or unset it.
- **`YAGRA_FLOW_RETENTION_DAYS` now seeds a brand-new deployment only.** On an existing one, flow
  retention comes from Settings ▸ System settings ▸ Data retention. The env var previously had no
  effect at all on an existing ClickHouse volume (see Bug Fixes), so nothing changes on upgrade —
  existing deployments keep the 30 days their tables are actually enforcing.

### New Features
- **CDP/LLDP neighbor discovery** (node detail ▸ Neighbors, ADR-038). Every SNMP node's LLDP and
  CDP tables are walked on a slow cadence, and the result is recorded as *what changed, and when* —
  the tab shows which local port faces which peer right now, plus a timeline of every time that
  moved. **A history row is written only when the adjacency actually changes**: the agent's own
  churn (LLDP's `TimeMark`, its remote-row renumbering, row ordering) is normalized away, so a rack
  nobody is repatching writes nothing at all. Both protocols are normalized onto one model, so a
  device running both shows one table and one capability legend.
  - **On by default.** After upgrading, each SNMP node gets one extra walk per hour. A device that
    speaks neither protocol answers in a single round trip and costs essentially nothing; a
    48-port switch returns a few dozen rows. Switch it off, or change the cadence, in
    **Settings ▸ System settings ▸ Neighbor discovery**. Turning it off keeps everything already
    recorded.
  - Read-only, like everything else Yagra does to a device — no configuration is written.
    Neighbor data **raises no alerts** and is deliberately **not** wired into dependency
    suppression: LLDP reports adjacency, which has no direction, and guessing the upstream wrong
    would silence a real outage rather than surface it (ADR-015).
  - New endpoints: **`GET /api/v1/nodes/{id}/neighbors`** and
    **`GET /api/v1/nodes/{id}/neighbors/history`** (View, node-scoped), plus
    **`GET`/`PUT /api/v1/settings/neighbors`** (View / ManageConfig).
- **Data retention is configurable from the UI** (Settings ▸ System settings ▸ Data retention,
  ADR-040). Alert-linked data, unmatched events, report runs and traffic flows each get their own
  window; changes apply on the next sweep with no restart, and a flow change is applied to
  ClickHouse immediately. The card lists **every** retained subject, including the ones Yagra cannot
  change: VictoriaMetrics and VictoriaLogs take retention as a container start flag with no runtime
  API, so those rows are read-only and show the value read back from the store's own `/flags`
  endpoint — what it is really enforcing, rather than a number mirrored from configuration. The
  audit log is listed as kept indefinitely, by design.
- **`GET /api/v1/settings/retention`** (View) and **`PUT`** (ManageConfig) expose the same policy.
- **`GET /api/v1/credentials/health`** (ManageCredentials) reports whether every stored credential
  still decrypts under the loaded key. This is the assertion a database restore cannot make on its
  own: rows can come back whole while the key-encryption key is a different one.
- **Configuration bundles** (Settings ▸ Configuration bundle, `GET`/`POST /api/v1/config/bundle`,
  ADR-040). Export a deployment's monitoring configuration — profiles, metric sets, classification
  rules, groups, nodes, thresholds, URL/DNS monitors, forwarding destinations, event sources and
  rules, report and analysis schedules — as one JSON file, and apply it to another deployment. This
  is for **migration**, not backup: a bundle carries no credentials, no notification-channel
  settings, no ingest tokens and no history. Import is **upsert only** — nothing is ever deleted and
  there is no replace mode — runs in one transaction, and `?dry_run=true` performs the real import
  and rolls it back, so its report is exactly what applying would do. The report names every row it
  skipped or changed and why: a missing required reference skips the row rather than widening it, a
  destination or webhook source that needs a secret arrives disabled, and schedules are recomputed
  on the target's clock. Notification channels and routing rules are deliberately **not** carried —
  a channel *is* its sealed config and no API can attach one to an existing channel id, so an
  imported rule would notify nobody, silently. The export refuses rather than truncating when a
  table exceeds 10,000 rows; use a database dump for a deployment that size.
- **A backup procedure that ships as scripts, and a way to prove it works.**
  `scripts/yagra-backup.sh` takes the tier-1 set (KEK first, then a full `pg_dump`, then a
  VictoriaMetrics snapshot) with a manifest; `scripts/yagra-restore-verify.sh` restores it into a
  throwaway stack and asserts `/readyz`, the node count, the audit-log row count, and that
  credentials actually decrypt. ADR-017 has required a backup and rollback path for destructive
  migrations since it was written, and until now the repository contained no `pg_dump` at all.

### Bug Fixes
- **`YAGRA_FLOW_RETENTION_DAYS` did nothing on an existing deployment.** The ClickHouse TTL was only
  ever emitted inside `CREATE TABLE IF NOT EXISTS`, which is a no-op once the tables exist, so the
  retention an existing volume ran with was whatever it was created with — while `DEPLOYMENT.md`
  documented the variable as live. Retention changes are now applied with `ALTER TABLE … MODIFY TTL`,
  and only when the declared TTL actually differs (issuing it unconditionally would re-mutate every
  part on each restart). **Lowering the window deletes flow rows older than it**, so the change is
  logged at `warn` with the old and new values.
- **The five envelope-encrypted stores each loaded their own key.** Credentials, notification
  channels, forwarding destinations, OIDC and LLM config called the key loader independently, so on
  the ephemeral (unset) path each got a *different* random key — despite the code's own comment
  saying they shared one. They now share a single loaded key.

## v0.1.20 — Group-scoped visibility, end to end

Group scoping went from a type nothing consulted to a working control: the read surface filters by
it, an admin can hand one out, and both API tokens and `/mcp` honour it. Alongside that, the
Troubleshoot tools gained a cross-run findings search and scheduled runs, and the Geo map became a
real page.

### Breaking changes
- **An API token now acts as an account, and one whose owner cannot be resolved stops working.**
  Tokens used to be free-standing identities: `api_tokens` carried its own role and had no link to
  `users` at all, so deleting, disabling or demoting the account that issued a token changed nothing
  about the token. Migration `0057` binds each token to an owner, matching on the `created_by`
  username. **A token whose issuing account no longer exists cannot be matched, and no longer
  authenticates** — it is listed as "No owner" so it can be revoked deliberately. Re-issue any such
  token, preferably owned by a service account (below).
- **The four Top-N endpoints now return an object, not a bare array.**
  `GET /api/v1/metrics/top`, `/metrics/interface-top`, `/metrics/interface-delta` and
  `/alerts/top-nodes` answer `{"entries": [...], "partial": false}`; what used to be the whole body
  is now `entries`. `/metrics/interface-heatmap` keeps its shape and gains the same `partial` field.
  `partial` is `true` when the ranking covers only the groups the calling account may see and rows
  it is entitled to may be missing — a Top-N is ranked by the metric store, which knows nothing
  about groups, so a scoped account's list is filtered afterwards and can come back short. It is
  always `false` for an account with unrestricted visibility, which is every account today. The
  WebUI is updated; an external client reading these endpoints must read `entries`.
- **`:latest` now means the latest stable release, not the development trunk.** Until now every
  push to `main` published `ghcr.io/horryworks/yagra-*:<sha>` and moved `:latest`, so the default
  in `docker-compose.deploy.yml` and `docker-compose.poller.yml` handed you a development build.
  Development builds are no longer published at all — the registry holds releases and nothing else.
  `:latest` moves when a release is tagged without a `-beta`/`-rc` suffix, and `:<sha>` exists only
  for commits that were released. If you were following `main` through `:latest` you are now
  following releases; pin an explicit tag if you wanted something else. Note `:latest` follows the
  most recently pushed stable tag rather than the highest version, so a hotfix cut after a larger
  release moves it backwards.
- **`docker-compose.deploy.yml` takes a new `YAGRA_IMAGE_REPO`,** defaulting to
  `ghcr.io/horryworks`. Leave it unset for published releases; it exists so a development machine
  can point at a private registry holding unreleased builds.

### New Features
- **Group-scoped API tokens and `/mcp` connections work.** The two refusals that stood in for
  enforcement are lifted, and the promise made in v0.1.19 — *"group scoping will be accepted again
  when the read paths actually filter by it"* — is kept. `POST /api/v1/api-tokens` accepts
  `{"scope": {"Groups": [...]}}` instead of answering `400 unsupported_scope`, and `/mcp` admits a
  group-scoped token instead of `403`-ing it. Every MCP tool now resolves the caller's scope and
  applies the same rule its REST counterpart does: node lists and event searches filter in the
  query, rankings and histories filter after, a tool naming an out-of-scope node answers exactly
  what an unknown id answers, and `run_analysis` cannot be launched over a scope the caller does not
  hold. Settings ▸ API tokens gained a scope picker and a **Can see** column.
  Two containment rules: a token can never exceed its owner, so a token owned by a **group-scoped
  account inherits that account's scope** (narrowing the account narrows its tokens at once, with
  nothing to re-issue) and giving such a token a different scope is refused with
  `400 owner_is_scoped`; and a token scope must name groups that exist. To give a token a narrower
  view than its owner, own it with a service account scoped to what the token should see.
- **Accounts can be limited to a set of node groups.** Settings ▸ Users ▸ *Change scope*
  (`PUT /api/v1/users/{id}/scope`) narrows what an account sees to the groups you pick and
  everything beneath them; `"All"` restores the whole fleet. Enforcement across the read surface
  shipped earlier in this release — this is the part that hands a scope out, so a scope is now
  something an account can actually hold rather than a value nothing could be set to. A node in no
  group stays visible only to unrestricted accounts, and a node outside the scope answers `404` (the
  same answer an unknown id gets, so the scope cannot be used to probe for what exists). Saving a
  scope signs the account out of its current sessions, the way a role change does — the scope is
  captured in the session token, so a live one would keep the old, wider view.
  Two rules worth knowing: **an Admin cannot be scoped** (`409 admin_is_unscoped`) because
  administration is fleet-wide, and promoting an account to Admin clears whatever scope it held; and
  a scope naming **no** groups is refused (`400 empty_scope`) rather than stored, since it would
  otherwise be an account that signs in successfully to an empty inventory with nothing to explain
  why. `GET /api/v1/users` and `GET /api/v1/auth/me` now carry `scope`, and the account menu says so
  out loud when the signed-in account is limited. SSO accounts are provisioned unrestricted and are
  narrowed here; the stored assignment survives every subsequent login (mapping IdP groups to a
  scope is a later increment).
- **Scheduled analyses.** Troubleshoot ▸ Scheduled runs an analysis on a preset cadence — daily,
  weekly or monthly at a time of day (UTC), over the whole fleet, a site or one node. Until now
  every analysis had to be launched by hand, so a nightly anomaly sweep meant someone remembering.
  `GET/POST /api/v1/analysis/schedules` and `PUT/DELETE /api/v1/analysis/schedules/{id}`, all
  Operator-and-up like launching a run.
  Two behaviours worth knowing: a fire the runner's admission control refuses is **deferred, not
  skipped** — the schedule stays due and the next minute's tick retries, rather than losing a whole
  period to a busy moment — and the traffic-flow analyses **cannot be scheduled on a deployment
  with no flow store**, because each fire would write an empty run forever. A schedule defaults to
  not notifying, unlike a run you launch and wait for.
- **Saved findings — search what the analyses found, across every run.** Troubleshoot ▸ Saved
  findings (`GET /api/v1/analysis/findings`) lists findings from every analysis, newest first,
  filterable by node or site, by analysis, by severity and by time window. Until now a finding was
  only reachable through the run that produced it, so "has anything been found about this switch
  lately" meant opening runs one at a time. Rows link back to the run's report. The endpoint is
  keyset-paged: pass the last row's `at` and `id` back as `before` and `before_id`.
- **The REST API accepts an API token.** Until now a `yat_…` token authenticated `/mcp` alone and
  the REST API answered `401`, so unattended automation had to store a password and log in on every
  run. A token now names the **surfaces** it may be presented at, and one that includes `rest` works
  on `/api/v1` exactly like a session token: `Authorization: Bearer yat_…`. Existing tokens carry
  `mcp` alone, so upgrading cannot turn a credential minted for an AI client into one that can
  reconfigure monitoring — reaching REST is an explicit choice made when the token is issued.
  Two limits apply to a token wherever it is used: it **cannot administer users** (a credential that
  could mint its own successor would outlive every revocation of the original), and endpoints that
  identify the signed-in account — `GET /api/v1/auth/me`, the personal dashboard — answer `403`.
- **API tokens have an owner and an optional expiry.** A token's effective role is
  `min(token role, owner's current role)`, so demoting an account narrows its tokens at once, and
  disabling or deleting an account revokes them. Expiry is optional: `POST /api/v1/api-tokens` takes
  `expires_at`, and omitting it still means no expiry — appropriate for a service account driving an
  integration. Settings ▸ API tokens shows the owner, the surfaces, the expiry and why a token is
  refused when it is.
- **Service accounts.** `POST /api/v1/users` takes `kind: "service"` — a machine account with no
  password that cannot sign in through either the local form or SSO. It exists to own API tokens, so
  an integration keeps working when the person who set it up changes teams, and so that disabling it
  stops every credential it owns at once. `password` is now optional in that request body and is
  **refused** for a service account rather than ignored. The lock-out guard that protects the last
  admin now counts only accounts a human can sign in with, so a service account cannot become the
  only administrator.
- **`YAGRA_PAT_OIDC_IDLE_DAYS`** (default `30`) bounds how long an API token owned by an
  SSO-provisioned account survives its owner's silence. Yagra is never told when an identity
  provider disables an account — the accounts table is only refreshed by a *successful* SSO login —
  so an absent owner is the only signal available. Local and service accounts are unaffected.
- **URL monitors can present credentials.** A new `http_auth` credential kind covers Basic, Bearer
  and a custom header; bind one to a URL monitor and the poller presents it. The credential is
  envelope-encrypted at rest and inlined into the poll job at dispatch time, the same path SNMP
  credentials already take — the poller never reads a credential store. The existing `api_token`
  kind, which until now was creatable and consumed by nothing, is accepted as a bearer token.
  A monitor that presents credentials must verify TLS (`400 credential_needs_tls` otherwise).
- **Pollers hand their nodes over when they shut down.** A poller now sends a final heartbeat
  marked `leaving` on SIGTERM, so core drops it from its pool's hash ring immediately and
  reassigns its nodes. Previously a shutdown was indistinguishable from a network partition, so
  core waited out three missed beats (30s) — and if the restart finished inside that window the
  ring never changed at all and those nodes went unpolled for the whole restart. Rolling upgrades
  now hand over in seconds without the operator doing anything.
- **Monitoring gaps say what passive data was lost.** A gap row now records which passive
  listeners the poller had bound (`syslog:514`, `trap:162`, …). Polled metrics are backfilled from
  the poller's buffer on reconnect; syslog, traps and flow exports are not, so this is the
  difference between an unexplained silence in the event log and a known loss. (SNMP informs are
  the exception — the sender retries until acknowledged.)
- **The global search box in the top bar works.** It has been present but permanently disabled
  since the shell was built, with a code comment saying no search endpoint existed —
  `GET /api/v1/nodes/search` has existed since the node picker was added. It searches nodes,
  debounced, with arrow-key navigation, `Ctrl`/`Cmd`+K and `/` to focus, and Enter to open the
  node. The popover states that only nodes are searched: alerts, events and groups have no
  server-side search, and a nodes-only result set that looks fleet-wide is worse than none.
  Mobile gets it as a tap-to-open row under the top bar.
- **Topology ▸ Geo map is a real page.** It was a "Coming soon" placeholder; it now draws a pin per
  node group that has coordinates, on a world outline, coloured by that group's worst member state,
  with wheel/drag/pinch pan-zoom and click-through to the group's nodes. It reads the same
  per-group health rollup the dashboard's Geo map widget does, so the two cannot disagree about a
  site. The outline is bundled in the app — no tile server, no external request, no new dependency
  — because a monitoring console is what you open when the network is broken, and a map that needs
  the internet is blank exactly when it matters.
- **Group coordinates can be set again.** `PUT /api/v1/node-groups/{id}/geo` was removed in
  v0.1.19 as an uncalled endpoint, but the dashboard's Geo map widget still read those
  coordinates — leaving a widget with no way to be populated. The endpoint is restored and the
  group dialog now has latitude/longitude fields.
- **"Notify me" on a Troubleshoot run now notifies.** The choice was offered and stored, and
  consumed by nothing. A completion notice now appears from any page, with a link to the report.
  It fires only while Yagra is open in this browser, which the control now says.
- **Active alerts can be muted from the row.** Alerts ▸ Active gains a working Mute action that
  opens the mute dialog with the node fixed and the metric that fired pre-filled, so the mute
  covers exactly the check being triaged rather than the whole node. It appears for operators and
  admins — the roles `POST /api/v1/mutes` already accepts — and is absent for viewers instead of
  offered and rejected. Muting suppresses *notification* only: the alert stays in the list and in
  the history, unchanged.
- **The permanently disabled "Open external" action is gone.** It had been shipped disabled since
  it was written, promising a deep link into PagerDuty/JSM that no configuration could ever supply
  — Yagra stores those integrations as outbound endpoints, not per-incident URLs. The `acked` pill
  already names the tool and the person who acknowledged, which is the honest version of the same
  information. No API changed.

### Bug Fixes
- **An API token with the `rest` surface was refused by most read endpoints.** Anything that filters
  by group scope — the node lists, the fleet summary, alerts, events, metric rankings, topology —
  resolved the caller through the session store alone, so a valid `yat_…` token answered
  `401 unauthorized` even though the same token passed the permission guard on the same request.
  Introduced with group-scope enforcement earlier in this release, so no tagged version shipped it.
  Both guards now read the one credential each request resolves, and a test pins that a token
  reaches a scoped read.
- **Editing a URL monitor cleared its credential binding.** The form's own comment said every
  field is sent explicitly *because* the request is a replace, and then omitted the credential.
  This was invisible while nothing consumed the binding; with the feature above it would have
  logged a monitor out on any unrelated edit.
- **Filtering the node inventory returned at most 100 matches, silently.** The API clamped the
  limit to 500 and documented that as the maximum, while the query re-clamped to 100 — so a filter
  matching thousands of nodes showed 100, with nothing indicating the list had been cut. The cap is
  now a single constant (500) used by both the edge and the query, and the tree shows a notice when
  a filter fills the page.
- **The WebUI could keep serving a pre-upgrade page after an upgrade.** nginx sent no
  `Cache-Control` for the SPA at all, so browsers fell back to heuristic freshness (RFC 9111
  §4.2.2) and could reuse a cached `index.html` for hours. Because each image replaces the whole
  document root, that stale page names hashed assets the new image no longer contains — and the
  SPA fallback answered those requests with `index.html`, so the browser rejected a script served
  as `text/html` and rendered nothing, with no indication why. `index.html` is now `no-cache`
  (revalidated on every load, still a 304 when unchanged), hashed assets under `/assets/` are
  `immutable` for a year, and a missing asset returns 404 instead of HTML.

## v0.1.19

**Ask why, and see who polls what.** Two additions lead this release. **AI-assisted root-cause
analysis** runs from a live alert: Yagra already assembles the incident — the cascade's root cause,
the metric anomaly, the passive events and the dominant traffic around it — and now asks a language
model for the sentence a human writes at the end. It is off until you configure a provider, and
nothing in the alert path depends on it. Alongside it, **node→poller visibility**: every node says
which pool it belongs to and which poller is actually polling it, and pools are assignable from the
inventory tree — including on a folder, which is the only bulk assignment the tree has. Underneath
both, the northbound API now **publishes its own OpenAPI 3.1 document**, generated from the handlers,
with the WebUI's types generated from that — which removed 3,340 lines of hand-transcribed
TypeScript and closed a class of contract drift for good.

### Breaking changes
- **An unauthenticated request now answers 401 across the whole API, not 503.** Yagra answers 503
  when a subsystem an endpoint needs is not configured ("skeleton mode"), and roughly a hundred
  handlers ran that availability check *before* the permission guard — so an anonymous caller could
  tell a configured deployment from an unconfigured one without holding any credential. Guard order
  is now uniform API-wide: authenticate and authorize first, check availability second. Authenticated
  callers see no change; only anonymous requests to an unconfigured subsystem move from 503 to 401.
- **`VITE_API_BASE` is now an origin, not a base path.** It used to default to `/api/v1` and be
  prepended to relative paths; it now defaults to empty and the `/api/v1` prefix is part of every
  path. Only a WebUI build that overrides it is affected, and only to drop the `/api/v1` suffix:
  `VITE_API_BASE=https://core.example.net`, not `…/api/v1`. This also fixes the live-update streams,
  which never appended `/api/v1` themselves and so pointed at a different host than the API client
  whenever the variable was set.
- **`GET /api/v1/thresholds` returns an envelope and is capped.** The response changed from a bare
  `StoredThreshold[]` to `{ "items": [...], "total": <n>, "truncated": <bool> }`, and the server
  returns at most **500** rules per request — `?limit=` can narrow that, never widen it. The WebUI
  ships in the same image and was updated with it, so this affects **external automation only**.
  Anonymous requests to this endpoint now answer **401** rather than 503, matching the rest of the
  API: a caller is authenticated before the server discloses whether a subsystem is configured.
  Reading the rules requires **ManageConfig**, not View — a threshold set describes when and whom
  Yagra will page — so it stays closed on a public dashboard.
- **`POST /api/v1/api-tokens` rejects a group scope.** Sending `{"scope": {"Groups": [...]}}` now
  answers **400 `unsupported_scope`** instead of minting the token; omit `scope` or send `"All"`.
  Nothing enforced a group scope on either surface — `/mcp` refuses a group-scoped token outright
  and the REST endpoints never consulted the scope at all — so what was issued was a credential
  that looked least-privileged and was in fact either unusable or unrestricted, depending on where
  it was pointed. The WebUI never offered the field, so this affects API clients only. Group scoping
  will be accepted again when the read paths actually filter by it.

- **Four report and group endpoints now answer the status code the rest of the API does.** The three
  report deletes — `DELETE /api/v1/reports/definitions/{id}`, `/runs/{id}` and `/schedules/{id}` —
  returned `200 {"ok": true}` where the other 24 deletes in the API return `204 No Content`; they now
  return **204** with no body. `POST /api/v1/node-groups` was the only creator that discarded the id
  it had just generated, answering 204; it now returns **201** with `{"id": "<uuid>"}` like the other
  twenty creators. `POST /api/v1/reports/schedules` returned its `{"id": …}` under 200 and now
  returns **201**. A client that checks for an exact status code, or reads `ok` off a delete
  response, needs updating; the WebUI ships in the same image and ignored both.
- **`GET /api/v1/rca/{id}` and `POST /api/v1/rca` now describe what is inside a report.** The
  `body` field was published as an untyped JSON blob, so a generated client got `unknown` for the
  entire AI answer and its evidence. The document now carries real schemas for the answer
  (`summary`, `root_cause`, `dependents`, `next_steps`, `confidence`, `raw`) and for the incident
  context it was grounded in. **The bytes on the wire are unchanged** — only the description was
  missing. Regenerate your client to pick the types up.

- **Running a Troubleshoot analysis is now an Operator action on both surfaces.** It required
  **Admin** over the REST API and merely **Viewer** over MCP, so the on-call operator was refused in
  the WebUI while the same person could run the identical analysis through an AI client. Both now
  ask for the acknowledge-alerts permission (Operator and up), which is also what cancelling a run
  takes. An analysis changes no configuration — the admin requirement was standing in for a rate
  limit, and real admission control has done that job since it was added. Reading past runs and
  their findings is unchanged and still open to Viewers. **Viewer-scoped API tokens can no longer
  launch analyses over `/mcp`.**

- **Four endpoints nothing called have been removed.** `GET /api/v1/rca/{id}`,
  `GET /api/v1/reports/definitions/{id}`, `PUT /api/v1/node-groups/{id}/geo` and
  `POST /api/v1/events/alerts/close` were reachable but called by neither the WebUI, the MCP tool
  surface, nor any documented automation — each answering requests, appearing in the published
  contract and carrying tests for a feature that had no way in. They now `404`. The data each read
  is still served: an RCA report comes back from the `POST /api/v1/rca` that produced it, and a
  report definition from `GET /api/v1/reports/definitions`. **Two are a capability loss, not a
  cleanup:** group map coordinates can no longer be set at all — the Sites map widget reads them
  and nothing writes them any more — and an event-raised alert can no longer be closed by hand; it
  clears on its rule's TTL as before. Say so if either mattered to you and it can come back with a
  UI attached.

### New Features
- **AI-assisted root-cause analysis, on demand.** Yagra already assembles the evidence for an
  incident: dependency suppression attributes a cascade to its root cause, and the incident timeline
  gathers the metric anomaly, the passive events and the dominant traffic around it. What no amount
  of correlation produces is the sentence a human writes at the end. **Active alerts** gains a button
  that asks a language model for that sentence, grounded in exactly that evidence, and returns a
  summary, a probable root cause, the dependents it explains and suggested next steps — with a
  confidence, and the model's raw answer kept beside it.
  **It is off until you configure it.** With no provider row there is no client, no credential and no
  egress. Nothing in the alert path calls into it, so hysteresis, suppression, dedup and notification
  behave identically whether the provider answers, times out, or was never set up. Choose one
  provider — **Vertex AI**, **Gemini** or **Claude** — whose credential is sealed with the same
  envelope cipher as every other stored secret and is write-only once saved. Generating a report
  takes **Operator** (the people carrying the pager are the ones who need the explanation) and is
  bounded by a concurrency limit, a rate window and a context cache rather than by a narrower role;
  reading one back takes View; configuring the provider takes Admin. Device output quoted into the
  prompt is fenced, each provider's endpoint is a compiled-in constant rather than a settings field
  so a configuration screen cannot become an exfiltration channel, and **Yagra still has no way to
  configure a network device**.
- **Every node says which poller polls it, and pools are assignable from the inventory tree.**
  Node→poller assignment existed but was effectively invisible: answering "which poller polls this
  node?" meant running `redis-cli`, and a node's pool was writable only through an API field no
  screen sent. Node detail now shows **Pool** and **Polled by** — the latter a five-state answer
  (assigned / pending / legacy fan-out / Meraki / unknown) read from the working set core actually
  published rather than re-derived from the hash ring, so a node's answer and a poller's node list
  cannot disagree. **Settings ▸ Pollers** drills into any poller's node set inline. Pool is now
  editable on a node and on a folder, and **right-clicking either in the inventory tree offers a
  pool chip row** — the pools that exist, plus Inherit and Custom — which is the only bulk assignment
  the tree has, since it has no multi-select. A node's effective pool resolves as its own → nearest
  ancestor folder → `default`, and every chip says whether that pool has a **live poller**: assigning
  to a pool with none publishes its jobs to a subject nothing subscribes to, and the node silently
  stops being monitored.
- **A URL or DNS monitor's configuration can be edited and removed after it is created.** Until now
  the add-node dialog could create one and the node detail could display it, but changing a URL, a
  timeout, a resolver or a record type meant deleting the node and making it again — the endpoints
  existed the whole time with nothing calling them. The node's URL/DNS health card gains a ⋮ menu
  with **Edit** and **Remove monitoring**; the editor covers every field including the expected
  HTTP status (any 2xx, an explicit code list, or a range). Removing the configuration leaves the
  node in the inventory and its recorded history intact, and simply stops probing it. Requires the
  same permission as any other monitoring change (Admin).

- **The API now publishes its own OpenAPI 3.1 document, at `GET /api/v1/openapi.json`.** It is
  generated from the handlers themselves — every path, query parameter, request body, response shape
  and error code — so it describes what the server actually does rather than what someone remembered
  to write down. The endpoint is unauthenticated, like `/api/v1/version` and `/api/v1/config`: it
  contains no inventory, configuration or state, and is identical on every deployment. Point any
  OpenAPI client generator at it. The WebUI's own types and API client are now generated from this
  same document, which removes 3,340 lines of hand-transcribed TypeScript that nothing was checking.

### Improvements
- **Group-scoped visibility (ADR-014) is now enforced across the whole read surface.** Every
  endpoint that returns node-associated data filters by the calling account's folder-group scope:
  SQL lists gain an indexed predicate, rankings and in-memory aggregates are filtered after the
  fact, both live SSE streams drop events for nodes outside the scope, and a per-node route answers
  `404` — not `403` — for a node the caller cannot see, so node ids cannot be enumerated. Operator
  actions that name a node in the request body (acknowledge, mute, maintenance window, immediate
  poll, launch an analysis) are checked the same way. **No account can hold a scope yet**, so no
  behaviour changes for any existing deployment; this is the enforcement that has to exist before
  scopes can be issued. Five reads cannot be narrowed and refuse a scoped account with
  `403 scope_unsupported` rather than answering with fleet-wide numbers: saved reports, the fleet
  state timeline, aggregate throughput, and the fleet-wide `/flow/*` aggregates — each is stored or
  computed already summed, with no per-node attribution left to filter on.
- **Active alerts now say which device and what broke, instead of two UUIDs.** A triage row read
  `● 550e8400-… 7c9e6679-… 2m ago`. The node resolves to its name (the id is on hover, as everywhere
  else), and the check's id — a one-way hash of node and metric, so it has no name to resolve to —
  is replaced by what the check actually measured: `icmp_rtt_ms above 100 (was 450)`. That detail
  was already stored with every alert and shown on Alerts ▸ History; the triage screen simply never
  displayed it. Both screens now render it through the same formatter, so they cannot disagree.
- **Settings ▸ System Health lists the flow store, and shows the server's own verdict.** The
  ClickHouse row was missing while the page's aggregate health counted it, so a flow-store outage
  read as "everything reachable". The card now lists all five backing stores plus the bus, and
  carries an "All reachable" / "Degraded" badge that comes from the server rather than being
  re-derived from the rows — so the next dependency the page forgets disagrees visibly.
- **Poll-loop health reports working-set distribution.** The widget adds pools served as a working
  set versus pools falling back to per-job publish (the latter turns amber when non-zero — it means
  a pool has no live registered poller), working-set snapshots versus deltas, and assignment-mirror
  writes. Settings ▸ Pollers also gains a **Registered** column showing when each poller first
  checked in.
- **The app icon now matches the one on the Yagra website.** The browser-tab favicon and the mark in
  the top bar, the mobile top bar and the sign-in panel are the topology fork that also reads as a
  "Y" — one root node branching to two — replacing the older double-ring mark. The orange seal, the
  colors and the sizes are unchanged; a hard refresh may be needed for the tab icon.
- **Event search filters and pages on when an event happened, not when Yagra ingested it.** The
  PostgreSQL path used the ingest timestamp while the VictoriaLogs path already used the event
  timestamp, so the same search returned different rows and a different order depending on which log
  store was enabled. Both now agree on the event timestamp. One consequence is inherent to
  time-ordered logs: when a remote poller reconnects and replays buffered results
  (store-and-forward), older events can land *behind* a page you have already scrolled past — which
  is how the VictoriaLogs path has always behaved.
- **The Flapping watchlist names its nodes.** The dashboard widget showed raw node and check UUIDs;
  it now resolves the node's name (UUID on hover) and shows what the flapping check measures, the
  same way the Active alerts list does.

### Bug Fixes
- **A report run in a state the WebUI didn't recognise was shown as "Failed".** The status badge
  ended in a catch-all that painted anything unfamiliar critical-red, so a run that had actually
  succeeded could read as broken — most visibly during a rolling upgrade, where an older WebUI sees
  rows written by a newer core. Run state, trigger and schedule cadence are now closed sets in the
  API contract (`queued`/`running`/`succeeded`/`failed`/`unknown` and so on), the badge is a
  per-state map with no catch-all, and a genuinely unrecognised state renders neutrally as "Unknown
  state" rather than as a failure. **No wire change** — the same strings, now described.
- **A DNS monitor's failure reason was shown as a raw internal token.** Node ▸ DNS rendered
  `nx_domain`, `serv_fail`, `depth_exceeded` and six others verbatim, untranslated — so a Japanese
  operator got English snake_case in the resolution column. All nine now read as sentences in both
  languages ("No such name (NXDOMAIN)", "名前が存在しない（NXDOMAIN）").
- **An alerting rule scoped to one event stream could silently widen to all of them.** An event rule
  naming a source kind this build did not recognise parsed to "no kind filter", which the matcher
  reads as *any* kind — so the rule fired on syslog, traps and webhooks alike, rather than the one
  stream it was written for. Such a rule is now left out of the engine and logged until the core
  understands it.
- **A poller went on polling nodes that had moved to another pool.** The scheduler built its pool map
  from node rows alone, so a pool whose last node moved away vanished from the map and was never
  reconciled again — its poller kept polling the stale working set for the life of the core process,
  double-polling every node that had left. Recovery took a poller restart. Every live pool is now
  seeded into the map before the node pass. Until this release it took a hand-written database edit
  to reach; making pool editable from the WebUI turns it into the first thing an operator does.
- **A filtered flow destination dropped the template datagrams its collector needed.** A forwarding
  destination with a filter tests the decoded flow records, and a NetFlow v9 datagram carrying only
  template definitions has none — so an exporter that refreshes templates in their own record-free
  datagrams left a filtered collector holding data sets it could never decode, silently and
  permanently. A datagram with template definitions and no flow records now bypasses the filter. The
  rule is deliberately "templates and no records", not "no records": a data set whose template is
  unknown also decodes to zero records but teaches a collector nothing, so there the filter still
  decides. Exporters that inline templates in every export were never affected. Found by on-metal
  validation against a real collector.
- **A node could hold both a URL and a DNS monitor, and the DNS one would never run.** The "a node is
  exactly one kind" guard was enforced on the DNS writer only, so attaching a URL check to a
  DNS-monitored node was accepted and stored — and the scheduler, which resolves URL first, then
  never ran the DNS check. Both writers now ask the same guard, and the precedence is stated once
  rather than twice.
- **`get_fleet_summary` over MCP left out node states it had not seen.** The REST rollup pre-seeds all
  six states; the MCP tool inserted only the ones present in the fleet, so an AI client reading
  `states["warning"]` got a missing key where the WebUI got a zero — and reported, confidently, that
  there was no warning data. Both surfaces now return the same tally. Relatedly, an empty fleet's
  data coverage reads as 100% rather than 0%, so the blind-spot widget no longer lights up on every
  fresh install and gets tuned out.
- **A broken key or an unreachable database told the operator their OIDC settings were invalid.**
  Saving an identity provider rendered *every* failure as `400 invalid_provider` with the internal
  error text attached, so a key-encryption problem or a failed database write was reported as bad
  input — and the internal message went out on the wire. A bad submission is now a 400 that says
  which field, a fault is an opaque 500 with the cause in the log, and an IdP that cannot be reached
  during sign-in is a 502 rather than a 500.
- **A regular-expression event search was case-sensitive on VictoriaLogs, case-insensitive on
  PostgreSQL.** The same regex matched different rows depending on which log store was enabled; both
  are now case-insensitive. A *plain* search term still matches whole tokens on VictoriaLogs and
  substrings on PostgreSQL — an inverted word index cannot serve a leading substring without scanning
  every block, measured at 30s against 0.22s on the live fleet — so that one difference is deliberate,
  and the search box's regex toggle is the escape hatch when you need to reach inside a token.
- **The poller's store-and-forward buffer can use its disk again.** The container image never
  created the spill directory, and `/var/lib` is not writable by the non-root runtime user, so the
  buffer fell back to memory-only after a single startup warning. A bus outage lasting longer than
  the in-memory ring therefore dropped the oldest poll results instead of spilling them, and a
  poller restart mid-outage lost everything it was holding. The directory is now created in the
  image with the right ownership, and the test-server deployment gives it a named volume so the
  spill survives container recreation.
- **Adding a node twice.** Nothing guarded the add-monitor dialog's submit button while the request
  was in flight, so a double-click created two nodes. The dialog also kept the previous attempt's
  failure message, showing a stale error above a blank form when it was reopened after a cancel.
- **The OpenAPI document said personal access tokens work on the REST API. They do not.** The
  published `bearer` scheme offered a `yat_…` token as an alternative to a session token, but this
  API's auth edge accepts session tokens only, so a client following the contract got 401 on every
  call. The description now says what is true: personal access tokens authenticate the MCP surface
  at `/mcp`; the REST API wants a token from `POST /api/v1/auth/login`.
- **Dialogs keep the keyboard inside them.** Tab used to walk straight out of an open dialog into
  the page behind it, so a keyboard or screen-reader user could end up editing controls hidden
  under the overlay while `aria-modal` promised the opposite. Tab and Shift+Tab now cycle within
  the dialog, and only the frontmost one traps.
- **The notification bell does something.** It showed the active-alert count but ignored clicks; it
  now opens Active alerts, on both the desktop and mobile top bars.
- **Three chart colors were unreadable in dark mode.** The palette's fourth, fifth and sixth
  entries kept their light-theme values on the dark surface — which covered most Troubleshoot
  report bodies and the passive-event and capacity dashboard widgets, not just charts.
- **Thresholds no longer judge raw counters.** A threshold on a counter metric (`if_hc_in_octets`,
  errors, discards — anything the collection catalog declares a counter) compared the raw monotonic
  total against the bound, so an `above` rule latched permanently once the counter passed it and a
  `below` rule fired a phantom alert at every reboot's counter reset. Counter samples are now read
  as OK — which also drains any alert such a rule had latched, through the normal recovery path —
  and `POST /api/v1/thresholds` on a counter metric answers **400 `counter_metric`**. Rates stay a
  query-time concern (ADR-012); set thresholds on gauges.
- **A failed report delete no longer closes silently.** Deleting a report template, saved run or
  schedule swallowed the error and closed the dialog looking like success; the confirmation now
  stays open and shows the message, like every other delete dialog. The report builder and schedule
  dialog also disable Cancel while a save is in flight.

### Security
- **Disabling an SSO account did not stop that person signing back in.** The local-password login
  path has always refused a disabled account, but the SSO callback went straight from "the identity
  provider says who you are" to issuing a session without ever reading `users.enabled`. Disabling an
  SSO-provisioned account therefore revoked its live sessions and then let the very next SSO login
  mint a fresh one — the control was effectively a no-op for exactly the accounts an operator is
  least able to switch off at the source. The SSO path now refuses a disabled account, answering the
  same opaque `oidc_denied` as every other callback failure.
- **A mutating request authenticated by an API token was audited as anonymous.** `audit_mw` resolved
  the actor by looking the bearer up in the *session* store only, which no token is in. With tokens
  confined to `/mcp` nothing reached that path; opening REST to them would have made it reachable.
  The bearer is now resolved once per request and the audit row names both the account and the
  credential (`svc-ci (token:grafana)`), so a token-driven change is attributable to something a
  person is answerable for.
- **A URL monitor accepted an IPv6 loopback or link-local target.** The edge validator blocked
  SSRF-prone destinations by parsing the URL's host as an IP address — but a URL parser returns an
  IPv6 literal *with* its brackets (`[::1]`), which the address parser rejects, so every IPv6 target
  fell through to the hostname path and skipped the block entirely. `http://[::1]/` and
  `http://[fe80::1]/` were accepted when creating or editing a URL monitor. **This was a check that
  silently did nothing rather than an exfiltration path** — the poller refused the same addresses at
  probe time, so no request was ever made to one. Three other places parse a URL host and each had
  its own correct copy; all four now share one implementation.

## v0.1.18

**Hand your passive data onward, and see it.** Yagra already received syslog, SNMP traps and flow
exports; this release lets it **forward** them — a filtered tee to a SIEM or collector, byte-for-byte
or rebuilt, over UDP/TCP/TLS, plus **BigQuery** destinations that stream normalized rows for long-term
querying. Alongside it: **DNS name-resolution monitoring** as a first-class node kind, **eleven new
Troubleshoot analyses** over the passive-event and flow stores, a **tailored report screen for every
one of the 15 analyses**, and **thirteen new dashboard widgets** for passive events and traffic flow.

### New Features
- **Forwarding ("tee") destinations** — a new **Settings ▸ Forwarding** page defines destinations that
  say "everything matching this filter also goes there". Core does the sending from one leader-only
  egress point, so you allow **one** address through the firewall rather than one per poller, and you
  stop configuring a second export target on every device.
  - **Syslog and SNMP traps** relay **byte-for-byte**: the original datagram is carried alongside the
    parsed event, so the collector sees exactly what the device sent. Where no original exists, Yagra
    rebuilds a faithful RFC 5424 / SNMPv2c message instead.
  - **Flow exports** (NetFlow v5/v9/IPFIX, sFlow) relay verbatim, template datagrams included — the
    aggregated flow records can't stand in for them, because bucketing, top-N truncation and 5-tuple
    folding are irreversible. Flow filters are an **any-record** test: one matching record forwards the
    whole datagram, since records can't be removed without re-encoding it.
  - **Syslog over TLS** (RFC 5425) verifies the collector against the system trust store plus an
    optional per-destination CA certificate.
  - **BigQuery destinations** stream **normalized, typed rows** — one per event, one per flow record —
    via `tabledata.insertAll`, for querying months of history rather than mirroring a live stream.
    Because rows are independent, flow filtering here is **exact per record**. The table is created for
    you with day partitioning and clustering; the dataset is not, so Yagra never picks your data
    residency for you.
  - Each destination has a bounded queue, rate limit and circuit breaker, and **cannot silently
    degrade**: a destination promised byte-exact output but given none counts it, and any poller that
    can't supply original bytes is named on the page.
- **DNS name-resolution monitoring (a node kind)** — monitor a *name* the way you monitor a URL. Bind a
  node to the built-in **DNS name resolution** profile and Yagra records whether the name resolves and
  the dig-like recursive **CNAME chain** it resolves through, with a history that appends **only when
  the chain actually changes** (TTL countdown and round-robin reordering don't count as a change).
  Numeric summaries (`dns_up`, `dns_resolve_ms`, `dns_chain_length`, `dns_answer_count`) are graphable
  and alertable, with a default `dns_up` threshold seeded so a new monitor alerts out of the box.
- **Eleven new Troubleshoot analyses** over the passive-event and flow stores: `event_storm`,
  `event_flap`, `severity_shift`, `rule_gap`, `auth_probe` (passive); `traffic_anomaly`, `talker_shift`,
  `new_destination`, `flow_scan` (flow); and `saturation` + `incident_correlate` reading across metrics,
  events and flow together. As before, an analysis is a **read** — no device I/O — under the same
  concurrency and rate limits.
- **A tailored report for every Troubleshoot analysis.** Previously only the anomaly scan had a real
  report and the other 14 tools showed a "coming soon" toast; now all 15 have their own screen built
  for their own findings — including an SVG **incident timeline** for `incident_correlate` (the order
  signals arrived in is what points at a cause), a scan-shape scatter for `flow_scan`, and a share
  meter with capacity context for `saturation`. Every report supports CSV export and `?job=` deep links.
- **Passive-events and traffic-flow dashboard widgets** — two new catalog sections adding **13 widgets**:
  event feed, volume, kind mix, top traps, triage, noisy sources and rule coverage; plus top talkers,
  top AS, top ports, protocol mix, a conversation Sankey and a traffic trend. The flow widgets read
  **fleet-wide** (every exporter), not one node at a time.
- **New MCP tools for flow and events** — `flow_fanout` and `event_stats`, and `top_flows` gains
  protocol/port/peer/ASN/direction filters with AS-name resolution. `run_analysis` accepts all the new
  analysis kinds.

### Improvements
- **Container images for the metrics and log stores are pinned.** `docker-compose.yml` and
  `docker-compose.deploy.yml` referenced `victoria-metrics:latest` and `victoria-logs:latest`; both now
  name an explicit version, so an unrelated `docker compose pull` can no longer roll the storage engine
  underneath your history. Bump them deliberately, with a backup — the same policy as the pinned Rust
  base images.
- **Store-and-forward spilling is cheaper under pressure.** A remote poller buffering results during a
  bus outage no longer issues a filesystem free-space syscall per spilled result on its async runtime;
  the reading is cached and debited by bytes written, so the safety floor still trips **early** rather
  than late.
- **Traffic-flow views read better** — conversation endpoints get a minimum label slot so long
  addresses stay legible, the conversation Sankey is size-capped instead of growing without bound, and
  source/destination AS numbers are shown alongside the addresses.
- **Fleet-wide flow endpoints.** The flow API is no longer node-scoped only; series, top talkers,
  conversations, top ports, protocols and top-AS all take a fleet scope. A new `/events/stats` endpoint
  serves categorical and time-series event aggregates over the same filter as the event log, via
  PostgreSQL or VictoriaLogs, so summaries and the log always agree.

### Bug Fixes
- **A poller no longer stalls when it meets a check kind it doesn't understand.** Working-set snapshot
  chunks are now decoded per element, so one unknown check can't fail a whole chunk, gap the sequence
  and spin that poller in a resync loop — which stalled *all* of its polling, not just the unknown
  check. This is a permanent fix for every future check type, not just DNS.
- **DNS checks no longer suppress one another.** The per-target single-flight poll guard would drop
  every DNS check but one per cycle, because DNS monitors share a resolver target by design; they now
  take the global guard instead.
- **The AS drill-down no longer breaks the Conversation flow view.** Filtering by AS returned a 500 for
  the conversation table and Sankey (the aggregate was aliased to the same name as the column it
  filtered on), leaving stale data on screen while the other panels updated.
- **Troubleshoot report deep links resolve correctly.** A `?job=` older than the recent-jobs window
  rendered as if nothing had been requested, and a `?job=` belonging to a different tool rendered the
  wrong report over foreign findings; the report shell now fetches the job directly and redirects to
  the report that can actually read it.

### Security
- **Forwarding is built so it can't quietly leak.** TLS destinations have **no option to disable
  certificate verification**. BigQuery destinations deliberately have **no raw-payload column** — a
  relayed datagram passes once to a collector you chose, but a table persists, and a raw-bytes column
  would make the credentials that routinely appear in syslog bodies permanently queryable off-box; the
  API rejects verbatim mode for BigQuery rather than relying on a hint being read. Service-account
  signing uses a constant-time RSA implementation.
- **Removed an unmaintained TLS dependency.** `rustls-pemfile` (RUSTSEC-2025-0134, archived upstream)
  is gone; certificate and key parsing now uses the same code directly from `rustls-pki-types`. The
  supply-chain policy check passes clean across advisories, bans, licenses and sources.
- **Resolved a client-side open-redirect advisory** by updating the frontend router, and a
  path-traversal advisory in the build toolchain. The router fix is user-facing; the toolchain fix is
  build-time only and never shipped.

## v0.1.17

**Talk to Yagra from an AI assistant.** Yagra now ships a built-in **MCP (Model Context Protocol)
server** — an opt-in, authenticated tool surface at `/mcp` that lets an AI client (Claude Code/Desktop,
or any MCP-capable assistant) query live monitoring state and run diagnostics in your own words. It is
off by default and, when enabled, is mostly read-only; the few write tools are permission-gated and
audited, and nothing can change device configuration.

### New Features
- **MCP server (AI / automation tool surface)** — enable with `YAGRA_ENABLE_MCP` to expose a
  Streamable-HTTP MCP endpoint at `/mcp` on the API port. It offers **15 tools**: read tools for fleet
  summary, nodes, node status, active/historical alerts, metrics, topology, traffic flows, and passive
  events (syslog/traps/webhooks); on-demand **Troubleshoot analyses** (anomaly / correlation / capacity /
  flap); and **three write tools** — acknowledge an alert, open a maintenance window, and trigger an
  immediate poll. Tool output is sanitized (monitoring credentials never leave the system) and there is
  **no tool that configures or changes a network device**.
- **API tokens** — a new **Settings ▸ API tokens** page issues long-lived `yat_` tokens (each with a role
  and group scope) for non-browser clients such as an MCP assistant. The raw token is shown **once** at
  creation and only its hash is stored; issuing and revoking are admin actions and are audited.

### Improvements
- **On-demand analyses are rate- and concurrency-limited.** The Troubleshoot analysis runner now caps how
  many analyses run at once (`YAGRA_ANALYSIS_MAX_CONCURRENT`, default 4) and how many may start per minute
  (`YAGRA_ANALYSIS_RATE_PER_MIN`, default 30); when saturated, the API returns `429` instead of piling on
  work. This bounds the cost of both the UI and the new MCP analysis tools.

### Security
- **Per-tool authorization and audit for MCP write actions.** Every MCP write tool re-checks the caller's
  role (RBAC) and records an audit entry; the surface is fail-closed — if the caller can't be authorized,
  the write is refused. A **Viewer** token stays read-only.
- **Resolved two High-severity advisories in the frontend build toolchain** by updating transitive dev
  dependencies (`brace-expansion`, `js-yaml`). These are build-time only and never shipped in the product,
  but the dependency tree is now clean.

## v0.1.16

**Licensing and flow-ingestion hardening.** Yagra is now released under the **GNU Affero General
Public License v3.0 (AGPL-3.0-only)**, and this release hardens the traffic-flow ingestion path so a
busy or slow flow store can't disturb the rest of the system. There are no new user-facing features —
a default install behaves exactly as v0.1.15 did, now under the new license.

### Licensing
- **Relicensed to AGPL-3.0-only.** Every source file now carries an SPDX header and the repository
  ships under `AGPL-3.0-only`. Because Yagra is typically run as a network service, note AGPL **§13**:
  if you run a *modified* version and let others use it over a network, you must offer them its source.
  For terms other than the AGPL (e.g. embedding Yagra in a proprietary product), a **commercial
  license** may be available — see the README.

### Improvements
- **Flow ingestion is isolated from a slow flow store.** The ClickHouse flow writer now runs
  separately from the flow bus consumer, handing rows off over a bounded queue. A slow or hung
  ClickHouse can no longer back the `yagra.flows` subscription up into a silent message drop — under
  pressure, flow rows are dropped and counted (the store is loss-tolerant by design) instead of
  stalling ingestion.
- **Flow exporter resolution no longer reloads the node table per batch.** Flow from an exporter whose
  source IP isn't a registered node (common for routers that export from a loopback) used to re-scan
  the whole node table on every batch; each unmapped exporter is now retried at most once, so flow
  ingestion stays cheap at tens of thousands of nodes.

## v0.1.15

**See what your traffic is doing.** Yagra now collects and analyzes network **flow** records — NetFlow,
IPFIX, and sFlow — so alongside the health monitoring you already had you can see the top talkers,
ports, protocols, and autonomous systems moving traffic across your network. Incoming SNMP traps are
decoded to readable names too, and the active-alerts screen stays smooth even during a large outage.

### New Features
- **Traffic-flow monitoring (NetFlow v5/v9, IPFIX, sFlow v5)** — point your devices' flow export at a
  poller (`:2055` for NetFlow/IPFIX, `:6343` for sFlow) and Yagra collects, edge-aggregates (top-N per
  time bucket), and stores the records in a dedicated **ClickHouse** flow store (1-month retention,
  loss-tolerant). Each node gains a **Flow** tab — top talkers, top ports, top protocols, and a
  conversation **Sankey** diagram — and you can click any talker / port / protocol to filter. Flow is
  opt-in per poller and entirely separate from metrics: leave it off and nothing changes.
- **Autonomous-system (AS) enrichment for flows** — flow endpoints are resolved to their **AS number
  and name** from an offline IP→ASN table, with a **Top AS** card and drill-down (click an AS to filter
  its flows) and an AS-level conversation view. Exporters that don't send AS themselves (most non-BGP
  devices) are filled in automatically. The dataset is kept fresh entirely inside Docker by an updater
  sidecar and **hot-reloaded** into the running core with no restart. Enrichment is opt-in.
- **Remote-site pollers can collect flow** — a poller at a branch site can receive local flow exports
  and forward them to the core over the (authenticated) bus, so flow works in the same distributed
  poller topology as polling.
- **SNMP trap names** — incoming SNMP traps (v1/v2c) now resolve the trap OID to a **human-readable
  name**, ship with a set of **built-in trap event rules**, and show a trap badge, so a trap arrives as
  a named event instead of a raw OID.

### Improvements
- **Active Alerts stays responsive during a major outage** — the active-alerts triage list is now
  virtualized (windowed), so thousands of simultaneous alerts render only the rows on screen instead of
  the entire list, keeping the page smooth exactly when you need it.
- **Smoother flow-enrichment refresh** — the periodic reload of the IP→ASN dataset no longer briefly
  stalls the core; the large table is now parsed off the async runtime.

## v0.1.14

**Hardening for multi-core and distributed deployments.** Sessions now survive a core failover, and
each remote poller can be handed only the credentials for its own pool — tightening the message bus
for sites that run pollers across the network. Both changes are opt-in; a default single-node install
behaves exactly as before.

### New Features
- **Sessions survive a core failover (opt-in)** — in a high-availability setup, mount a shared
  session signing key on each `yagra-core` (`YAGRA_SESSION_KEY_FILE`) and a login is accepted on
  every core and survives a core restart or failover, so an HA failover no longer signs everyone out.
  Logging out, disabling a user, changing a role, or resetting a password takes effect across all
  cores right away. Without a key, sessions behave exactly as before (per-core tokens).

### Bug Fixes
- **Metric backfill now works on a secured remote bus** — on a NATS bus with TLS and authentication,
  a reconnecting remote poller was not allowed to publish its buffered metrics, so store-and-forward
  backfill (new in v0.1.13) could not replay over a secured bus. Pollers may now publish to the
  backfill channel, so the outage-window metrics fill in as intended. Plaintext single-node buses
  were unaffected.

### Security
- **Per-poller credentials on the message bus (opt-in)** — when you expose the NATS bus to remote
  pollers, core can now issue each poller its own credential scoped to just that poller's pool, so a
  poller only receives the jobs and device credentials for its **own** pool instead of every poller
  sharing one bus account with fleet-wide reach. This significantly narrows what a single compromised
  remote poller can see. Enable it alongside the bus TLS+auth setup; it is off by default and the
  shared-account behavior is unchanged until you turn it on.

## v0.1.13

**Remote pollers survive a network partition.** A poller cut off from the central core keeps
monitoring locally and backfills the metrics it collected once the link returns — so a WAN blip
becomes a filled-in gap in your history, not a hole.

### New Features
- **Store-and-forward for remote pollers (on by default)** — when a poller loses its connection to
  the core (a WAN outage, a firewall blip), it keeps polling its devices locally and buffers the
  results instead of dropping them. On reconnect it bulk-replays them, and the core imports the
  metrics at their original timestamps, so graphs and history fill in the outage window with no
  false spike. Alerts are deliberately **not** replayed — alert evaluation resumes from "now", so a
  reconnect never floods you with stale, already-resolved alerts. The buffer is bounded (an
  in-memory ring plus an on-disk spill that survives a poller restart) and drops the oldest data
  first, so it can never fill the poller's disk. Tune the caps — or turn it off — with the
  `YAGRA_STORE_FORWARD*` settings (see `docker-compose.poller.yml`).
- **Recent monitoring gaps on the Pollers page** — Settings ▸ Pollers now lists each window during
  which the core lost contact with a poller (which poller, which pool, when, and for how long), so
  you can see at a glance when monitoring was blind and that the metrics were backfilled.

## v0.1.12

**Single sign-on and core high availability.** Sign in with your organization's identity provider,
and run more than one core so a single failure doesn't stop monitoring.

### New Features
- **Sign in with SSO (OpenID Connect)** — connect an external identity provider (Google Workspace,
  Microsoft Entra, Okta, Keycloak, …) so people log in with your organization's SSO alongside local
  accounts. Configure a provider under Settings ▸ Auth (issuer, client ID, client secret, redirect
  URI, scopes) and map IdP groups to Yagra roles; accounts are provisioned automatically on first
  sign-in and their role follows the IdP on every login. The client secret is encrypted at rest and
  never shown again, and the "Continue with SSO" button appears on the login screen once a provider
  is enabled.
- **Core high availability (opt-in)** — run multiple `yagra-core` instances against the same
  PostgreSQL / Redis / NATS / VictoriaMetrics and only one (the leader, elected via a PostgreSQL
  advisory lock) drives scheduling, polling, and alerting; the others stand by and take over
  automatically within seconds if the leader fails — no double-polling or duplicate notifications.
  Enable with `YAGRA_ENABLE_HA`; route API/web traffic to the active core via the new `/readyz`
  readiness probe. Off by default, so single-core deployments are byte-for-byte unchanged.

### Performance
- **Node detail refreshes on one shared timer** — the node detail view previously ran up to seven
  independent 15-second refresh loops on a single node; these are now consolidated into one shared
  tick, and polling pauses while the browser tab is hidden and catches up immediately on return.

## v0.1.11

**Mobile WebUI polish (parity round 3)** — the remaining screens whose interactions still needed a
mouse now work by touch: dashboard tiles reorder by press-and-hold, the topology map zooms with a
two-finger pinch, and the report builder reflows to fit a phone.

### Improvements
- **Dashboard editing works by touch** — in Customize mode, press and hold a tile's move handle to
  reorder it (a normal swipe still scrolls the board), and the edit controls have larger tap targets.
  On phones, where the board collapses to a single full-width column, the resize grip is hidden since
  it can't change the stacked layout. Desktop mouse editing is unchanged.
- **Pinch to zoom the topology map** — the dependency map now zooms with a two-finger pinch and pans
  with two fingers, alongside the existing drag-pan and wheel-zoom; the on-map zoom controls are
  larger for touch.
- **Report builder fits phones** — the report tabs scroll sideways instead of crushing, and the
  builder's section settings stack full-width in the sheet, with larger touch targets for the tab and
  section-reorder controls.

Desktop rendering is unchanged.

## v0.1.10

**Mobile WebUI polish (parity round 2)** — the settings and admin screens the phone layout hadn't
reached yet now adapt cleanly: crowded row actions collapse behind a "⋯" menu, the user list becomes
a stacked card, and the wide matrix / grid screens scroll instead of crushing their columns.

### Improvements
- **Row actions collapse into a "⋯" menu on phones** — on the Users list and the multi-action
  settings lists (notification routing, credentials, monitoring profiles, event rules, event sources,
  classification rules, and maintenance windows), a card's edit / enable / delete actions now collapse
  behind a single overflow menu instead of crowding the card. Desktop keeps its inline action row.
- **Comfortable Users list on phones** — each account restructures into a stacked card, with the role
  control and actions on their own full-width row and a larger tap target for the role selector.
- **Roles and Discovery screens scroll on phones** — the role-vs-privilege matrix and the discovery
  import grid now scroll sideways with a pinned first column at phone widths, instead of squeezing
  their columns until the controls are unusable.
- **Troubleshoot fits the phone** — the analysis launch drawer goes full-width and the run controls
  get larger touch targets on small screens.

Desktop rendering is unchanged.

## v0.1.9

**Phone support for the WebUI, scaling to tens of thousands of nodes, and a round of security
hardening** — the WebUI now adapts to phones, the inventory / topology / dashboards stay responsive
at fleet scale (searching the fleet on the server instead of loading it into the browser), and
sessions, logins, TLS guidance, and metric ingest are hardened.

### New Features
- **Phone / mobile support for the WebUI** — the WebUI now adapts to phone-sized screens: a mobile
  app shell with a navigation drawer and bottom sheets, card-style tables, forms, and node detail, a
  pane switcher so a node's detail is reachable on a small screen, fuller event and interface views,
  and a Preferences toggle to force the mobile or desktop layout. Desktop rendering is unchanged.

### Improvements
- **Scales to tens of thousands of nodes** — the inventory tree is virtualized and loads a group's
  members lazily (only open, visible groups fetch), topology is paged on the server, and per-group
  and fleet status rollups are computed server-side; live node status now arrives incrementally over
  a stream instead of full-page refetches. Large inventories stay responsive.
- **Fleet-wide search instead of loading the whole inventory** — the node inventory filter and the
  Troubleshoot scope picker now search on the server (debounced and capped), and node pickers use the
  same typeahead, so the browser never has to hold the entire fleet to find a node.
- **Human names instead of raw IDs across the fleet** — referenced nodes, groups, and profiles now
  resolve to their names on the Dependencies, Classification-rules, Mutes, and Maintenance pages
  (with copy-of-the-id on hover), and name resolution covers the whole fleet, not just the first page.
- **Faster, steadier metric ingest and polling at scale** — poll results are handled by a matcher
  plus asynchronous batch writers, SNMP table walks are folded into a single session, dependency
  suppression re-sweeps incrementally, and alert-config and sweep-spec resolution are cached per
  configuration generation — smoothing throughput on large fleets.
- **Faster node-detail interfaces list** — a node's per-interface throughput and status now load with
  a constant handful of time-series queries for the whole node instead of several queries per
  interface, so the interface list on a many-port switch opens far faster and refreshes cheaply.
- **Snappier chart and health views** — the interface, busiest-links heatmap, and host-resource
  charts fetch their independent series concurrently, and the URL-monitor probe reuses a pooled HTTP
  client, trimming latency on those views.
- **Higher default edge intake limits** — the syslog / SNMP-trap per-source and global rate-limit
  defaults were raised (200 / 5000 messages per second) to suit chassis-scale event volume.
- **Polished report viewer and confirmations** — the report viewer is now the standard app dialog
  (keyboard focus handling, and a bottom sheet on mobile), and deleting a report, template, or
  schedule uses the app's themed confirmation dialog instead of a native browser prompt.
- **Container health checks** — the core image now reports readiness through a real health check, and
  the web container waits for the core to be serving before it starts, avoiding first-request errors
  on a cold start.

### Bug Fixes
- **Consistent spacing and emphasis** — several WebUI styles referenced undefined spacing tokens
  (rendering with collapsed spacing) or off-scale font weights; these now use shared tokens, so
  spacing and emphasis are consistent and theme-aware.
- **Chart series colors follow the theme** — time-series chart colors are now drawn from the theme's
  series palette, so they adapt to light and dark mode and no longer reuse the status
  (warning / critical) colors.

### Security
- **Sessions can be revoked and now expire** — disabling, demoting, resetting the password of, or
  deleting a user immediately invalidates that account's active sessions, and sessions now expire
  after an idle period and an absolute lifetime; a new logout endpoint revokes the current token
  server-side. Previously a bearer token stayed valid until the core restarted, so an admin action on
  a compromised account did not cut off an already-issued token.
- **Login brute-force protection** — the login endpoint now applies a per-account exponential lockout
  after repeated failures plus a global attempt-rate cap, throttling password-guessing runs and the
  CPU-exhaustion vector of repeatedly forcing a password hash.
- **Metric-name validation at ingest** — metric names arriving from pollers are re-validated where
  they enter the time-series store, so a malformed or hostile name can't inject stray series or
  labels (a cardinality / data-integrity safeguard).
- **TLS guidance for the WebUI / API** — the WebUI image and deployment now document how to terminate
  HTTPS — an opt-in in-container TLS server block or an external reverse proxy — so the login
  password, bearer tokens, and submitted device credentials are not exposed in plaintext beyond a
  trusted network. The default remains plain HTTP for LAN / behind-proxy use; this is guidance and
  configuration only.

## v0.1.8

**Searchable passive-event log store, richer Events and dashboards, SSRF hardening, and large-fleet
speedups** — received syslog messages and SNMP traps now land in a searchable log store, the Events views
gained filtering, dashboard widgets can be resized and made taller, the URL-monitor probe is hardened
against SSRF, and node inventory and topology load faster on big fleets.

### New Features
- **Passive-event log store with full-text search** — received syslog messages and SNMP traps are now
  persisted to a dedicated log store (VictoriaLogs) via asynchronous batched writes and can be searched
  full-text from the Events views, so passive events are retained and queryable instead of transient.
- **Filters on the Events views** — events can now be narrowed by time range, event kind, whether they
  matched a rule, and free-text / regex search, making it far easier to find a specific event in a busy
  stream.
- **Resizable, taller dashboard widgets** — dashboard widgets now support a stepped per-widget height (with
  an edit-mode height selector) and corner drag-to-resize on a snap-aligned, gap-free grid; edits can be
  discarded with Cancel, and a widget's content fills its resized cell.

### Improvements
- **Faster node list and topology on large fleets** — the node inventory and topology views now derive the
  fallback status of not-yet-observed nodes with a single time-series query instead of one query per node.
  This is most noticeable on large inventories and immediately after a core restart, when the alert engine
  has not yet formed an opinion on every node.
- **Tunable database connection pool** — the core's PostgreSQL connection pool size is now configurable via
  the `YAGRA_PG_MAX_CONNECTIONS` environment variable and defaults higher, lifting a concurrency ceiling
  that could throttle the scheduler, result ingest, and API under tens-of-thousands-of-nodes load.
- **Faster node-detail open** — a node's SNMP scalar readings now load concurrently instead of one after
  another, so the Overview tab fills in more quickly on devices with many scalar metrics.

### Bug Fixes
- **Dashboard chart flicker** — fixed a ResizeObserver-driven flicker on fill-mode (height-filling)
  dashboard charts.
- **UI text-size consistency** — the remaining hardcoded WebUI text sizes (including one that referenced a
  previously-undefined style token and silently rendered at the wrong size) now use shared size tokens, so
  text renders at consistent, themeable sizes.

### Security
- **URL-monitor SSRF hardening** — the HTTP(S) monitoring probe now resolves and connects only through an
  SSRF-filtered resolver, applied to the initial target **and every redirect hop**. This closes two ways
  the loopback / link-local / cloud-metadata blocklist could be bypassed: a monitored endpoint could
  redirect the probe to a hostname that resolved to a blocked address, and a DNS-rebinding target could
  pass the pre-flight check yet be dialed at a blocked address. Private/internal ranges remain allowed (an
  NMS legitimately monitors them); only the loopback/link-local/metadata escalation surface is refused.

## v0.1.7

**Host-resource trends, fuller localization, and a smoother large-fleet + upgrade experience** — System
Health now charts CPU/load/memory/disk over time for the core and every poller, more of the WebUI is
available in Japanese, and both the polling scheduler and shutdown behavior were hardened for scale and
clean restarts.

### New Features
- **Host-resource trends in System Health** — the System Health page now charts **CPU, load, memory, and
  disk** over time for the **core and each poller**, so you can watch resource pressure build up rather
  than only seeing the current snapshot.

### Improvements
- **Fuller English / 日本語 localization** — the remaining WebUI areas — the application shell (top bar,
  sidebar, breadcrumbs), alerts triage, the shared node picker and time-range controls, and the
  remaining screens — are now localized, so switching to Japanese covers far more of the interface. Any
  still-untranslated text falls back to English.
- **Reorganized navigation** — the sidebar is grouped into **Monitor** and **Configure** sections with
  clearer labels, and third-party **Integrations** are gathered into a catalog.
- **Graceful shutdown** — the core and pollers now stop cleanly on `SIGTERM` / Ctrl-C (for example during
  a `docker compose` restart or a rolling upgrade), draining in-flight work and in-flight API requests
  instead of being terminated abruptly.
- **Faster scheduling and ingest for large fleets** — the poll scheduler builds each pool's working set
  concurrently, and notification delivery now runs off the result-ingest path, so a slow notification
  endpoint can no longer stall metric ingestion — both help throughput at tens of thousands of nodes.

### Bug Fixes
- **UI sizing/style consistency** — corrected several WebUI text sizes and modal styles that referenced a
  non-existent style token and rendered slightly larger than intended.

## v0.1.6

**Multi-language WebUI + per-interface metrics on SNMPv3 nodes** — the interface can now be switched
between English and Japanese instantly from Settings ▸ Preferences, and SNMPv3-monitored devices now
collect the same per-interface metrics that v2c devices already did.

### New Features
- **Interface language switching (English / 日本語)** — a new **Settings ▸ Preferences ▸ Language**
  control switches the whole UI language on the fly, with no reload. English is the default and the
  choice is remembered per browser. The application framework — top navigation, sidebar, breadcrumbs,
  sign-in, and the Preferences and About screens — and all shared value formatting (dates and times,
  relative times like "5m ago", node-status and HTTP-status labels, certificate-expiry text) are
  localized, with the Japanese resources loaded on demand only when you switch (zero cost on the
  default English path). Remaining screens are being localized progressively; any not-yet-translated
  text falls back to English, so nothing is ever blank.
- **Per-interface metrics on SNMPv3 nodes** — SNMPv3 (USM) nodes now walk interface tables with
  GETBULK the same way v2c nodes do, so per-interface counters are collected on devices that only
  permit v3. Previously v3 nodes were limited to scalar metrics.

## v0.1.5

**Distributed tracing (OpenTelemetry)** — Yagra can now export OpenTelemetry traces so a single
poll is traceable end to end across the central core and its pollers. It is opt-in and off by
default (structured logs and Prometheus metrics are unchanged), so there is zero overhead until you
point it at a collector.

### New Features
- **OpenTelemetry distributed tracing (self-observability)** — set `YAGRA_OTEL_ENDPOINT` (or the
  standard `OTEL_EXPORTER_OTLP_ENDPOINT`) to an OTLP/HTTP collector and both core and poller export
  spans that stitch one poll into a single trace — core's dispatch → the poller's poll → core's
  result ingest — plus a span per northbound API request. The trace context rides with jobs and
  results over the bus, so a trace spans distributed pollers and stays compatible with older pollers
  during a rolling upgrade. For large fleets, sample with `OTEL_TRACES_SAMPLER=parentbased_traceidratio`
  (+ `OTEL_TRACES_SAMPLER_ARG`) instead of tracing every poll. `docker compose --profile tracing up`
  starts a bundled Jaeger to view traces locally; leave the endpoint unset and Yagra logs exactly as
  before with no tracing overhead. See **DEPLOYMENT.md ▸ Distributed tracing (OpenTelemetry)**.

### Security
- **Patched a transitive advisory** — updated `crossbeam-epoch` to 0.9.20 to clear RUSTSEC-2026-0204
  (an invalid pointer dereference reachable only by debug-formatting a null pointer inside a metrics
  dependency; not exercised by Yagra, patched proactively).

## v0.1.4

**Dependency management** — the dependency graph that drives alert suppression is now editable, not
just viewable. Set a node's upstream from the WebUI, and dependency suppression now reacts to
parent-down events so a child that failed first is rolled up under its parent instead of paging on
its own.

### New Features
- **Manage node dependencies from the WebUI** — a node's upstream (the edge that drives parent-down
  alert suppression and root-cause roll-up) can now be set, changed, or cleared after the node is
  created. The node-detail header gains a **Dependency…** action, and a new **Topology ▸ Dependency
  view** page lists every node with its upstream, live status, and current root-cause attribution,
  and lets you edit each edge inline. Self-dependencies and cycles are rejected. The Network map
  already visualized these edges; now you can define them.

### Bug Fixes
- **Dependency suppression no longer misses out-of-order outages** — root-cause attribution is now
  event-driven. Previously a child's alert was attributed to its parent only at the moment the child
  first went down, so a child that failed *before* its parent was never rolled up and kept paging on
  its own. Now, when a parent goes down, already-active downstream alerts are re-evaluated and rolled
  up under the parent's incident (their standalone page is closed); symmetrically, a child left
  suppressed after its parent recovers while still down re-pages on its own.

## v0.1.3

**Distributed poller pools** — Yagra can now spread polling across pollers placed close to the
monitored devices. Each node carries a **pool** attribute (a site or region); the central server
tracks poller liveness, assigns each pool's nodes across its live pollers with consistent hashing,
and pushes each poller its working set over the bus. A remote poller dials out to the central NATS
bus (NAT/firewall-friendly), so branch-site segments can be monitored without inbound access. This
release also brings the passive event monitoring, topology map, and per-node event features.

### New Features
- **Distributed poller pools (location-affinity assignment + failover)** — deploy pollers at remote
  sites and assign nodes to them by a **pool** attribute (defaults to one pool, `default`). The
  central coordinator watches poller heartbeats, distributes each pool's work across its live
  pollers using a consistent-hash ring (so adding or losing a poller reshuffles the minimum), and
  fails a departed poller's nodes over to the survivors in the same pool automatically. Pollers
  receive their assignment as a working-set snapshot and then incremental deltas, and reconnect with
  a full resync — no polling gaps across restarts. A new **Settings ▸ Pollers** page lists every
  poller with its pool, status, version, working-set size, and last heartbeat, warns when a pool has
  nodes but no live poller, and includes a **Register poller** dialog that generates the remote
  poller's configuration. A pool with no live poller falls back to the existing central polling, so
  upgrades are seamless. Ships with a `docker-compose.poller.yml` for remote sites and a NATS
  TLS + authentication configuration for exposing the bus across trust boundaries.
- **Passive event monitoring (syslog / SNMP traps / webhooks)** — besides active polling, Yagra now
  receives syslog (RFC 5424/3164) and SNMP v1/v2c traps and accepts inbound webhooks, matches them
  against operator-defined rules (substring / regex) to raise alerts, and can forward the
  fire/resolve lifecycle to PagerDuty (Events API v2) and Jira Service Management (Alerts API).
- **Topology Network map** — a new visualization of the dependency graph, so you can see how nodes
  relate and how a parent outage cascades to its children.
- **Per-node event view** — the node-detail page gains an **Events** tab, and the Events page can be
  filtered to a single node (with a node picker and a `?node_id=` deep link). The event log fetching,
  paging, and columns are shared between the two views.
- **Free-text search on Alerts ▸ Events** — filter the event log by an arbitrary search string.

## v0.1.2

Cisco Meraki monitoring over the **read-only** Dashboard API. Meraki devices appear as ordinary
nodes in the same HostTree as your SNMP/ICMP hosts, but are collected per **organization** (one
paged, org-wide API call covers many devices) so a large Meraki estate never trips the org's API
rate limit — and the integration can only ever *read* from Meraki.

### New Features
- **Cisco Meraki (Dashboard API) monitoring** — add a Meraki organization under **Settings ▸
  Integrations ▸ Cisco Meraki** with a read-only API key (one key can onboard several organizations
  at once). An import wizard enumerates the org's networks and devices and lets you choose which to
  monitor; imported devices become normal nodes, auto-placed under an **Organization → Network**
  group tree, and carry a **Meraki** badge in the inventory tree. Collected metrics — device
  availability, WAN uplink loss / latency / status, client count, and traffic usage — surface on a
  new Cisco Meraki card on the node-detail page. Per-organization controls let you pause/resume
  collection, tune per-tier polling cadence and the request-rate budget, and edit which networks are
  in scope.

### Security
- **Read-only by design, with layered safeguards** — the integration issues HTTP **GET only** to
  Meraki (no configuration is ever written), and every request is restricted to allow-listed Meraki
  API hosts so the API key cannot be sent elsewhere. The key is **encrypted at rest** (never
  returned by the API or written to logs), polling is paced well under the per-organization API cap
  to leave headroom for your own tooling, and a global **Meraki polling kill switch** on the
  Integrations page can halt all Meraki collection instantly.

## v0.1.1

A small follow-up to the first release: a dedicated About page, a self-health page split out of
Pollers, and a genuinely rootless WebUI container.

### New Features
- **Settings ▸ About** — a new page that gathers product identity and running versions in one place:
  the live Core / API version (via a new public `GET /api/v1/version` endpoint), the WebUI build
  version, the repository link, and the license. Showing both versions side by side makes a
  core/web skew during a rolling upgrade obvious at a glance.

### Improvements
- **System Health is now its own page** — Yagra's self-health (poll-loop counters, backing-service
  reachability, data coverage) moved out of Settings ▸ Pollers into a dedicated Settings ▸ System
  Health page. The Pollers entry is reserved as a placeholder for future distributed-poller
  configuration.

### Security
- **The WebUI container now runs fully rootless** — the web image is built on `nginx-unprivileged`
  and serves as a non-root user, so it listens on container port **8080** instead of 80. The
  bundled Docker Compose files already map this, so standard deployments are unaffected (the WebUI
  stays on host port 3000 by default); update any custom reverse proxy or orchestration that
  targeted the web container's port 80.

## v0.1.0

First release of Yagra — a network monitoring system (NMS) that watches network devices and
servers via ICMP / SNMP / API calls, built from the start for tens of thousands of nodes and
distributed polling. Ships as Docker containers with a Rust backend and a React WebUI.

### New Features
- **Monitoring & polling** — ICMP liveness, SNMP v2c and v3 (USM auth/priv) scalar and table
  collection (including multi-index tables), and URL / HTTP(S) endpoint monitoring with TLS
  certificate-expiry tracking. Raw counters are stored and rates derived at query time (wrap/reset
  safe). Configurable global and per-Device-Profile poll intervals, with interval jitter and
  per-device rate limiting / backpressure.
- **Discovery & classification** — network discovery over IP ranges and lists with a
  stored-credential picker; automatic device classification by sysObjectID / sysDescr that applies
  the matching Device Profile; a Credential Finder that tries stored credentials by reference
  (rate-limited per device, attempted values never logged).
- **Device profiles & collection** — a role × network-OS profile taxonomy with editable Collection
  Templates, Classification Rules, and a curated MIB/OID catalog, all searchable. Built-in coverage
  for common vendors (Cisco, Huawei — including USG firewall sessions and RAM, Meraki MX/MS, A10)
  plus standard host and interface MIBs.
- **Alerting** — an explicit state machine with dwell-time hysteresis, flapping detection,
  dependency suppression with root-cause roll-up (parent down ⇒ children suppressed), maintenance
  windows, mutes, and dedup / grouping. Append-only alert history shows human-readable node/check
  names and a "What" (metric + condition) column, with read-only acknowledgement reflected from
  external tools such as PagerDuty / Jira Service Management (ADR-015). Email and webhook
  notification channels with retry.
- **Dashboards & visualization** — an admin-customizable Shared Dashboard and a per-user,
  multi-board My Dashboard built from a widget library: fleet health timeline, Top-N CPU / memory /
  interfaces, traffic deltas, aggregate throughput, utilization heatmap, geo map, dependency /
  root-cause topology, data-coverage gauge, alert-history aggregations, and poller / collection
  health. A node tree with split-view node detail, per-interface sparklines, and time-series charts
  with a shared range control (1h–7d + custom) and configured-bandwidth reference lines.
- **Reports** — customizable report sections rendered to HTML / CSV / PDF, with saved definitions
  and scheduled runs (Dashboard ▸ Reports).
- **Troubleshooting** — an analysis-tools catalog running asynchronous analysis jobs over
  VictoriaMetrics (e.g. anomaly detection), scoped to a single node or a group (recursively).
- **Administration** — role-based access control (viewer / operator / admin) with group-scoped
  visibility, user management, an immutable audit log of every configuration change and login, and
  Yagra self-health monitoring (Settings ▸ Pollers and `GET /api/v1/system-health`).
- **WebUI** — a React + TypeScript single-page app over one typed REST client, with live updates
  via SSE (bearer-authenticated), light / dark theming, and virtualized, server-paged lists built
  for tens of thousands of nodes.
- **Deployment** — a single-node Docker Compose stack (Yagra-core, Yagra-poller, Yagra-web,
  PostgreSQL, Redis, NATS, VictoriaMetrics) with non-root images and `CAP_NET_RAW` granted only to
  the poller for raw-socket ICMP. Expand-contract database migrations keep upgrades data-preserving.

### Security
- Monitoring credentials (SNMP communities, SNMPv3 USM credentials, API tokens) are encrypted at
  rest with envelope encryption — AES-256-GCM data keys wrapped by a key-encryption key loaded from
  a mounted secret file. Secrets are never logged, returned in API responses, or used as metric /
  trace labels.
- Every state-changing API endpoint enforces authentication and RBAC and is recorded in the audit
  log.
- Outbound HTTP from URL monitors and notification webhooks is guarded against SSRF: loopback,
  link-local, and cloud-metadata targets are refused — including IPv4-mapped IPv6 forms and across
  HTTP redirects — while legitimate private / internal ranges stay monitorable.
- TLS certificate verification is on by default for URL monitors; disabling it is an explicit,
  per-monitor opt-in.
