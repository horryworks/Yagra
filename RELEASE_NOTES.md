# Release Notes

<!-- ## Unreleased is where a shipped behaviour change waits for a version.
     Yagra deploys to the test server on every push to main, so changes go live long before a
     release is tagged. Add a bullet here in the SAME commit that ships the change — anything an
     operator or an API client could notice: response shapes, status codes, defaults, the meaning of
     a query, removed behaviour. At release time `/docs` folds this section into the new `## v<x>`
     heading and leaves an empty one behind. An empty Unreleased is the normal resting state; a
     missing one means someone deleted the mechanism. -->

## Unreleased

### Bug Fixes

- **Optical: a port with no transceiver no longer charts a reading.** Huawei reports `-1` on every
  column of an empty port and the poller scaled that to a flat **-0.01 dBm** line; the vendor-neutral
  dialect (Cisco, Arista) reports `0`, which was stored as **0.00 dBm** — 1 mW, stronger than any
  working port on the same switch, so a dark port read as the healthiest one. Both are now recognised
  as "no module" and dropped, leaving a gap. **Existing series stop rather than change**: the false
  points already written stay in the TSDB until they age out of retention.
- **A device's later checks are no longer dropped every cycle.** A node's checks start about a second
  apart, but a table walk against a switch with many interfaces takes 1–6 seconds, so each check after
  the first arrived while the previous one was still running and was discarded — every cycle, for the
  same check. On the test deployment 37% of all polls were being dropped, and the optical check
  recorded nothing for an hour while the check ahead of it recorded every sample. A check now waits
  for the device to free up (bounded by its own interval, capped at 60s) instead of being thrown away.
  A device that stays busy past that deadline still sheds the poll, so backpressure is unchanged.

## v0.2.14 — a sweep can be left, returned to and stopped, and a dashboard card plots the links you name

### Breaking changes
- **A discovery sweep no longer tries SNMP on an address that does not answer ping.**
  `POST /api/v1/discovery/scan` takes a new `snmp_when_unreachable`, and **omitting it means no** —
  so a caller sending the body it sent before will now miss a device that filters ICMP and answers
  SNMP. Send `"snmp_when_unreachable": true` for the previous behaviour. In the WebUI it is a
  checkbox on the scan form, off by default.

### New Features
- **Every dashboard card can be given a name of your own.** Customize the board and the card's
  heading gains a text box: type "HQ uplink" and the card reads *Interface traffic · HQ uplink*.
  The widget type stays put beside it, so a card still says what kind of thing it is — which is the
  word it shares with the widget catalogue. Clearing the box removes the name again.
  - It applies to all forty-eight widgets. None of them limits how many copies you may place, so
    two cards of the same type on one board were previously indistinguishable — most visibly for
    the ones whose contents you choose (Metric chart, Top nodes by metric, Latest events,
    Interface traffic).
  - The remove, drag and resize controls announce the full heading, so a screen reader can tell six
    cards of one type apart.
- **A dashboard widget for the specific links you watch.** Capacity ▸ *Interface traffic* plots up
  to six interfaces you name, and they may live on different nodes — so an uplink and its backup
  can be compared on one chart instead of in two browser tabs. Receive is drawn above the zero line
  and transmit below it, which gives each link a single colour and makes an unbalanced link visible
  at a glance.
  - The unit switches between bits/sec and unicast packets/sec, and the window between 1h, 6h, 24h
    and 7d. Both are per widget, so a board can carry a bits view and a packets view side by side.
  - Six is the maximum, because the chart has six colours and a seventh line would reuse one.
  - The dashboard's other interface widgets rank whatever is busiest right now; this one plots what
    *you* chose and keeps plotting it.
  - Packets/sec only has history from v0.2.11 onward, when those counters were first collected, so
    a 7d packets window on an older deployment is mostly empty. That is expected, not a fault.
- **A discovery sweep survives leaving the page.** Nodes ▸ Discovery lists the sweeps the core is
  holding and reattaches to one when you come back, so navigating away no longer loses a sweep the
  poller is still running. The scan id is in the URL, so a reload or a shared link lands on the same
  sweep.
  - **API:** `GET /api/v1/discovery/scans` lists the retained sweeps (ManageConfig). Also reachable
    over MCP as `get_config(kind="discovery_scans")`.
  - **API:** a scan's status carries three new fields — `state` (`queued` / `running` /
    `cancelling` / `cancelled` / `done`), `started_at` / `updated_at` (RFC 3339), and `pool`, the
    route the job was actually published on, which is not necessarily the pool that was requested.
    `done` is unchanged and still means "terminal".
- **You can choose which site a sweep runs from.** The scan form grew a poller-pool picker. Without
  it the sweep went to whichever poller answered first, so on a multi-site deployment a remote
  poller could end up sweeping head office — reaching nothing and reporting a successful, empty
  scan. Leaving it unset keeps the old behaviour, and the screen now says what that means. A pool
  with no live poller is still offered but marked, because the server falls back to "any poller"
  for it.
  - **API:** `POST /api/v1/discovery/scan` accepts `pool`, and now answers `503` on a standby core
    in an HA pair. Only the leader consumes discovery results, so a standby would have published a
    sweep whose every result was discarded.
- **A running sweep can be stopped.** The poller stops probing, so the ICMP and SNMP traffic
  actually ceases rather than the screen merely looking stopped. Devices found before the stop stay
  on screen and can still be imported.
  - The stop takes a few seconds: a probe already in flight is left to time out rather than having
    its connection dropped.
  - **The screen distinguishes "asked" from "stopped".** Yagra broadcasts the stop and cannot know
    which poller was running the sweep, so the sweep stays *Stopping…* until the poller confirms.
    A sweep that finishes first is reported as finished, not as stopped.
  - **A poller older than this release does not understand the command** and runs the sweep to
    completion. When one of those could be holding the sweep, the screen says so as you press stop
    rather than leaving you waiting.
  - **API:** `POST /api/v1/discovery/scan/{id}/cancel` asks the poller running a sweep to stop
    (ManageConfig). **200 means the stop was published, not that the sweep stopped** — watch the
    scan's `state` for that: `cancelled` is a confirmed stop, `done` means it finished first.
    `poller_supports_cancel` in the response is an advance warning about older pollers, not a
    verdict; for a sweep on the global route it is the answer across every live poller. Unlike
    `POST /analysis/jobs/{id}/cancel`, this answers 200 for a scan the core has no record of — scan
    state is in memory, so requiring one would make a sweep orphaned by a core restart impossible
    to stop.

### Improvements
- **Sweeping a subnet is much faster.** Every address in the range used to be asked for its identity
  with every candidate credential, spaced two seconds apart to protect devices from lockout — so on
  the test network a /24 carrying eight devices took **5m21s**, almost all of it spent on the 246
  addresses with nothing at them. Those are now skipped at the ping.
  - ⚠️ The trade is under *Breaking changes* above. Note also that discovery sends a **single** echo
    request, so one lost packet is indistinguishable from an empty address — unlike liveness
    monitoring, which waits for three consecutive failures before believing a node is down.
- **A dashboard widget's subject can now only be changed while the board is being customized.**
  Choosing what a card is *about* — which node and metric a Metric chart draws, the metric a Top
  nodes by metric ranks, the interfaces an Interface traffic chart plots — moved out of the card
  header and behind a ⚙ that appears on that card in Customize mode. What stays in the header
  outside Customize is how the chosen subject is shown: the now/1h-peak window, the bps↔pps toggle,
  the interface chart's time window, the event feed's kind filter, the top-AS direction, and the
  "View all" links.
  - This matters most on the **Shared dashboard**, the one board everyone sees. Editing it requires
    the *Manage configuration* privilege and a confirmation that the change applies to every user —
    but the header controls sat outside that gate, so an interface could be added to everyone's
    chart without pressing Customize and without the confirmation ever appearing.
  - **Saved boards are unchanged**: no stored setting changed shape or meaning, and nothing needs
    migrating. A card whose subject is not yet chosen now says where to choose it.
  - Widgets with nothing to configure — 45 of the 48 — show no ⚙ at all.
- **A sweep that no poller has picked up now says so, instead of claiming to be running.** A
  discovery scan starts in a new state, *Waiting for a poller*, and only becomes *Running* once a
  poller reports. This matters most when a scan is sent to a poller pool with nothing alive in it:
  such a sweep used to read `Running · 0/254` for the life of the core process, indistinguishable
  from one that was running and finding nothing. A queued sweep can be stopped — that is the
  cheapest stop there is, since it has not probed anything yet — and one nobody ever picks up is
  retired after two hours rather than listed forever.
  - ⚠️ **API clients**: `state` on `GET /api/v1/discovery/scan/{id}` and `/discovery/scans` can now
    return `queued`. It is an addition; no existing value changed meaning.
  - A poller running an older release does not send the new "I have it" message, so a sweep it is
    genuinely running reads as queued until its first batch of 32 addresses completes.
- **A discovery sweep no longer reports an empty subnet when the poller cannot send ICMP at all.**
  With the ping gate on, a probe that *failed* (no raw socket, no route, an address family the
  poller cannot use) was treated exactly like an address that stayed silent — so a poller with
  broken ICMP swept a whole range without sending one SNMP packet and finished successfully with no
  devices and no error. A failed probe now bypasses the gate, and the sweep logs one line naming how
  many targets it could not reach that way.
- **A sweep the core no longer knows about is now reported as such.** Previously the page kept
  polling a 404 every two seconds with its progress line frozen mid-sentence. It now says the core
  does not know that sweep — which is the honest reading, since a poller may still be running it —
  and stops asking after repeated failures.
- **Finished sweeps are now discarded after six hours (at most twenty are kept).** They were held
  for the life of the core process. The window is applied whenever either surface is read, so old
  scans age out on a deployment where nobody sweeps again — previously it was only applied when a
  new sweep was started, and they stayed in *Recent sweeps* indefinitely. Note the side effect: the
  dashboard's *Discovery queue* widget reads those same sweeps, so it now shows recent finds rather
  than everything since the last restart.

### Bug Fixes
- **A dashboard widget at the bottom of a board can be made taller again.** The corner grip
  resizes by being dragged downwards, and the board ended flush with its last card — so once
  scrolled to the end there was nowhere left to drag: the grip sat two pixels above the edge of the
  scrolling area with a whole row of travel needed to reach the next size, and a pointer cannot
  leave the window. Customize mode now keeps a row of empty board below the last card, and holding
  the pointer at the top or bottom edge during a resize scrolls the board under it. (The keyboard
  route was never affected: focus the grip and press ↑ / ↓.)
- **A node picker low on the screen no longer opens its list off the bottom of the window.** It
  dropped downwards unconditionally, so near the foot of a page it showed a row and a half of an
  otherwise 240px list — and neither surface can be scrolled to, because the list is positioned
  against its trigger rather than the document. It now opens upwards when it does not fit below and
  there is more room above, the same rule the app's shared popover already applied to itself. Most
  visible on a dashboard widget placed at the bottom of a board, but it affects every picker: node
  search on Events, Pollers, Support bundle, and the Add node / Set parent / Add mute / Add
  maintenance dialogs.
- **The Discovery progress line no longer describes the wrong sweep.** Choosing another sweep from
  *Recent sweeps* left the previous one's "Scan complete: 8 devices" on screen, along with any error
  or import message, because the two places that reset the page had drifted apart.
- **Discovery progress can no longer appear to go backwards.** The page asked for the scan's status
  every two seconds regardless of whether the previous request had come back, so on a slow
  connection replies could arrive out of order and an older one would win. It now waits for each
  reply before scheduling the next.

## v0.2.13 — a remote site's poller is stood up from the WebUI, and a poller id is no longer taken on trust

### New Features
- **Standing up a poller at a remote site is now done from the WebUI.** Settings ▸ Pollers grew a
  **Remote pollers** panel and a **Token** column. Previously this meant generating a certificate
  with `openssl`, hand-editing two blocks of `docker-compose.deploy.yml`, and handing every site the
  same password. None of that is needed any more.
  - **The bus certificate is generated for you and kept in PostgreSQL**, the same way the WebUI's
    own certificate is (the private key envelope-encrypted, the certificate itself plaintext because
    it is public). The panel shows what it covers, when it expires and its fingerprint, and reissues
    it with the addresses your sites will dial. There is no import — a bus certificate only has to
    be trusted by pollers Yagra also configures.
  - **"Accept remote pollers" is a switch.** It reissues the certificate for the address you give
    it, turns on TLS and authentication, publishes the bus port, and moves the co-located core and
    poller to `tls://` in the same change. The bus, core and that poller are recreated, so monitoring
    stops for roughly a minute — a fleet-wide maintenance window is opened first so nothing pages.
  - **Each poller can have a bus token of its own**, and issuing one downloads a single archive
    holding everything the site needs: its `.env`, the certificate to pin, `docker-compose.poller.yml`
    taken from this core's image, and a README. The site's whole procedure is unpack and
    `docker compose up -d`. The token is stored only as a SHA-256 digest, so it is shown once and
    cannot be recovered — issue a new one if the archive is lost.

### Bug Fixes
- **A fresh install could stop partway through, with no error message.** The published instructions
  fetched `docker-compose.deploy.yml` from `main`, while the images that composition pulls are
  `:latest` — the latest *stable* release. The moment a composition on `main` required something only
  an unreleased image provides, a new deployment stalled: the one-shot `bus-cert-init` sat at
  `Up (healthy)` instead of exiting, and `nats`, `core`, `poller` and `web` never left `Created`.
  **The composition is now attached to each release**, and the instructions fetch it from
  `https://github.com/horryworks/Yagra/releases/latest/download/docker-compose.deploy.yml`, which
  resolves to the same release the `:latest` images come from. Existing deployments are unaffected —
  an in-place upgrade already installs the composition carried inside the image it is installing.
- **The compose edits that enabled the TLS bus were erased by every upgrade, and only remote sites
  noticed.** Settings ▸ Upgrade reinstalls `docker-compose.deploy.yml` from the target image, so the
  bus reverted to plaintext and core to `nats://` while the central stack kept working perfectly —
  the quietest possible failure. The switch is now expressed as `.env` variables, which upgrades do
  preserve, and the `nats` service reads them without any compose edit.
- **`docker/nats/nats-server.conf` was not inside the published images**, so a deployment installed
  without a repository checkout had nothing at the path the procedure told it to bind-mount. Docker
  creates an empty *directory* there, nats was handed a directory as its `-c` argument, and the bus
  failed to start — taking core with it. The file now ships in the core image and is placed on the
  bus volume automatically.
- **Core ignored `YAGRA_BUS_CA_FILE`.** The documented procedure told you to set it on core, and
  core connected to NATS with no root certificate regardless — so turning the bus TLS would have
  left core unable to reach its own bus. It is now read, with the same empty-means-unset rule the
  poller applies.

### Security
- **A poller id that is not in the inventory is now refused, whatever secret it presents.** The
  connection's poller id was self-asserted and checked against nothing, so a `.env` leaked at one
  site let the holder claim *any* id — and the working set core then sent it carries the plaintext
  SNMP communities, SNMPv3 credentials and API tokens of whatever nodes that id is assigned. This is
  closed without issuing any tokens at all.
- **A poller that has its own token can no longer be admitted by the deployment-wide bootstrap
  secret**, so issuing tokens narrows the blast radius one site at a time. A poller with no token
  still uses the shared secret, which is what keeps an existing fleet connected across this upgrade;
  the Token column on Settings ▸ Pollers shows which pollers are still in that state.
- **Removing a poller now sticks** on a deployment using NATS Auth Callout. NATS does not
  re-authenticate an established connection, so a deleted poller's live heartbeat used to recreate
  its inventory row within ten seconds — refusing its next connection was not enough on its own.

## v0.2.12 — the support bundle reaches the poller that did the polling, wherever that poller runs

### New Features
- **A support bundle can now be taken *about one node*.** Settings ▸ Support bundle gains an optional
  node picker beside the log window, and `GET /api/v1/system/support-bundle` accepts `node_id`.
  Naming a node adds a `node/` section: its inventory row and owning poller, its **stored interface
  rows verbatim** (names, speed, duplex, media, transceiver, optical bounds), the metrics it is
  configured to collect, which of those are actually arriving, and its alerts. Omitting the
  parameter changes nothing about the rest of the bundle.
  - The pair worth knowing about is `node/collection.json` and `node/metrics.json`. One says what
    the node is configured to collect, the other says which of those are arriving — together they
    separate "not configured" from "configured but no data", which neither answers alone.
  - This is the section to include when reporting an interface problem: it says what Yagra has
    *stored* for that port, which is the half of the question a maintainer cannot otherwise see.
- **A co-located poller's log files are carried in the support bundle.** Previously only core's own
  log was — but SNMP is walked by the poller, so the collecting side left no trace. The poller now
  writes rotated hourly logs into a `pollers/` subdirectory of core's log volume, each file named
  with the poller's id, and the bundle carries them under `logs/`.
  - **This reaches only a poller that shares a host with core** (the single-node composition). A
    remote-site poller writes at its own site and the bundle says so by name rather than leaving an
    absence. It *does* reach a poller that has since died, which is the case a live request never
    can.
  - The poller log budget is separate from core's, so this cannot displace core's own log.
- **A remote-site poller's log now reaches the bundle too, over the bus.** Core asks every live
  poller that advertises the capability for a window of its own log and carries the answers under
  `logs/remote/`. This is the half a shared volume structurally cannot reach: a poller at a
  monitored site has its own disk.
  - **The poller scans its own log before sending, and refuses the whole reply on a match.** It has
    to be that way round: core's redaction scan is built from the secrets core can see, which never
    include a monitored device's SNMP community — that value is decrypted by core and then lives in
    the poller. The refusal names the rule and never the value, and reaches the bundle as an
    explanation rather than an absence.
  - **Every site that was asked and sent nothing is listed by name**, with why — unreachable, out of
    order, refused, or "answered, and had nothing in that window". "No file from Tokyo" and "no
    poller in Tokyo" are indistinguishable in an archive unless someone writes the difference down.
  - It cannot fail a bundle. A site that is gone costs a 20-second wait once and then an omission.

### Improvements
- **The support bundle is now its own screen: Settings ▸ Support bundle.** It used to be the last
  card on Settings ▸ Yagra health, which is a read-only self-monitoring page anyone with View may
  open — while taking a bundle is an Admin-only export of the deployment's state. The controls,
  the log window, the node picker and the API are all unchanged; only where you find it moved. Yagra
  health now shows exactly the four things its own description names (poll loop, dependencies, data
  coverage, host resources).
  - A signed-in user without the privilege now sees the screen and is told which privilege it needs,
    rather than the card simply not being drawn.
- `MANIFEST.json`'s omission list is more specific about what a bundle cannot answer — including
  that an empty duplex or media column is consistent with both "the device does not implement that
  MIB" and "the rows did not map to an ifIndex", and that telling them apart needs an `snmpwalk`
  from the poller's own host with the credential this deployment uses.

### Upgrade notes
- **Existing deployments need a compose change to get the poller logs.** `docker-compose.yml` and
  `docker-compose.deploy.yml` now mount the shared `logdata` volume into the poller and set
  `YAGRA_LOG_DIR=/var/log/yagra/pollers` (override with `YAGRA_POLLER_LOG_DIR`). Without it the
  poller logs to stdout only, exactly as before, and the bundle records that no co-located poller
  log directory was found.
  - Both compositions also gain a **one-shot `log-init` container** that makes the shared volume
    writable by core and the poller alike — they run as different uids, and an image's ownership
    only gets a vote the first time Docker seeds an *empty* named volume, so an existing deployment
    could not have fixed this by pulling a new image. Expect it at `Exited (0)` beside `kek-init`
    after startup; that is success.
- **A remote-site poller must have `YAGRA_LOG_DIR` set to take part in the bus path**, and be
  upgraded to a build that understands the request. `docker-compose.poller.yml` already sets it.
  A poller without it does not advertise `log-ship`, core does not ask, and the bundle names the
  site as unrepresented rather than waiting on it.
- **Exposing the bus to remote sites needs two new subject grants.** `docker/nats/nats-server.conf`
  gains `yagra.poller.logs.>` (subscribe) and `yagra.poller.logreply` (publish) on the static
  `poller` account; the Auth Callout path grants the same pair scoped to each poller's own id.
  Replace the file if you deployed a copy of it — a missing grant is denied silently by the broker,
  so the only symptom is that remote sites are always recorded as not having answered.

## v0.2.11 — an interface says what it negotiated, what it is made of, and how much light it sees

> [!IMPORTANT]
> **Most of the interface work in this release has not been exercised against real hardware.**
> Duplex, media type, and the optical readings and their power window are all read from MIBs no
> device in this project's lab implements: the one SNMP device available answers neither
> EtherLike-MIB nor MAU-MIB, and has no transceiver in it. Every one of those paths is covered by
> unit tests and by the browser walk against mocked data, and every parser **drops what it cannot
> recognise rather than guessing** — so the expected failure is an empty column or a missing chart,
> not a wrong number. That is a design intent, though, not a measurement.
>
> **If something looks wrong on your equipment, please open an issue** at
> https://github.com/horryworks/Yagra/issues — a blank column on a device that should answer, a
> reading that cannot be right, a chart that never appears. The vendor and model, plus an `snmpwalk`
> of the table concerned, is what makes it fixable.

### New Features
- **The Interfaces tab shows each port's speed and duplex.** A node's **Interfaces** tab gains
  **Speed** and **Duplex** columns (and a **Media** column — see below), so a port
  negotiated below its rate is visible from the list instead of requiring a device login. Both are
  filterable from the column filter row, which is the point: "show me the 100 Mbps ports" is how the
  mismatch gets found.
  - **Speed needed no new collection** — Yagra has always stored each interface's nominal rate and
    used it only to draw the bandwidth line on the throughput chart. It is now also a value you can
    read and filter on.
  - **Duplex is new, and it is free**: `dot3StatsDuplexStatus` (EtherLike-MIB) is indexed by
    ifIndex, so it rides the interface walk the poller already performs. **No extra SNMP session,
    no change to what core sends a poller** — an older poller keeps working unchanged and simply
    reports no duplex.
  - ⚠️ **A blank duplex on a fibre port is correct, not a fault.** IEEE 802.3 defines no half duplex
    above 1 Gbit/s, so there is nothing to negotiate and agents report "unknown". The column is a
    copper diagnostic — one end forced to full against an auto-negotiating peer is a classic cause
    of a link that works but is slow. The filter's hint says so on screen.
  - **A device that does not implement EtherLike-MIB shows a blank column** and is otherwise
    unaffected. The same blank covers a port that is down.
  - **`GET /api/v1/nodes/{id}/interfaces` gains `if_duplex`** (`"half"` / `"full"` / null) **and
    `if_type`** (the IANAifType integer, 6 = ethernetCsmacd). `if_type` is what distinguishes
    "duplex does not apply here" — a loopback, a tunnel, a dialer — from "we could not read it";
    both are also on the MCP `get_node_status` tool's interface entries.
  - **The Media column shows the port's physical medium** — 1000BASE-T for copper, 1000BASE-SX or
    -LX for fibre — read from the device's MAU table (`ifMauTable`) once an hour, since a medium
    changes only when someone swaps a module. Where that table says nothing, Yagra falls back to the
    transceiver's own part number from ENTITY-MIB, which covers pluggables but not fixed copper
    ports. Filtering the column also searches the part number, so a search for `SX` still finds a
    module whose medium could not be resolved.
    - ⚠️ **Many devices do not implement MAU-MIB, and then the column stays empty.** That is a gap
      in what the device will tell you, not a fault — the same shape as the duplex column above.
    - **A component whose only description is its own port name is not treated as a transceiver.**
      ENTITY-MIB describes every component, and the one a port resolves to is often the port itself,
      so a device that answers with the port name would otherwise be reported as its own module.
    - **`GET /api/v1/nodes/{id}/interfaces` gains `if_media` and `transceiver_model`.** They are
      deliberately separate: a part number is not a media type, and `if_media` is filled from one
      only when it demonstrably contains a standard designation. Both appear on the MCP
      `get_node_status` tool's interface entries.
    - Media designations are recognised from the IEEE/IANA registry up to 100GBASE-ER4. A device
      reporting something outside that range shows an empty cell rather than a guess.
    - **Settings ▸ System settings ▸ Discovery walks gains a fifth walk, on by default**, to turn
      the collection off or change its interval.
  - **On a phone the three columns are not drawn** — speed and duplex appear in the chart dock
    instead, which is where a narrow screen has room for them.

- **Optical transceivers report their transmit and receive light levels.** A node's **Interfaces**
  tab gains a third chart, **Optical power (Rx / Tx, dBm)**, for any port that has a transceiver in
  it, plus **Rx** and **Tx** figures in the dock header. Fibre degrades gradually — a receive level
  drifting from -7 dBm toward -18 dBm is a link on its way out — and until now Yagra could not show
  that at all.
  - **The chart appears only for ports that report light.** There is no setting and no "is optical"
    flag: a copper port, a virtual interface, or a device whose transceiver MIB Yagra does not speak
    simply keeps the two charts it has always had.
  - **Four vendor dialects are read**, each attached to the matching built-in device profiles:
    ENTITY-SENSOR-MIB (Cisco, Arista and other standards-based agents), Huawei
    `hwOpticalModuleInfoTable`, JUNIPER-DOM-MIB, and HH3C-TRANSCEIVER-INFO-MIB (H3C and the HPE
    Comware switches OEMed from it). Every reading is normalised to dBm before storage, so
    `if_rx_power_dbm` and `if_tx_power_dbm` mean the same thing on every vendor.
  - **Most vendors report optical power against a physical-entity index, not an interface index.**
    Yagra resolves that to a real ifIndex through ENTITY-MIB's alias mapping; a row that cannot be
    attached to an interface is discarded rather than stored under an index no chart can use.
  - Readings outside a transceiver's physically possible range are discarded, so a vendor whose
    scaling differs from its own MIB produces a gap rather than a plausible wrong number. A
    multi-lane transceiver (QSFP) reports its first lane; per-lane series are not offered.
  - **`GET /api/v1/nodes/{id}/interfaces/{ifindex}/series` gains `rx_power_dbm` and
    `tx_power_dbm`**, on the same shared timestamp axis as the existing eight arrays and likewise
    exposed through the MCP `get_interface_series` tool. Both are **gauges read as reported**, not
    counter rates, and are **normally negative** — 0 dBm is one milliwatt, not "no signal". They are
    entirely `null` for a port with no transceiver, which is how a client tells an optical
    interface from any other.
  - **An upgraded deployment has no optical history before the upgrade**, so the chart is empty for
    older windows and fills in from the first poll onward.
  - **Not included:** module temperature, voltage and laser bias current, and threshold alerts on
    optical power. Alerting on a per-interface metric is separate work — an alert is currently
    identified by node and metric name with no room for an interface, so a threshold would be shared
    by every port on the device and could not say which one crossed it.
- **The optical chart shows the transceiver's own acceptable power window.** A dBm figure on its own
  cannot be judged — -7 dBm is comfortable on one module and failing on another — so the module's
  published limits are now read alongside the readings and drawn as a shaded lane behind each line,
  tinted to match it, with the numbers written out beside the current value (`-24.0 dBm … -3.0 dBm`).
  Nothing alerts on them: these are the module's figures, not a threshold anyone configured, and
  Yagra only shows them.
  - Read for **Huawei** (`hwOpticalModuleInfo` thresholds) and **Juniper** (JUNIPER-DOM-MIB alarm
    thresholds). **Not available for the standards-based dialect**: ENTITY-SENSOR-MIB (RFC 3433)
    defines no threshold objects at all, so a Cisco or Arista port shows its lines without a lane.
    H3C is also without one for now. Those ports are unaffected otherwise.
  - **Limits the module reports implausibly are discarded** — a low bound above the high one, or
    either end outside a transceiver's physical range, which has been observed in the field. A wrong
    window would accuse a healthy link, so the lane is simply not drawn.
  - **`GET /api/v1/nodes/{id}/interfaces` gains `rx_power_low_dbm`, `rx_power_high_dbm`,
    `tx_power_low_dbm` and `tx_power_high_dbm`**, null for every interface without them.
  - The chart's Y axis widens to contain the window, so a link with a lot of margin shows a flatter
    line — that flatness *is* the margin. Use a shorter range to read the trend on its own.
- **The interface Throughput chart switches between bits/sec and packets/sec.** A device's
  forwarding ceiling is often a packet rate rather than a bit rate, so a link with bandwidth to
  spare can still be saturated — until now there was no way to see that. A **bps / pps** button in
  the chart header flips the unit; the choice is remembered and applies to every interface.
  - **Two metrics are now collected by default**: `if_hc_in_ucast_pkts` and
    `if_hc_out_ucast_pkts` (IF-MIB `ifHCInUcastPkts` / `ifHCOutUcastPkts`,
    `1.3.6.1.2.1.31.1.1.1.7` and `.11`). They live in the same ifXTable as the octet counters the
    bits/sec line already uses, so no device loses coverage — but they add two series per
    interface, and **an upgraded deployment has no packet history before the upgrade**, so the pps
    view is empty for older windows and fills in from the first two polls onward.
  - Unicast only: multicast and broadcast frames are not counted, so a link carrying heavy
    broadcast reads low, and dividing bits by packets overstates the average frame size.
  - The bandwidth reference line and the fit/capacity axis toggle are hidden in pps mode —
    `ifSpeed` is a bit rate and would draw a meaningless line on a packet axis.
  - **The Errors / discards chart is unaffected and has no bps form.** IF-MIB counts errored and
    discarded *frames* but never their octets, so it is packets/sec by nature. Its unit label now
    says so (`(pps)` rather than the previous `(In / Out, /s)`).
- **Interface discards are now graphed, on one chart with errors.** `ifInDiscards` /
  `ifOutDiscards` (IF-MIB `1.3.6.1.2.1.2.2.1.13` and `.19`, standard on essentially every SNMP
  agent) have been collected since v0.1.x but had no reader, so the counters were in the TSDB and
  invisible everywhere. A node's **Interfaces** tab now plots them beside the error counters on a
  single **Errors / discards (pps)** chart — four lines, in and out of each, one colour apiece —
  and the dock header gains a **Disc** figure when the rate is non-zero. The two mean different
  faults: an error is a frame that arrived damaged (cabling, optics, NIC), a discard is a frame the
  device dropped although nothing was wrong with it (congestion, queue overflow, ACL). Sharing an
  axis means a much smaller rate can flatten against a much larger one; the header figures give the
  exact current values for each.
- **New dashboard widget: "Most interface discards".** Ranks the fleet's interfaces by discards/sec
  (in + out), alongside the existing "Most interface errors".
- **`GET /api/v1/nodes/{node_id}/interfaces/{ifindex}/series` returns four more arrays**:
  `in_discards` / `out_discards`, and `in_ucast_pps` / `out_ucast_pps`, all on the same shared
  timestamp axis as the existing four. This is an additive change — existing clients are
  unaffected. The MCP `get_interface_series` tool returns them too. The packet arrays are named for
  what they count so that a future total (adding multicast and broadcast) can arrive as a new field
  rather than silently changing what an existing number means.

### Improvements
- **The MCP tools now describe the optical readings they were already returning.** An AI client
  picks a tool by reading its description, and the optical work did not touch one — so
  `get_interface_series` still announced itself as *traffic* history and listed eight series while
  returning ten, and `get_node_status` enumerated an interface's fields without the power window it
  had gained. The data shipped; nothing told a client it was there or how to read it.
  - `get_interface_series` now names every array it returns, including `rx_power_dbm` and
    `tx_power_dbm`, and states what is easy to get wrong about them: they are gauges rather than
    counter rates, they are **normally negative** (a healthy receive level is roughly -3 to -20 dBm
    and 0 dBm means one milliwatt, not nothing), and both arrays being entirely null is how an
    optical port is told apart from a copper one rather than a sign of failed collection.
  - `get_node_status` now describes the acceptable power window on each interface and says that
    nothing alerts on it — those are the module's published figures, not a threshold configured in
    Yagra.
  - `query_metrics` collapses a node's per-interface gauges to their maximum, and now says which
    direction that is: for a metric where *low* is the fault, such as an optical receive level, the
    maximum is the healthiest port rather than the worst.
  - No API, schema or WebUI behaviour changes — these are the descriptions MCP publishes at
    connection time. **An already-connected client keeps the old text until it reconnects.**
- **`get_node_status` now reports each interface's state and load, not just its name.** The MCP
  tool listed a node's ports with their ifindex, name, alias and nominal speed — enough to name a
  port, not enough to say anything about it. Each interface now also carries `oper_status`
  (1 = up), `in_bps` / `out_bps`, `in_util_pct` / `out_util_pct` and `stale`, which is what the
  WebUI's Interfaces tab has always shown. "Which port on this node is down?" and "which one is
  busy?" now have an answer over MCP.
- **The interface charts wrap to a second row instead of being squeezed.** The dock lays its charts
  out on the available width, and the minimum readable width per chart has been raised from 220px to
  320px — so three charts sit side by side on a wide pane and drop to two-plus-one on a narrower
  one, rather than shrinking until the axes are unreadable. Narrow panes showing two charts now stack
  them sooner for the same reason.
- **Every chart's legend now reads the latest values when nothing is hovering it.** The legend is
  live — it reports the sample under the cursor — so with no cursor on the plot, which is how a
  chart spends nearly all of its time, every row read `--`. It now falls back to the most recent
  sample any of its series has: not simply the last column, because a window that runs to *now*
  normally ends in a bucket no poll has filled yet. All rows report the same instant, so they stay
  comparable. Applies to every chart in the WebUI, dashboard widgets included.
- **The two interface charts share a cursor.** Hovering either the throughput chart or the
  errors/discards chart moves both crosshairs and both legends to the same moment, so "traffic
  spiked — did discards spike with it?" is one reading rather than two hovers and a comparison of
  timestamps done by eye. Moving the pointer away restores the latest values in both.

### Bug Fixes
- **A 10G interface no longer reports its speed as 4.29 Gbps.** When a device's 32-bit `ifSpeed`
  saturates (it maxes out at 4,294,967,295) and it publishes no usable `ifHighSpeed`, Yagra was
  storing the saturation value itself as though it were a measurement. The rate is now recorded as
  unknown, which also corrects `in_util_pct` / `out_util_pct` — utilisation had been computed
  against a rate no interface actually has. Affected ports show a blank speed until the device
  reports one; nothing else changes.
  - **Ports already recorded that way are corrected on upgrade.** The interface upsert preserves a
    stored value when a poll reports nothing for it — which is what lets the metadata walk and the
    optical probe write different columns of one row — so the poller fix alone could not clean up
    after itself. A one-time migration clears the sentinel. Only the exact saturation value is
    touched; it is not a rate any interface has.
- **The MCP `query_metrics` tool no longer answers a per-interface or per-component metric with one
  arbitrary series' value.** A node-level query selects every series sharing the metric's name, and
  the store took the first of them — so asking a 16-port firewall for `if_hc_in_octets` returned
  the rate of whichever port the TSDB happened to list first, commonly an idle one reading `0`.
  There was no error and no empty result, just a plausible wrong number. The same applied to
  per-component gauges: `huawei_cpu_usage` on a 15-entity device read `0` while the WebUI's
  Device-health card showed the maximum across all fifteen. The tool now consults the metric's
  dimension first (the same inventory `list_node_metrics` reports) and either **collapses gauges to
  the node maximum**, saying so in a `note` on the response, or **refuses counters** with a message
  naming the tool that can answer — `get_interface_series` for one interface, `top_interfaces` for
  the fleet. Single-series metrics (`icmp_rtt_ms`, `http_up`, …) are unaffected. **This changes the
  answers an AI client gets**: a call that previously returned a number may now return an error
  that says where the number lives.
- **Data coverage no longer reports healthy URL, DNS and Meraki monitors as silent.** The gauge on
  `Settings ▸ Yagra health` — and the "Stale data" list beside it — asked every node in the
  inventory for a recent **ICMP round-trip sample**. A URL monitor, a DNS monitor and a Meraki
  device are never pinged and have no such series, so three of the four node kinds counted as
  missing data however well they were working: a site with five devices and five URL monitors read
  **50% fresh** with every monitor up. Coverage now asks each kind for **its own** liveness metric
  (`icmp_rtt_ms`, `http_up`, `dns_up`, `meraki_device_up`). The same correction applies to the MCP
  `get_fleet_summary(kind="coverage")` tool, which shares the calculation.
- **A node's state no longer falls to "unknown" just because it is not pinged.** Before the alert
  engine has an opinion about a node — one just added, or any node right after a core restart —
  its displayed state falls back to "has it reported recently". That fallback also asked only about
  ICMP, so URL and DNS monitors showed as `unknown` until their first evaluation. It now uses the
  same per-kind liveness metrics.
- **The single-node and list views now agree on how old a sample may be.** The fallback above used
  a 10-minute freshness window when answering for a page of nodes, but **no window at all** when
  answering for one node, so the same silent node could read `ok` on its detail page and `unknown`
  in the list. Both now apply the 10-minute window. A node whose last sample is older than that, and
  which the alert engine has not yet evaluated, now correctly reads `unknown` on its detail page.

## v0.2.10 — an operator can run the monitoring, and a control you may not use is no longer drawn

### Breaking changes
- **Operators can now run the monitoring.** `ManageConfig` — the permission behind roughly a
  hundred endpoints, from adding a node to replacing the TLS certificate — was Admin-only, so an
  Operator could not add a node, edit a threshold, run a discovery sweep or change a device
  profile. It has been split. **"Manage monitoring" (the old `manage_config`) and "Manage
  credentials" are now held by Operator and up**; a new **"Manage the deployment"
  (`manage_system`)** privilege stays Admin-only and covers what changes the deployment or sends
  its data elsewhere: notification delivery and routing, forwarding, the TLS certificate, upgrades,
  data retention, the AI provider, the configuration bundle, the support bundle, and removing a
  poller.
  - **This widens what an Operator account can do.** Review your Operator accounts before
    upgrading. Viewer and Admin are unchanged.
  - `Settings ▸ Roles & privileges` shows the new matrix. It is derived from the server, so it is
    always what the API actually enforces.
  - The `403` descriptions in the OpenAPI document were corrected in both directions: some said
    "Role below Admin" for endpoints an Operator now reaches, and others named `ManageConfig` for
    endpoints that now need `ManageSystem`.

### New Features
- **The interface charts are bigger, and you can decide how big.** The throughput and error charts
  under `Nodes ▸ <node> ▸ Interfaces` were a fixed 132px tall — the second-smallest chart in the
  product, and too small to read a trend off (issue #65). They now **open roughly twice that height**
  on an ordinary screen, and the dock they sit in has a **drag handle along its top edge**: pull it up
  for taller charts, down for more of the interface list. The handle is keyboard-operable (`↑`/`↓`
  once focused) and double-clicking it restores the default. The list above always keeps a few rows,
  so the dock cannot take the whole pane. Unchanged on phones, where both charts already stack and
  the dock scrolls.
- **A WebUI preference can now follow you to another machine.** The chart-dock height is saved
  against your **account**, on the server, rather than only in the browser that set it — so signing in
  from a second machine, a second browser, or after a deployment moves from `http://…:3000` to
  `https://…`, restores it. Two new endpoints back it, `GET` and `PUT /api/v1/preferences`, holding
  **one opaque JSON document per account**: the server stores it and never reads inside it, so a
  future preference needs no API change and no migration. Preferences are strictly per-account — an
  API token is refused (`403 session_required`; a token names no person), and on a public-dashboard
  deployment an anonymous visitor is refused too, because a shared preferences row would be
  everyone's. A core that predates this answers `404` and the WebUI keeps using the browser-local
  value without saying anything.

### Improvements
- **Buttons you are not allowed to press are no longer drawn.** Every write control in the WebUI —
  every `+ Add`, every edit, every delete, every Save — decided whether to appear from *"am I
  signed in?"* rather than from *"may I do this?"*. A Viewer was offered `+ Add window` and
  `+ Add mute`; an Operator was offered every administrator-only `+ Add` on the screens whose list
  anyone may read. The server refused all of them, so nothing happened that should not have, but
  the operator had to press the button to find out. Controls now ask for the privilege the action
  itself requires — the same one the API checks — and are simply absent otherwise. Where a whole
  panel *is* the action (Configuration bundle's export and import, the three System-settings forms),
  it now names the privilege you need instead of telling a signed-in user to sign in.
- **A refusal names the privilege it wants.** A screen you may not read used to say only "you don't
  have permission"; it now says which of the eight privileges would grant it, taken from the
  server's own catalogue — the same text `Settings ▸ Roles & privileges` renders. Extended to three
  screens that previously reported a permission refusal as "unavailable" or as a load error:
  `Settings ▸ AI analysis`, `Settings ▸ API tokens` and `Settings ▸ Audit log`.
- **The shared dashboard's `Customize` button is hidden rather than disabled** for an account that
  cannot change it. It carried its explanation in a hover tooltip, which a touch device never
  shows — so on a phone the button read as broken.
- `Settings ▸ Pollers ▸ Register poller` now requires the *Manage configuration* privilege, matching
  the rest of that screen; it was previously offered to any signed-in account.

### Bug Fixes
- **Reading a page no longer makes the server re-resolve the whole fleet.** Every successful write
  to `/api/v1` marks the monitoring configuration as changed, which is what tells four background
  jobs their cached work is stale. Seven endpoints that change no configuration were doing that,
  the worst by a wide margin being `POST /api/v1/node-names` — the batch id→name lookup every table
  showing a node reference calls on **each render**. So merely opening or reloading a page cost the
  deployment a full node scan, a per-node poll-spec rebuild with credential decryption, an
  alert-configuration reload, a poller-coverage recount and, within five minutes, a topology
  re-derivation. Nothing failed and nothing was logged, because the recomputed answer was always
  correct; the cost was visible only as CPU, and only at fleet scale. Now exempt:
  - `POST /api/v1/node-names` — resolves ids to display names; a `POST` only because the id list is
    too long for a query string.
  - `PUT /api/v1/dashboard` and `PUT /api/v1/shared-dashboard` — a widget layout is presentation
    state, and it is saved on every add, move, resize and setting change, so one editing session
    repeated all of the above many times over.
  - `POST /api/v1/event-rules/test`, `POST /api/v1/llm/test`,
    `POST /api/v1/settings/ldap/test` and `POST /api/v1/meraki/orgs/discover` — the
    "test this before you save it" probes, pressed repeatedly while someone iterates on a form.
    `POST /api/v1/notification-channels/preview` was already exempt for exactly this reason; these
    four had drifted out of the list.

  A test now fails the build when a route registered with a mutating method has a handler that
  demands only a read permission and is not exempt — which is the shape `node-names` had. It
  cannot cover the four config-test probes, because a test that writes nothing still legitimately
  requires the *Manage monitoring* privilege; those were found by measuring a running deployment.
- **The node tree's right-click menu is back for operators.** The previous release gated the whole
  context menu on the permission its *strictest* entry needs, so an account that could open a
  maintenance window or mute a node — the two entries in that menu that are not administration —
  got no menu at all. Each entry now asks for the permission it actually requires, and the menu
  opens when at least one of them is available. It read as deliberate because it was consistent: an
  administrator saw the menu, so nothing looked broken.
- **A screen you lack permission for now says so, instead of reporting that it is empty.** Signed
  in as a Viewer, `Nodes ▸ Credentials & secrets` showed "**No credentials yet**" on a deployment
  holding two credentials: the list request was refused with `403`, the page had a branch for
  "this deployment has no admin state" and none for "you may not see this", and the refusal fell
  through into an empty table. **The failure arrived in the shape of a success**, so nothing —
  no test, no log line, no glance at the screen — could tell it apart from a genuinely empty list.
  Sixteen screens shared that code and only one of them (Users) handled the refusal; all sixteen
  now go through one shared classifier, and a test fails the build if a new screen hand-rolls it
  again. The `+ Add credential` button that a Viewer could press — and that would only fail on
  submit — is gone with it, because a blocked screen no longer renders its toolbar at all.
- **Correction to the v0.2.9 notes.** They said moving Credentials into `Nodes` left the entry
  "visible-but-forbidden" for a Viewer. That described what *should* have happened. What actually
  happened is the empty list above; the move did not cause it (the defect predates it by many
  releases) but did take it from the bottom of Settings to the second item under Nodes.

## v0.2.9 — the menu says what each screen is for, and passive monitoring gets its own tab

Every change in this release is in the WebUI's navigation and wording. Nothing in the backend
moved: no API endpoint changed shape, no database migration ran, and a poller from v0.2.8 works
unchanged. **Four screens changed address, and every old address redirects with its query string
intact**, so bookmarks and links keep working.

### Improvements
- **Passive monitoring has its own top-level tab: `Events`.** Receiving and reading events were
  under `Alerts` while relaying them was under `Settings ▸ Forwarding`, so nothing in the menu named
  the pipeline as one thing. Four screens moved, and **every old address redirects, keeping its
  query string** — a bookmarked `/alerts/events?node_id=…` still opens that node's events:

  | Was | Is now |
  |---|---|
  | `Alerts ▸ Events` — `/alerts/events` | `Events ▸ Events` — `/events` |
  | `Alerts ▸ Event sources` — `/alerts/event-sources` | `Events ▸ Webhook sources` — `/events/webhooks` |
  | `Settings ▸ Forwarding` — `/settings/forwarding` | `Events ▸ Forwarding` — `/events/forwarding` |
  | `Settings ▸ Credentials & secrets` — `/settings/credentials` | `Nodes ▸ Monitoring setup ▸ Credentials & secrets` — `/nodes/credentials` |

  `Event alert rules` deliberately **stays under Alerts**: it produces alerts, and splitting the two
  rule screens across two tabs would repeat the problem being fixed.
- **`Event sources` is now `Webhook sources`, because that is all it manages.** Its description
  claimed "Syslog and SNMP trap listeners" and the screen has only ever held webhook senders. The
  question that description was really answering — *where do I send syslog?* — is now answered on
  the Events page itself (see below).
- **Credentials moved to Nodes because they are device keys, not sign-in.** They sat in
  `Settings ▸ Access` beside Users, Roles and Authentication, which is how a *person* signs in to
  Yagra. ⚠️ The page still requires **ManageCredentials**, so on a Viewer account the entry is now
  visible-but-forbidden inside Nodes where it was previously buried in Settings.
- **Four menu items were renamed to say what they are.** `Alerts ▸ Alert rules` is now **Metric
  alert rules** and `Alerts ▸ Event rules` is now **Event alert rules** — both produce alerts, and
  the old pair named one by its output and the other by its input, so neither said what kind of
  rule it was. `Settings ▸ System health` is now **Yagra health** and `Settings ▸ System settings`
  is now **Monitoring defaults**: two adjacent items both beginning "System" answered completely
  different questions ("is Yagra itself OK" vs "how should the fleet be polled"). No URL changed
  and no bookmark broke.
- **`Nodes ▸ Monitoring config` is now `Monitoring setup`, and its items are in the order you build
  them**: MIB repository → Metric sets → Device profiles → Classification rules. Device profiles
  used to come first while its own description said "Attach Metric sets here" — two entries further
  down.
- **The Events page now says where Yagra is listening.** A line under the title names each bound
  syslog and SNMP-trap endpoint and the pollers holding it, so "I sent syslog and see nothing" can
  be answered on the screen where it is asked. When no listener is bound it names the environment
  variables that enable them instead. Nothing new is collected — this is the same data
  `Settings ▸ Pollers` already showed.
- **The Acked filter on Active alerts now explains where Acked comes from.** Acknowledgement is
  mirrored inbound from your on-call tool (PagerDuty / JSM) and Yagra never sets it; that was
  previously only a parenthetical under the page title, which is not where anyone looking for the
  ack action goes. The parenthetical is gone from the title now that the filter says it.
- **Shared dashboard, My dashboard and Preferences now have the one-line description every other
  screen has**, saying who a change affects.
- **The global search box now reads "Search nodes…".** It has only ever searched nodes; you had to
  start typing to find that out.
- **`Alerts ▸ Notification routing` is now `Notification delivery`, and it moved below the two
  rule screens.** *Routing* already means something else in a network monitoring product — the
  discovery walk under Monitoring defaults is literally called "Routing adjacency (OSPF / BGP /
  routes)" — so one word carried two meanings. The order in **Configure** now follows the work:
  decide what fires (**Metric alert rules**, **Event alert rules**, side by side because they are
  the two ways an alert comes into existence), then who hears about it, then when to stay quiet.
- **`Settings ▸ About` moved from the Personal group to the end of System.** It describes the
  deployment, not the account.
- **A collapsed sidebar now shows the real item names instead of two-letter codes.** It was 52px of
  `Ms`, `Mb`, `Cl`, `Pr` — legible only to someone who already knew the menu, which is precisely
  the person who does not need it. The rail is 120px now and wraps the label over two lines, so
  collapsing buys 100px of width instead of 168px. Group headings still disappear when collapsed;
  no *item* is reduced to a code.
- **Every menu item now has a one-line description.** Half of them had none — 21 of 42 — and the
  missing half included the pairs most easily confused with each other.
- **`Troubleshoot ▸ Tools` groups its fifteen analyses into Metrics, Passive events, Traffic flow
  and Across stores**, instead of one flat wall of cards you had to read end to end.

## v0.2.8 — the filter row waits to be asked for

### Improvements
- **The column filter row is now hidden until you ask for it.** Press **Filter** in the toolbar
  above any list to show it, press it again to hide it; the choice is remembered across screens and
  across reloads. It reached every list in v0.2.7, which meant it also occupied a band on the many
  screens nobody was filtering. **While a filter is active the row stays visible and the button
  will not hide it** — a list with rows missing and no visible control responsible for it is worse
  than the space it saves — so opening a shared link that carries a filter shows the row that
  produced it. Use **Clear all filters** beside the button to get back. Nothing changed on a phone:
  the **Filter** button there still opens the same bottom sheet.
- **The `Filter` and `Clear all filters` buttons are now the same height as the controls beside
  them.** Both carried the 44px tap target meant for a phone, which is half again taller than the
  30px pickers and search boxes they share a toolbar with. On a phone both keep the tap target,
  since there the button is the only way to filter at all. `Filter` also dims while an active
  filter is holding the row open, rather than only refusing the press — with nothing but a cursor
  change it read as unresponsive.
- **Nodes ▸ Discovery, "Seen on network": "Clear all filters (1)" no longer appears on the default
  view.** That table deliberately starts narrowed to unmonitored endpoints, and the button was
  counting that default as a filter the operator had set — so pressing it changed nothing. It now
  appears only once you actually change something.
- **Nodes ▸ Device profiles: the `Filter` sheet on a phone now shows the same option counts the
  desktop filter row does.** "Poll interval" listed *Inherited* and *Overridden* with no numbers
  beside them there, while the row on a desktop had them — the two surfaces were being handed
  different sets of counts. Both now read one source.
- **Alerts ▸ Alert rules: the "Dwell" column is now "Breaches".** *Dwell* is radar and telecom
  jargon, not a word network engineers use for this — the number is how many consecutive readings
  must cross the bound before the alert fires, so the column, the add-rule field and the hint all
  say that instead. `dwell_samples` is unchanged on the API and in the configuration bundle.
- **Alerts ▸ History and Alerts ▸ Alert rules: a saved link whose search term begins with `!`, `~`
  or `\` is written differently.** Those four screens (with Settings ▸ Audit and Troubleshoot ▸ All
  findings) now share one filter codec with every other list, and it spells a text filter the way
  the rest of the app does — a leading `!` means *exclude* and a leading `~` means *regular
  expression*, so a term that starts with one of those characters is escaped with a backslash:
  `?node_q=%5C%21core` for `!core`. **A link saved before this change still opens**, but a term of
  that shape reads as the rest of the word — `?node_q=!core` searches for `core`. Every other saved
  link is unaffected, and nothing changed about what the filters do.

### Bug Fixes
- **Five screens drew a desktop column-filter row on a phone, on top of the `Filter` button that is
  supposed to replace it.** On the node detail **Interfaces** tab the row also pinned itself
  partway down the list — it sticks to the bottom of the column header, and the header is hidden in
  mobile layout — so interface rows scrolled through the gap above it. The **Collection** tab,
  both tables on **Nodes ▸ Discovery** and **Monitoring ▸ Profiles** showed the same row, with its
  controls too narrow to read or sitting away from the columns they filter. All five now offer the
  `Filter` sheet alone, as every other screen already did; nothing is filterable on a desktop and
  not on a phone.
- **Selecting an interface could scroll it under the filter row.** The Interfaces tab kept the
  selected row clear of its own sticky header by a fixed 32px, which stopped being the full height
  when the filter row was added below the header in v0.2.6.

## v0.2.7 — every screen filters by its own columns, and the Events search shows what it matched

### Breaking changes
- **The Events screens' filters have moved from the toolbar into a filter row under the column
  headers**, and the toolbar is now an action row. Each column carries its own control: Kind and
  Result are multi-selects with counts, Message and Source take a condition that can be a substring,
  a regular expression (Message only) or an exclusion, and When keeps the range presets with the
  From/To instants under "Custom…". The single search box that matched *either* the source or the
  message is gone from the UI — ask about the column you mean instead. `q` and `regex` are unchanged
  on the API and are what the MCP `search_events` tool still calls `search`.
- **The Events "All events / Matched a rule / Unmatched" selector has been replaced by the Result
  column's multi-select**, which says *what* the rule did rather than only whether one matched.
  `matched=true|false` is unchanged on `GET /api/v1/events` and `GET /api/v1/events/stats`; the
  equivalent selection is `action=fired,refreshed,cleared,suppressed`.
- **Every Events filter is now in the URL**, so a filtered view can be linked. A filter at its
  default has no key at all, which means a bare `/alerts/events` is always the default view.
- **Saved reports is filtered by the report's name rather than picked from a list of reports.** A run
  keeps the name its report had when it ran, and that is what the column shows, so a list of report
  *ids* labelled with today's names would have silently dropped the older runs of a renamed report.
  Finding a renamed report's history now means searching for either name.
- **The single search box on API tokens is gone**, replaced by separate Name and Owner filters —
  "owned by alice" no longer also matches a token *called* alice. The Neighbors tab's one box became
  three, one per end of the link.
- **The column filters on Alert history, Audit, Alert rules and Saved findings now take several
  values**, replacing the single-choice controls those screens shipped with. `GET /alerts/history`
  (`severity`, `state`), `GET /audit` (`action`, `status`), `GET /thresholds` (`scope_level`,
  `direction`) and `GET /analysis/findings` (`tool`, `severity`) each accept a comma-separated set,
  as `GET /events` already did. A single value is unchanged, so an existing link or client keeps
  working; an unknown token is a 400 rather than a silently widened result, and a set accepts at
  most 32 values. The same parameters are on the MCP `get_alert_history`, `get_audit` and
  `search_analysis_findings` tools.
- **The flow endpoints' drill-down filters take several values, and a value they cannot parse is now
  a 400 rather than being ignored.** `proto`, `port`, `peer` and `asn` on all twelve `/flow/*` and
  `/nodes/{id}/flow/*` endpoints accept a comma-separated set of up to 8 (`proto=6,17`,
  `port=80,443`). ⚠️ **The refusal is the part to check before upgrading**: a request like
  `port=not-a-port` used to be answered with the *unfiltered* top-N — an answer to a question nobody
  asked — and now returns `invalid_filter`. A client that was relying on a malformed filter being
  dropped will start seeing errors, which is the point: it was never getting the rows it asked for.
  The MCP `top_flows` and `flow_fanout` tools are unchanged and still take one value each.
- **`action` and `status` on `GET /api/v1/audit`, and `scope_level`/`direction` on
  `GET /api/v1/thresholds`, are now strings rather than typed enums in the OpenAPI document** —
  that is what carrying several values requires. The accepted vocabulary has not changed and an
  unknown value is still refused; what changed is that the refusal now comes back as the standard
  error body instead of a plain-text parameter rejection.
- **The search box on Maintenance windows, Mutes, Classification rules, Pollers and Credentials has
  been replaced by per-column filters, and this is not a like-for-like swap.** Each of those boxes
  searched several fields at once and could not say which was meant — on Pollers, `0.2` matched a
  poller running v0.2.4 *and* a poller in a pool named `site-0.2`. Ask about the column you mean
  instead. The Credentials search covered the name and the credential id together; those are two
  columns now.
- **The same replacement has now reached every remaining screen**: Active alerts, Troubleshoot ▸
  Runs, Users, the Nodes tree, node detail ▸ Interfaces and ▸ Collection, both Discovery tables,
  Metric sets and Device profiles. On each of them the one box that searched several fields is gone
  in favour of a control per column — or, on the lists that have no column headers (Active alerts,
  Runs, Users, the Nodes tree), a labelled row of the same controls above the list. Every one of
  them takes **several values** where it used to take one.
- **`state`, `kind` and `pool` on `GET /api/v1/nodes` now take comma-separated sets**, and so does
  the MCP `list_nodes` tool. A single value is unchanged. `state=warning,critical,unreachable` is
  "everything that is not healthy" — three separate looks at the tree before. An unknown `state` or
  `kind` token is a 400 rather than a silently widened list; a `pool` name is not checked against
  anything, because pool names are yours, so an unrecognised one simply matches nothing.
- **`state` and `kind` on `GET /api/v1/nodes` are strings rather than typed enums in the OpenAPI
  document**, for the same reason `action`/`status` on the audit log are: carrying several values
  requires it. The accepted vocabulary is unchanged.
- **The "Arriving only" checkbox on node detail ▸ Collection is now a Status filter with three
  values.** The checkbox could only say "hide what is not arriving"; "show me only the
  configured-but-silent metrics" — the question when a collection set has stopped working — was
  unsayable. Selecting *Collecting* and *Not configured* together is the old checkbox's meaning.
- **The "Not yet monitored" checkbox on Discovery's seen-endpoints table is now a two-valued
  filter**, so "the endpoints someone has already imported" can be asked for directly. The table
  still opens on *Not yet monitored*, as it always has.
- **The "Answered only" checkbox on Discovery's sweep results is gone, with nothing in its place.**
  It was never quite what it said: a swept address is reported when it answered ICMP **or** gave up
  an SNMP identity, so "not answered" actually meant "answered SNMP but not ping" — a device
  filtering ICMP, which is real but rare and cannot be labelled honestly in a filter's width. The
  Identity column already separates a device that spoke from one that did not, and the `ping` badge
  still marks each row.
- **Device profiles are no longer matched by their role through the search box.** Role is the
  heading rows are grouped under, not a column, so it has a control of its own above the table —
  and, unlike the box, it takes several roles at once.

### New Features
- **Alert history can be filtered by node name, by metric and by whether the incident was
  acknowledged.** `GET /api/v1/alerts/history` and the MCP `get_alert_history` tool take `node_q`
  (substring of the node's current name — distinct from `node_id`, which names exactly one),
  `metric` (substring of the metric name) and `acked` (`true`/`false`). "What fired this week that
  nobody has looked at" is now one filter rather than a read-through.
  ⚠️ On a large history table a **more selective** `metric` term is the slower one: the index still
  serves the ordering and the page size, so the query walks it until the page is full — a metric
  matching one row in a million walks the whole index, while a common one stops almost immediately.
- **Saved findings can be filtered by node name and by finding text.**
  `GET /api/v1/analysis/findings` and the MCP `search_analysis_findings` tool take `node_q` and `q`,
  where `q` matches the metric **or** the finding kind — the two halves the What column shows. A
  fleet-wide finding has no node, so it never matches `node_q`.
- **Saved findings can be filtered by score.** `GET /api/v1/analysis/findings` and the MCP
  `search_analysis_findings` tool take `min_score` and `max_score`, both inclusive and each usable
  on its own — "score 60 and up" is one bound, not a window. This is the first numeric filter in the
  WebUI's filter row, which is why it arrived an increment after the other Saved-findings columns.
- Every list that had a hand-written table now scrolls virtualized and carries the filter row:
  Maintenance windows, Mutes, MIB repository, Classification rules, Event sources, Event rules,
  Notification routing (both tables), Credentials, Pollers and Metric sets. **Two screens keep
  their hand-written table on purpose**: Device profiles, whose rows are grouped under role
  headings, and the metric editor inside an expanded row, which would be a virtualized list inside
  a virtualized row.
- **Three Troubleshoot reports gained the filter row.** Unmatched signatures (rule gap) can be
  narrowed by source kind, signature, event volume and where it was seen; Scan detection filters on
  every one of its seven columns, so "sources that touched more than 500 destinations" is one
  control rather than a read-through; and Authentication probes gained a filter bar over the source
  address, the severity and the failure count. The severity chips there became a multi-select, so
  "critical and warning" is now sayable and `Info` is selectable at all. The other twelve reports
  keep their chips on purpose — those select a diagnostic lens (`chronic` vs `intermittent`, for
  instance), not a row attribute, and a generic filter cannot say them without lying.
- `GET /api/v1/events` and `GET /api/v1/events/stats` take five new optional filters, and the MCP
  `search_events` tool takes the same five: `action` and `severity` (comma-separated sets, like
  `kind` now is), `msg` + `msg_regex` + `msg_not` for a message-only condition, and `src` +
  `src_not` for a condition on the event's source IP or the name of the node it came from. An
  unknown token in any set is a 400 rather than a silently widened search, and each set accepts at
  most 32 values.
- `kind` now accepts several values (`kind=syslog,trap`). A single value is unchanged.
- `GET /api/v1/system-health` gained **`search_semantics`** (`prefix` | `substring`): how a plain
  search term matches on this deployment. On a VictoriaLogs deployment a term matches from the start
  of a word, so `POLICY` finds `POLICYPERMIT` but `PERMIT` does not — the Events page says so beside
  the filter, and its empty state names that specific case instead of "nothing matches these
  filters".

### Improvements
- **Troubleshoot ▸ "Saved findings" is now "All findings".** The old name promised a step that does
  not exist: findings are written the instant a run completes, there is nothing to save and no
  button to look for. The screen shows every finding from every run until that run is pruned
  (Settings ▸ Retention, "Diagnostic", default 90 days). Only the label changed — the URL,
  `GET /api/v1/analysis/findings` and the `SavedFinding` schema name are untouched.
- **The Credentials type filter now offers every kind that is in the table.** It listed three, so
  an `http_auth` credential — or a Meraki API key, which the integration creates rather than an
  operator — could sit in the list and not be filterable at all.
- Disabled notification channels and routing rules are still dimmed after the table rewrite.
- **Filtering the node tree by state, kind or pool now narrows the folders too, and expands them.**
  Those three run server-side, so the tree could not see them: picking *Critical* left every folder
  on screen — including the ones with no critical node under them — and a collapsed folder stayed
  collapsed over its own match. Only the search box had ever told the tree it was filtering.
- **A folder's bar and count describe the rows on screen while a filter is on**, rather than the
  whole folder. "DNS 3" beside a single row asked the operator to work out which number was the
  answer. Browsing is unchanged: there the count is the server's rollup, which is what makes a
  folder nobody has opened report its real size.
- **The metric editor's add form has moved below the metric list**, under a "New metric" heading.
  Above the list it was a line of inputs sitting directly on top of a table — which is now what a
  filter row looks like everywhere else — so it read as one, and typing a metric name into it did
  nothing to the list below. No filter was added there instead: the largest metric set holds eleven
  metrics and the average is between three and four.
- Negation is offered for both plain terms and regular expressions. It was measured on 6.7M real
  events before it shipped: excluding costs what including costs, on both stores and in both modes.
- **A plain search term now matches from the start of a word on a log-store deployment**, so
  `POLICY` finds `POLICYPERMIT`. It previously had to be the whole word. Measured on 6,695,066 real
  events before shipping: a word prefix is answered from the store's index and costs about what an
  exact word costs (0.12s against 0.06s over 24 hours), while a match *inside* a word is a full scan
  of the window and is ~15× slower — which is why that stays behind the Regex switch.
- **When a plain term finds nothing, the Events screens ask once more, looking inside words too, and
  say that they did.** The expensive query is paid only where the cheap one had already reached a
  dead end.
- **The parts of a message that matched the filter are highlighted**, in the table, in the full-text
  popover and on the mobile card. A negated condition highlights nothing — the rows on screen are
  the ones that did *not* match.
- **The full message opens in a panel**: hover it, or click to pin it open and select the text.
  It replaces the browser tooltip, which appeared slowly, could not be copied, and was cut short by
  the platform on the long lines this is most needed for.
- **"Clear all filters" is back on desktop**, in the action row, with a count of what it will clear.
  It appears only while something is narrowing the list, and on the Events page it clears the node
  selection too.
- The Events empty state distinguishes three cases — nothing in the window, nothing matching the
  filters, and nothing whose word *starts* with the term — because the default range narrows, so "no
  events" was never the right sentence.
- **Twelve more screens moved their filters into the column filter row**: API tokens, Forwarding,
  Dependencies, a node's Neighbors tab, Troubleshoot ▸ Scheduled, all three Reports tables, Alert
  history, Audit, Alert rules and Saved findings. Each also gained "Clear all filters" and the
  mobile filter sheet, and most gained filters they never had — a time window on when a token was
  last used, the state of a node on the Dependencies list, the trigger of a saved report.
- **Two tables that were showing two facts in one column now show them in two.** Forwarding's Target
  held the address *and* the protocol, so the destination-kind filter had nowhere to live; it now has
  its own column, as does the enabled/disabled state that used to be a badge beside the name.
- **Dependencies' "All nodes / With upstream / Currently suppressed" selector became two filters**,
  on the two columns it was really asking about, so both can now be applied at once.
- On screens whose list is fully in the browser, an option's count says how many rows you would get
  by switching to it — it excludes that column's own filter, the way a spreadsheet's autofilter does.

### Bug Fixes
- **A table wider than the pane now scrolls sideways instead of hiding the columns that did not
  fit.** Settings ▸ Pollers, Forwarding and API tokens each declared more column width than a
  1280px window gives them — on API tokens by 354px — and the surplus was drawn outside a box that
  clipped it, so the Actions column existed but could not be reached and no scrollbar said it was
  there. The same overflow squeezed the first column down to its own padding, which is why its
  name was blank and its filter button was 14px wide and unreadable. A table that already fits is
  unchanged and gains no scrollbar.
- **The notification-channel kind is now written the same way everywhere.** The filter under the
  Kind column on Alerts ▸ Routing showed the raw token (`pagerduty`, `jsm`) while the dialog above
  it showed the product name, because the two were separate lists.
- **A column filter now accepts typing as soon as it is opened.** Clicking a filter took two clicks:
  the panel opened, and the box inside it had to be clicked again before it would take a keystroke.
  The panel is hidden for the frame in which it is measured, and a hidden element cannot be focused,
  so the focus the code already asked for was silently dropped. The caret now lands in the panel's
  text box — the term on a text filter, the value list on the flow drill-downs, the lower bound on a
  numeric range, and the option search on a long list of choices. A panel with nothing to type in
  (the time-range presets, a short list of choices) leaves focus on the filter itself rather than
  picking an option for you, and the mobile filter sheet is unchanged: it shows every column at
  once, so nothing there has a claim on the keyboard.

## v0.2.6 — every check on a device runs again, and twenty-two lists can be narrowed

**⚠️ If you are running v0.2.3, v0.2.4 or v0.2.5, upgrade as soon as you can.** Those three
releases poll a device for only **one** of its checks. On an SNMP node the system scalars keep
arriving while the interface walk, the vendor health tables, the topology walks and even ICMP are
discarded on every cycle — SNMP collection quietly stops filling in, and nothing says so, because
the check that survives *is* the liveness check. The first entry under Bug Fixes explains why it
is invisible and why a **smaller** deployment is hit harder than a large one. There is no
configuration to change: the fix is in the poller, and upgrading is the whole remedy.

The rest of the release is about narrowing lists. Twenty-two screens had no filter of any kind,
including the two opened first during an incident — Alerts ▸ Active and Alerts ▸ History — and the
one where a missing match is a correctness problem rather than an inconvenience: Settings ▸ Audit
filtered only the rows it had already fetched, and hid older matching entries without saying so.

### Breaking changes
- **The event log's time range now defaults to the last 24 hours instead of all time**, and the
  From/To instants have moved under a "Custom…" choice in the new range dropdown. This is what pays
  for the search change below: over an unbounded range a case-insensitive term costs about ten times
  a case-sensitive one — 9.4 seconds against 6.7M events on our own test deployment — while over 24
  hours it is about 1.1×. "All time" is still one click away, and the API is unchanged: `start` and
  `end` on `GET /api/v1/events` remain optional and unbounded by default, so an API client that sent
  no range still gets none.
- `GET /api/v1/system/upgrade` (and `get_system_health(section="upgrade")`) gained
  **`updater.installed`**: whether this deployment has an upgrade mechanism at all. Read it before
  `updater.present`, which now means only "the updater has reported" and is meaningful only where
  `installed` is true.
- A request to upgrade a deployment that has no mechanism now answers **`upgrade_unsupported`**
  (503) instead of `upgrade_unavailable`. The three 503 codes on this surface are now distinct:
  `upgrade_unsupported` (no mechanism here), `upgrade_disabled` (there is one and it is switched
  off) and `upgrade_unavailable` (there is one and its updater is not answering). Uploading an
  image archive to a deployment with no mechanism answered `upgrade_unavailable` before and answers
  `upgrade_unsupported` now.

### New Features
- **Nodes ▸ All nodes can be narrowed by state, kind and poll pool.** The tree could search names
  and addresses and nothing else, so "which URL monitors are down" and "what does the tokyo pool
  actually poll" had no answer on the screen that lists them. The three filters live in the URL, so
  a narrowed inventory can be shared. `GET /api/v1/nodes` accepts `state`, `kind` and `pool`, and
  the MCP `list_nodes` tool takes the same three. Two notes on what they mean. `pool` is the
  **effective** pool — a node's own if it sets one, otherwise the nearest folder that does,
  otherwise `default` — because filtering the stored column alone would find nothing for `default`,
  which is the pool most of a fleet is actually in. And none of the three can be a SQL `WHERE`
  clause: a node's state lives in the alert engine, its kind is derived from which side table
  carries a row, and its pool is inherited. They are applied by the same resolvers every other
  screen asks, over a bounded scan of at most 5,000 candidates, and the response carries a new
  `truncated` flag when that bound was reached — so a short answer says whether it is complete
  instead of leaving you to assume it.
- **Reports ▸ Saved reports can be narrowed** by report, run state and free text.
  `GET /api/v1/reports/runs` accepts `definition_id`, `state` and `since` for API clients, and the
  MCP `get_report_runs` tool takes the same three; the page filters in the browser, because its
  rows arrive over SSE and a filtered fetch would be undone by the next progress frame. Filtering by
  report uses the definition's id rather than its name, so renaming a report does not orphan its own
  history.
- **Settings ▸ API tokens sorts by column.** Click a header to sort, click again to reverse. Status
  sorts by severity rather than alphabetically, so the tokens that do not work group at one end, and
  a token with no expiry sorts last in both directions rather than filling the top of the screen
  when the order is reversed. The shared table component gained the affordance; it deliberately does
  not sort the rows itself, so a screen that pages through a large list cannot accidentally reorder
  the pages it happens to have loaded and present that as the order.
- **Alerts ▸ Rules can be narrowed, in the database.** This is the one configuration table that
  grows with the fleet — a node-level override is per node × metric — which is why the list has
  always been capped at 500 with a "showing N of M" note. It had no filter at all, and one added in
  the browser would have run over that prefix: "show me the cpu_util rules" would have examined the
  newest 500 and reported on those. `GET /api/v1/thresholds` now accepts `q` (a metric substring),
  `scope_level` and `direction`, and `total` counts the rules matching the filter, so the "N of M"
  stays true. The filters live in the URL, so a narrowed ruleset can be shared.
- **Troubleshoot ▸ Runs can be narrowed** by analysis, run state and free text over the scope and
  the summary. `GET /api/v1/analysis/jobs` accepts `tool`, `state` and `since` for API clients; the
  page itself filters in the browser, because its rows arrive over SSE and a filtered fetch would be
  undone by the next unfiltered progress frame.
- **Thirteen more screens can be narrowed.** Alerts ▸ Maintenance windows and Mutes, the two tables
  on Alerts ▸ Routing, Settings ▸ Forwarding, API tokens and Pollers, both tables on Nodes ▸
  Discovery, Troubleshoot ▸ Scheduled, the Reports page's Templates and Schedules tabs, the node
  detail's Interfaces, Neighbours and Collection tabs, and the add-widget catalog. Each gets a
  search box and the one or two filters that screen is actually asked about — status, kind, enabled,
  protocol, pool. These all run in the browser, and legitimately: every one of these lists is
  bounded by what an operator configured rather than by how many nodes the fleet has. Two are worth
  calling out. The Discovery endpoint table has always hidden already-imported endpoints with
  nothing on screen saying so; that is now a control, so an endpoint that disappeared because a
  colleague imported it can be told from one that stopped being seen. And the add-widget catalog's
  search matches the words on the card rather than the identifiers behind them, so it works the same
  in Japanese.
- **The Events search is no longer case-sensitive.** On a deployment with a log store (ADR-024) a
  plain search term was matched case-sensitively, so `SSH` did not find `ssh` — while the same
  search on a deployment without one did, because PostgreSQL's `ILIKE` never cared. One query, two
  answers, depending on which store the deployment happens to run. Both are now case-insensitive, in
  plain and regex mode alike. One difference remains and is deliberate: a log store matches whole
  words where PostgreSQL matches any substring, because a leading substring cannot be served from an
  inverted word index without scanning every block — that costs ~300× and reaches VictoriaLogs'
  30-second query ceiling. The search box's regex toggle is the escape hatch for it, and reaches
  inside words on either store.
- **Alerts ▸ Active can be narrowed.** The triage screen — the first one open during an incident —
  had no filter at all: a major outage produced thousands of rows and the only way through them was
  to scroll. It now filters by severity, node state, whether the alert has been acknowledged in your
  external on-call tool, and free text over the node name, the node's id and the metric that fired.
  The filters live in the URL, so a narrowed view survives a reload and can be pasted to whoever is
  looking at the same incident. Unlike History and Audit these run in the browser, and deliberately:
  the whole active-alert set is already there over SSE, so there is no page boundary for a filter to
  hide matches behind. The dashboard's alert widgets are unaffected.
- **Alerts ▸ History can be narrowed.** The screen had no filter of any kind: an append-only log
  that only grows, readable by scrolling and nothing else. It now filters by severity, state, fire
  or clear, node or folder group, and a time window — all applied in the database, so a filter
  reaches the whole log rather than the pages already on screen. The filters live in the URL, so a
  narrowed view survives a reload and can be shared, and a link can point straight at one node's
  alert history. `GET /api/v1/alerts/history` accepts `severity`, `state`, `resolved`, `node_id`,
  `group_id`, `since` and `until`; the MCP `get_alert_history` tool takes the same set through the
  same code path. There is deliberately no free-text search: the only free-text column is the metric
  name, which is unindexed, and searching it would turn every page into a full table scan.

### Improvements
- **Settings ▸ System health compares hosts instead of showing one at a time.** Host resources was a
  dropdown of core and every poller, so asking whether a poller was busy — against core, or against
  its pool-mates — meant switching between them and remembering. It is now one section for core and
  one per poller pool, with every host in a section drawn on the same CPU, load, memory and disk
  charts. A section holding one host keeps the 1m/5m/15m load detail; two or more collapse to the
  1-minute average so the colour can mean the host, and each card's headline names the host reading
  highest rather than a number that describes none of them. Disk cards are the union of the mounts
  the section reports, so core's `metrics` and `database` volumes no longer vanish when a poller is
  selected. Sections fold away, and a folded one still shows each host's current CPU, memory and
  disk — without fetching any history, which is what bounds the cost on a fleet with many pollers.
- **A URL or DNS monitor's own settings are now in Edit node.** Changing a monitored URL, its
  expected status, a DNS record type or a resolver previously meant finding a ⋮ menu on the health
  card in the Overview tab, which only appeared while that card was rendered. Those fields are now
  part of Edit node in the header, alongside the profile and pool. The ⋮ menu keeps "Remove
  monitoring". If the monitor settings save and the node settings do not, the dialog stays open and
  says which half landed — saving again is safe, both writes are replacements.
- **Edit node is on the inventory tree's right-click menu.** Editing a node meant selecting its row
  first and then finding the button in the detail pane's header. Nodes ▸ All nodes now offers
  "Edit node…" directly under "Open" in any node row's context menu — the same dialog, without
  moving the selection, so the pane keeps showing whatever was already open.
- **Paired fields in a dialog stack on a phone.** Side-by-side pairs inside modals (a DNS
  resolver + port, a URL monitor's status bounds, a credential's kind + name) kept two columns at
  ~390px and squeezed each to half a screen. They now stack, as the non-modal forms already did.
- **Troubleshoot ▸ Tools no longer repeats the analysis-run list.** The panel it showed was a
  verbatim duplicate of Troubleshoot ▸ Analysis runs — the same component over the same job list,
  unfiltered and untruncated — so it only pushed the tool grid down the page. The tool grid now
  follows the stat strip directly; the *running now* counter at the top of the page still shows
  in-flight jobs, and the full list stays one click away under Analysis runs.
- **Settings ▸ Upgrade no longer offers to upgrade a deployment that cannot be upgraded from
  there.** Upgrading from the WebUI needs a container deployment that runs the `yagra-updater`
  sidecar alongside core — the composition in `docker-compose.deploy.yml`. Every other way of
  installing Yagra (natively, or from a composition without that sidecar) has no such mechanism,
  and the page did not say so: it showed the release list, the apply button and the on/off switch,
  and pressing any of them returned a 503. It now says the deployment cannot be upgraded from
  there, points at the command line, and shows none of those controls. What is running, the applied
  schema and the downgrade window are unchanged and still shown on every deployment.
  - Distinct from a deployment that *has* the mechanism whose updater is missing or has stopped —
    that is a fault, and it is still reported as one, as loudly as before.
- **The documented way to install Yagra is now the published images, and it no longer asks you to
  clone anything.** Every install path in the README, the deployment guide and the website led with
  `docker compose up --build` — a from-source build of `docker-compose.yml`, which is precisely the
  composition that has no updater sidecar and no persistent KEK. So the documented first step landed
  new users on a deployment that cannot upgrade itself and loses stored device credentials on
  restart, and, since the change above, says so on the Upgrade page with nothing nearby to explain
  why. The recommended path is now `docker-compose.deploy.yml` — fetched with `curl`, started with
  `docker compose up -d`. It needs no checkout and no build: every variable it interpolates has a
  default, and it bind-mounts nothing outside the Docker socket. `POSTGRES_PASSWORD` is the one
  setting worth choosing before the first start, because Postgres bakes it into the data volume
  then.
  - Building from source is unchanged and still fully documented — it is now addressed to people
    developing on Yagra, auditing it, or making a custom build, with its two limits stated where
    the instructions are rather than a section away.
  - The two Docker compositions swapped letters in `DEPLOYMENT.md`: **A** is now the pre-built
    images and **B** the source build. In-page anchors moved with them; `C`/`D`/`E` did not change.
- **Searching the inventory tree for a group name now shows that group's contents.** On Nodes ▸ All
  nodes, a search term matching a folder returned the folder row and nothing under it: the search
  runs on the server against node names and addresses, so a matched folder's members were never in
  the answer and the row rendered empty next to a health bar saying it had members. A group matched
  by name now loads its whole subtree — sub-folders and every member node, whether or not the node
  itself matches — while a term matching only node names behaves exactly as before. A term matching
  more folders than can be opened at once says so rather than showing some of them empty.

### Bug Fixes
- **A device with more than one check was polled for only one of them — silently, and without
  alerting. Affects v0.2.3, v0.2.4 and v0.2.5; upgrade.** On a typical SNMP node that meant the
  system scalars kept arriving while the interface walk, the vendor health tables, the topology
  walks and even ICMP were discarded on every cycle. Nothing looked wrong from the outside: the
  surviving scalar check *is* the liveness check, so the node stayed `ok`, no gap was recorded, and
  the only visible symptom was graphs that stopped filling in. URL, DNS and ICMP-only monitors carry
  a single check each and were never affected. Three things had to line up. The poller's local
  scheduler hands the worker everything due in one 500 ms tick as a single burst; the worker's
  per-device guard — which exists so a slow device cannot pile polls up — lets one probe per target
  address run and **drops** the rest rather than deferring them; and a dropped check's timer still
  advances a whole interval, so once two checks of one node landed in the same tick they stayed
  there for good. v0.2.3 made that alignment the default by narrowing the window a newly adopted
  check is scheduled in from its poll interval to `checks ÷ 200 per second` — about 70 ms on a small
  deployment, far inside one tick. Note the direction: **the smaller the deployment, the narrower
  the window, so first installations were hit hardest and a large fleet not at all.** A node's checks
  are now placed one second apart by position instead of each drawing its own offset, which is both
  what anti-stampede actually calls for — spreading is between nodes — and a separation that
  arithmetic guarantees rather than luck, including across the harmonically related 60 s / 300 s /
  3600 s tiers where a collision would otherwise have repeated forever. The v0.2.3 handover
  improvement is unchanged. `YAGRA_ADOPT_RATE_PER_SEC=0` remains the escape hatch to the pre-v0.2.3
  scheduling, and is no longer needed to avoid this.
- **A report exported as CSV could carry a spreadsheet formula out of the product.** The exporter
  quoted per RFC 4180 and stopped there, which does not help: a spreadsheet strips the quotes and
  then evaluates the text underneath. Report tables are built from device-supplied strings — a node
  name, an interface description, a `sysDescr` — so a device (or anyone who could set a node's name)
  could plant `=HYPERLINK(…)` and have it run for whoever opened the export. The WebUI had already
  paid for this exact omission once and neutralizes it; the backend had a second encoder that did
  not, which is what a duplicated encoder costs. There is one encoder now, and a test holds it
  against the WebUI's. Exported values are unchanged except that every field is quoted and a
  formula-triggering one gains a leading apostrophe.
- **The completion notice for a Troubleshoot analysis announced every success as a failure.** "Notify
  me" compared the finished run against `succeeded`, which is the word a *report* run uses — an
  analysis run says `done`. The comparison was simply never true, so every run that finished
  normally produced "your analysis failed". `AnalysisJob.state` is a closed set on both sides now,
  which makes that comparison a compile error rather than a silent mismatch; the API additionally
  rejects a filter naming a state nothing writes instead of answering an empty list.
- **The node detail's Collection tab hid every live metric on a URL or DNS monitor.** Its "Arriving
  only" toggle read "arriving" as one status, but the status crosses two facts: one value means
  *configured and arriving* and another means *arriving with no collection set behind it* — which is
  what reachability, `http_up`, `dns_up`, the neighbour count and JSON-extracted values all are. A
  URL or DNS monitor has no collection set at all, so the toggle emptied the list it was meant to
  narrow.
- **Node names showed up as raw UUIDs, and whether they did was a coin toss per page load.** Any
  list that references a node — active alerts, alert history, events, saved findings, pollers —
  resolves ids to names in one batched request. The request was sent from an effect on the component
  that owns the resolver, but the ids are collected while the *cells* render, and a virtualized list
  draws no rows on its first pass and then re-renders **by itself** once it has measured. So the ids
  arrived in a render the owner took no part in, its effect never ran, and the request was never
  sent. What usually rescued it was an unrelated fetch landing a moment later and re-rendering the
  owner — which is why the same screen showed a name on one load and a UUID on the next. The batch
  is now scheduled by the lookup itself, so it no longer depends on which component re-rendered.
  Two further ways a name could never arrive are closed with it: a failed request no longer marks
  those ids as permanently asked (one network blip used to pin them to a UUID until reload, and the
  retry is bounded so it cannot become a loop), and a batch larger than the endpoint's cap is split
  rather than silently truncated to its first 1000 ids.
- **A poller-pool alert appeared in the Flapping watchlist as a broken reference, and took its
  neighbours' names down with it.** An alert about Yagra's own polling coverage has no node, and
  carries `pool:<name>` where a node id would be. The widget rendered that string as an unresolvable
  id instead of naming the pool, and — worse — sent it to the node-name endpoint, where it fails to
  parse and fails the **whole request**, so every other row batched with it lost its name too. The
  triage list and the history table already handled this; the widget was the copy that did not, and
  all three now share one implementation.
- **Alerts ▸ History skipped rows while scrolling, and did it most often during a fleet-wide
  event.** The page's keyset cursor was the last row's `recorded_at` alone. That column defaults to
  PostgreSQL's `now()`, which is the *transaction* timestamp, and Yagra writes a whole flush of
  alert transitions as one multi-row insert — so every row of a flush carries an identical
  `recorded_at`. Whenever a page boundary landed inside a flush, the next request asked for rows
  strictly older than that instant and silently dropped the flush's remaining rows. The bigger the
  incident, the bigger the flush, and the more was lost. `GET /api/v1/alerts/history` now returns
  each row's `id` and accepts `before_id` beside `before`; the two together are the cursor.
  Sending `before` alone still means "strictly before that instant", so an older client is not
  broken by the change. Migration 0082 replaces the `recorded_at` index with `(recorded_at, id)`.
- **The MCP `get_alert_history` tool could not page at all, and its instructions pointed at the
  wrong field.** It advertised a `before` cursor but returned nothing a caller could build one
  from, and its description told clients to page on `at` — the time the alert fired, which is a
  different clock from the `recorded_at` the cursor compares. Rows now carry `cursor_at` and
  `cursor_id`, the tool takes `before_id`, and the description says which fields to use.
- **Settings ▸ Audit hid older matching entries.** The search box and the action / status / time-range
  filters ran in the browser over the pages already loaded, so "last 30 days, DELETE only" examined
  the newest 100 entries and silently dropped every older match — and Export handed the operator
  that same partial set. In a log whose purpose is completeness that is a correctness problem, not a
  missing feature. The filters now run in the database: `GET /api/v1/audit` accepts `q`, `action`,
  `status`, `since` and `until` alongside the existing `limit` and `before`, and the MCP `get_audit`
  tool takes the same parameters through the same code path. The toolbar is unchanged. Export still
  writes the rows loaded so far and now says so.
- **LDAP and OIDC sign-ins were not recognised as sign-ins in the audit log.** Yagra records a
  directory sign-in as `auth.login.ldap` (and `auth.login.ldap_unavailable` /
  `auth.login.ldap_conflict`) beside the local `auth.login`, but the audit view matched the exact
  string — so those entries showed a raw `auth.login.ldap` chip instead of the "Sign in" label, and
  the "Sign in" filter never returned one. All sign-in methods are now matched by prefix, in the UI
  and in the new server-side filter.
- **Actions taken through the MCP tool surface could not be filtered for.** `/mcp` does not pass
  through the REST audit middleware, so an acknowledgement or a triggered poll from an AI client is
  recorded as `mcp.<tool> …` — a shape the action filter's fixed list of HTTP methods could not
  express. "MCP tool" is now one of the choices.
- **An SNMPv3 credential could not be bound to a node from the UI.** Add node and Edit node listed
  only `snmp_v2c` credentials, so a v3 credential could be stored, and even matched against a device
  by Discovery's credential finder, and then never attached to the node it had just authenticated
  against — although the poller has always decrypted and used one. Both pickers now offer v2c and
  v3, which is what Discovery already did. Only those two kinds are ever offered: an HTTP or Meraki
  credential bound as a node's SNMP credential would be sent to the device as a community string.
- **Edit node showed a URL and a DNS monitor an SNMP credential picker they can never use.** The
  dialog rendered the same five device fields — device profile, SNMP credential, maker, model, pool
  — for every kind of node, having never read the node's kind. A URL or DNS monitor is dispatched as
  one HTTP/DNS job and is never SNMP-walked, so a credential bound there was stored, counted against
  that credential's "in use" total (which blocks deleting it), and read by nothing. Each kind now
  gets the fields it actually has: an SNMP credential only on an ordinary device, maker/model only
  where a device reports them, and the device-profile picker restricted to profiles of a matching
  category — with any profile already bound kept in the list so an edit cannot silently re-bind it.
- **Editing a node while the inventory search box held a term emptied the tree.** On Nodes ▸ All
  nodes, every add / edit / move / delete — and every drag inside the tree — reloads the inventory,
  which drops both of the tree's caches: the per-group members and the server-side search page. Only
  the first was re-fetched, so the matches disappeared and did not return until the term was
  retyped, leaving a pane that read as "nothing matches this search" rather than as a failed
  refresh. The search is now re-issued with the rest of the inventory, and the previous matches stay
  on screen until the fresh ones land, so the tree no longer blinks empty.

## v0.2.5 — the tree answers why a row is silent, and lets one node out of a group's window

Suppression was easy to *set* from All nodes and impossible to read or undo there: a row carried a
🔧 or a 🔕 and said nothing about which window put it there, whether it came from the row itself or
from a folder group above it, or when it would stop. This release makes both markers open a panel
that answers all three and offers exactly the one control that fits where the suppression comes
from — including taking a single node out of a group's window without disturbing its siblings.

The rest is the same feature meeting a real screen: two of the three bug fixes below came from using
it on the test server rather than from a test, and one of those — a hover that moved the buttons
already under the pointer — had been in the tree long before any of this and only became dangerous
once the markers became clickable.

### New Features
- **Suppression can now be released from the tree it was set in, including for one node inside a
  suppressed group.** All nodes ▸ the maintenance (🔧) and mute (🔕) markers are now buttons: click
  one and a panel names what is silencing that row — the window's name, whether it comes from the
  row itself or from a folder group above it, and when it stops. Until now the tree could set
  suppression in one click but never say *why* a row was silent, and removing it meant finding the
  row by name on Alerts ▸ Maintenance windows or Alerts ▸ Mutes. The same panel is reachable from
  the right-click menu.
  What each control does depends on where the suppression comes from, and the labels say so:
  - A window that names the row is **ended now** rather than deleted — `ends_at` moves to the
    current time, so the row survives on the Maintenance page as a record of the maintenance that
    actually happened and is swept by the existing "clear ended" button. A mute naming the row is
    lifted, as it always was.
  - A window or mute the row only *inherits* cannot be ended without releasing every sibling under
    it, so a **node** offers "take this node out of maintenance" — or "unmute this node" — instead:
    that one node returns to normal alerting while the rest of the group stays covered. The release
    expires by itself when the coverage it was carved out of ends — the server computes that, the
    browser sends no expiry, and it re-derives it whenever coverage stops sooner than it said it
    would (a window ended early, disabled or deleted; a mute lifted), so a release can never outlive
    its reason and silently exclude the node from the *next* window. A
    released node's marker says so on its own: a suppression **in force** is a filled chip — blue
    for maintenance, yellow for a mute — and a row released from one is drained of the colour and
    outlined in a dashed border instead. Fill-versus-outline is the primary signal and survives
    greyscale, so the hue only has to say *which* of the two it is; the mute glyph also loses its
    slash and goes back to a plain ringing bell. The panel then reports what the row is standing
    outside of and until when, with putting it back as the only thing left to do.
  - A **group** covered by an ancestor's window shows the cause read-only and names the group to
    release it on. Ending an ancestor's window from a child row would silence-and-unsilence a set
    the operator cannot see from there.
  New endpoints: `POST /api/v1/maintenance-windows/{id}/end` (ManageMaintenance; `404` unless the
  window is currently active, which is also what stops an unstarted window being given an end date
  before its start), `PUT /api/v1/nodes/{node_id}/maintenance-exemption` (ManageMaintenance) and
  `PUT /api/v1/nodes/{node_id}/mute-exemption` (AckAlerts) taking `{"exempt": bool}` and answering
  `400 not_suppressed` when the node inherits nothing, and `GET /api/v1/suppression-exemptions`
  (View). The MCP `list_suppressions` tool gains an `exemptions` array — a released node is not
  silenced, and a reader that saw only the window would conclude the opposite.

### Improvements
- **Downloading a support bundle no longer stalls the rest of the API while it is assembled.** The
  last stretch of the request reads up to 24 MB of rotated log files off disk and then runs the
  redaction scan and the gzip over those same bytes — all of it synchronous, and until now all of
  it on a request-handling thread. It now runs on the blocking pool. The bundle itself is
  unchanged; what changes is that a deployment stays responsive while producing one, which is the
  case that matters, because a bundle is requested when something is already wrong.

### Bug Fixes
- **The repair instruction a failed upgrade prints did not repair anything.** v0.2.4 made a
  deployment whose compose labels are stale stop at the first step and name the command that fixes
  it. The command it named was wrong: `docker compose up -d` recreates only containers whose
  definition changed, and after a poisoned apply that is core, web and poller — never the updater,
  whose definition matches the composition it just installed. Since the updater reads its **own**
  label, the one container the command leaves alone is the only one whose label is ever read. It
  printed four green lines and changed nothing, which is worse than failing. The message now names
  `docker compose -p yagra -f docker-compose.deploy.yml up -d --force-recreate yagra-updater`, says
  why the bare form does not work, and no longer tells you to run it from the directory that is
  itself the problem.
  **This affects every deployment on v0.2.3 or earlier**, and it is not avoidable: an upgrade is
  carried out by the updater of the release being *replaced*, so v0.2.4's fix could not apply itself
  to the upgrade that delivered it — and because that fix changes the updater's own service
  definition, the recreate that stamps the bad label is guaranteed rather than possible. So the
  first upgrade after moving to v0.2.4 stops at the first step. Run the command above once on the
  host and every upgrade after it is unaffected; nothing is at risk in the meantime, because the
  run stops before it pulls an image or stops a container.
- **Hovering a row in All nodes moved the buttons that were already on it.** The inventory tree
  reveals its per-row actions on hover (＋ / ✎ / 🗑 on a group, ↗ on a node), and revealing them
  *inserted* them into the row: everything to their left — the maintenance and mute markers, the
  member count, the health bar — slid about 80px across at the moment the pointer arrived. It had
  always done this, and it became dangerous when the markers became buttons, because aiming at a
  group's wrench put **Delete group** under the pointer before the click landed. The markers are
  now rendered *after* the actions rather than before them: a flex row's last child ends flush
  against the right padding whatever happens further left, so the marker cannot move out from under
  a pointer aiming at it, and no row has to give up width to a reserved slot.
- **All nodes went on marking rows as suppressed after the suppression had ended.** The tree loads
  the maintenance-window and mute lists when the page opens and after an action taken on it, and
  nothing refreshed them in between — so a tab left open kept drawing 🔧 and 🔕 on rows that were
  alerting normally again, until someone reloaded it. A row is now judged against the clock rather
  than against a flag that was true when the lists were fetched, and the page refetches at the
  moment the next window or mute runs out. Nothing polls in the meantime.

## v0.2.4 — the second upgrade works, and the backup it takes first has the metrics in it

Two bug fixes, both in the self-upgrade path that v0.2.2 introduced, and both found the same way:
by upgrading a real deployment more than twice. Neither damaged anything, and that is most of the
problem — one failed loudly at the wrong step, and the other succeeded while quietly writing an
incomplete backup. **Anyone running v0.2.2 or v0.2.3 who has upgraded from the WebUI should read
both entries**: the first names a one-command repair, and the second means the pre-upgrade backups
already on disk hold no metrics.

### Bug Fixes
- **A deployment that had upgraded itself from the WebUI could not do it a second time.** The
  updater finds the deployment directory by reading the compose label its own container carries, and
  an upgrade recreates the stack by running `docker compose up -d` from inside a throwaway container
  — which stamps *that container's* directory onto every container it creates, rather than the
  host's. As soon as one of those recreated containers was the updater, the next upgrade mounted a
  directory that exists on no host, Docker obligingly created it empty, and the run died at the
  first step reporting `the pre-upgrade backup failed`: the wrong step, in the wrong component. The
  upgrade container now sees the deployment directory at the path the host knows it by, and a run
  that cannot find the composition says so and names the command that repairs it. Nothing was ever
  damaged — it failed before pulling an image or stopping a container.
  **If a deployment is already affected** — upgrades from Settings ▸ Upgrade fail at the backup step
  within a second — run, once, on the host, from the directory that holds this deployment's
  `docker-compose.deploy.yml`:
  `docker compose -p yagra -f docker-compose.deploy.yml up -d --force-recreate yagra-updater`.
  That re-stamps the label, and the WebUI works from then on. (Corrected after publication: this
  originally named a bare `up -d`, which leaves the updater running untouched — its definition has
  not changed — and therefore repairs nothing. The updater reads its own label, so it is the one
  container that has to be recreated.)
- **Pre-upgrade backups contained no metrics, and nothing said so.** `yagra-backup.sh` asks
  VictoriaMetrics for a snapshot over HTTP from inside the stack, and it asked through
  `docker exec core` — but the core runtime image ships neither `wget` nor `curl`, so the call
  returned nothing and the script took its "no snapshot name in the response" branch. That branch is
  deliberately not fatal (a site with no metrics store still needs its configuration backed up), it
  reports itself in a line of output, and the updater discards the upgrade container's output — so
  every backup taken this way held the database and the KEK, an empty `vm/` directory, and
  `"yagra_version": "unknown"`. Both calls now go through a container that has an HTTP client, the
  manifest carries `metrics_snapshot` (a name, or `null` — a claim rather than an omission), and the
  closing summary says in words when a backup carries no metrics.

## v0.2.3 — the upgrade reaches every site, one poller at a time, and a handover no longer costs samples

### New Features
- **A remote-site poller can now be upgraded by the same button that upgrades the deployment.**
  Until now Settings ▸ Upgrade replaced core, the WebUI and a co-located poller — everything in one
  compose project — and left every poller at a monitored site untouched, with nothing on screen
  saying so. Add the `yagra-poller-updater` sidecar to `docker-compose.poller.yml` (shipped
  commented out, and off unless you uncomment it) and the site declares itself able to install a
  release; core then hands it the same one it just installed, **one poller at a time per pool**, so
  a pool with two or more pollers keeps monitoring throughout. The poller never touches the Docker
  socket: it validates the command and writes it into a shared volume for the sidecar, exactly as
  core does centrally. Images are fetched *before* anything stops, so a single-poller site is out
  for the container recreate rather than for the download.
- **The Upgrade page says which pollers come along and which do not.** The plan and the confirmation
  both name the pollers that will stay on their current version, so a partial upgrade is something
  you decide rather than something you discover. `GET /api/v1/system/upgrade` gained a `pollers`
  object (`with_core`, `manual`); it is `null` — not an empty list — until the updater reports which
  pollers share its compose project, because "nobody is left behind" and "nobody asked" are
  different answers.
- **Ended maintenance windows can be cleared in one action.** The windows list keeps every window
  until someone deletes it, so an operator who schedules recurring work reads a page that is mostly
  history — and every upgrade adds one more, at the top. Alerts ▸ Maintenance windows now has a
  "Clear ended" button beside the count, backed by `DELETE /api/v1/maintenance-windows?status=ended`
  (manage-maintenance). The server's clock decides what has ended and the caller's group scope
  decides which rows are eligible — the browser sends no list of ids — so a window still running or
  still scheduled is never removed, and a group-scoped account clears only what it can see. The
  response says how many rows went. `status` is required and must be `ended`: a bare `DELETE` on the
  collection is refused rather than read as "delete them all".
- **Settings ▸ Upgrade says when it last looked for a release, and has a button to look again.**
  The updater checks the registry every 24 hours (`YAGRA_UPGRADE_CHECK_SECS`), which means the hour
  a release is published is the hour the list is guaranteed to be out of date — the one hour anyone
  opens this page. `POST /api/v1/system/upgrade/check` (manage-configuration) asks the updater to
  re-read it now; the page shows the result landing. It installs nothing and takes no argument: the
  repository queried is still fixed by the host, and the command is refused while the mechanism is
  switched off, so a deployment that makes no outbound connections still makes none.

### Improvements
- **A poller taking over another poller's nodes now polls them promptly instead of after up to a
  full interval.** When a poller left a pool — a restart, a failure, a rolling upgrade, a scale-in —
  the survivor treated the arriving nodes as brand new and spread their first poll uniformly across
  the poll interval. The previous owner had already stopped, so consecutive samples for those nodes
  could be up to **twice the interval** apart (a minute at the 30s default), and nothing reported
  it: the pool still had a live poller, so no coverage alert fired, and `monitoring_gaps` records
  core↔poller visibility rather than a missed poll. Adopted work is now spread over how long it
  takes to poll it once at a sustained rate (`YAGRA_ADOPT_RATE_PER_SEC`, default 200 specs/s,
  clamped to the interval), so a handover of fifty nodes completes in a fraction of a second while a
  full cold start behaves exactly as before. Set it to `0` for the previous behaviour.
- **A poller returning to the fleet is given its nodes back at once.** A departing poller already
  triggered an immediate reassignment; a rejoining one waited for the next scheduler sweep — up to
  the fleet-minimum poll interval — during which its share stayed with whoever had covered for it.
  Both directions are now immediate.
- **A poller shutting down now finishes what it started.** It waits (up to 5s) for probes already in
  flight to report before exiting, instead of dropping their results with the runtime, and it
  confirms its final "I am leaving" heartbeat has actually left the process. That heartbeat is what
  lets core reassign immediately rather than waiting out three missed beats, and it could previously
  be lost to the race with process exit.
- **`GET /api/v1/pollers` now returns each poller's `caps` and `listeners`,** and Settings ▸ Pollers
  marks a poller that is not on core's version and one that can upgrade itself. The listener list is
  worth reading before restarting a site: syslog, traps and flow exports have no buffer and no
  standby, so unlike active polling nothing recovers what arrives while a poller is down.
- **An upgrade that would strand a poller two releases behind is now refused rather than offered.**
  The bus supports one release of skew (N/N-1); ADR-050 wrote that down and nothing enforced it, so
  the jump it called dangerous was as pressable as any other. Such a release now appears in the list
  with the reason and a disabled button — visible, because a version that looks absent reads as
  "never released". Only pollers the operation will *not* upgrade count, so a deployment whose
  pollers all come along is never blocked.
- **The Upgrade page now shows what an upgrade is doing while it does it.** A progress bar and the
  named phase — backing up, fetching images, replacing containers, verifying — replace a screen
  that changed in no visible way for the ~65 seconds an upgrade takes. It also distinguishes the
  connection being lost *because the containers are being replaced* from the page having frozen,
  which previously looked identical.
- **Less text on the Upgrade page.** The explanatory paragraphs are cut to a sentence each and three
  are gone entirely; the release buttons now read "Upgrade" / "Downgrade" rather than a sentence
  repeating the version number already printed beside them.

### Bug Fixes
- **A disabled maintenance window whose time had passed read as "disabled" forever.** The status
  column tested `enabled` before it tested the clock, so a window that was switched off and then ran
  out never reached the `ended` badge — it sat in the list looking like something still waiting to be
  turned on. It now reads `ended` once its end time has passed, which is also what the new bulk clear
  counts. A window the server reports as currently active is still never shown as ended, whatever the
  browser's clock says.
- **The Upgrade page went silent for the whole of an upgrade.** After the confirmation dialog closed
  it showed nothing — no progress, no state change — until the page was reloaded by hand. It armed
  its polling from the server's run state, but the updater only looks for a request every five
  seconds and then has to start a container, so for the first 5–10 seconds the server was still
  reporting the *previous* run as finished. The poll was therefore never armed at all. It now starts
  from the request being accepted. The same window let a second release be clicked, which the
  backend's own conflict check could not see either; both are closed.
- **An upgrade no longer silences the whole fleet for 15 minutes after it has finished.** Applying an
  upgrade opens a deployment-wide maintenance window so the restart cannot alert about itself. The
  window was only ever bounded — nothing closed it — so a 65-second upgrade left every node in
  `maintenance`, and every real outage invisible, for the remaining ~14 minutes. The core that comes
  back now closes the window as soon as the run reports its outcome. The bound remains as the
  backstop for a run that never reports at all.
- **A completed upgrade is recorded in the audit log again — it never was.** core is recreated by the
  `compose` step of the run it is watching, so it came back while the run still read `running`,
  checked once, and never looked again; the updater wrote the outcome seconds later. The audit trail
  showed somebody requesting an upgrade and never showed whether it worked. core now waits for the
  outcome. The row reads `upgrade succeeded -> v0.2.2 (…)`, attributed to whoever pressed the button,
  and a failed run is recorded just as a successful one is.
- **The Upgrade page gave the wrong reason for a version it will not go back to.** It stated that the
  migration in question had narrowed the schema, which was not true of the only floor that exists:
  releases before 0.2.2 cannot start against a newer database because the tolerance for one first
  shipped in 0.2.2, not because anything was narrowed. The floor's own recorded reason was printed
  correctly right after it, and contradicted the sentence introducing it. The page no longer asserts
  a cause of its own.

## v0.2.2 — Yagra installs its own next version, says how far back it can be taken, and needs no registry to do it

### New Features
- **Settings ▸ Upgrade — Yagra can now say which build is running and how far back it can be taken**
  (ADR-050). `GET /api/v1/system/upgrade` (manage-configuration) reports the running binary's commit
  and build profile, how many migrations are applied, and the *compatibility floor* — the oldest
  version that can still run against this database. Until now the commit and build profile were
  readable only from inside a support bundle behind three permissions, which meant the question a
  green pipeline cannot answer ("is this actually the binary I built?") had no cheap way to be
  asked. The same answers are on the MCP surface as `get_system_health(section="upgrade")`.
- **Upgrade Yagra from the WebUI.** Settings ▸ Upgrade lists the releases it can move to and does
  the whole thing: backup → pull → install the composition out of the target image → recreate →
  verify, carried out by a new `yagra-updater` container. **core never gets the Docker socket**; the
  sidecar holds it, exactly as `ipasn-updater` holds the only outbound network path.
  **This is now the ordinary way to upgrade** — `/release` no longer deploys anywhere, and a
  deployment takes a new version when its operator presses the button.
  - **It ships switched on, and there is a switch on the page to turn it off.** Off means no upgrade
    can be requested and the updater stops contacting the registry; the container still exists.
    The setting lives in the database, so it survives the upgrades it governs — deleting the service
    from your compose file does not, because each version installs its own composition.
  - ⚠️ **This creates a path from Yagra's Admin role to root on the host**, which did not exist
    before. What bounds it: the image repository is fixed by the host environment, so an Admin can
    install a tag we published and nothing else; the command set is closed; and nothing in the
    shared volume is ever executed. Requesting an upgrade needs manage-configuration **and**
    manage-credentials, and none of it is on the MCP surface.
  - **Where releases are looked for is a host setting of its own** — `YAGRA_UPGRADE_REPO`, defaulting
    to the public GHCR repository — and deliberately not "wherever the running containers came from".
    A deployment pulling its images from a private mirror still finds the real release list.
  - The registry is checked once a day.
  - Each release is labelled with the direction it moves you, and a version that cannot read the
    current database is shown greyed out with the reason rather than hidden. Releases before 0.2.2
    are in that category for every deployment: the startup relaxation that lets a core run against
    a database migrated by a newer one first ships here, so there is nothing older to return to
    except from a backup.
- **Upgrading with no registry at all.** `POST /api/v1/system/upgrade/bundle` takes a `docker save`
  archive of a release's three images, streams it to the shared volume and installs it — the same
  backup, the same composition-from-the-target-image, the same provenance check afterwards. Only
  step two changes: `docker load` instead of `docker pull`, followed by a check that the archive
  really contains the release the operator named. It needs a **second** opt-in beyond the sidecar
  itself (`YAGRA_UPGRADE_ALLOW_BUNDLE=1`), because it is the one path that can put an image on the
  host that nothing in the composition names. A site with a reachable registry should leave it
  closed.

### Improvements
- **Downgrades are possible again, within a declared window.** A new `schema_compat` table lets a
  migration that narrows the schema declare the oldest version that can still run afterwards;
  saying nothing means reversible, which is true of all 77 migrations shipped so far. core now
  starts against a database migrated by a *newer* core when — and only when — every unrecognised
  migration is newer than everything it embeds. Anything else (a gap in the middle, a foreign
  history) still refuses to start, as it must. **Nothing is deleted by going back:** columns the
  newer version added stay in place, unread, and become visible again on the next upgrade.
  ⚠️ This takes effect for downgrades *to* releases that contain it, so the window opens from the
  release after this one.
- **`yagra-core migrations`** prints the migration set a binary embeds as JSON, with no database and
  no configuration — so an upgrade can be planned by running it inside the target image before
  anything is touched.
- Maintenance windows gained a fleet-wide `system` scope, used by the upgrade path to silence
  self-monitoring for a bounded period. It is deliberately not offered in the add-window dialog:
  nothing but an upgrade should be able to silence the whole fleet.

## v0.2.1 — The inventory says what each node is, tabs a kind cannot fill are gone, and ＋ adds a node

### Breaking changes
- **The node list now reports what each node *is*.** `GET /api/v1/nodes` and
  `GET /api/v1/nodes/by-group` replace the two-valued `source` field with `kind` — the same
  `meraki` / `url` / `dns` / `device` value `GET /api/v1/nodes/{id}` already returned, resolved by
  the one function the scheduler also asks, so a list row can no longer disagree with the detail
  page it opens. `source` is removed rather than deprecated: it was a lossy projection added for a
  single tree badge, and it would have read `"device"` for every monitor kind added since and every
  one added next. Mapping for API clients: `source == "meraki"` becomes `kind == "meraki"`, and
  everything that was `"device"` is now one of `device`, `url` or `dns`. The MCP `list_nodes` and
  `get_node_status` tools carry the same `kind`, so an AI client can tell a URL monitor from a
  switch before asking which metrics it has.

### Improvements
- **A node's detail page now shows only the tabs its kind can fill.** A URL or DNS monitor shows
  Overview and Collection; a Meraki device adds Events. Interfaces, Neighbors and Flow are gone from
  those kinds — not because they were empty today, but because they cannot ever be filled: a URL or
  DNS monitor is dispatched one HTTP or DNS check and is never SNMP-walked or pinged, and a Meraki
  device is polled through the Dashboard API with no per-node job at all. Nothing becomes
  unreachable: passive events for any node remain at **Alerts ▸ Events** filtered by node, and none
  of these kinds is ever a NetFlow exporter. A bookmarked link to a tab a node does not show now
  lands on Overview *and* corrects the address bar, instead of drawing a pane with no tab selected.
- **The Overview of a monitor stops describing a device it is not.** A DNS monitor used to lead with
  an empty "ICMP RTT · last 30 min" chart and a facts grid whose Maker, Model, SNMP credential and
  Uptime were four consecutive em dashes — all of them answers to an SNMP walk that kind never
  receives. The chart is now shown only where ICMP is actually sent, the grid lists only rows that
  can hold a value (a DNS monitor gains a Resolver row instead), and the line under the node's name
  reads `wg.example.net · A` rather than `0.0.0.0 · unknown device`. Each kind's "last seen" is now
  read from the metric it actually produces, so monitors show one at all.
- **URL, DNS and Meraki nodes are now labelled as such in the inventory.** A short `URL` / `DNS` /
  `Meraki` badge sits after the name in the node tree, in a group's member list and beside the title
  on the detail page, with the full kind on hover. Ordinary devices stay unmarked, so the badge means
  "read this one differently". Previously only Meraki was distinguishable and a URL monitor was
  indistinguishable from a switch.
- **The Collection tab no longer shows a `0` count pill** on nodes whose profile attaches no
  collection sets — which is every URL and DNS monitor. A zero there read as a fault; it now shows
  no pill, matching the Interfaces tab's existing rule.
- **The inventory's ＋ adds a node, not just a group.** It now opens a two-item menu — add node, add
  group — and both act on whatever the tree has selected: with a group (or a node inside one)
  selected they read `Add node to "Tokyo"…` / `Add subgroup in "Tokyo"…`, and with nothing selected
  they file at top level. Adding a node used to be reachable only by right-clicking a tree row,
  which is undiscoverable on a desktop and impossible on a touch device, where `contextmenu` never
  fires — so on a phone the inventory could grow folders but not nodes. The ＋ on a group row offers
  the same two, scoped to that group.
- **The Add node dialog now has a Group field.** It is preselected from wherever the dialog was
  opened and can be changed before saving, instead of the previous read-only "Adding to *top
  level*" note. This also fixes a node added from a selected group's pane landing at top level
  regardless of the group.
- **"Edit node" is now on the node pane of the Nodes page**, not only on the full detail page.
  Changing a node's profile, credential, maker/model or poller pool from the inventory split meant
  first pressing "Open detail" — a button that reads as navigation, not as editing, so the edit path
  was effectively hidden. It sits beside "Move…" and opens the same dialog.

### Bug Fixes
- **A DNS monitor's resolution history no longer paints its status chip over the next column.** The
  Resolution column was 140px wide while "No such name (NXDOMAIN)" — and its Japanese equivalent
  more so — needs half again as much, and a status chip is deliberately never squeezed, so it
  overflowed at every browser width. The column is now sized for the longest label in either locale,
  and the chip truncates with an ellipsis (full text on hover) rather than spilling if a future label
  or a wider system font still does not fit.

## v0.2.0 — The first public release: every metric a device reports, URL monitors that read the body, and an SSO form that knows your IdP

**This is Yagra's first public release.** Everything up to v0.1.23 was developed in a private
repository and never published; those notes are kept below as the record of how the system got here,
not as releases you could have been running. If you are arriving now, v0.2.0 is the beginning —
there is no upgrade to perform and no earlier published version to upgrade from. The minor bump
rather than `1.0.0` is deliberate: what changed is that the source and the images are public, not
that the API has stopped moving.

Three things worth reading before you deploy it:

- **Yagra is in open beta.** It is a working stack, but most of what these notes describe has not yet
  been validated in anyone else's production network. Run it alongside your existing monitoring
  rather than in place of it, and treat the feature list as *what is built*, not as *what has been
  proven at scale elsewhere*.
- **Bug reports are the most useful thing you can send**, and they go to
  [GitHub Issues](https://github.com/horryworks/Yagra/issues) — which is also the **only contact
  channel for this project**, questions and commercial-licensing inquiries included. There is no
  contact e-mail address. The one exception is a security vulnerability, which goes through
  [private vulnerability reporting](https://github.com/horryworks/Yagra/security/advisories/new)
  rather than a public issue. Pull requests are not being accepted yet — see
  [CONTRIBUTING.md](CONTRIBUTING.md).
- **Start from [DEPLOYMENT.md](DEPLOYMENT.md), not from `docker-compose.yml`.** The single-node
  compose file is an evaluation stack: default database credentials, an ephemeral key-encryption key
  (so stored device credentials do not survive a restart), and a self-signed certificate. What a real
  deployment changes is listed there, and the security-relevant essentials are in
  [SECURITY.md](SECURITY.md).

### New Features
- **URL monitors now record how long the endpoint took to answer**, as the new `http_response_time_ms`
  gauge, shown on the node's Overview and available to thresholds like any other metric. Until now a
  URL monitor could say whether an endpoint was up but nothing about whether it was slow — the
  probe has always measured this and simply discarded it. Two things worth knowing:
  - It is time to the response **headers**, not to a completed body: the probe does not read the
    response body.
  - **Nothing is recorded when the endpoint did not answer.** A timeout would otherwise appear as a
    flat "slow response" for the whole outage, and a latency threshold would page for the same
    incident `http_up` already covers.
  - No default threshold is seeded — response latency varies too much between environments for one
    to be right. Set one per profile, group or node.
- **URL monitors can now check the response body for a keyword.** A monitor may require that the
  body *contains* a keyword, or that it *does not* — the case that catches an endpoint answering
  `200` while its body says the service is broken, which availability monitoring structurally cannot
  see. Configure it on the node's URL-monitor dialog; it also rides on `PUT
  /api/v1/nodes/{id}/url-check` as the optional `body_match` object (`pattern`, `mode`, `max_bytes`)
  and is reported by the MCP `get_config` surface. What it reports:
  - `http_body_match` — `1` satisfied, `0` not. A `below 0.5` critical threshold is seeded on the
    built-in URL profile, so a rule alerts without a second configuration step. Existing monitors
    are unaffected: the metric is emitted only for a monitor that carries a rule.
  - `http_body_truncated` — diagnostic, `1` when the body outgrew the read budget.
  - **A body larger than the read budget reports "not satisfied", never "satisfied".** Truncating
    silently would let a `must not contain` rule report healthy about a page whose error text sits
    past the cut. Raise `max_bytes` (default 65536, up to 1048576) if a legitimate keyword is
    landing beyond it.
  - Matching is plain, **case-sensitive** substring matching — not a regular expression.
  - `GET`/`POST` only; a body rule on a `HEAD` monitor is refused (`body_match_needs_body`), because
    a HEAD response has no body and the rule could never be satisfied.
  - **Rolling upgrades:** a poller that has not been upgraded is not sent a content-checked monitor
    at all, and the withheld count is recorded on `yagra_specs_withheld_total{cap="http-body"}`. An
    older poller would drop the rule, never read the body, and report `http_up = 1` — a green
    dashboard for the exact outage the rule was guarding against — so the check pauses rather than
    reporting a result it did not compute. The same gate `http-auth` already uses.
- **URL monitors can now record numbers out of a JSON response body**, under metric names the
  operator chooses — a queue depth, a replication lag, a worker count. Configure up to 8 per monitor
  on the URL-monitor dialog, or as the `json_extract` array on `PUT
  /api/v1/nodes/{id}/url-check` (`metric` + `path`); the extracted values appear on the node's
  Overview and can carry thresholds like any other metric.
  - The path is **dot-separated and names exactly one value** — `data.queue.depth`,
    `items.0.value` (`items[0].value` is accepted and means the same). It is deliberately *not*
    JSONPath: a rule that could select many values would need a reduction nobody asked for.
  - Numbers, booleans (`true` → 1) and quoted numbers (`"42"`) are recorded. **Anything else
    records nothing for that poll — never a zero**, because a zero is indistinguishable from the
    value genuinely being zero. The same applies when the body is not valid JSON or was truncated.
  - A metric name must be a valid TSDB name and **may not be one the monitor already reports**
    (`http_up`, `http_status_code`, `http_response_time_ms`, `ssl_cert_days_to_expiry`,
    `http_body_match`, `http_body_truncated`) — that would overwrite the node's own availability
    series. Two rules may not write the same name.
  - Unlike the keyword check, extraction is **not** withheld from an older poller: it would record
    nothing (a visibly absent series) rather than a wrong reading, and withholding would stop the
    whole monitor including `http_up`.
- **`body_max_bytes` is a property of the monitor**, not of the keyword rule — one body, one read,
  one budget, shared by the keyword check and extraction. Default 65536, range 1024–1048576.
- `http_response_time_ms` still measures time to the response **headers** even when a body feature
  is configured, so the metric means the same thing on every monitor.

- **Every metric a node collects is now visible, not just the ones the UI was written to know
  about.** The node's Collection tab lists them all and charts any of them; until now it showed the
  latest value of *scalar* metrics only, with no history, so an operator who added a vendor table
  column could watch it collect successfully and never see a number. The new
  `GET /api/v1/nodes/{id}/metrics` answers it, and the MCP tool `list_node_metrics` mirrors it.
  Each entry states three things:
  - `status` — `ok` (configured and flowing), `no_data` (configured, nothing has arrived) or
    `unconfigured` (data exists with no collection item). The last is normal rather than a fault:
    reachability, the URL and DNS monitors, the neighbour count and values extracted from a
    monitored JSON response all come from checks rather than from a collection set, and so were
    invisible to every screen driven by that set. `snmp_neighbor_count` is chartable for the first
    time as a result.
  - `dimension` — `none`, `interface` (read those per interface instead) or `entity`, meaning one
    series per table row. Row identity is folded away when the values are collected, so these are
    shown as a node-wide maximum and labelled as such rather than implying a per-row breakdown
    that cannot be produced.
  - `metric_kind` — gauge or counter, which decides how it may be charted.
- **A counter can now be charted as a rate.** `GET /api/v1/nodes/{id}/metrics/{metric}/range` takes
  `rate=true`, returning the per-second rate instead of the stored values. There was previously no
  way to chart a node-level counter at all: its stored value is an odometer reading, and `agg=max`
  over one draws a rising line that looks like traffic and is not. `rate` cannot be combined with
  `agg` (`rate_with_agg`) — a per-entity counter has no node-level rate.
- **A new "Metric chart" dashboard widget charts any metric of any node.** Pick a node, pick one of
  its metrics from the list the device actually reports, and the widget draws the last 6 hours.
  Every other card on the board answers a question the catalog decided in advance; this one is for
  the metric your devices have and ours do not — a vendor temperature, a value lifted out of a
  monitored JSON body. Add it as many times as you like. Two behaviours worth knowing:
  - **It offers only what it can draw honestly.** Per-interface metrics are not listed — the node's
    Interfaces tab charts those per port, and collapsing eight ports to one line answers a different
    question. A counter is charted as a per-second rate, never as its stored value.
  - **A metric that stops being available says so.** If the node no longer reports the selected
    metric, the widget names it instead of drawing an empty chart.
- **A new "Top nodes by metric" dashboard widget ranks the whole fleet by any metric name.** The
  curated Top RTT / CPU / memory cards each rank one thing the catalog chose; this one ranks whatever
  you type, current value or trailing-hour peak, so a vendor metric no card covers can still answer
  "which of my devices is worst". Three behaviours worth knowing:
  - **The metric is typed, not picked from a list.** There is deliberately no fleet-wide metric
    catalogue to choose from — enumerating every series across every node is the one query that does
    not survive a large fleet. The field suggests the metrics this browser has already seen on nodes
    you opened, and accepts anything else.
  - **A counter is refused rather than ranked.** Ranking a counter's stored value ranks how long each
    node has been up, not how busy it is. Use the interface Top-N cards for traffic and errors, or
    chart the counter as a rate on a node.
  - **An empty ranking says why it might be empty.** Nothing reporting the metric and a name that
    does not match are indistinguishable from the browser, so the message admits both rather than
    implying the fleet is idle.

- **Adding an SSO provider now starts by picking which identity provider it is** — Microsoft Entra
  ID, Okta, Google Workspace, or "Other" for anything else — and then asks only for what that
  product needs. The form used to present eight free-text fields and assume the operator already
  knew what their IdP wanted, which was not a safe assumption: the scopes it pre-filled included
  `groups`, and **Entra ID rejects any non-standard scope outright, so an Entra deployment never
  reached a sign-in page at all**. Dropping `groups` everywhere was not the fix either — it is
  exactly how Okta delivers group membership. What each product now contributes:
  - **Entra ID** asks for the directory (tenant) ID and builds the issuer URL from it. It requests
    only the standard OIDC scopes, and says that the groups claim is turned on in the app
    registration's token configuration and arrives as group object IDs.
  - **Okta** asks for the org domain and builds the issuer URL from it, requesting the `groups`
    scope its org authorization server serves. A custom authorization server has a different issuer
    and belongs under "Other", which the form says.
  - **Google Workspace** has one issuer, so there is no URL to enter. **Its group→role mapping is
    gone, deliberately**: Google does not put group membership in the ID token, so a mapping
    configured against it could never match. The form explains this and requires a default role,
    since without one every sign-in would be denied.
  - **"Other"** is the previous form, unchanged, for any other OIDC provider.
  - **Existing providers are untouched.** They list and reopen as "Other", which is what they are —
    they were defined field by field. Nothing about how a configured provider signs users in has
    changed, and reopening one whose issuer this form does not build leaves it exactly as stored.
  - The API gains an optional `kind` on `POST`/`PUT /api/v1/settings/oidc` and reports it on the
    listing (and through the MCP `get_config` surface). Omitting it means `generic`.
  - ⚠️ **This has not been exercised against a live Entra, Okta or Google tenant.** What each
    product accepts is taken from its published documentation.

### Improvements
- **Device health and the metric list on a node's Overview no longer require admin rights.** They
  were read from the collection-set endpoint, which requires ManageConfig, so a Viewer saw a single
  built-in metric and no health gauges at all. They now come from the metric inventory, which is a
  read.
- **The Japanese UI copy was swept end to end.** It had accumulated two spellings for the same
  terms, English sentence structure carried through the translation, and notation that drifted
  between screens. Wording changed on most screens; no behaviour did. The conventions are now
  written down so new strings stay consistent.
- **The scheduler no longer queries `url_checks` once per node per sweep.** URL-monitor ids are now
  preloaded once a round, the way DNS monitors already were, so a fleet of tens of thousands of
  ordinary devices stops paying one database round trip each, every polling round, to discover it
  has no URL check.
- **The WebUI loads the Topology, Troubleshoot and Settings sections on demand.** Everything on an
  operator's daily path — dashboard, nodes, alerts — still arrives in the first download, but the
  ~25 screens behind those three sections no longer do, so opening the dashboard stops fetching the
  world-map outline, the report registry and seventeen settings pages nobody asked for. Moving
  between screens *inside* a section is unaffected: a section loads once, then behaves as before.
- **Two fleet-scale paths stopped doing work proportional to the fleet on every round.** Neither is
  visible on a small deployment; both were the difference between a steady state that costs nothing
  and one that does not. The scheduler no longer deep-copies every node's resolved check set — with
  its decrypted credentials, OID column lists and route-probe plans — once per pool per sweep just
  to discover nothing changed, and the coordinator now holds its registry lock only while reading
  membership, so poller heartbeats and the Pollers view no longer queue behind a working-set diff.
  Alert evaluation resolves each metric's threshold once per poll result rather than once per
  sample, which matters most for a wide SNMP table where one result carries a hundred samples of
  the same metric. The dwell window still sees every sample individually — only the repeated
  lookup went away.
- **The Nodes tree no longer rebuilds on every live status update.** The status dots arrive over a
  stream that flushes for any node in the fleet; the tree was being reconstructed on each flush
  even when nothing on screen had moved.

### Bug Fixes
- **The dashboard summary, the per-group tallies, the network map and the inventory report no longer
  disagree with the Nodes page about a node's state.** All five surfaces are supposed to apply the
  same rule — the alert engine's opinion when it has one, otherwise a recent ICMP sample means `ok`
  — but the rule had been written out by hand in five places and two of them had dropped the
  fallback. The visible symptom was a core restart: for the minutes before the first sweep the
  dashboard reported `unknown` for the same nodes the Nodes page beside it was showing as `ok`, and
  a PDF inventory report generated in that window printed `unknown` down the whole column. The rule
  now lives in one place. Deliberately unchanged: the **fleet health timeline** still records the
  raw engine view, so the post-restart gap remains visible in history — a historical record should
  say what was actually being monitored at that moment.
- **A Cisco Meraki organization collecting the `inventory` tier showed the raw key
  `meraki.tier.inventory` instead of a label.** The API accepts all four tiers and the org list
  prints what is stored, but only three of them had ever been given a name. The cadence dialog
  still offers three checkboxes — inventory is a reconciliation triggered from "Import devices",
  not a recurring collection, so a checkbox for it would promise polling that never happens.

### Security
- **CSV exports can no longer carry a spreadsheet formula that executes when the file is opened.**
  The audit log records the username submitted to a *failed* login, so anyone who can reach the
  sign-in page could plant a cell reading `=HYPERLINK("http://…"&A1,"Click")` and have it evaluated
  by the administrator who later exports and opens the log; the Troubleshoot exports carry
  device-supplied strings with the same problem. RFC 4180 quoting does not help — a spreadsheet
  strips the quotes and then evaluates the text. Values beginning with `=`, `+`, `-`, `@`, TAB or CR
  are now prefixed with an apostrophe so the cell is read as text. A value that is entirely a
  negative number (`-5`, `-0.31`) is exempt and still exports as a number: the Troubleshoot reports
  export correlation coefficients, and neutralizing those would leave a column that is text for half
  its rows and numeric for the other half — which sorts wrong rather than merely looking wrong.
- **PDF report rendering no longer has access to the container's filesystem.** Reports are rendered
  by `wkhtmltopdf`, which was being invoked with `--enable-local-file-access` for no reason: the
  generated document is self-contained (inline styles, inline SVG charts, no images, no links, no
  `url()`), so nothing in a report ever needed to read a file. The flag is what would turn an
  escaping mistake in the device-supplied text of a report into a local-file read, so it is now
  passed as `--disable-local-file-access` rather than left to a default that a future version could
  flip back.
- **The test transport that reports every device reachable is no longer compiled into the shipped
  binaries.** It is now behind a feature that only test builds enable.

## v0.1.23 — An alert for monitoring's own blind spot, a support bundle, and an RCA that investigates

### Breaking changes
- **An alert's `node` field is now its *subject*, and is no longer always a UUID.** For a
  poller-pool alert it reads `pool:<name>`. This affects `GET /api/v1/alerts`, `GET
  /api/v1/alerts/history`, the `/api/v1/stream/alerts` frames and the MCP alert tools. Every
  response carrying an alert now also carries `subject_kind` (`node` | `pool`) and, for a named
  subject, `subject_name` — **branch on `subject_kind` before treating `node` as a node id.**
  - On history rows and in the MCP DTOs, `node` / `node_id` is `null` for a non-node subject rather
    than a made-up UUID.
  - `POST /api/v1/alerts/ack` now takes **either** `node` (unchanged) **or** `subject`, the alert's
    flat subject form. `node` is no longer required; sending neither is a `400 invalid_subject`.
  - Node-oriented aggregates are unaffected and stay node-only: `/api/v1/alerts/top-nodes` and
    `/api/v1/alerts/transitions` never report a pool.
- **Four previously unbounded tables now declare a retention policy (ADR-040).** Troubleshoot
  analysis runs and their findings, generated AI root-cause reports, and monitoring-gap records are
  pruned on a schedule; the interface map is declared kept-until-node-deletion, with its reason. The
  first three had grown without limit since they were introduced — migration 0026 says "no auto-trim
  yet" in as many words, and scheduled analyses have been writing to that table on a cadence since.
  - A new **Diagnostic data** window (default 90 days) in Settings ▸ System settings covers analysis
    runs and RCA reports. It is deliberately separate from "Report runs": a window's *name* must not
    silently govern a second kind of data.
  - Monitoring gaps follow the alert-linked window instead, because a gap explains an absence of
    alerts and is only readable beside the history it explains.
  - The interface map is **not** pruned by age, and the reason is in the policy table: an orphaned
    `(node, ifIndex)` row is a stale identity rather than old data, and `last_seen` only advances
    while interface collection is running — so an age-based sweep would erase the names and speeds
    of every interface on a node whose polling was merely paused. Those rows disappear with their
    node. The real fix belongs in the poller and is a separate change.

### New Features
- **Yagra now alerts when a poller pool stops having a poller.** This was monitoring's own blind
  spot. The alert engine reasons about poll results, so when a pool loses its last live poller
  there is nothing for it to reason about: the scheduler falls back to publishing jobs on a subject
  nothing is subscribed to, plain NATS discards them, and the nodes drift to *unknown* rather than
  *down*. An entire site stops being monitored and every dashboard stays calm. Until now the
  condition was visible only to someone already looking at Settings ▸ Pollers.
  - Delivered over your configured notification channels at **critical** severity, so an existing
    `critical → PagerDuty` routing rule reaches it, and closed automatically when a poller returns.
  - **A pool must be uncovered for five minutes before it notifies** — a poller announces its own
    departure, so an ordinary rolling restart raises the condition instantly and the debounce is
    what stops that paging anyone. Tune or disable it with
    `YAGRA_POOL_COVERAGE_ALERT_AFTER_SECS` (default `300`, `0` = off).
  - Two new gauges regardless of that setting: `yagra_pools_without_live_poller` (unlabelled — the
    one to alert on, or to drive a scale-up from) and `yagra_pool_nodes_without_live_poller{pool}`,
    which reports `0` for a healthy pool rather than disappearing.
  - Meraki-managed nodes are excluded, as they are from the Pollers page: core's org collector
    polls them, so they do not depend on a pool.
  - **It is a full alert, not only a notification.** It appears on Active alerts and in alert
    history, streams live, is acknowledgeable from your incident tool, and renders through your
    notification templates. Two new template variables come with it — `subject_kind` and an
    always-present `subject_name` — so a template can read correctly for both a device and a pool.
  - **A group-scoped operator sees the pools their own nodes are polled by**, which is exactly the
    person whose site went dark. Pools holding no node they can see stay invisible to them.
  - Two things it is deliberately *not*: it never rolls into a node's displayed state (it belongs
    to no node), and it cannot be muted — a mute names a node.
- **The MCP surface can now read Yagra's own configuration — `get_config(kind=…)`.** One tool over
  28 reads: thresholds, event rules and sources, notification channels and routing rules, profiles
  and collection templates, a node's collected metrics, classification rules, the MIB catalog, a
  node's URL/DNS check, discovery candidates and scans, Meraki orgs/networks/polling, forwarding
  destinations, report definitions and schedules, and the retention / adjacency / LLM / roles /
  OIDC / LDAP settings. This closes the ADR-042 read-parity backlog: every read the WebUI can reach
  is now reachable from `/mcp` except the four live SSE streams, which have no subscription
  transport.
  - **Read-only does not mean readable-by-anyone.** Each `kind` demands the same permission its REST
    counterpart does — `manage-users` for OIDC and LDAP, `manage-config` for fourteen of them, and
    `view` for the rest — so a Viewer is served the role matrix and refused the threshold ruleset
    from the same tool.
  - No stored secret is returned. A node's URL check reports **whether** a credential is bound
    (`has_credential`), never which one; the REST body is unchanged.
- **The LLM root-cause analysis can now look things up for itself.** Previously it was handed a
  fixed set of facts — the alert, the dependents, the upstream chain, a signal timeline, recent
  config changes — and answered in one shot, so it could only reason about what had been decided in
  advance to include. It now gets the read-only MCP tools and asks: pull the interface series, check
  whether the poller was even up, read what syslog said, look at the threshold that fired.
  - **It runs under the caller's own visibility scope**, so a group-scoped operator's analysis
    cannot read a node they cannot see, and under a **view-only** tool allow-list — the write tools,
    `run_analysis`, `run_rca` and the audit log are all out of reach, checked per folded *branch*
    rather than per tool.
  - **What it looked up is stored with the answer** (`transcript` on the report body) and replayed
    on both surfaces, for the same reason the evidence always was: an explanation whose reader
    cannot check what it was based on is an assertion.
  - Bounded by turns (`YAGRA_RCA_MAX_TURNS`, default 6), wall clock
    (`YAGRA_RCA_TASK_BUDGET_SECS`, default 240) and total tool output. Hitting a bound returns the
    model's last answer rather than failing the request. **Set `YAGRA_RCA_MAX_TURNS=1` to get the
    previous single-shot behaviour back exactly** — no tools are offered and the request sent to the
    provider is byte-identical to before.
  - Tool output is device-supplied text and is fenced as such: a syslog line that says "ignore your
    instructions" arrives inside the same untrusted-output markers a device's `sysDescr` always did.
- **Support bundle (Settings ▸ System Health).** One download containing everything needed to
  diagnose a deployment from outside it: which binary is actually running (image source ref and
  build profile, not just the version), every system-health section, the allow-listed environment,
  applied migrations with their checksums, per-table sizes and connection counts, active alerts, the
  audit tail, core's Prometheus scrape, and core's own rotated log files. Also at
  `GET /api/v1/system/support-bundle?since_hours=N`.
  - It is built for a site where nobody can open a shell and data does not leave casually, so the
    archive is designed to be **reviewed before it is released**: every entry is JSON or plain text,
    and `MANIFEST.json` lists what is carried **and what is deliberately left out**, with reasons.
  - Secrets are handled two ways. The environment is carried by an **allow-list** — a deny-list of
    password-shaped names would miss the credential inside `YAGRA_DATABASE_URL`'s userinfo, which is
    the one that actually ships. Then every assembled byte is scanned, and a match **aborts the
    export** rather than redacting it: the strongest rule is the set of literal secret values the
    core process can see in its own environment, so a credential arriving through an unanticipated
    path is caught too. A refusal answers `500 support_bundle_redaction_failed` naming the file and
    the rule, never the value.
  - It requires **ManageConfig + ManageCredentials + ViewAudit** — all three, so this cannot become
    a way to read the audit log or the credential report through an endpoint whose name mentions
    neither. In practice that means Admin.
- **Core's log is now written to disk as well as stdout.** Hourly JSON-lines files under
  `YAGRA_LOG_DIR`, `YAGRA_LOG_RETAIN_HOURS` of them (default 48, pruned automatically), on a named
  volume so they outlive the container. Reading `docker logs` needs a shell on the host, which is
  exactly what a locked-down deployment does not grant — so a panic or an OOM used to leave nothing
  retrievable. A support bundle taken *after* a recovery now carries the run that died.
  - **On by default** in `docker-compose.yml` and `docker-compose.deploy.yml`; set `YAGRA_LOG_DIR`
    empty in `.env` to turn it off. Writes are non-blocking and drop rather than stall the poll loop,
    and an unwritable directory degrades to stdout-only with a warning instead of failing startup.
  - Pollers can opt in the same way (the image has the directory), but no compose file mounts one:
    the support bundle carries core's logs only. A poller's log body would have to cross the bus to
    reach one, which is a new bus message rather than a read. Poller heartbeat counters, poll-loop
    statistics and host resources are in the bundle already.
- **`incident_correlate` now correlates across topology neighbours.** An incident is assembled from
  a node *and* its directly-linked upstream/downstream peers when their signals coincide in time
  (within five minutes), so a failed uplink reads as one incident with its downstream peers named
  instead of a row of unrelated single-node findings. Each finding lists the corroborating
  neighbours, and a peer's entries in the timeline are labelled with the node they came from.
  - It uses the auto-derived connectivity graph **and** hand-authored parents, regardless of the
    topology mode. The mode gate exists because a wrong derived edge would *suppress* a real alert
    and silence is unrecoverable; a diagnostic that names an extra peer only errs toward noise. So
    this works on a deployment still in `manual` mode, which is the default.
  - A node still needs a signal of its own to produce a finding — neighbours corroborate, they do
    not manufacture. Peers are capped per finding, and a node whose alerts are opted out of
    suppression is not reasoned about as anyone's upstream.
  - An incident spanning two devices produces a finding on each, naming the other. Both devices are
    affected, and the per-node attribution keeps the report's node counts honest.

### Improvements
- **Troubleshoot's passive-event analyses read the event log store when one is configured.** They
  had been reading PostgreSQL directly, which holds only alert-linked rows once VictoriaLogs is
  enabled (ADR-024) — so they were answering about the subset of events that had *already* alerted.
  `rule_gap` was the extreme case: its entire purpose is finding high-volume **unmatched** events,
  and unmatched events never reach PostgreSQL on a log-store deployment, so it was structurally
  guaranteed to return nothing on exactly the deployments that generate enough syslog to need it.
  - `event_storm`, `severity_shift`, `auth_probe` and `incident_correlate`'s event lane now count
    the full firehose rather than the alert-linked subset. The MCP `event_stats` tool, which was
    built from three of the same queries, inherits the corrected answer.
  - `event_flap` is unchanged and was already complete: every action it counts is alert-linked, so
    PostgreSQL keeps all of them either way. That is now pinned by a test rather than a comment.
  - A log-store failure fails the analysis rather than falling back to PostgreSQL. Falling back
    would answer from the subset again with nothing to say so, which is the defect being fixed.
- **Unmatched events now cluster on the device's own event code.** The Troubleshoot **Rule gap**
  analysis (and MCP `event_stats`) grouped events by trap OID, else syslog APP-NAME. A large class
  of real network gear supplies neither — its timestamp format falls outside both RFC 3164 and
  RFC 5424, so the datagram parses as raw text and no APP-NAME is extracted — and such a device can
  emit six figures of events a day while producing **zero** rule-gap findings. "0 gaps" reads as
  "nothing is unrouted" when it actually meant "not measurable". Yagra now lifts the vendor's own
  code out of the message at ingest (`%%01URL/4/FILTER(l):` → `URL/4/FILTER`, `%LINEPROTO-5-UPDOWN:`
  → `LINEPROTO-5-UPDOWN`, a leading `SNMP_TRAP_LINK_DOWN:`) and clusters on it.
  - The extracted code is always a **verbatim slice of the message**, so a signature named in a
    finding can be pasted straight into a `substring` event rule and will match the events — which
    is the action a rule gap exists to prompt. Rules are matched in-process against the message
    text, so this holds on every deployment.
  - ⚠️ Free-text **search** for a whole signature is *not* guaranteed on a VictoriaLogs deployment.
    LogsQL matches whole tokens, so a vendor prefix that runs into the code — `%%01ATK/4/…`
    tokenizes with `01ATK`, not `ATK` — will not match on the leading segment. Search the
    distinctive tail instead (`FIREWALLATCK`). PostgreSQL-only deployments match on substrings and
    are unaffected; this is the same backend difference already documented for plain search terms.
  - Clustering precedence is trap OID → device event code → APP-NAME. Deployments whose devices
    already send an APP-NAME keep working; a device that sends **both** now clusters on the more
    specific code, so **an existing rule-gap finding may split into several finer ones**.
  - Extraction applies to newly received events only — there is no backfill and none is needed, as
    the analysis reads only unmatched events and those age out within one retention window.
- **`GET /api/v1/mib-catalog` accepts `limit` and is now bounded.** The query had no row cap on
  either edge, which was survivable while its only caller was a settings screen and stopped being so
  once an AI client could ask for the whole table. `limit` is 1–2000 and defaults to 2000, so an
  existing caller sees no change unless the catalog holds more than that.
- `MANIFEST.json` reports both size caps that can bite — log files dropped for size, and files
  outside the requested window — because a silently truncated log reads as "nothing was logged",
  which is a wrong answer rather than a missing one.
- The redaction report carries `secret_literals_skipped_short` beside
  `secret_literals_enforced`, and `README.txt` interprets the pair in words. On its own, an
  `enforced` count of zero has three meanings a reviewer cannot tell apart: this deployment holds
  no secrets in its environment, it holds some that were too short to enforce safely, or the
  collection broke. The first is a clean bill of health and the third is the strongest rule in the
  scan silently not running. Found on the first real bundle, where establishing which it was took
  a shell session on the deployment — the exact work a support bundle exists to remove.
  - The eight-character floor is **not** lowered: a lab `POSTGRES_PASSWORD=yagra` would forbid the
    substring `yagra`, which appears in every path and table name in the archive, and the scan
    would refuse every bundle forever. Counting the declined values is the fix; the README says
    plainly when the scan has fallen back to pattern matching alone, and what to do about it.
- **A group-scoped `rule_gap` or `auth_probe` restricts at the store rather than afterwards.** Both
  used to group fleet-wide and then keep a row only if its *representative* node was in scope, so a
  signature genuinely occurring inside your group vanished whenever some node outside it happened to
  sort lower. `auth_probe` additionally hid every auth-failure source that mapped to no inventory
  node — which is exactly what an external prober looks like. **Expect more rows than before**; the
  extra ones were always yours.
- `PUT /api/v1/settings/retention` gained `diagnostic_days`. It is optional, so a client sending the
  previous four-field body keeps working — but note that such a body is a full replace and therefore
  resets this window to the default (90). The WebUI always sends every field.

### Bug Fixes
- `db/connections.json` no longer reports a negative connection age. `now()` is the transaction's
  start time and the backend running the query sets its own `state_change` after it, so the
  `active` row reliably came back a few milliseconds below zero — an artefact of measuring from
  inside, not a fact about the deployment.

### Security
- **An OIDC login is now refused when the IdP delivers the groups claim out-of-band** — Microsoft
  Entra's "group overage", where a user in more than roughly 200 groups gets `_claim_names` /
  `_claim_sources` (or `hasgroups`) *instead of* their groups. Yagra reads groups from the ID token
  only, so it previously saw an empty group list and fell through to the provider's `default_role`.
  That is a **silent role change**, not a failure: an administrator signed in as whatever the
  default was, and where the default is Admin, a user who should have been a viewer signed in as an
  administrator. Neither left any trace an operator could find.
  - The client still receives the same generic 401 as every other SSO refusal — the callback
    deliberately does not tell a stranger which step failed. The audit log records
    `auth.oidc.group_overage` **with the username** (reaching this branch requires a verified ID
    token, so there is no prober to help), and the core log names the claim and the remediation.
  - ⚠️ **If your tenant relies on this fallback, affected users will stop being able to sign in.**
    Configure the app registration to emit only the groups assigned to it, or use a group-filtering
    claim. LDAP role mapping is unchanged.

## v0.1.22 — HTTPS by default, a network map that draws itself, and directory sign-in

### Breaking changes
- **The WebUI is now HTTPS, on port 443, and there is no plain-HTTP listener.** Everything the UI
  carries — the login password, bearer tokens, and device credentials on their way to being
  encrypted — used to cross the network in the clear by default. v0.1.9 shipped instructions for
  fixing that and a commented-out configuration block; a year later nobody had uncommented it. So
  the secure shape is now the one you get by doing nothing.
  - **`http://<host>:3000` no longer answers.** If your `.env` still sets `YAGRA_WEB_PORT=3000` you
    keep port 3000 and it becomes `https://<host>:3000` — the port stayed, the scheme changed.
    Delete the line to land on 443.
  - **A redirect was considered and deliberately rejected.** Most webhook senders do not follow
    redirects, and those that do turn a `301` on `POST` into a `GET`, so
    `POST /api/v1/ingest/webhook/:source_id` would have gone on returning success while events
    stopped arriving. Connection-refused is the failure you can see. **Move webhook senders and any
    other machine client** to `https://<host>/api/v1/…`, or to core's unchanged plaintext
    `http://<host>:8080/api/v1/…`.
  - **Everyone is signed out and loses their saved UI state.** `http://host:3000` and
    `https://host` are different origins, so the session token, dashboard layouts, theme and table
    preferences do not carry over. There is no migration for this that would be worth its
    complexity; sign in again and the layouts are rebuilt as you go.
  - **SSO stops working until two things are updated.** The stored OIDC redirect URI is an absolute
    URL and must now use the new scheme and port, and the same value has to be updated at the
    identity provider. Settings ▸ Auth shows a warning when the stored value no longer matches the
    address you are browsing from. Yagra will not rewrite it for you — changing where an IdP is
    permitted to send an authorization code is not something an upgrade should do on your behalf.
  - **Your browser will warn on first visit.** The certificate is self-signed until you import one,
    and because Yagra cannot know the hostname you will use, the name usually will not match either.
    Import a real certificate at **Settings ▸ TLS**, or regenerate the self-signed one with the
    correct names from the same page.
  - Set **`YAGRA_WEB_TLS=off`** to keep serving plain HTTP from the container — the supported shape
    when an external reverse proxy or load balancer already terminates HTTPS in front of it.
  - **Core's own API port is unchanged**: still plaintext, still published on the LAN. That is
    deliberate sequencing, not an oversight — closing it in the same upgrade that introduces an
    untrusted certificate would break every Prometheus scrape and API script at once, with two
    overlapping causes. Once those clients are on the TLS edge with a certificate they trust, set
    **`YAGRA_API_BIND=127.0.0.1`** to take it off the network. Settings ▸ TLS shows the current state.
  - **If you maintain your own compose file, core's container now has a fixed group as well as a
    fixed user — both `10001`.** The certificate bundle is written `0640` owned by that group, and
    the web container joins it with `group_add: ["10001"]` in order to read it. Leave the group to
    the base image and nginx traverses the directory, is refused the file itself, and the WebUI
    never comes up while every other signal stays green. The shipped compose files already do this.

### New Features
- **Import your own TLS certificate from the WebUI.** Settings ▸ TLS shows what is being served —
  subject, issuer, the names it covers, expiry and fingerprint — and takes a PEM certificate chain
  and private key, either pasted or from a file. The new certificate is live within seconds, with
  nothing restarted. Yagra will not accept a pair that does not go together, has already expired,
  or carries no subject alternative name, and says which of those it is rather than failing at the
  next handshake. Encrypted private keys and `.pfx` bundles are refused with the `openssl` command
  that converts them. The self-signed certificate can be regenerated with the hostnames and IP
  addresses you actually use, and renews itself before it expires.
  - The private key is envelope-encrypted at rest like every other secret and is never returned by
    the API. The certificate is downloadable, so you can hand it to a Prometheus `ca_file`, a
    `curl --cacert`, or an operating-system trust store.
  - **System Health says when it is about to expire.** An expired certificate takes the whole UI
    down — including the page you would use to fix it — so it is one more row there, and reaches the
    MCP `get_system_health` tool with it. *Degraded* is reserved for something somebody has to act
    on: expired, or an **imported** certificate inside its last 30 days. A self-signed one nearing
    expiry renews itself and is not reported as a problem. The same fact is on the Prometheus
    endpoint as `yagra_web_tls_expires_in_days`, for your own alerting.
  - This does **not** manage the NATS bus certificate, which the bus reads for itself at startup.
- **An AI client can now ask whether Yagra itself is healthy.** The MCP surface had no way to see
  the monitoring system's own state, which matters more than it sounds: with no way to tell that a
  poller is offline or a store unreachable, a model reads missing data as a healthy quiet and
  reports that the fleet is fine. Six new tools close that (`/mcp`, off by default):
  - **`get_system_health(section=…)`** — the poller fleet and per-pool summary, poll-loop counters,
    which nodes one poller holds, which poller owns one node, recent core↔poller outages,
    per-store reachability, core/poller host resources and their trends, forwarding delivery
    status, whether stored credentials still decrypt, the running version, and which optional
    tiers are enabled.
  - **`get_report_runs`**, **`get_audit`** (who changed or acknowledged what),
    **`fleet_state_history`**, **`get_dns_chain`**, and **`run_rca`** — an LLM explanation of one
    incident, the same one the WebUI's "Explain this incident" produces.
  - **`get_fleet_summary(kind="coverage")`** answers which nodes have actually reported recently,
    with a watchlist of the ones that have not.
  - **Sections require different permissions, matching the WebUI exactly**: most need view,
    forwarding status needs manage-config, credential health needs manage-credentials, the audit
    log needs view-audit, and `run_rca` needs ack-alerts. Read-only does not mean
    readable-by-anyone. Reports and the state timeline refuse a group-scoped token rather than
    showing it the whole fleet, as the REST endpoints do.
- **The Network map now draws the network, not a list of parent links you typed in.** Yagra derives
  the connectivity graph from what devices report: CDP/LLDP adjacency, and nodes that have an
  interface address in the same IP subnet. Two nodes sharing a subnet are adjacent as a matter of
  fact, so a map appears without anyone entering a single link. Each edge carries the evidence
  behind it — LLDP, CDP, or shared subnet — and the map labels and legends them. Redundant paths are
  kept rather than collapsed: a server reached through two routers shows both links. **Drawing the
  map is all this does on its own**; alert suppression keeps following the dependency graph you
  maintain by hand until you deliberately hand it over, which is the next feature down.
- **A new read endpoint, `GET /api/v1/topology/links`**, returns that graph in keyset pages, with a
  `summary` of everything the derivation observed but declined to turn into a link (unmatched
  neighbours, ambiguous management addresses, segments with no identifiable router). A group-scoped
  caller sees only links whose **both** endpoints are visible to them.
- **The network map now finds the links that share no subnet.** Until now a link was derived from
  two devices holding an address in the same prefix, which structurally cannot see a point-to-point
  `/32` (a PPPoE `Dialer`, a tunnel endpoint), an unnumbered OSPF link, or a peering across a
  segment whose addressing has not been collected. Yagra now also reads each device's **OSPF
  neighbours and BGP peers**, and asks its routing table about specific destinations — so those
  links appear on the map with `OSPF neighbor`, `BGP peer` or `Connected route` as their evidence.
  A link seen both ways is still one link carrying both.
  - **On by default**, at the same hourly cadence as the other two automatic walks, and switchable
    at Settings ▸ System settings ▸ Discovery walks. Unlike the ARP walk, the tables it reads are
    sized by the device's own peering mesh, not by the network.
  - **The routing table is never walked.** A router carrying a full table has hundreds of thousands
    of routes, so Yagra asks about one destination at a time — and only a device that holds a host
    address of its own is asked at all, capped at 64 destinations. On a fleet of ordinary devices
    this issues no route queries whatsoever.
  - **A down session still draws its link.** A BGP session in `active` is a link with a fault, and
    that is usually the thing being investigated — making the topology disappear in step with the
    outage it exists to explain would be exactly backwards.
  - **An iBGP session between loopbacks does not become a link.** A BGP peer is only treated as
    adjacent when it sits on a network the reporting device terminates, so a route reflector does
    not acquire a false star to every client it peers with. The count of peers declined this way is
    reported alongside the map's other diagnostics.
  - Known limits, stated rather than half-answered: BGP4-MIB is IPv4-only, so **IPv6 BGP peers are
    out of scope**; OSPF collection is OSPFv2; and virtual links (`ospfVirtNbrTable`) are not read.
    One older limit also stands: a segment with more than two members where no member can be
    identified as routing for the others produces **no** links rather than a guessed one, and is
    counted in the map's summary instead.
- **Yagra can now tell you what is on your network that it is not watching.** Turn on the new
  **ARP / IPv6 neighbor cache** walk (Settings ▸ System settings ▸ Discovery walks) and every
  monitored router reports the hosts it has actually spoken to. Anything not already in the
  inventory appears under **Nodes ▸ Discovery ▸ Seen on the network**, with the address, its MAC,
  and which device saw it on which port — and a *Monitor* button that turns it into a node through
  the same import path a subnet scan uses. No scan required; it is a by-product of the polling you
  already do.
  - ⚠️ **Off by default, deliberately.** Unlike the other two discovery walks, this one reads a
    table sized by the *network* rather than by the device — thousands of rows on a campus switch —
    so an upgrade will not start issuing it against your fleet. The default cadence is six hours.
  - The list says which kind of empty it is: "no device has reported a cache yet" (nobody looked)
    reads differently from "0 unmonitored addresses" (nothing to find), and if any router's cache
    hit its row budget the list declares itself a **sample** rather than a complete answer.
  - Endpoints are deliberately **not** drawn on the network map. An unmonitored host has no state
    to show, and a few thousand stateless boxes would bury the nodes that do. Importing one makes it
    a node, and the ordinary derivation picks it up from there.
  - New: `GET /api/v1/discovered-endpoints` (keyset-paged, group-scoped through the observing node)
    and `POST /api/v1/discovered-endpoints/{id}/import`, plus the MCP tool
    `list_discovered_endpoints`.
  - Rows age out seven days after they were last seen, and the table is capped fleet-wide.
- **The interface-address walk finally has a UI.** It has been running since it shipped, but the
  settings card only ever knew about CDP/LLDP. Settings ▸ System settings now shows all three
  discovery walks — neighbours, interface addresses and ARP — each with its own switch and cadence.
- **One box can be excluded from derived suppression entirely.** Tick *Never suppress* against a
  node on **Topology ▸ Dependencies** (or `PUT /api/v1/nodes/{id}/suppression-opt-out`) and its
  alert always stands on its own, whatever the discovered graph says. The node keeps its place in
  the graph, so everything behind it still resolves through it. This only ever *removes*
  suppression, so it cannot cause an outage to go unreported — which is why it is a per-node switch
  where per-edge approval was rejected.
- **The comparison now shows how much of the fleet the derived graph actually covers**: how many
  nodes would get an upstream, how many are excluded by hand, and how long the deployment has been
  comparing. Advisory, not a gate — the one blocking condition is still an unplaced poller.
- **See what the derived graph would do to your alerts, before it does anything.**
  **Topology ▸ Dependencies** gained a mode switch with three positions. *The hand-authored graph*
  is the default and is what every existing deployment stays on. *Comparing* changes nothing about
  alerting and shows, node by node, where the graph Yagra derived and the one you maintain by hand
  disagree — plus the two numbers that matter: how many active alerts the derived graph **would
  newly suppress** (the risky direction — each of those is an alert that would stop being raised)
  and how many it would stop suppressing. *The derived graph* hands suppression over. Nothing moves
  between these on its own; an upgrade lands on the mode you were already on.
- **Dependency suppression can now have more than one upstream per node.** The derived graph gives a
  node every neighbour that sits one hop closer to a poller, so a server reached through a redundant
  pair of routers gets both as parents — and its alert keeps standing while either one is alive.
  This is what `is_suppressed`'s "suppressed only when *every* parent is down" rule was written for;
  a single hand-typed `parent_id` could never express it.
- **Correct a wrong link instead of working around it.** New endpoints record operator decisions
  about a link — `pin` it into existence, `hide` it, or declare which end is upstream — and those
  always beat what was derived, on every recomputation. `GET`/`POST /api/v1/topology/link-overrides`
  and `DELETE /api/v1/topology/link-overrides/{id}`. A pinned link never expires the way an
  unobserved derived link does.
- **`GET /api/v1/topology/shadow`** returns the whole comparison: edge counts, the differing edges
  in each direction, the affected active alerts, the nodes acting as graph roots, and any pools
  whose poller could not be placed.
- **The MCP `get_topology` tool takes a `kind` parameter**: `dependency` (the default, and what
  every existing call keeps doing), `links` for the connectivity graph, and `overrides` and `shadow`
  for the operator decisions and the comparison. Asked whether derived suppression is safe to
  enable, an AI client can now answer from the same data an operator sees.
- **Pollers report their own interface addresses, and can be given an anchor node.** Direction in
  the derived graph comes from distance to a poller, so Yagra has to know where each poller sits. It
  works that out from the addresses the poller reports — but ⚠️ **a poller running in a container
  reports a container-network address that matches no monitored node**, which is the normal case
  rather than an unusual one. **Settings ▸ Pollers** gained an *Anchor node* column for naming where
  such a poller really attaches (`PUT /api/v1/pollers/{id}/anchor`), and `GET /api/v1/pollers` now
  returns `mgmt_addrs` and `anchor_node_id`. Switching to the derived graph is **refused** while a
  pool that has nodes has an unplaced poller — such a pool would contribute no roots, so nothing in
  it would ever be suppressed while the screen showed the feature as on.
- **`PUT /api/v1/settings/topology`** sets the mode (`manual` / `shadow` / `derived`). There is
  deliberately no matching `GET`: the current mode is part of the `/topology/shadow` response.
- **Sign in with an LDAP or Active Directory account.** Configure your directory at
  **Settings ▸ Auth ▸ Directory (LDAP/AD)** and people log in with their corporate credentials at the
  ordinary login form — there is no second button and no separate URL. Yagra searches for the person
  with a service account and then re-binds as the entry it found, so no DN pattern has to be guessed;
  group membership maps to a Yagra role through the same mapping the SSO provider uses, matching a
  group by its full DN or just its name. An account is created on first successful sign-in.
  **Local accounts are always tried first**, so a directory that is unreachable can never lock an
  administrator out — keep one local admin and a rollback stays survivable. LDAPS and StartTLS only,
  with a field for your private CA; there is deliberately no way to skip certificate verification.
  A **Test** button reports each stage separately and, given a username, shows the DN, the groups and
  **the role that person would receive** — including when the answer is "denied", which the login
  form otherwise reports as an ordinary wrong password. Nothing changes for a deployment that does
  not configure a directory.
- **SAML is answered with a documented bridge rather than an implementation.** `DEPLOYMENT.md` now
  describes putting Keycloak or Dex in front as a SAML→OIDC bridge, and says why Yagra does not
  verify XML signatures itself.

### Improvements
- **Yagra-core and Yagra-poller use far less memory, and stop growing.** Both binaries now use the
  mimalloc allocator instead of the system one. Measured on a 50,000-node deployment over a
  20-minute window, at identical polling throughput: core's resident set averaged **183 MB instead
  of 397 MB**, and grew by **6 MB instead of 162 MB** across the window; the poller's stopped
  creeping upward at all. The old behaviour was not a leak but the system allocator holding on to
  per-thread arenas it never returned — which meant core's footprint kept climbing for as long as it
  was watched, and could not be sized with confidence. If you provisioned a host from the previous
  profile, it will now sit comfortably under it. Building with `--no-default-features` restores the
  system allocator.
- **Upgrading a poller now downloads half as much.** The poller image stored its binary twice: once
  where it was copied in, and again because granting it `CAP_NET_RAW` rewrote the file into a second
  layer — and both layers change with every release. Placing the binary and granting the capability
  are now one layer, so the per-release download for `yagra-poller` drops from 10.2 MB to 4.7 MB.
  Most of that is the duplicate going away; the rest is a smaller binary. The image behaves
  identically and raw-socket ICMP is unaffected.
  - ⚠️ **`yagra-core` moved the other way, and it is only fair to say so**: the release build now
    uses fat link-time optimization, which grew core's binary from 11.9 to 13 MiB compressed. On
    top of `codegen-units = 1` there is no duplicate code left for LTO to collapse, so what it adds
    is inlining across crate boundaries — and inlining duplicates code. The poller's binary went
    the other way, which is where the "smaller binary" above comes from. Taken together these two
    changes still subtract about 4.4 MB from an upgrade that pulls both images, but core alone
    costs a little more than it did.
- The **Dependency / root-cause dashboard widget** now lists each root cause with the alerts rolled
  up under it, biggest first, instead of an indented parent→child tree. The dependency graph is no
  longer a tree — a node can have two upstreams — and a tree could only have shown one of them.
- **Interface addresses are collected and their changes recorded**, the same way CDP/LLDP adjacency
  already was: one current set per node plus an append-on-change history, visible under
  **Settings ▸ Data retention** as *Interface address changes*. Collection is on by default at the
  same hourly cadence, with its own toggle at `PUT /api/v1/settings/neighbors`
  (`l3_enabled` / `l3_interval_secs`; omitting them leaves those settings unchanged).
- **LLDP neighbours now carry the peer's management address.** That is what lets an adjacency be
  matched to a monitored node, and it is why the map can be built from L2 at all. ⚠️ The first poll
  after upgrading records **one extra neighbour-change row per LLDP-speaking node**, because the
  recorded set genuinely gained a field. Devices that do not implement LLDP-MIB record nothing new.
- **An API token owned by an LDAP account now expires with its owner's silence**, the same way an
  SSO-owned one already did. A directory disabling somebody is not something Yagra is told about, so
  the owner going quiet is the only signal there is — previously that rule was written for OIDC
  alone, and a token owned by a disabled directory account would have kept working indefinitely.
  `YAGRA_PAT_OIDC_IDLE_DAYS` keeps its name and now governs both.

### Bug Fixes
- **A Troubleshoot analysis started over MCP now appears in the audit log.** The identical run
  started from the WebUI or the REST API was recorded; the one launched through `/mcp` left no
  trace at all, because auditing is REST middleware that the MCP surface does not pass through. Any
  deployment with `/mcp` enabled has been under-recording who started analyses.
- **Signing in as a disabled SSO account now answers 401 instead of 500.** The refusal was correct;
  the status code said Yagra had broken.
- **Resetting the password of an SSO or directory account is now refused** with a message saying so.
  It used to answer 200 and write a hash that can never be used, telling an administrator they had
  set a password when they had not.

## v0.1.21 — Notification templates, and MCP can see what silenced the fleet

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
- **Three new MCP tools, and the biggest gap in that surface is closed** (ADR-042 increment 2).
  `/mcp` could open a maintenance window but could not list one back, so an AI client could silence
  a node and then report the fleet healthy — having caused the silence itself.
  - **`list_suppressions`** — every maintenance window and mute the caller may see, in one answer.
    Check it before concluding a fleet is quiet; a silenced fleet looks the same as a healthy one.
  - **`alert_trends`** — how alerting has behaved over time: `top_nodes` (which nodes alert most
    often — chronic offenders, which the active-alert list cannot show), `transitions` (the latest
    fires and recoveries), and `calendar` (fires bucketed by weekday and hour, for spotting a
    nightly pattern).
  - **`search_analysis_findings`** — Troubleshoot findings across every run, filtered by node,
    folder, diagnostic, severity or time. Distinct from reading one run you already know about.
  - **`list_analyses` takes `kind=schedules`** to list recurring analyses, and **`list_node_groups`
    takes `include_state`** to return each folder's health tally alongside its name.
  - MCP remains **read-only**: no tool was added that changes anything.
- **Notification templates** (Alerts ▸ Notification routing ▸ a channel ▸ Edit notification
  template, ADR-039). Each channel can override the subject and body it sends, written as a Jinja2
  template over a fixed set of alert variables. The immediate reason: the built-in subject named the
  node by **UUID** — `node 6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60 is critical` — so a template can now
  say `{{ severity | upper }}: {{ node_name }} ({{ group }})` instead. Conditionals work, which is
  the usual second request: `{% if event == 'resolve' %}Recovered{% endif %}`.
  - **Existing channels are unchanged, byte for byte.** No template means the wording Yagra has
    always sent; upgrading changes no notification.
  - **A broken template never costs you a notification.** If it cannot be rendered when an alert
    fires — a bad filter, output too large, or a body that stopped being valid JSON on a channel
    that sends JSON — that field falls back to the built-in wording and the alert still goes out.
    The fallback is per field, so a mistake in the body does not discard a subject that was right.
    Each occurrence increments `yagra_notification_template_errors_total{reason}` and is logged.
  - **Preview before you save.** A template's first real execution is during an outage, so the
    editor renders it against a representative alert and shows exactly what would be sent,
    including whether the body parses as JSON. A template that does not compile is refused at save
    time rather than at 3am.
  - Variables available: node name/id/address, group, profile, severity, state, metric, value,
    threshold, direction, time, flapping, the root cause when an alert is rolled up, and the dedup
    key Yagra sends to PagerDuty/JSM. Credentials are never in scope. Interpolating into a JSON
    body wants the `tojson` filter, which the editor's error message tells you.
  - Editing is Admin-only and audited, and takes effect within ~30 seconds without a restart.
  - **Not covered**: a webhook or SMTP destination configured through `YAGRA_WEBHOOK_URL` /
    `YAGRA_SMTP_*` keeps the built-in wording — it has no channel record to attach a template to.
    Add it as a channel in the UI to template it.
- **The MCP tool surface now answers the questions the WebUI answers** (ADR-042). It had drifted
  into a subset — 110 read endpoints against 17 tools — and the gaps were the ones a troubleshooting
  session hits first. Six new read-only tools, taking the surface to 23:
  - **`get_interface_series`** — one interface's in/out throughput and error rates over time.
    `query_metrics` is node-level only, so per-interface history had no tool at all.
  - **`top_metrics`** / **`top_interfaces`** — fleet-wide rankings. "Which nodes are worst on CPU
    right now", and "which links are busiest, most erroring, or moved the most". Previously an AI
    client could only read one node's metric at a time.
  - **`get_neighbors`** — a node's CDP/LLDP adjacency and its recent changes (ADR-038).
  - **`list_node_groups`** — the folder tree, which is also how a caller finds the group id
    `run_analysis(scope="group")` asks for.
  - **`fleet_throughput`** — total in/out bits per second across every exporter.
  - **`top_flows` now takes an optional `node_id`**: omit it for the fleet-wide aggregations that
    previously had no tool. Like the REST endpoints, the fleet-wide form is refused for a token
    limited to a group, since the rows keep no exporter attribution to narrow.
  - **MCP stays read-only.** The write surface is unchanged — acknowledging an alert, opening a
    maintenance window, and triggering a poll — and this release adds none. Reading Yagra's own
    configuration over MCP still requires the same permission the UI does; read-only does not mean
    readable by anyone.
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
- **The MCP `top_flows` and `flow_fanout` tools did not clamp their row limit.** The REST flow
  endpoints cap a request at 1000 rows; the MCP tools had their own copy of that query builder and
  it had lost the cap, so an AI client asking for 100000 rows got a ClickHouse query with no bound
  on it. **Both surfaces now share one window-and-limit rule**, so the cap applies wherever the
  query comes from. A tool call asking for more than 1000 rows now receives 1000. The default
  (100 rows) is unchanged.
- **A site pin on the Geo map counted only the nodes filed directly in that folder.** Nodes
  normally live in sub-folders — racks, floors, closets — so an operator who placed their Tokyo
  site and filed the switches under *Tokyo ▸ Floor 2 ▸ Rack A* got a pin that was permanently
  green and empty, showing nothing about a site that was on fire. **Group coordinates are now
  inherited**: a folder with none of its own belongs to its nearest placed ancestor, and a pin
  counts everything that resolves to it. Both the Geo map page and the dashboard widget change
  together, so a site cannot read amber in one and green in the other.
  - **Inheritance does not add pins.** Thirty racks under one building stay one pin — thirty
    exactly-overlapping ones would only hide the building. The number of pins is still the number
    of folders carrying their own coordinates; what changed is what each one counts.
  - The group dialog now says when a folder is already on the map through its parent, so an empty
    pair of coordinate boxes no longer reads as "this site is missing from the map".
  - API: `GET /api/v1/node-groups` rows gain `effective_latitude`, `effective_longitude`,
    `geo_source` (`own` / `inherited` / `unset`) and `geo_group` — the folder whose pin this one
    belongs to. `latitude`/`longitude` still mean the folder's *own* coordinates and are unchanged.
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
  > **Superseded.** The commented-out block this shipped stayed commented out. TLS is now on by
  > default and the plain-HTTP listener is gone — see the Breaking changes at the top of this file.

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
