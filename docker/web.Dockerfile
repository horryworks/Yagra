# Yagra-web — WebUI image. Build context is ./web.
# Multi-stage: build the Vite/React app, serve the static bundle with nginx (which also
# proxies /api to Yagra-core — see web/nginx.conf).

FROM node:22-alpine AS build
WORKDIR /app
COPY package*.json ./
RUN npm ci || npm install
COPY . .
RUN npm run build

FROM nginx:1.27-alpine AS runtime
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=build /app/dist /usr/share/nginx/html
EXPOSE 80
