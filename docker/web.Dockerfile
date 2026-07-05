# Yagra-web — WebUI image. Build context is ./web.
# Multi-stage: build the Vite/React app, serve the static bundle with nginx (which also
# proxies /api to Yagra-core — see web/nginx.conf).

FROM node:22-alpine AS build
WORKDIR /app
COPY package*.json ./
RUN npm ci || npm install
COPY . .
RUN npm run build

# Rootless runtime (security.md — containers run as non-root). The `-unprivileged` variant runs as
# the non-root `nginx` user (uid 101) instead of a root master like stock `nginx:alpine`. A non-root
# process cannot bind a privileged (<1024) port, so our server block listens on 8080 (web/nginx.conf);
# the compose files map that container port to the host. `COPY` runs as root at build time regardless
# of USER, so the static bundle/config land readable — nginx only needs read access.
FROM nginxinc/nginx-unprivileged:1.27-alpine AS runtime
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=build /app/dist /usr/share/nginx/html
EXPOSE 8080
