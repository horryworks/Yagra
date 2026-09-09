# Yagra relocation archive

This archive is a whole Yagra deployment. Unpack it on a fresh Linux server, run one command, and
that server becomes the deployment this came from — same keys, same accounts, same history.

## 🚨 Treat this file exactly as you would the encryption key itself

It contains the **key-encryption key (KEK)** and the **full database** together. Anyone holding both
can read every SNMP community, every SNMPv3 credential, every device login, every API token and
every notification secret the original deployment stored.

* Do not put it on shared storage, in a ticket, or in chat.
* Delete it from every machine it passed through once the move is done — including the server it
  came from and the laptop that carried it.
* The restore below deletes its own copy for you (`backup/`, `tier2/`, `images.tar`). Everything
  else is yours to clean up.

## What is in it

| Path | What it is |
|---|---|
| `RELOCATION.json` | What this archive is: the version to install, the schema version to expect, the counts to check |
| `backup/kek/` | The KEK, and the session and NATS callout keys beside it |
| `backup/pg/yagra.dump` | The whole database — nodes, groups, thresholds, users, alert history, the audit log |
| `backup/vm/` | The metrics snapshot, if the metrics were carried |
| `backup/manifest.json` | A SHA-256 for every file above; the restore checks all of them |
| `tier2/` | The event store (VictoriaLogs) and the flow store (ClickHouse), if they were carried |
| `images.tar` | The three Yagra images, if they were carried. Absent means the new host pulls them |
| `.env` | The deployment's own settings, including its PostgreSQL password |
| `docker-compose.deploy.yml` | The composition, taken from the deployment being moved |
| `docker-compose.local.yml` | That deployment's own compose changes, if it had any |
| `yagra-relocate.sh` | The restore procedure — the one that shipped with the version being moved |

**Not** in it, deliberately: Redis (rebuildable), the materialized TLS and bus certificate files
(recreated from the database rows on the first start), core's rotated log files, the poller's
store-and-forward buffer, and the IP→ASN dataset. Nothing there is a source of truth.

## What the new server needs

* Linux on **x86-64**, with `sh` and `tar`.
* **Docker** with the `docker compose` v2 plugin, usable by the account you log in as. If it has
  none, the WebUI's "Move to another server" can install it for you over SSH — that needs `sudo`
  and internet on that host, and it runs the official `get.docker.com` script as root.
* Enough free disk for the archive plus what it unpacks to.
* **Nothing of Yagra's already on it.** The restore refuses a host that has `yagra_*` volumes or
  containers of the `yagra` compose project. It never replaces, merges with, or upgrades an
  existing deployment; removing one is your decision, and the command is printed rather than run.

## Restoring by hand

```sh
mkdir yagra && cd yagra
tar -xzf ~/yagra-relocation-<stamp>.tar.gz
./yagra-relocate.sh
```

It prints what it is about to do, asks once, and then works in this order — **the order matters**:

1. checks this host and the archive's checksums;
2. loads or pulls the three images at the exact version the archive came from;
3. creates the volumes without starting anything;
4. writes the **KEK** first;
5. unpacks the metrics, and the event and flow stores;
6. starts PostgreSQL alone and restores the dump into it;
7. starts everything else;
8. checks it: the one-shot containers exited 0, core is healthy, the build matches, the audit rows
   are all there, and — the one that matters — **every sealed secret opens on this host**.

Its last line is its verdict: `RESULT ok url=…` or `RESULT fail reason=…`.

If it refuses, it has written nothing you need to undo. If it fails after step 6, remove what it
made and start again:

```sh
docker compose -f docker-compose.deploy.yml down -v
```

There is no rollback and no partial state to repair. That is the design: a relocation either
produced a working deployment or produced nothing.

## After it succeeds — five things it cannot decide for you

1. **Stop the old server.** Both are polling now, and both will notify.
   `docker compose -p yagra -f docker-compose.deploy.yml stop` on the old host.
2. **Remote-site pollers do not follow.** They dial an address written in their own `.env` and pin
   a certificate in their own `certs/`, and this host can change neither — the only channel to a
   site is the bus, which is the thing that stops working. Nothing to do if the IP moved with the
   server; otherwise see "Reconnecting remote-site pollers" below.
3. **OIDC redirect URIs** (Settings ▸ Authentication) still point at the old host.
4. **Devices** sending syslog, SNMP traps or flow records to the old IP need repointing.
5. **The firewall**, if `ufw` or `firewalld` is running: open the WebUI port.

The WebUI's certificate is still the old host's self-signed one, so a browser will warn until you
replace it in Settings ▸ TLS certificate.

## Reconnecting remote-site pollers

In this order, in this deployment's WebUI. Step 3 is only correct once 1 and 2 have happened.

1. **Settings ▸ Pollers ▸ Reissue certificate…** with this host's address. The row is written, but
   the bus keeps serving the old certificate — `nats-server` reads its own at startup — and the
   panel says so.

2. **Make it take effect.** There is no button that restarts only the bus, and a relocated
   deployment arrives with remote acceptance already on, so: **Stop accepting**, then **Accept
   remote pollers** with this host's address. Monitoring stops twice, about a minute each, and the
   second press reissues the certificate again. Skip it and every site fails its handshake against
   a certificate the bus is not serving, with nothing visible here.

3. **Issue each site's bundle again** from the Token column of its row, naming this host's address.
   A fresh token comes with it; the old one stops working.

4. **At the site**, over the directory that is already there:

   ```sh
   cd ~/yagra-poller
   tar -czf ~/yagra-poller-before-relocate-$(date +%Y%m%d-%H%M%S).tar.gz .
   tar -xzf ~/yagra-poller-<id>.tar.gz -C ~/yagra-poller
   docker compose -p yagra-poller -f docker-compose.poller.yml up -d --force-recreate
   ```

   It registers within about ten seconds. Online is not polling: watch the Working set and Results
   columns move before calling it done.

## Upgrading afterwards

The archive pins the version it came from, so the new server starts on exactly that. Upgrade it
from its own WebUI, in Settings ▸ Upgrade, once it is up. Relocation only ever moves sideways —
it never installs a different version, and it cannot restore into an older one.
