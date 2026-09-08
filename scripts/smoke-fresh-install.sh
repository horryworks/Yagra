#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# The /release fresh-install smoke test (ADR-120). Run from the repo root AFTER the GitHub release
# exists (Step 7) and BEFORE the website is pushed (Step 9). It IS Step 8.
#
# It rolls a throwaway Proxmox VM back to a pristine snapshot and types the README's four install
# commands **verbatim** — the same `releases/latest/download` URL a stranger would use. Nothing else
# guards that path: /verify stops at `docker compose config`, CI only builds images, and
# /flashdeploy updates boxes that are already grown. The one time a human walked it by hand
# (2026-09-01, `.215`) it turned up a real mismatch between the documented steps and the product
# (ADR-065 Inc.9).
#
# 🚨 **The box is the documented MINIMUM on purpose — 2 vCPU / 4 GB / 20 GB.** README says that is
# enough to evaluate Yagra and nothing has ever checked the claim. Passing on a roomy box would not
# be evidence for the sentence we publish. If this fails for want of resources, the README is wrong.
#
# ⚠️ **What this restores, and what it does not.** Retiring /deploy lost the property "do not
# announce until the published artefact was seen to boot" (see /release Step 11). This gives back
# the *fresh-install* half — it runs before the site push, so the version badge can still be
# withheld. It does NOT give back the *upgrade* half: whether a real deployment can move to this
# version is Step 11's question and still cannot gate. This is a throwaway box, not a deployment,
# so it does not reopen ADR-050.
#
# Usage:  scripts/smoke-fresh-install.sh v0.3.12
#
# Environment overrides (all optional):
#   SMOKE_PVE       ssh alias of the hypervisor        (default: pve)
#   SMOKE_HOST      ssh alias of the throwaway box     (default: yagra-smoke1)
#   SMOKE_VMID      its VMID                           (default: 217)
#   SMOKE_SNAPSHOT  the pristine snapshot to roll to   (default: pristine)
#   SMOKE_KEEP      1 = leave the VM running afterwards, to look at it
#
# Exit codes:  0 pass · 1 a check failed · 2 the rig itself is not usable (never a product verdict)

set -euo pipefail

PVE="${SMOKE_PVE:-pve}"
HOST="${SMOKE_HOST:-yagra-smoke1}"
VMID="${SMOKE_VMID:-217}"
SNAP="${SMOKE_SNAPSHOT:-pristine}"
BOOT_TIMEOUT="${SMOKE_BOOT_TIMEOUT:-300}"
UP_TIMEOUT="${SMOKE_UP_TIMEOUT:-900}"

VERSION="${1:-}"
[ -n "$VERSION" ] || { echo "usage: $0 v<version>" >&2; exit 2; }
# `/api/v1/version` reports CARGO_PKG_VERSION, which carries no leading `v`.
BARE="${VERSION#v}"

say()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
ok()   { printf '  \033[32m✔\033[0m %s\n' "$*"; }
bad()  { printf '  \033[31m✘\033[0m %s\n' "$*"; }
rig()  { echo; echo "RIG NOT USABLE: $*" >&2; echo "This is not a verdict on the release." >&2; exit 2; }

# ── 0. The rig ────────────────────────────────────────────────────────────────────────────────
# Checked before anything is torn down, so a missing prerequisite costs nothing.
say "Preflight"
ssh -o ConnectTimeout=10 -o BatchMode=yes "$PVE" true 2>/dev/null \
  || rig "cannot ssh '$PVE' — the hypervisor alias is in ~/.ssh/config, see ~/.claude/proxmox.md"
ssh "$PVE" "qm config $VMID" >/dev/null 2>&1 \
  || rig "VMID $VMID does not exist on $PVE"
ssh "$PVE" "qm listsnapshot $VMID" 2>/dev/null | grep -qw "$SNAP" \
  || rig "VMID $VMID has no '$SNAP' snapshot — create it from a clean box: ssh $PVE 'qm snapshot $VMID $SNAP'"
ok "hypervisor '$PVE', VM $VMID, snapshot '$SNAP'"

# 🚨 The spec is an assertion, not a default. A box quietly grown to 8 GB would pass this smoke and
# still leave the README's "2 vCPU / 4 GB" claim unverified — the failure mode is a green run that
# proves something nobody published.
CORES=$(ssh "$PVE" "qm config $VMID" | sed -n 's/^cores: //p')
MEM=$(ssh "$PVE" "qm config $VMID" | sed -n 's/^memory: //p')
if [ "${CORES:-0}" != "2" ] || [ "${MEM:-0}" != "4096" ]; then
  rig "VM $VMID is ${CORES:-?} vCPU / ${MEM:-?} MB — it must be the documented minimum, 2 vCPU / 4096 MB (ADR-120 decision 2)"
fi
ok "box is the documented minimum: 2 vCPU / 4096 MB"

# ── 1. Back to pristine ───────────────────────────────────────────────────────────────────────
say "Rolling $VMID back to '$SNAP'"
ssh "$PVE" "qm status $VMID | grep -q running && qm stop $VMID || true" >/dev/null 2>&1
ssh "$PVE" "qm rollback $VMID $SNAP" >/dev/null 2>&1 || rig "qm rollback failed"
ssh "$PVE" "qm start $VMID" >/dev/null 2>&1 || rig "qm start failed"
ok "rolled back and started"

printf '  waiting for ssh'
deadline=$(( $(date +%s) + BOOT_TIMEOUT ))
until ssh -o ConnectTimeout=5 -o BatchMode=yes -o StrictHostKeyChecking=no "$HOST" true 2>/dev/null; do
  [ "$(date +%s)" -lt "$deadline" ] || { echo; rig "'$HOST' did not answer ssh within ${BOOT_TIMEOUT}s"; }
  printf '.'
done
echo
# ⚠️ Group membership does not reach a session that predates it (see ~/.claude/proxmox.md), so ask
# the system, not this connection.
ssh "$HOST" 'getent group docker >/dev/null && docker info >/dev/null 2>&1' \
  || rig "'$HOST' has no usable docker — the pristine snapshot should have been taken after cloud-init finished"
ok "box is up with a working docker"

# ── 2. The documented install, verbatim ───────────────────────────────────────────────────────
# 🚨 Do NOT improve these four lines. They are copied from README.md and the point of this step is
# that a stranger typing them gets a working deployment. In particular there is no `-p yagra` here:
# the project name is derived from the directory, which is exactly what a reader would get.
say "Installing $VERSION the documented way"
INSTALL_LOG=$(mktemp)
trap 'rm -f "$INSTALL_LOG"' EXIT
if ! ssh "$HOST" "timeout ${UP_TIMEOUT} bash -s" <<'REMOTE' >"$INSTALL_LOG" 2>&1
set -euo pipefail
mkdir yagra && cd yagra
curl -fsSL -o docker-compose.deploy.yml \
  https://github.com/horryworks/Yagra/releases/latest/download/docker-compose.deploy.yml
printf 'POSTGRES_PASSWORD=%s\n' "$(openssl rand -hex 16)" > .env
docker compose -f docker-compose.deploy.yml up -d
REMOTE
then
  bad "the documented install did not complete"
  echo "--- last 40 lines ---"; tail -40 "$INSTALL_LOG"
  exit 1
fi
ok "the four documented commands completed"

# ── 3. The four verdicts (ADR-120 decision 5) ─────────────────────────────────────────────────
say "Checks"
FAIL=0

# 🚨 Read the state with plain `docker inspect`, not `docker compose ps --format`: what that flag
# accepts (table / json / a Go template) has changed between Compose releases, and a smoke test
# whose parser silently returns empty would report a healthy stack as broken — or worse, the other
# way round. Container names are stable here because the documented install passes no `-p`, so the
# project is the directory name, `yagra`.
insp() { ssh "$HOST" "docker inspect -f '$2' yagra-$1-1 2>/dev/null" || true; }

# (a) Every one-shot reached Exited (0). A container left `created` is the classic "a dependency
#     never became ready" shape, and it is not an error anywhere in the compose output.
for s in bus-init tls-init log-init kek-init bus-cert-init; do
  st=$(insp "$s" '{{.State.Status}} {{.State.ExitCode}}')
  case "$st" in
    "exited 0") ok "$s exited 0" ;;
    "")         bad "$s: no such container"; FAIL=1 ;;
    *)          bad "$s: $st"; FAIL=1 ;;
  esac
done

# (b) core is healthy — its own HEALTHCHECK probes /healthz, so this is the image's own opinion
#     rather than ours.
ch=$(insp core '{{if .State.Health}}{{.State.Health.Status}}{{else}}no-healthcheck{{end}}')
if [ "$ch" = "healthy" ]; then ok "core is healthy"; else bad "core health: ${ch:-<no container>}"; FAIL=1; fi

# (c) /healthz answers over the API port.
if ssh "$HOST" 'curl -fsS --max-time 10 http://localhost:8080/healthz >/dev/null'; then
  ok "/healthz answers 200"
else
  bad "/healthz did not answer"; FAIL=1
fi

# (d) 🚨 THE ONE THAT MATTERS. The three above pass on any release; only this says the version we
#     just published is the one that booted. A smoke test that cannot tell v0.3.11 from v0.3.12 is
#     testing that Yagra exists.
GOT=$(ssh "$HOST" 'curl -fsS --max-time 10 http://localhost:8080/api/v1/version' 2>/dev/null || true)
if printf '%s' "$GOT" | grep -q "\"core\":\"${BARE}\""; then
  ok "/api/v1/version reports ${BARE}"
else
  bad "/api/v1/version says ${GOT:-<nothing>}, expected core=${BARE}"; FAIL=1
fi

# ── 4. Two numbers, recorded and never asserted (ADR-120 decision 6) ──────────────────────────
# ⚠️ Both move with how fast the host is. Making either a threshold would manufacture failures that
# say nothing about the release.
#
# 🚨 **The reading depends on what the installed composition actually declares, so ask it.** The
# first version of this block said zero retries proved ADR-065 Inc.10's healthcheck had made
# `bus-cert-init` wait. It printed exactly that against v0.3.11 — a composition with **no
# healthcheck at all** — where zero retries only means postgres happened to be ready first. An
# instrument that states a cause it cannot observe is the whole defect this smoke exists to catch;
# it must not be one itself.
say "Recorded (not asserted)"
HAS_HC=$(ssh "$HOST" 'cd yagra && grep -c "pg_isready" docker-compose.deploy.yml 2>/dev/null || true')
PG_WAIT=$(grep -oE 'Container [a-z0-9-]*postgres-1 +Healthy +[0-9.]+s' "$INSTALL_LOG" | grep -oE '[0-9.]+s$' || true)
RETRIES=$(ssh "$HOST" 'cd yagra && docker compose -f docker-compose.deploy.yml logs bus-cert-init 2>/dev/null | grep -c "not ready" || true')
echo "  composition declares a postgres healthcheck : $([ "${HAS_HC:-0}" -gt 0 ] 2>/dev/null && echo yes || echo no)"
echo "  postgres reached Healthy after              : ${PG_WAIT:-not reported}"
echo "  bus-cert-init retry lines                   : ${RETRIES:-unknown}"
if [ "${HAS_HC:-0}" -gt 0 ] 2>/dev/null; then
  if [ "${RETRIES:-1}" = "0" ]; then
    echo "  ⇒ healthcheck present and zero retries: the wait covered initdb (ADR-065 Inc.10 decision 2)"
  else
    echo "  ⇒ healthcheck present but ${RETRIES} retries: the wait did NOT fully cover initdb on this host"
  fi
else
  echo "  ⇒ no healthcheck in this composition, so the retry count says nothing about ADR-065 Inc.10."
  echo "    Zero here only means postgres happened to be ready before bus-cert-init asked."
fi

# ── 5. Put the box away ───────────────────────────────────────────────────────────────────────
if [ "${SMOKE_KEEP:-0}" = "1" ]; then
  say "Leaving $VMID running (SMOKE_KEEP=1) — ssh $HOST"
else
  ssh "$PVE" "qm stop $VMID" >/dev/null 2>&1 || true
  say "Stopped $VMID (rolled back on the next run, so nothing to clean up)"
fi

if [ "$FAIL" = "0" ]; then
  printf '\n\033[32mSMOKE PASS\033[0m — %s installs and boots from the documented path on the documented minimum.\n' "$VERSION"
  exit 0
fi
cat >&2 <<EOF

SMOKE FAIL — $VERSION did not come up from the documented path.

The GitHub release already exists at this point, so this is a decision, not an automatic action:
  · fix and re-cut     — gh release delete $VERSION --yes && git tag -d $VERSION && git push origin :$VERSION
  · ship it knowingly  — continue to /release Step 8 and record what is broken

Do NOT push the website until that is decided: the version badge is what announces this release.
EOF
exit 1
