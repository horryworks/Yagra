# Yagra-web — WebUI image. Build context is ./web.
# Stub — static build served by a minimal web server. Wire to the API base URL via
# build arg / runtime config once the frontend is scaffolded.

FROM node:22-alpine AS build
WORKDIR /app
COPY package*.json ./
RUN npm ci || npm install
COPY . .
RUN npm run build

FROM nginx:1.27-alpine AS runtime
COPY --from=build /app/dist /usr/share/nginx/html
EXPOSE 80
