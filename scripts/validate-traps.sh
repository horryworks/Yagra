#!/usr/bin/env bash
# validate-traps.sh — SNMP trap-reception validation battery (ADR-006 / ADR-021, Phase 2).
#
# The pure-Rust trap PARSER (yagra-ingest) and LISTENER (yagra-poller) are unit/integration
# tested, but those tests encode their fixtures with the SAME crate (snmp2) they decode with —
# so they can't prove snmp2 handles PDUs produced by a FOREIGN encoder. This script closes that
# gap: it fires a battery of real Net-SNMP-encoded traps/informs at a running poller, so the
# whole path (UDP recv → snmp2 decode → normalize → NATS → core matcher → events table) is
# exercised against independently-encoded datagrams. That is the empirical evidence the ADR-006
# "is pure Rust sufficient, or do we need net-snmp FFI?" determination needs.
#
# It runs `snmptrap`/`snmpinform` from a throwaway net-snmp container (no host install needed).
# Point it at the poller's trap port. On the test server the poller listens on udp/1162 and the
# compose maps host :162 → :1162, so TARGET defaults to 127.0.0.1:162.
#
#   TARGET      poller host:port to send to   (default 127.0.0.1:162)
#   COMMUNITY   SNMP community to send with   (default public)
#   AGENT_IP    v1 agent-address / sender id  (default 192.0.2.10, a TEST-NET-1 placeholder)
#
# Usage (on the test server, or any Docker host that can reach the poller):
#   TARGET=127.0.0.1:162 COMMUNITY=public ./scripts/validate-traps.sh
#
# VERIFY (definitive decode check) — in the Yagra Postgres container:
#   docker compose exec -T postgres psql -U yagra -d yagra -c \
#     "SELECT kind, trap_oid, action, left(message,80) AS message \
#        FROM events WHERE kind='trap' ORDER BY recorded_at DESC LIMIT 20;"
#   Expect: coldStart/linkDown/linkUp/authenticationFailure rows with the right trap_oid, the
#   linkDown/linkUp varbinds present, and (if a node has AGENT_IP) the built-in rules' action.
#   Counters (unauth /metrics): poller yagra_events_received_total{kind="trap"} and
#   yagra_events_dropped_total{reason="community"|"parse_error"}; core yagra_events_ingested_total
#   {kind="trap"} and yagra_events_matched_total.
#
# TEARDOWN (required — this is synthetic traffic; the user requires cleaning it up):
#   docker compose exec -T postgres psql -U yagra -d yagra -c \
#     "DELETE FROM events WHERE kind='trap' AND source_ip = '<sender-ip>';"
#   ...and close/ack any alerts the built-in rules raised (Alerts page or the alerts table).
#   NOTE the sender IP the poller actually recorded (with a Docker bridge it is the container's
#   bridge IP, not AGENT_IP) via the VERIFY query before deleting.

set -euo pipefail

TARGET="${TARGET:-127.0.0.1:162}"
COMMUNITY="${COMMUNITY:-public}"
AGENT_IP="${AGENT_IP:-192.0.2.10}"
HOST="${TARGET%%:*}"
PORT="${TARGET##*:}"

# Standard trap identity OIDs (SNMPv2-MIB snmpTraps; yagra_common::trap_oid_name resolves these).
COLDSTART="1.3.6.1.6.3.1.1.5.1"
LINKDOWN="1.3.6.1.6.3.1.1.5.3"
LINKUP="1.3.6.1.6.3.1.1.5.4"
AUTHFAIL="1.3.6.1.6.3.1.1.5.5"
# Interface varbinds carried by link traps.
IFINDEX="1.3.6.1.2.1.2.2.1.1.4"
IFDESCR="1.3.6.1.2.1.2.2.1.2.4"
IFOPER="1.3.6.1.2.1.2.2.1.8.4"

echo "→ firing Net-SNMP trap battery at ${HOST}:${PORT} (community=${COMMUNITY})"

# One container, net-snmp-tools installed once, whole battery run inside it. --network host so the
# datagrams reach the host-mapped trap port; TARGET can also be a poller container name on its net.
docker run --rm --network host alpine:3.20 sh -s <<EOF
set -e
apk add --no-cache net-snmp-tools >/dev/null 2>&1

echo "  [1] v2c coldStart"
snmptrap -v 2c -c "${COMMUNITY}" "${HOST}:${PORT}" '' ${COLDSTART}

echo "  [2] v2c linkDown (+ ifIndex/ifDescr/ifOperStatus varbinds)"
snmptrap -v 2c -c "${COMMUNITY}" "${HOST}:${PORT}" '' ${LINKDOWN} \
  ${IFINDEX} i 4 ${IFDESCR} s "ge-0/0/1" ${IFOPER} i 2

echo "  [3] v2c linkUp (should clear the linkDown alert via the built-in rule's clear_pattern)"
snmptrap -v 2c -c "${COMMUNITY}" "${HOST}:${PORT}" '' ${LINKUP} \
  ${IFINDEX} i 4 ${IFDESCR} s "ge-0/0/1" ${IFOPER} i 1

echo "  [4] v2c authenticationFailure"
snmptrap -v 2c -c "${COMMUNITY}" "${HOST}:${PORT}" '' ${AUTHFAIL}

echo "  [5] v1 linkDown (generic=2 → RFC 3584 §3.1 maps to ${LINKDOWN})"
snmptrap -v 1 -c "${COMMUNITY}" "${HOST}:${PORT}" \
  1.3.6.1.4.1.8072 ${AGENT_IP} 2 0 12345 \
  ${IFINDEX} i 4 ${IFDESCR} s "ge-0/0/1"

echo "  [6] v2c inform (poller must send back a Response ack; snmpinform blocks until it does)"
snmpinform -v 2c -c "${COMMUNITY}" -r 1 -t 2 "${HOST}:${PORT}" '' ${LINKDOWN} \
  ${IFINDEX} i 4 || echo "     (no inform ack within timeout — check the listener)"

echo "  [7] wrong-community trap (dropped if YAGRA_TRAP_COMMUNITY is set on the poller)"
snmptrap -v 2c -c "definitely-wrong" "${HOST}:${PORT}" '' ${COLDSTART} || true

echo "  [8] garbage datagram (must be dropped without killing the listener)"
printf '\xff\x00\x13\x37garbage' | nc -u -w1 "${HOST}" "${PORT}" || true
EOF

echo "✓ battery sent. Now run the VERIFY query (see this script's header) and then TEARDOWN."
