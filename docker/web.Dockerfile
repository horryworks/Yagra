# syntax=docker/dockerfile:1
# Yagra-web — WebUI image. Build context is ./web.
# Multi-stage: build the Vite/React app, serve the static bundle with nginx (which also
# proxies /api and /mcp to Yagra-core — see web/nginx.conf).
#
# ── Where the bundle comes from: WEB_SRC ──────────────────────────────────────────────────────
# Same selector as docker/yagra-rust.Dockerfile, for the same reason. BuildKit prunes whichever
# stage is not selected, so the unused one is never evaluated.
#
#   WEB_SRC=build (default) — run `npm ci` + `npm run build` in the image. The `/release` path
#     and CI's: self-contained and reproducible.
#
#   WEB_SRC=prebuilt — use the `dist/` the caller already built. `/flashdeploy`'s local guard runs
#     `npm run build` (`tsc --noEmit && vite build`) on the host before committing, and the image
#     build then repeated that work verbatim — install, typecheck and bundle, a second time. The
#     host run is the gate; this makes it also be the artefact.
#
# The context is `./web` either way, because the runtime stage takes `nginx.conf` and the TLS
# entrypoint from it — which is also why `web/.dockerignore` no longer excludes `dist`. On the
# release path that costs a few MB of context and nothing else: the `build` stage regenerates the
# bundle inside the image and never looks at the copy that rode along.
ARG WEB_SRC=build

FROM node:22-alpine AS build
WORKDIR /app
COPY package*.json ./
# `@playwright/test` (ADR-052) fetches a ~150 MB Chromium in its install script. This image runs
# `vite build` and nothing else — it never opens a browser — so the download would be pure build
# latency on every uncached `npm ci`. The BUNDLE is unaffected either way: the runtime stage below
# copies only `dist/`, so neither `node_modules` nor a browser has a path into the shipped image.
# ⚠️ CI's `frontend` job must NOT set this — it runs the suite and needs `npx playwright install
# chromium` (ADR-052 Inc.5, when the walk moves to CI).
ENV PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
# Cache mount for the npm cache: the layer cache alone means any package-lock.json change refetches
# the whole dependency tree from the registry. Not part of the image, so it does not ship.
RUN --mount=type=cache,target=/root/.npm npm ci || npm install

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

# ── prebuilt — use the caller's dist/ (WEB_SRC=prebuilt) ──
# busybox rather than scratch so the provenance marker can be written to the same path the `build`
# stage writes it to; the runtime stage copies it from whichever stage won and must not know which.
FROM busybox:stable AS prebuilt
ARG SOURCE_REF=unknown
RUN echo "${SOURCE_REF}" > /etc/yagra-source-ref
COPY dist /app/dist

# ── The selector. BuildKit builds only the stage this resolves to. ──
FROM ${WEB_SRC} AS bundle

# Rootless runtime (security.md — containers run as non-root). The `-unprivileged` variant runs as
# the non-root `nginx` user (uid 101) instead of a root master like stock `nginx:alpine`. A non-root
# process cannot bind a privileged (<1024) port, so our server block listens on 8080 (web/nginx.conf);
# the compose files map that container port to the host. `COPY` runs as root at build time regardless
# of USER, so the static bundle/config land readable — nginx only needs read access.
#
# The container port is 8080 whether or not TLS is on (ADR-044): only the scheme changes, so nothing
# downstream has to track a moving port. The host port defaults to 443.
#
# Pinned to a patch release, not `1.30-alpine`: a dependency is taken only after it has been out a
# week (ADR-211 decision 5), which a moving tag cannot promise.
FROM nginxinc/nginx-unprivileged:1.30.6-alpine AS runtime
COPY nginx.conf /etc/nginx/conf.d/default.conf
# The security headers (ADR-211), included from three places in nginx.conf.
COPY nginx-security-headers.conf /etc/nginx/yagra-security-headers.conf
# The proxy directives the three core-facing locations share (ADR-133). An absolute path, matching
# the listener fragment the entrypoint renders — nginx resolves a relative `include` against its
# prefix, and two spellings of "where the fragments live" is the kind of thing that only shows up
# when one of them moves.
COPY nginx-proxy-core.conf /etc/nginx/yagra-proxy-core.conf

# --chmod is load-bearing, not tidiness. The stock nginx entrypoint runs /docker-entrypoint.d/*.sh
# only if the file is executable and otherwise **logs "Ignoring … not executable" and continues** —
# so on a checkout with no exec bit (any Windows clone) TLS would simply not happen, with no error
# anywhere and a plaintext listener on the port the compose file publishes as HTTPS. Setting the
# mode here means the source tree's permissions cannot decide it. This is also what the
# `# syntax=docker/dockerfile:1` line at the top is for — COPY --chmod needs it.
COPY --chmod=0755 docker-entrypoint.d/40-yagra-tls.sh /docker-entrypoint.d/40-yagra-tls.sh

COPY --from=bundle /etc/yagra-source-ref /etc/yagra-source-ref
COPY --from=bundle /app/dist /usr/share/nginx/html

# Prepare the two paths the entrypoint owns: the mount point for the certificate volume, and the
# listener fragment nginx.conf includes. The fragment is written in its plaintext shape so the image
# has a valid configuration even if the entrypoint never runs.
#
# ⚠️ **`nginx -t` cannot run here, and this comment exists so nobody adds it back.** It was tried and
# reverted while writing ADR-044. nginx resolves a `proxy_pass` upstream **when it parses the
# config**, not per request — which is why a web container reports `[emerg] host not found in
# upstream "core"` whenever core is down, as the deploy runbooks note. During a build there is no
# `core`, so the parse fails. The two ways around it are both worse than no check: writing
# `/etc/hosts` fails because BuildKit bind-mounts it read-only during RUN, and `--add-host` on the
# build command makes the Dockerfile silently unbuildable for anyone who does not pass that flag.
#
# What covers the config instead: the entrypoint runs `nginx -t` before every reload, and the deploy
# smoke test fails if the container does not come up.
#
# 🚨 Brotli first (CVE-2026-111497, a heap overflow in its decoder): the base image ships
# `brotli-libs` 1.2.0, which freetype links. Named with its fixed version so the build fails rather
# than quietly keeping the old one. Alpine built it on 2026-10-22, past the two days ADR-211
# decision 5 gives a security fix. Drop the line once the base image carries it.
#
# zlib likewise (CVE-2026-85091, in gzwrite — nginx links zlib but never calls it; 1.3.2-r1 was built
# 2026-10-06). And curl is removed rather than upgraded: the base image carries it, nothing here runs
# it (no healthcheck, no entrypoint script), and nothing else requires it, so `apk del` takes libcurl
# and its six dependencies with it — a package that is not there has no advisories (ADR-215
# increment 2, decision 6).
USER root
RUN apk del --no-cache curl \
 && apk add --no-cache --upgrade 'brotli-libs>=1.2.1-r0' 'zlib>=1.3.2-r1' \
 && install -d -o 101 -g 0 -m 0755 /etc/nginx/certs \
 && printf 'listen 8080;\n' > /etc/nginx/yagra-listen.conf \
 && chown 101:0 /etc/nginx/yagra-listen.conf \
 && chmod 0664 /etc/nginx/yagra-listen.conf
USER 101

# TLS on by default (ADR-044). Set YAGRA_WEB_TLS=off when something in front already terminates it.
ENV YAGRA_WEB_TLS=on \
    YAGRA_WEB_TLS_CERT=/etc/nginx/certs/server.pem \
    YAGRA_WEB_TLS_WAIT_SECS=90 \
    YAGRA_WEB_TLS_RELOAD_SECS=15

EXPOSE 8080
