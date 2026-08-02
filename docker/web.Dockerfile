# Yagra-web — WebUI image. Build context is ./web.
# Multi-stage: build the Vite/React app, serve the static bundle with nginx (which also
# proxies /api to Yagra-core — see web/nginx.conf).

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
FROM nginxinc/nginx-unprivileged:1.27-alpine AS runtime
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=build /etc/yagra-source-ref /etc/yagra-source-ref
COPY --from=build /app/dist /usr/share/nginx/html
EXPOSE 8080
