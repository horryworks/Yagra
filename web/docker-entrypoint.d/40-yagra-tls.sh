#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
#
# Render the listener nginx.conf includes, then arm a certificate-reload watcher (ADR-044).
#
# Run by the stock nginx entrypoint before it execs nginx. That entrypoint runs with `set -e`, so a
# non-zero exit here stops the container — which is the fail-closed behaviour this script wants and
# relies on. It also **silently ignores a /docker-entrypoint.d/*.sh that is not executable** (it logs
# "Ignoring … not executable" and moves on), which would mean no TLS, no error, and a plaintext
# listener on the TLS port. A Windows checkout carries no exec bit, so docker/web.Dockerfile sets the
# mode with `COPY --chmod=0755` rather than trusting the source tree.
#
# THE WATCHER
# Backgrounded here, so after the entrypoint's `exec nginx` it is re-parented to the nginx master
# (PID 1). nginx reaps unknown children in its own `waitpid(-1, …)` loop, so this leaves no zombie.
# It signals with `nginx -s reload`, never `kill -HUP 1` — the latter breaks the moment anyone sets
# `init: true` on the service and PID 1 stops being nginx.
#
# `nginx -t` runs BEFORE every reload, and a failure is logged rather than fatal. That ordering is
# the safety property: a reload nginx refuses leaves the master serving the previous configuration,
# so a bad certificate cannot take the UI down — it just does not get applied.
set -eu

TLS="${YAGRA_WEB_TLS:-on}"
CERT="${YAGRA_WEB_TLS_CERT:-/etc/nginx/certs/server.pem}"
LISTEN_CONF=/etc/nginx/yagra-listen.conf
WAIT_SECS="${YAGRA_WEB_TLS_WAIT_SECS:-90}"
RELOAD_SECS="${YAGRA_WEB_TLS_RELOAD_SECS:-15}"

log() { echo "40-yagra-tls: $*"; }

case "$TLS" in
    off | OFF | false | FALSE | 0)
        # The supported shape when something in front already terminates HTTPS. Byte-identical to
        # the pre-ADR-044 listener, so an external-reverse-proxy deployment is unaffected.
        log "YAGRA_WEB_TLS=$TLS — serving plaintext HTTP on :8080."
        log "  Terminate TLS in front of this container; do not expose this port to an untrusted network."
        printf 'listen 8080;\n' >"$LISTEN_CONF"
        exit 0
        ;;
esac

# ── Wait for the bundle ───────────────────────────────────────────────────────────────────────
# yagra-core writes it from the certificate of record in PostgreSQL, before it binds its own
# listener — so `core: condition: service_healthy` in the compose files means the file is already
# there and this loop normally does not iterate. It exists for the orderings compose does not
# guarantee (a `--no-deps` restart, `up -d web` on its own) and for a first start racing core.
waited=0
while [ ! -r "$CERT" ]; do
    if [ "$waited" -ge "$WAIT_SECS" ]; then
        if [ -e "$CERT" ]; then
            log "FATAL: $CERT exists but is not readable as uid $(id -u) gid $(id -g)."
            log "  The volume is owned by core's user; this service needs group access to it."
            log "  In compose that is 'group_add: [\"10001\"]' on the web service."
        else
            log "FATAL: $CERT still does not exist after ${WAIT_SECS}s."
            log "  It is written by yagra-core. Check that core is healthy and that both services"
            log "  mount the same volume (core read-write at /var/lib/yagra/tls, web read-only here)."
        fi
        log "Refusing to start: TLS was requested and must never silently downgrade to plaintext."
        exit 1
    fi
    if [ "$waited" -eq 0 ]; then
        log "waiting for $CERT (up to ${WAIT_SECS}s)…"
    fi
    sleep 1
    waited=$((waited + 1))
done

# ── Render the listener ───────────────────────────────────────────────────────────────────────
# One file holds the chain and the key; nginx accepts the same path for both directives. That is
# what makes the writer's job a single temp-file + rename, with no window in which nginx could read
# a certificate that does not match the key beside it.
cat >"$LISTEN_CONF" <<EOF
listen 8080 ssl;
http2 on;
ssl_certificate     $CERT;
ssl_certificate_key $CERT;
ssl_protocols       TLSv1.2 TLSv1.3;
ssl_ciphers         HIGH:!aNULL:!MD5;
ssl_prefer_server_ciphers off;
ssl_session_cache   shared:SSL:10m;
ssl_session_timeout 1h;
EOF

log "TLS enabled — HTTPS on :8080 (container), certificate $CERT"

# ── Reload watcher ────────────────────────────────────────────────────────────────────────────
# Polling a hash rather than watching with inotify: eight lines, no extra package, and the latency
# is bounded by RELOAD_SECS which is already far below any certificate's useful lifetime. This is
# what keeps ADR-044 out of the "settings that need a restart" class the repo refuses to ship.
(
    # Deliberately not `set -e` in here: a transient failure (the file mid-rename, a reload racing a
    # restart) must not end the watch and leave the container running with the watcher dead.
    set +e
    last="$(sha256sum "$CERT" 2>/dev/null | cut -d' ' -f1)"
    while :; do
        sleep "$RELOAD_SECS"
        now="$(sha256sum "$CERT" 2>/dev/null | cut -d' ' -f1)"
        [ -n "$now" ] || continue
        [ "$now" = "$last" ] && continue
        last="$now"
        if nginx -t >/dev/null 2>&1; then
            if nginx -s reload; then
                log "certificate changed — nginx reloaded"
            else
                log "certificate changed and 'nginx -t' passed, but reload failed"
            fi
        else
            log "certificate changed but 'nginx -t' FAILED — keeping the running configuration:"
            nginx -t 2>&1 | sed 's/^/40-yagra-tls:   /'
        fi
    done
) &

log "watching $CERT every ${RELOAD_SECS}s for certificate changes"
