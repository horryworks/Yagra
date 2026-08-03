#!/usr/bin/env bash
# yagra-restore-verify.sh — prove a backup can actually be restored (ADR-040 decision 1).
#
# This, not the backup script, is the deliverable. ADR-017 requires a pre-upgrade backup and a
# rollback procedure for any destructive migration; a procedure nobody has executed end to end is
# an assumption. This restores a backup into a THROWAWAY stack and asserts four things:
#
#   1. /readyz returns 200                      — the core starts against the restored data
#   2. the node count matches the manifest       — the configuration came back
#   3. every credential still DECRYPTS           — the KEK came back, and matches the ciphertext
#   4. the audit_log row count matches           — the "who changed what" trail survived
#
# Assertion 3 is the one that cannot be inferred. A database restore can look perfect — right row
# counts, healthy API, no errors — while the key-encryption key is a different one, in which case
# every stored credential is permanently unreadable and nothing says so until the next poll fails.
# `GET /api/v1/credentials/health` exists for this check.
#
# A backup with zero credentials reports SKIPPED for assertion 3, not PASS. A verification script
# that has only ever printed PASS on a vacuous case has verified nothing.
#
# SAFETY: this script ends with `docker compose down -v`, which DESTROYS VOLUMES. It refuses to run
# unless the verify project name differs from the production one. Read that guard before changing it.
#
# Usage (on a Linux Docker host; from Windows use WSL or ssh):
#   ./scripts/yagra-restore-verify.sh <BACKUP_DIR>
#
#   PROJECT           throwaway compose project     (default yagra-verify)
#   PROD_PROJECT      the project to refuse to touch (default yagra)
#   COMPOSE_FILE      compose file                  (default docker-compose.deploy.yml)
#   YAGRA_IMAGE_TAG   tag to restore into           (default: the manifest's image tag)
#   KEEP              set to 1 to skip teardown, for poking at the restored stack

set -euo pipefail

BACKUP_DIR="${1:-}"
PROJECT="${PROJECT:-yagra-verify}"
PROD_PROJECT="${PROD_PROJECT:-yagra}"
COMPOSE_FILE="${COMPOSE_FILE:-docker-compose.deploy.yml}"
PG_USER="${PG_USER:-yagra}"
PG_DB="${PG_DB:-yagra}"
VERIFY_ADMIN_PW="verify-$(date -u +%s)-$RANDOM"

PASS=0; FAIL=0; SKIP=0
declare -a RESULTS=()

log()  { printf '  %s\n' "$*"; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 2; }
ok()   { RESULTS+=("PASS  $*"); PASS=$((PASS+1)); }
bad()  { RESULTS+=("FAIL  $*"); FAIL=$((FAIL+1)); }
skip() { RESULTS+=("SKIP  $*"); SKIP=$((SKIP+1)); }

dc() { docker compose -p "$PROJECT" -f "$COMPOSE_FILE" "$@"; }

# ── 0. Preflight — the guard comes first, because everything after it is destructive ────────────
[ -n "$BACKUP_DIR" ] || die "usage: $0 <BACKUP_DIR>"
[ -d "$BACKUP_DIR" ] || die "no such backup directory: $BACKUP_DIR"
[ -f "$BACKUP_DIR/manifest.json" ] || die "$BACKUP_DIR/manifest.json missing — not a backup made by yagra-backup.sh"
command -v docker >/dev/null || die "docker not found"
docker compose version >/dev/null 2>&1 || die "docker compose v2 not found"

if [ "$PROJECT" = "$PROD_PROJECT" ]; then
  die "PROJECT ('$PROJECT') must differ from PROD_PROJECT ('$PROD_PROJECT'): this script ends with
       'docker compose down -v' and would destroy the production volumes."
fi

json_field() { sed -n "s/.*\"$1\": *\"\{0,1\}\([^\",}]*\)\"\{0,1\}.*/\1/p" "$BACKUP_DIR/manifest.json" | head -1; }
SRC_VERSION="$(json_field yagra_version)"
MANIFEST_TAG="$(json_field image_tag)"
WANT_NODES="$(json_field node_count)"
WANT_CREDS="$(json_field credential_count)"
WANT_AUDIT="$(json_field audit_row_count)"
TARGET_TAG="${YAGRA_IMAGE_TAG:-$MANIFEST_TAG}"

echo "Restore verification"
log "backup:  $BACKUP_DIR (yagra $SRC_VERSION, nodes=$WANT_NODES creds=$WANT_CREDS audit=$WANT_AUDIT)"
log "target:  project '$PROJECT', image tag '$TARGET_TAG'"

# ADR-040: migrations only move forward, so a restore into an OLDER build is unsupported. Enforced
# by the tool and not merely written down, because this is the mistake that corrupts data quietly.
ver_lte() { [ "$1" = "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -1)" ]; }
if [ -n "$SRC_VERSION" ] && [ "$SRC_VERSION" != unknown ] && [ "$TARGET_TAG" != unknown ]; then
  TGT_VERSION="${TARGET_TAG##*:}"; TGT_VERSION="${TGT_VERSION#v}"
  SRC_CLEAN="${SRC_VERSION#v}"
  if [ -n "$TGT_VERSION" ] && [ "$TGT_VERSION" != "$TARGET_TAG" ] && ! ver_lte "$SRC_CLEAN" "$TGT_VERSION"; then
    die "downgrade restore is unsupported: backup is $SRC_CLEAN, target is $TGT_VERSION (ADR-040)"
  fi
fi

# Artefact integrity, before spending minutes on a stack that will fail at step 4.
echo "[1/7] verifying artefact checksums"
bad_sums=0
while IFS='|' read -r rel want; do
  [ -n "$rel" ] || continue
  got="$(sha256sum "$BACKUP_DIR/$rel" 2>/dev/null | cut -d' ' -f1)"
  if [ "$got" != "$want" ]; then
    printf 'ERROR: checksum mismatch for %s\n' "$rel" >&2
    bad_sums=$((bad_sums+1))
  fi
done < <(tr -d '\n' < "$BACKUP_DIR/manifest.json" \
         | grep -o '{"path": "[^"]*", "sha256": "[^"]*"}' \
         | sed 's/{"path": "\([^"]*\)", "sha256": "\([^"]*\)"}/\1|\2/')
[ "$bad_sums" -eq 0 ] || die "$bad_sums artefact(s) failed checksum verification"
log "all artefacts intact"

# ── Teardown always, including on failure ───────────────────────────────────────────────────────
teardown() {
  if [ "${KEEP:-0}" = "1" ]; then
    printf '\nKEEP=1 — leaving project %s up. Tear down with:\n  docker compose -p %s -f %s down -v\n' \
      "$PROJECT" "$PROJECT" "$COMPOSE_FILE"
    return
  fi
  printf '\nTearing down %s\n' "$PROJECT"
  dc down -v --remove-orphans >/dev/null 2>&1 || true
}
trap teardown EXIT

# ── 2. Empty stack (core deliberately not started yet) ──────────────────────────────────────────
echo "[2/7] starting an empty stack"
export YAGRA_IMAGE_TAG="$TARGET_TAG"
export YAGRA_ADMIN_PASSWORD="$VERIFY_ADMIN_PW"
dc down -v --remove-orphans >/dev/null 2>&1 || true
dc up -d postgres redis nats victoriametrics >/dev/null
for _ in $(seq 1 60); do
  dc exec -T postgres pg_isready -U "$PG_USER" >/dev/null 2>&1 && break
  sleep 2
done
dc exec -T postgres pg_isready -U "$PG_USER" >/dev/null 2>&1 || die "postgres did not become ready"
log "stores up on fresh volumes"

# ── 3. Restore, KEK first ───────────────────────────────────────────────────────────────────────
echo "[3/7] restoring"
[ -f "$BACKUP_DIR/kek/kek" ] || die "no KEK in the backup — this is the failure the whole exercise exists to catch"
dc up -d --no-start core >/dev/null 2>&1 || true
KEK_CID="$(dc ps -aq core)"
[ -n "$KEK_CID" ] || die "could not create the core container to seed the KEK volume"
docker cp "$BACKUP_DIR/kek/kek" "$KEK_CID:/kek/key" 2>/dev/null \
  || die "could not write the KEK into the verify stack's kek volume"
log "KEK restored"

dc exec -T postgres psql -U "$PG_USER" -d postgres -c "DROP DATABASE IF EXISTS $PG_DB WITH (FORCE);" >/dev/null
dc exec -T postgres psql -U "$PG_USER" -d postgres -c "CREATE DATABASE $PG_DB OWNER $PG_USER;" >/dev/null
dc exec -T postgres pg_restore -U "$PG_USER" -d "$PG_DB" --clean --if-exists --no-owner < "$BACKUP_DIR/pg/yagra.dump" >/dev/null 2>&1 || true
log "PostgreSQL restored"

if [ -f "$BACKUP_DIR/vm/snapshot.tar.gz" ]; then
  VM_CID="$(dc ps -q victoriametrics)"
  TMPD="$(mktemp -d)"
  tar -C "$TMPD" -xzf "$BACKUP_DIR/vm/snapshot.tar.gz"
  SNAP_DIR="$(find "$TMPD" -mindepth 1 -maxdepth 1 -type d | head -1)"
  if [ -n "$SNAP_DIR" ]; then
    docker cp "$SNAP_DIR/." "$VM_CID:/victoria-metrics-data/" 2>/dev/null || log "metrics copy failed (non-fatal)"
    dc restart victoriametrics >/dev/null
    log "VictoriaMetrics restored"
  fi
  rm -rf "$TMPD"
else
  log "no metrics snapshot in the backup — skipping"
fi

# ── 4. Start core ───────────────────────────────────────────────────────────────────────────────
echo "[4/7] starting core at $TARGET_TAG (migrations run forward)"
dc up -d core >/dev/null
API=""
for _ in $(seq 1 60); do
  if dc exec -T core sh -c 'wget -qO- http://127.0.0.1:8080/readyz >/dev/null 2>&1'; then API=up; break; fi
  sleep 2
done

# ── 5-7. Assertions ─────────────────────────────────────────────────────────────────────────────
echo "[5/7] asserting"
if [ "$API" = up ]; then ok "/readyz returned 200"; else bad "/readyz never returned 200 within 120s"; fi

api_get() { dc exec -T core sh -c "wget -qO- --header='Authorization: Bearer $1' http://127.0.0.1:8080$2" 2>/dev/null || true; }

TOKEN=""
if [ "$API" = up ]; then
  LOGIN="$(dc exec -T core sh -c \
    "wget -qO- --header='Content-Type: application/json' \
     --post-data='{\"username\":\"admin\",\"password\":\"$VERIFY_ADMIN_PW\"}' \
     http://127.0.0.1:8080/api/v1/auth/login" 2>/dev/null || true)"
  TOKEN="$(printf '%s' "$LOGIN" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
fi

# Node count straight from the database: it is the restored artefact, and this avoids a failed
# login turning a real data check into an inconclusive one.
GOT_NODES="$(dc exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SELECT count(*) FROM nodes' 2>/dev/null | tr -d '[:space:]')"
if [ "${GOT_NODES:-x}" = "$WANT_NODES" ]; then
  ok "node count is $GOT_NODES, matching the manifest"
else
  bad "node count is ${GOT_NODES:-unreadable}, manifest says $WANT_NODES"
fi

GOT_AUDIT="$(dc exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SELECT count(*) FROM audit_log' 2>/dev/null | tr -d '[:space:]')"
if [ "${GOT_AUDIT:-x}" = "$WANT_AUDIT" ]; then
  ok "audit_log has $GOT_AUDIT rows, matching the manifest"
else
  bad "audit_log has ${GOT_AUDIT:-unreadable} rows, manifest says $WANT_AUDIT"
fi

echo "[6/7] asserting credentials decrypt"
if [ "${WANT_CREDS:-0}" = "0" ]; then
  # Deliberately not a PASS. Nothing was proved about the KEK, and reporting green here is how a
  # verification script comes to be trusted for something it never checked.
  skip "the backup contains no credentials — the KEK was NOT exercised"
elif [ -z "$TOKEN" ]; then
  bad "could not authenticate to check credential health (admin password not the one this script set?)"
else
  HEALTH="$(api_get "$TOKEN" /api/v1/credentials/health)"
  TOTAL="$(printf '%s' "$HEALTH" | sed -n 's/.*"total":\([0-9]*\).*/\1/p')"
  DECRYPTABLE="$(printf '%s' "$HEALTH" | sed -n 's/.*"decryptable":\([0-9]*\).*/\1/p')"
  if [ -z "$TOTAL" ]; then
    bad "credential health endpoint returned nothing usable: ${HEALTH:-empty}"
  elif [ "$TOTAL" != "$WANT_CREDS" ]; then
    bad "credential count is $TOTAL, manifest says $WANT_CREDS"
  elif [ "$DECRYPTABLE" != "$TOTAL" ]; then
    bad "only $DECRYPTABLE of $TOTAL credentials decrypt — the restored KEK does not match the ciphertext"
  else
    ok "all $TOTAL credentials decrypt with the restored KEK"
  fi
fi

echo "[7/7] summary"
printf '\n'
for r in "${RESULTS[@]}"; do printf '  %s\n' "$r"; done
printf '\n  %d passed, %d failed, %d skipped\n' "$PASS" "$FAIL" "$SKIP"
[ "$FAIL" -eq 0 ] || exit 1
if [ "$SKIP" -gt 0 ]; then
  printf '  Restore verified, but see the SKIP above — it names something this run did not prove.\n'
fi
