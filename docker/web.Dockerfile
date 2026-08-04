# syntax=docker/dockerfile:1
# Yagra-web — WebUI image. Build context is ./web.
# Multi-stage: build the Vite/React app, serve the static bundle with nginx (which also
# proxies /api and /mcp to Yagra-core — see web/nginx.conf).

FROM node:22-alpine AS build
WORKDIR /app
COPY package*.json ./
RUN npm ci || npm install

# Same cache-bust and provenance marker as docker/yagra-rust.Dockerfile, and for the same reason:
# BuildKit has been observed reporting `COPY . .` as CACHED against a changed context, which ships
# the previous commit's bundle with every job green. Placed after `npm ci` so the dependency layer
# still caches. Recording the ref also makes the running image identifiable —
#     docker exec yagra-web-1 cat /etc/yagra-source-ref
# which matters more now that development builds are not published to a registry and so have no
# digest to compare against. Written here rather than in the runtime stage because
# nginx-unprivileged already runs as uid 101 and cannot write to /etc.
# (No /etc/yagra-build-profile counterpart: the web image has only one build mode.)
ARG SOURCE_REF=unknown
RUN echo "${SOURCE_REF}" > /etc/yagra-source-ref

COPY . .
RUN npm run build

# Rootless runtime (security.md — containers run as non-root). The `-unprivileged` variant runs as
# the non-root `nginx` user (uid 101) instead of a root master like stock `nginx:alpine`. A non-root
# process cannot bind a privileged (<1024) port, so our server block listens on 8080 (web/nginx.conf);
# the compose files map that container port to the host. `COPY` runs as root at build time regardless
# of USER, so the static bundle/config land readable — nginx only needs read access.
#
# The container port is 8080 whether or not TLS is on (ADR-044): only the scheme changes, so nothing
# downstream has to track a moving port. The host port defaults to 443.
FROM nginxinc/nginx-unprivileged:1.27-alpine AS runtime
COPY nginx.conf /etc/nginx/conf.d/default.conf

# --chmod is load-bearing, not tidiness. The stock nginx entrypoint runs /docker-entrypoint.d/*.sh
# only if the file is executable and otherwise **logs "Ignoring … not executable" and continues** —
# so on a checkout with no exec bit (any Windows clone) TLS would simply not happen, with no error
# anywhere and a plaintext listener on the port the compose file publishes as HTTPS. Setting the
# mode here means the source tree's permissions cannot decide it. This is also what the
# `# syntax=docker/dockerfile:1` line at the top is for — COPY --chmod needs it.
COPY --chmod=0755 docker-entrypoint.d/40-yagra-tls.sh /docker-entrypoint.d/40-yagra-tls.sh

COPY --from=build /etc/yagra-source-ref /etc/yagra-source-ref
COPY --from=build /app/dist /usr/share/nginx/html

# Prepare the two paths the entrypoint owns, and prove the configuration parses.
#
# `nginx -t` here is a build-time gate on web/nginx.conf: a syntax error in it would otherwise first
# appear as a crash-looping container on a deployed machine. It runs in the plaintext shape because
# nginx will not parse `ssl_certificate` pointing at a file that does not exist, and generating a
# throwaway certificate would mean pulling openssl into this image for one build step. The three
# generated `ssl_*` lines are covered instead by the runtime `nginx -t` the entrypoint runs before
# every reload. The plaintext fragment it leaves behind is also a sane default if the entrypoint
# never runs at all.
USER root
RUN install -d -o 101 -g 0 -m 0755 /etc/nginx/certs \
 && printf 'listen 8080;\n' > /etc/nginx/yagra-listen.conf \
 && chown 101:0 /etc/nginx/yagra-listen.conf \
 && chmod 0664 /etc/nginx/yagra-listen.conf \
 && nginx -t
USER 101

# TLS on by default (ADR-044). Set YAGRA_WEB_TLS=off when something in front already terminates it.
ENV YAGRA_WEB_TLS=on \
    YAGRA_WEB_TLS_CERT=/etc/nginx/certs/server.pem \
    YAGRA_WEB_TLS_WAIT_SECS=90 \
    YAGRA_WEB_TLS_RELOAD_SECS=15

EXPOSE 8080
