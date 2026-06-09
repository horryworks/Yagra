# Yagra-poller — poller worker image.
# Needs CAP_NET_RAW at runtime for raw-socket ICMP (granted in compose / k8s, not here).
# Stub — flesh out build caching once dependencies land.

FROM rust:1.85-slim AS build
WORKDIR /app
COPY . .
RUN cargo build --release --bin banshu

FROM debian:bookworm-slim AS runtime
RUN useradd -r -u 10002 yagra
COPY --from=build /app/target/release/banshu /usr/local/bin/banshu
USER yagra
ENTRYPOINT ["/usr/local/bin/banshu"]
