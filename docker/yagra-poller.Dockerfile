# syntax=docker/dockerfile:1
# Yagra-poller — poller worker image.
# Needs CAP_NET_RAW for raw-socket ICMP. The container runs as a non-root user
# (least privilege, security.md), so `cap_add: NET_RAW` in compose is not enough on
# its own — a non-root process drops capabilities on the uid switch. We grant the
# capability to the binary itself via a file capability (`setcap cap_net_raw+ep`),
# so only this one program (still non-root) can open raw ICMP sockets.

# Pin the build base to bookworm so the binary's glibc matches the bookworm runtime below
# (`rust:1.90-slim` is a moving tag now on Debian trixie / glibc 2.39 — a mismatch with the
# bookworm runtime causes a `GLIBC_2.39 not found` crash at startup).
FROM rust:1.90-slim-bookworm AS build
WORKDIR /app
COPY . .
# Shares the same BuildKit cache mounts as the core image build (see docker/yagra-core.Dockerfile):
# compiled workspace deps + cargo registry persist across CI runs and across both image builds, so a
# one-line change is an incremental recompile. Copy the binary out of the cache-mounted target dir.
RUN --mount=type=cache,target=/app/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --bin yagra-poller \
    && cp target/release/yagra-poller /app/yagra-poller

FROM debian:bookworm-slim AS runtime
RUN useradd -r -u 10002 yagra \
 && apt-get update \
 && apt-get install -y --no-install-recommends libcap2-bin \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/yagra-poller /usr/local/bin/yagra-poller
# File capability: grants CAP_NET_RAW (effective+permitted) on exec without root.
RUN setcap cap_net_raw+ep /usr/local/bin/yagra-poller
USER yagra
ENTRYPOINT ["/usr/local/bin/yagra-poller"]
