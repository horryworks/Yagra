# Yagra-poller — poller worker image.
# Needs CAP_NET_RAW for raw-socket ICMP. The container runs as a non-root user
# (least privilege, security.md), so `cap_add: NET_RAW` in compose is not enough on
# its own — a non-root process drops capabilities on the uid switch. We grant the
# capability to the binary itself via a file capability (`setcap cap_net_raw+ep`),
# so only this one program (still non-root) can open raw ICMP sockets.

FROM rust:1.85-slim AS build
WORKDIR /app
COPY . .
RUN cargo build --release --bin banshu

FROM debian:bookworm-slim AS runtime
RUN useradd -r -u 10002 yagra \
 && apt-get update \
 && apt-get install -y --no-install-recommends libcap2-bin \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/banshu /usr/local/bin/banshu
# File capability: grants CAP_NET_RAW (effective+permitted) on exec without root.
RUN setcap cap_net_raw+ep /usr/local/bin/banshu
USER yagra
ENTRYPOINT ["/usr/local/bin/banshu"]
