#!/bin/sh
# yagra-relocate.sh — restore a relocation archive onto a bare Linux host (ADR-121).
#
# This is the *other side* of Settings ▸ Move to another server. It runs on the NEW server, from
# the directory the archive was unpacked into, and brings up a deployment holding the same keys,
# the same users and the same history as the one it came from.
#
# It ships inside the core image and travels **inside the archive**, so the procedure that restores
# a deployment is the one that shipped with the deployment being moved — never a newer one that has
# learned different assumptions.
#
# WHY THIS IS POSIX `sh` AND USES ALMOST NOTHING
#   The target is a bare Linux box. It is assumed to have `sh`, `tar` and Docker, and nothing else:
#   no bash, no curl, no wget, no jq, no python. Every value read out of a JSON file below is read
#   with `sed`, deliberately.
#
# WHAT IT REFUSES, AND WHY IT NEVER REPAIRS
#   * A host that is not bare. It will not upgrade, merge with, or replace an existing Yagra —
#     removing one is a human decision, and the command is printed rather than run.
#   * A different CPU architecture, or a host with no tar.
#   * An archive missing its KEK or its database dump.
#   * A restored database whose row counts do not match the manifest.
#   In every one of those it stops and says what to do. It has no rollback: the only recovery it
#   describes is `docker compose -f docker-compose.deploy.yml down -v`, which is the same command
#   as "there is nothing here", because that is what a refusal has to leave behind.
#
# ORDER, AND WHY IT IS THIS ORDER
#   KEK → volume contents → database → start everything. `bus-cert-init` runs migrations and then
#   opens the bus certificate's private key with the KEK; if it cannot, it *regenerates the row*.
#   Starting before the key and the database are in place therefore does not fail — it silently
#   replaces the certificate every remote poller was pinned to.
#
# USAGE
#   ./yagra-relocate.sh              restore (asks for confirmation on a terminal)
#   ./yagra-relocate.sh --preflight  print `key=value` facts about this host and exit, writing
#                                    nothing. This is what the sending side runs first.
#   YAGRA_RELOCATE_YES=1             skip the confirmation (what the SSH push sets)
#
# ITS LAST LINE IS ITS VERDICT: `RESULT ok url=…` or `RESULT fail reason=…`. The sending side reads
# exactly that and nothing else, so keep it last and keep it one line.

set -eu

say()  { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }

result_ok()  { printf 'RESULT ok url=%s\n' "$1"; exit 0; }
result_bad() { printf '\n%s\n' "$2"; printf 'RESULT fail reason=%s\n' "$1"; exit 1; }

# ── Preflight: facts about this host, printed as key=value, writing nothing ─────────────────────
#
# It is a *report*, not a decision: it never exits non-zero for a host it dislikes, because the
# sending side has to be able to tell "this box has no Docker" (fixable, and it offers to fix it)
# from "the checks could not run at all" (not fixable from here).
preflight() {
  ID=""; VERSION_ID=""
  # ⚠️ Guarded with a test rather than `|| true`. `.` is a POSIX *special built-in*, so a failure
  # to find the file exits the shell even with `set -e` disarmed by `||` — measured, and it made
  # this whole function unrunnable anywhere /etc/os-release is absent.
  if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091  # not present at lint time; its absence is handled above
    . /etc/os-release
  fi
  printf 'os=%s %s\n' "${ID:-unknown}" "${VERSION_ID:-}"
  # The list get.docker.com actually supports. Alpine and Arch are not on it, and a host running
  # one of them is refused with that sentence rather than watched to fail.
  case "${ID:-}" in
    ubuntu|debian|raspbian|centos|rhel|fedora|sles|rocky|almalinux) printf 'os_supported=yes\n' ;;
    *) printf 'os_supported=no\n' ;;
  esac
  printf 'arch=%s\n' "$(uname -m)"
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    printf 'docker=ok\n'
    if docker compose version >/dev/null 2>&1; then printf 'compose=ok\n'; else printf 'compose=missing\n'; fi
  elif command -v docker >/dev/null 2>&1; then
    # Installed but this account cannot reach the socket — usually a missing `docker` group, and
    # usually a session that has not been reopened since it was granted.
    printf 'docker=nogroup\ncompose=unknown\n'
  else
    printf 'docker=missing\ncompose=missing\n'
  fi
  if command -v tar >/dev/null 2>&1; then printf 'tar=ok\n'; else printf 'tar=missing\n'; fi
  # "Bare" means: nothing of ours is here. With no usable Docker, nothing of ours *can* be here.
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    if docker ps -aq --filter label=com.docker.compose.project=yagra 2>/dev/null | grep -q .; then
      printf 'bare=no\nbare_reason=containers of the compose project yagra already exist here\n'
    elif docker volume ls -q 2>/dev/null | grep -q '^yagra_'; then
      printf 'bare=no\nbare_reason=yagra_* volumes already exist here\n'
    else
      printf 'bare=yes\nbare_reason=\n'
    fi
  else
    printf 'bare=yes\nbare_reason=\n'
  fi
  printf 'free_kb=%s\n' "$(df -P -k "$HOME" 2>/dev/null | awk 'NR==2 {print $4}')"
  if sudo -n true >/dev/null 2>&1; then printf 'sudo=ok\n'; else printf 'sudo=password\n'; fi
  printf 'user=%s\nhome=%s\n' "$(id -un)" "$HOME"
}

if [ "${1:-}" = "--preflight" ]; then
  preflight
  exit 0
fi

# ── From here on: the restore ───────────────────────────────────────────────────────────────────

# Run from the archive's own directory whatever the caller's working directory is. The push invokes
# this by absolute path (the updater's `cd` is held to one directory by a check in `api/pollers.rs`),
# and an operator doing it by hand types `./yagra-relocate.sh` — both have to work.
cd "$(dirname "$0")"

PROJECT="${PROJECT:-yagra}"
COMPOSE_FILE=docker-compose.deploy.yml
LOCAL=""
[ -f docker-compose.local.yml ] && LOCAL="-f docker-compose.local.yml"

# 🚨 The local overlay travels with the deployment, and it is the one file in the archive that can
# name things that exist only on the machine it came from -- an external network, a bind mount, a
# device. Carrying it active to a different host is how a restore that has already put back the
# key, the database, the metrics and both tier-2 stores fails on its very last command.
#
# Measured 2026-09-08: the source declared `yagra-sim` (an snmpsim bridge that exists on one lab
# box), and `docker compose up` refused with "network yagra-sim declared as external, but could not
# be found" -- after every irreversible step had already succeeded.
#
# So the overlay is checked before it is used, and set aside rather than obeyed. Set aside, not
# deleted: it is the operator's own configuration and the parts of it that do not name this host
# are still what they wanted. External networks are the class that was measured; a bind mount to a
# path that does not exist here would fail later and differently, which is why the file is kept
# where a person can read it.
if [ -n "$LOCAL" ]; then
  missing_nets=""
  # Only networks under a `networks:` block marked external, which is the shape compose refuses.
  for n in $(awk '
      /^networks:/        { inblock = 1; next }
      /^[a-zA-Z]/         { inblock = 0 }
      inblock && /^  [a-zA-Z0-9._-]+:/ { gsub(/[ :]/, "", $0); name = $0; next }
      inblock && /external:[[:space:]]*true/ { if (name != "") print name }
    ' docker-compose.local.yml); do
    docker network inspect "$n" >/dev/null 2>&1 || missing_nets="$missing_nets $n"
  done
  if [ -n "$missing_nets" ]; then
    mv docker-compose.local.yml docker-compose.local.yml.needs-review
    LOCAL=""
    say "  ⚠ docker-compose.local.yml names external network(s)$missing_nets, which do not exist on"
    say "    this host. It has been set aside as docker-compose.local.yml.needs-review so the"
    say "    deployment can start. Re-create those networks here, or edit the file, then rename it"
    say "    back and run: docker compose -f $COMPOSE_FILE -f docker-compose.local.yml up -d"
  fi
fi

# ⚠️ No `-p`: `docker-compose.deploy.yml` carries `name: yagra`, and the four commands in the README
# do not pass one either. Adding it here would be a second answer to what this project is called.
# shellcheck disable=SC2086  # $LOCAL is an intentional two-word option pair
dc() { docker compose -f "$COMPOSE_FILE" $LOCAL "$@"; }

# One field out of a flat JSON file, without a JSON parser. Only ever pointed at files this
# deployment wrote itself.
jget() { sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*\"\{0,1\}\([^\",}]*\).*/\1/p" "$1" | head -1; }

step "checking this host"
PF="$(preflight)"
pf() { printf '%s\n' "$PF" | sed -n "s/^$1=//p" | head -1; }
say "  $(pf os), $(pf arch), docker=$(pf docker), $(( $(pf free_kb) / 1024 )) MB free"

[ "$(pf bare)" = yes ] || result_bad not_bare \
"This host already has a Yagra deployment on it: $(pf bare_reason).
This never replaces one. If that deployment is finished with, remove it there first:
    docker compose -f docker-compose.deploy.yml down -v
and then run this again."
[ "$(pf docker)" = ok ] || result_bad no_docker \
"Docker is not usable by $(id -un) on this host (docker=$(pf docker)).
Install it, or add this account to the docker group and open a new session."
[ "$(pf compose)" = ok ] || result_bad no_compose "This host has Docker but no 'docker compose' v2 plugin."
[ "$(pf tar)" = ok ] || result_bad no_tar "This host has no tar."
[ "$(pf arch)" = x86_64 ] || result_bad wrong_arch \
"This host is $(pf arch); the published Yagra images are amd64 only."

step "checking the archive"
for f in RELOCATION.json backup/manifest.json backup/kek/kek backup/pg/yagra.dump .env "$COMPOSE_FILE" yagra-relocate.sh; do
  [ -f "$f" ] || result_bad incomplete_archive "The archive is missing $f — it is not a complete relocation archive."
done

# Every artefact the backup recorded, checked against what arrived. `manifest.json` holds one
# {"path": …, "sha256": …} object per file, and this reads them with the same expression
# yagra-restore-verify.sh uses — one spelling of that shape, not two.
BAD=0
CHECKED=0
for pair in $(grep -o '{"path": "[^"]*", "sha256": "[^"]*"}' backup/manifest.json | tr -d ' ' | sed 's/{"path":"\([^"]*\)","sha256":"\([^"]*\)"}/\1:\2/'); do
  REL="${pair%%:*}"; WANT="${pair##*:}"
  CHECKED=$((CHECKED + 1))
  [ -f "backup/$REL" ] || result_bad incomplete_archive "The archive's manifest names backup/$REL, which is not in it."
  GOT="$(sha256sum "backup/$REL" | cut -d' ' -f1)"
  [ "$GOT" = "$WANT" ] || { say "  CORRUPT backup/$REL"; BAD=$((BAD + 1)); }
done
# A checksum loop that matched nothing would report a perfect archive, so the count is asserted too.
[ "$CHECKED" -ge 2 ] || result_bad incomplete_archive \
"The manifest listed $CHECKED files, which is fewer than a backup can have — it is truncated or was written by a version this script cannot read."
[ "$BAD" -eq 0 ] || result_bad corrupt_archive "$BAD file(s) in the archive do not match their recorded checksum."
say "  $CHECKED files, all checksums match"

IMAGE_TAG="$(jget RELOCATION.json image_tag)"
IMAGE_REPO="$(jget RELOCATION.json image_repo)"
SOURCE_REF="$(jget RELOCATION.json source_ref)"
SCHEMA_MAX="$(jget RELOCATION.json schema_latest_version)"
POLLER_ID="$(jget RELOCATION.json local_poller_id)"
WANT_METRICS="$(jget RELOCATION.json metrics)"
WANT_TIER2="$(jget RELOCATION.json tier2)"
NODE_COUNT="$(jget backup/manifest.json node_count)"
CRED_COUNT="$(jget backup/manifest.json credential_count)"
AUDIT_COUNT="$(jget backup/manifest.json audit_row_count)"
TAKEN_AT="$(jget RELOCATION.json taken_at)"

say ""
say "  from      : $IMAGE_REPO, $IMAGE_TAG ($SOURCE_REF), taken $TAKEN_AT"
say "  carries   : $NODE_COUNT nodes, $CRED_COUNT credentials, $AUDIT_COUNT audit rows, schema $SCHEMA_MAX"
say "  metrics   : $WANT_METRICS      events and flows: $WANT_TIER2"
say "  images    : $([ -f images.tar ] && echo 'in the archive' || echo "pulled from $IMAGE_REPO")"

if [ "${YAGRA_RELOCATE_YES:-0}" != 1 ]; then
  [ -t 0 ] || result_bad confirmation_needed \
"This is not a terminal and YAGRA_RELOCATE_YES is not set, so there is nobody to confirm to."
  printf '\nRestore this onto %s? [y/N] ' "$(hostname)"
  read -r ANSWER
  case "$ANSWER" in y|Y|yes|YES) ;; *) result_bad cancelled "Cancelled; nothing was written." ;; esac
fi

# `.env` carries POSTGRES_PASSWORD and, on a deployment that accepts remote pollers, two more.
# `tar` restored whatever mode it was archived with; this makes the guarantee independent of that.
chmod 600 .env

step "images"
if [ -f images.tar ]; then
  docker load -i images.tar || result_bad image_load_failed "docker load could not read images.tar."
  PULL=missing
  # 🚨 And make that stick, because otherwise this host can start exactly once -- by us, with the
  # flag. `docker-compose.deploy.yml` asks for `pull_policy: always`, and the images we just loaded
  # are named for a registry that answered on the OLD host: a deployment moved off `localhost:5000`
  # carries image references no `docker compose up -d` here can ever resolve. The operator's next
  # restart, the next upgrade, and any compose command they type by hand would all fail on a
  # deployment that is otherwise complete and running.
  #
  # `missing` is the honest policy for images that arrived in the archive: they are present, they
  # are pinned to an immutable tag, and a later upgrade pulls its own target before it starts
  # anything. Written by removing the line first -- a .env that already carried a policy would
  # otherwise keep it, which is the defect this is fixing one level up.
  { grep -v '^YAGRA_PULL_POLICY=' .env || true; } > .env.pull
  printf 'YAGRA_PULL_POLICY=missing\n' >> .env.pull
  mv .env.pull .env; chmod 600 .env
  say "  pinned YAGRA_PULL_POLICY=missing (the images came in the archive, not from a registry)"
else
  PULL=always
  dc pull || result_bad pull_failed \
"Could not pull $IMAGE_REPO images at $IMAGE_TAG. If this host cannot reach the registry, take the
archive again with 'Carry the Yagra images' ticked."
fi

# ── The volumes, before anything runs ───────────────────────────────────────────────────────────
# `create` makes the containers and, with them, the named volumes — labelled as this project's, so
# the `up` at the end adopts them rather than making a second set. Nothing is started: a
# VictoriaMetrics that has already written to an empty volume is not a place to unpack a snapshot.
step "creating the volumes"
CREATE_SERVICES="kek-init"
[ "$WANT_METRICS" = true ] && CREATE_SERVICES="$CREATE_SERVICES victoriametrics"
[ "$WANT_TIER2" = true ] && CREATE_SERVICES="$CREATE_SERVICES victorialogs clickhouse"
# shellcheck disable=SC2086  # the service list is intentionally word-split
dc create --pull "$PULL" $CREATE_SERVICES >/dev/null 2>&1 \
  || result_bad create_failed "docker compose create failed; this host cannot make the volumes."

# The KEK first, and on its own, because it is the artefact whose absence cannot be recovered from
# anywhere else. 0444 is what `kek-init` leaves behind: core reads it as uid 10001, and a mode this
# script invents instead would make core unable to open a single credential.
step "restoring the key"
docker run --rm -i -v "${PROJECT}_kekdata:/kek" busybox:stable \
  sh -c 'cat > /kek/key && chmod 0444 /kek/key' < backup/kek/kek \
  || result_bad kek_failed "Could not write the KEK onto this host's volume."
say "  KEK in place"

if [ "$WANT_METRICS" = true ] && [ -f backup/vm/snapshot.tar.gz ]; then
  step "restoring the metrics"
  # The archive holds <snapshot-name>/data|indexdb|metadata, and VictoriaMetrics wants those at the
  # root of its data directory — hence --strip-components=1.
  docker run --rm -i -v "${PROJECT}_vmdata:/victoria-metrics-data" busybox:stable \
    sh -c 'tar -xzf - --strip-components=1 -C /victoria-metrics-data' < backup/vm/snapshot.tar.gz \
    || result_bad metrics_failed "Could not unpack the metrics snapshot."
  say "  metrics in place"
fi

if [ "$WANT_TIER2" = true ]; then
  for v in vldata chdata; do
    [ -f "tier2/$v.tar.gz" ] || continue
    step "restoring $v"
    docker run --rm -i -v "${PROJECT}_$v:/v" busybox:stable sh -c 'tar -xzf - -C /v' < "tier2/$v.tar.gz" \
      || result_bad tier2_failed "Could not unpack tier2/$v.tar.gz."
  done
fi

# ── The database ────────────────────────────────────────────────────────────────────────────────
step "starting PostgreSQL"
dc up -d --pull "$PULL" postgres >/dev/null 2>&1 || result_bad pg_start_failed "PostgreSQL did not start."
i=0
while [ "$i" -lt 90 ]; do
  dc exec -T postgres pg_isready -U yagra >/dev/null 2>&1 && break
  i=$((i + 1)); sleep 2
done
[ "$i" -lt 90 ] || result_bad pg_start_failed "PostgreSQL did not become ready within three minutes."

step "restoring the database"
# `--exit-on-error` rather than pg_restore's default of "log it and carry on": a half-restored
# database that starts is worse than one that never did. No `--clean`: this database was created by
# initdb moments ago and holds nothing.
dc exec -T postgres pg_restore -U yagra -d yagra --no-owner --exit-on-error < backup/pg/yagra.dump \
  || result_bad restore_failed \
"pg_restore refused the dump. Nothing further has been done. Remove what this created with:
    docker compose -f docker-compose.deploy.yml down -v"

psql1() { dc exec -T postgres psql -U yagra -d yagra -tAc "$1" 2>/dev/null | tr -d '[:space:]'; }
GOT_NODES="$(psql1 'SELECT count(*) FROM nodes')"
GOT_SCHEMA="$(psql1 'SELECT COALESCE(max(version),0) FROM _sqlx_migrations')"
[ "$GOT_NODES" = "$NODE_COUNT" ] || result_bad count_mismatch \
"The restored database holds $GOT_NODES nodes; the archive recorded $NODE_COUNT."
[ "$GOT_SCHEMA" = "$SCHEMA_MAX" ] || result_bad schema_mismatch \
"The restored database is at schema $GOT_SCHEMA; the archive recorded $SCHEMA_MAX."
say "  $GOT_NODES nodes, schema $GOT_SCHEMA"

# The co-located poller's identity, pinned so this host's poller is the same poller as far as the
# assignment ring and the `pollers` table are concerned. Without it the id comes from the container
# hostname, which is new here — so the moved deployment would show one dead poller and one new one.
if [ -n "$POLLER_ID" ] && ! grep -q '^YAGRA_POLLER_ID=' .env; then
  printf 'YAGRA_POLLER_ID=%s\n' "$POLLER_ID" >> .env
  say "  pinned YAGRA_POLLER_ID=$POLLER_ID"
fi

step "starting everything"
dc up -d --pull "$PULL" || result_bad start_failed "docker compose up failed. See 'docker compose logs'."

# ── The verdicts ────────────────────────────────────────────────────────────────────────────────
# Deliberately no HTTP: the core image ships no HTTP client, and asking through the API would need
# an account and a token on a deployment that has only just come back. Everything below is answered
# by Docker or by core's own subcommands.
step "checking what came up"
for s in kek-init tls-init log-init bus-init bus-cert-init; do
  CID="$(dc ps -aq "$s" 2>/dev/null | head -1)"
  [ -n "$CID" ] || continue
  CODE="$(docker inspect -f '{{.State.ExitCode}}' "$CID" 2>/dev/null)"
  [ "$CODE" = 0 ] || result_bad oneshot_failed \
"The one-shot container '$s' exited $CODE. See: docker compose -f $COMPOSE_FILE logs $s"
  say "  $s exited 0"
done

CORE_CID="$(dc ps -q core 2>/dev/null | head -1)"
[ -n "$CORE_CID" ] || result_bad core_missing "core is not running."
i=0
while [ "$i" -lt 60 ]; do
  H="$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$CORE_CID" 2>/dev/null)"
  [ "$H" = healthy ] && break
  [ "$H" = none ] && break
  i=$((i + 1)); sleep 5
done
[ "$i" -lt 60 ] || result_bad core_unhealthy \
"core did not become healthy within five minutes. See: docker compose -f $COMPOSE_FILE logs core"
say "  core is healthy"

GOT_REF="$(docker exec "$CORE_CID" cat /etc/yagra-source-ref 2>/dev/null | tr -d '[:space:]')"
[ -z "$SOURCE_REF" ] || [ "$GOT_REF" = "$SOURCE_REF" ] || result_bad version_mismatch \
"This host is running $GOT_REF; the archive came from $SOURCE_REF. Relocation is same-version only."
say "  running $GOT_REF"

GOT_AUDIT="$(psql1 'SELECT count(*) FROM audit_log')"
# `-ge`, not `=`: core has been up for a moment and may have written its own startup rows. Fewer
# than the archive recorded is the failure; more is normal.
[ "$GOT_AUDIT" -ge "$AUDIT_COUNT" ] 2>/dev/null || result_bad history_missing \
"The audit log holds $GOT_AUDIT rows; the archive recorded $AUDIT_COUNT."
say "  $GOT_AUDIT audit rows"

# 🚨 The verdict that matters: are the secrets *openable* on this host? A database full of
# credentials nobody can decrypt looks exactly like a healthy one until the first poll.
SEC="$(dc exec -T core yagra-core verify-secrets 2>/dev/null || true)"
# 🚨 Read the SUMMARY, not the last table. The answer is
#   {"total":9,"decryptable":9,"tables":{"credentials":{"total":6,...},"bus_callout_config":{"total":1,...}}}
# and `sed 's/.*"total":\(...\).*/\1/'` is greedy, so it returns the LAST match -- the trailing
# one-row table. Measured 2026-09-09: a relocation that had in fact opened all nine reported
# "1/1 sealed secrets open" and passed, because 1 equals 1. An under-reading check is the
# dangerous kind -- it is indistinguishable from one that looked at everything and approved it.
SUMMARY="${SEC%%\"tables\"*}"
TOTAL="$(printf '%s' "$SUMMARY" | sed -n 's/.*"total":\([0-9]*\).*/\1/p' | head -1)"
OPENED="$(printf '%s' "$SUMMARY" | sed -n 's/.*"decryptable":\([0-9]*\).*/\1/p' | head -1)"
[ -n "$TOTAL" ] || result_bad verify_failed \
"'yagra-core verify-secrets' produced no answer, so it is unknown whether the key travelled."
# The floor, and it is the half that would have caught the misread above. `verify-secrets` spans
# nine tables while the manifest counts one of them, so this can only ever be `-lt` — but a total
# below the credentials alone means the NUMBER is wrong, not that secrets went missing, and a
# verdict computed from a wrong number is not a verdict. `yagra-restore-verify.sh` has carried this
# floor since it was written and would have failed loudly on the same JSON; this script did not,
# and passed. Two implementations of one check, and only the one with the floor was honest.
if [ -n "${CRED_COUNT:-}" ] && [ "$TOTAL" -lt "$CRED_COUNT" ] 2>/dev/null; then
  result_bad verify_misread \
"Only $TOTAL sealed secrets were counted, but the archive holds $CRED_COUNT credentials by itself.
The check is reading the wrong number, so its verdict about the key means nothing."
fi
[ "$OPENED" = "$TOTAL" ] || result_bad secrets_unreadable \
"$OPENED of $TOTAL sealed secrets could be opened on this host — the KEK does not match the database."
say "  $OPENED/$TOTAL sealed secrets open"

step "tidying up"
# The archive is every secret this deployment has. It has been restored; keeping a second copy of
# it on disk is the worst artefact this procedure could leave behind.
rm -rf backup tier2 images.tar
say "  removed backup/, tier2/ and images.tar from this directory"

WEB_PORT="$(sed -n 's/^YAGRA_WEB_PORT=//p' .env | head -1)"
IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
[ -n "$IP" ] || IP="$(hostname)"
URL="https://$IP:${WEB_PORT:-443}"

cat <<EOF

Yagra is running here, with the same keys, users and history as the server it came from.

  $URL

BEFORE YOU CALL THIS DONE — five things this cannot decide for you:

  1. STOP THE OLD SERVER. Both are polling now, and both will notify. On the old host:
         docker compose -p yagra -f docker-compose.deploy.yml stop
  2. The bus certificate still names the OLD host's addresses. If you have remote-site pollers and
     this host's IP is different, reissue it in Settings > Pollers and hand out the site bundles
     again. If the IP moved with you, there is nothing to do.
  3. OIDC redirect URIs (Settings > Authentication) point at the old host.
  4. Devices sending syslog, SNMP traps or flow records to the old host's IP need repointing.
  5. If ufw or firewalld is running here, open the WebUI port (${WEB_PORT:-443}).

  And delete the archive from wherever you copied it through.
EOF

result_ok "$URL"
