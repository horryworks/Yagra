# syntax=docker/dockerfile:1
# Yagra Rust images — builds BOTH the core and poller binaries from one shared build stage, then
# ships each from its own slim runtime stage (`--target core` / `--target poller`).
#
# Why one Dockerfile with two targets (S3): the old split (yagra-core.Dockerfile + yagra-poller.-
# Dockerfile) walked the workspace dependency graph and compiled shared crates once PER image. Here a
# single `cargo build --bin yagra-core --bin yagra-poller` compiles the shared graph once; on the
# serialized CI matrix the poller image build then reuses the cached build stage and only assembles
# its runtime layer.

# Pin the build base to bookworm so the binary's glibc matches the bookworm runtimes below.
# `rust:1.90-slim` is a moving tag that has rolled to Debian trixie (glibc 2.39); building there
# while the runtimes are `debian:bookworm-slim` (glibc 2.36) yields a binary that fails at startup
# with `GLIBC_2.39 not found`. Keep all three on bookworm.
FROM rust:1.90-slim-bookworm AS build
WORKDIR /app

# mold linker (S2): linking is the serial tail of every Rust build and is repeated on every CI run.
# mold is several times faster than the default GNU ld, which is most visible on incremental one-file
# changes (the common dev-cycle case). bookworm ships gcc 12, which resolves `-fuse-ld=mold` by
# finding `mold` on PATH.
RUN apt-get update \
    && apt-get install -y --no-install-recommends mold \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTFLAGS="-C link-arg=-fuse-ld=mold"

# Compile profile (S1): `release` (default) is fully optimized and is what `/release` (v* tags)
# publishes. `/flashdeploy` and CI's main/PR validation builds override it to `ci-fast` so the dev
# cycle stays short. See [profile.ci-fast] in the workspace Cargo.toml. `release` → target/release,
# a custom profile → target/<name>, so the copy path is parameterized by the profile name.
ARG CARGO_PROFILE=release

# Cache-bust the source copy and the compile, every commit.
#
# On 2026-07-31 CI shipped a binary built from the *previous* commit's source, with all six jobs
# green: BuildKit reported `COPY . .` as CACHED even though the context had changed, so the changed
# files never entered the image, cargo saw an unchanged tree, and the `cp` below carried the stale
# binary out of the cache mount. `gh run rerun` reproduced it exactly. Nothing downstream could
# catch it — the deploy pulled the right digest, the container restarted, and `/api/v1/config`
# answered 200 while the API served the old contract.
#
# So the Docker layer cache no longer gets a vote on whether the Rust build is fresh. Cargo decides
# that, and it still can: `target/` and the registry are `--mount=type=cache`, which is **not** part
# of the layer cache and survives this bust — so a one-line change still recompiles one crate, not
# the world. What is thrown away is only the layer cache's *opinion*, which is the thing that was
# wrong.
#
# The ref is written into the image because an unused ARG does not affect the cache at all (Docker
# invalidates at an ARG's first *use*, not its definition). Recording it also makes the failure this
# prevents diagnosable in one command:
#     docker exec yagra-core-1 cat /etc/yagra-source-ref
ARG SOURCE_REF=unknown
RUN echo "${SOURCE_REF}" > /etc/yagra-source-ref

# The commit alone does not identify the binary. A release and a `/flashdeploy` build of the SAME
# commit are different binaries — `release` vs `ci-fast` — and both write the same source ref, so
# re-flashing an already-released commit would swap an optimized binary for a fast-compile one with
# every provenance check still green. Record the profile alongside the ref so `/deploy` can assert
# `release` and `/flashdeploy` can assert `ci-fast`:
#     docker exec yagra-core-1 cat /etc/yagra-build-profile
# Separate file on purpose — the format of /etc/yagra-source-ref must not change, or images already
# on a server stop matching the check that reads it.
RUN echo "${CARGO_PROFILE}" > /etc/yagra-build-profile

COPY . .
# Reuse compiled deps + cargo registry across builds via BuildKit cache mounts. On the persistent
# self-hosted CI runner these survive between runs, so a one-line source change recompiles only the
# changed crates instead of every dependency from scratch. Both binaries are built in one invocation
# so shared workspace deps compile once, not once per image. Cache mounts aren't part of the image
# filesystem, so copy the finished binaries out of /app/target before the stage ends.
RUN --mount=type=cache,target=/app/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --profile ${CARGO_PROFILE} --bin yagra-core --bin yagra-poller \
    && cp target/${CARGO_PROFILE}/yagra-core /app/yagra-core \
    && cp target/${CARGO_PROFILE}/yagra-poller /app/yagra-poller

# ── Yagra-core — Core/API runtime ──
FROM debian:bookworm-slim AS core
# Report PDF export shells out to `wkhtmltopdf` (Reports → Export → PDF). Install the official
# wkhtmltox build, which bundles a patched Qt so it renders HTML→PDF fully headless (no X server) —
# the core reads HTML on the binary's stdin and the binary writes PDF to stdout. If this layer is
# ever dropped, PDF export degrades gracefully (the API returns 503 "pdf_unavailable"); HTML and CSV
# export still work. The deb's runtime deps (fontconfig, X libs, base fonts) are pulled by apt.
ARG WKHTMLTOPDF_VERSION=0.12.6.1-3
ARG TARGETARCH=amd64
# Best-effort: the report PDF path degrades to a 503 at runtime if wkhtmltopdf is missing, so a
# transient download/install failure must NOT fail the image build (and block the gated deploy). The
# fetch+install is wrapped so only ca-certificates is a hard requirement; the rest logs and continues.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && ( wget -q "https://github.com/wkhtmltopdf/packaging/releases/download/${WKHTMLTOPDF_VERSION}/wkhtmltox_${WKHTMLTOPDF_VERSION}.bookworm_${TARGETARCH}.deb" -O /tmp/wkhtmltox.deb \
         && apt-get install -y --no-install-recommends /tmp/wkhtmltox.deb \
         || echo "WARN: wkhtmltopdf install skipped — report PDF export will return 503 at runtime" ) \
    && rm -f /tmp/wkhtmltox.deb \
    && apt-get purge -y wget \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*
# The `install -d` creates the directory core materializes the WebUI's TLS certificate into
# (ADR-044), owned by the runtime user. Same reasoning as the poller's buffer directory below, and
# the same trap: /var/lib is root-owned, and Docker seeds an empty named volume from the image path
# *including ownership*, whereas a mount point it has to invent itself is root-owned. Without this,
# core could not write its own certificate and the WebUI would never come up. 0750 because the file
# it holds contains the server's private key; the web container reads it as a group member.
#
# The `tls-init` one-shot in the compose files covers the orderings this does not — if the *web*
# container mounts the empty volume first, this image's ownership never gets a vote.
# ⚠️ The GROUP id is pinned, not just the user id, and that is load-bearing since ADR-044.
# `useradd -r -u 10001 yagra` alone fixes only the uid and lets the system pick a gid — on Debian
# bookworm that came out as 999. The certificate this stage's directory holds is written 0640 and
# read by nginx in another container through `group_add: ["10001"]`, so a gid of anything but 10001
# means the web container joins a group the file does not belong to: it can traverse the 0750
# directory and still gets "Permission denied" on the file, and the WebUI never comes up. Found on
# the first real deployment — nothing before that point can see it, because the uid was right and
# every other signal was green.
RUN groupadd -r -g 10001 yagra \
 && useradd -r -u 10001 -g 10001 yagra \
 && install -d -o yagra -g yagra -m 0750 /var/lib/yagra/tls
COPY --from=build /etc/yagra-source-ref /etc/yagra-source-ref
COPY --from=build /etc/yagra-build-profile /etc/yagra-build-profile
COPY --from=build /app/yagra-core /usr/local/bin/yagra-core
USER yagra
EXPOSE 8080
# Liveness: the binary probes its own /healthz (dependency-free — the slim runtime has no curl/wget).
# Gives orchestrators (compose/k8s) a real readiness signal instead of "process is up".
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD ["/usr/local/bin/yagra-core", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/yagra-core"]

# ── Yagra-poller — poller worker runtime ──
# Needs CAP_NET_RAW for raw-socket ICMP. The container runs as a non-root user (least privilege,
# security.md), so `cap_add: NET_RAW` in compose is not enough on its own — a non-root process drops
# capabilities on the uid switch. We grant the capability to the binary itself via a file capability
# (`setcap cap_net_raw+ep`), so only this one program (still non-root) can open raw ICMP sockets.
FROM debian:bookworm-slim AS poller
# The `install -d` below creates the store-and-forward spill directory (Phase 3) owned by the
# runtime user. /var/lib is root-owned, so without it the non-root poller cannot create the
# directory and the buffer degrades to memory-only after one startup WARN — invisible until a bus
# outage outlasts the in-memory ring and the oldest poll results are dropped. It also fixes the
# mounted case: Docker seeds an empty named volume from the image path, ownership included, whereas
# a mount point it has to invent itself is root-owned.
RUN useradd -r -u 10002 yagra \
 && apt-get update \
 && apt-get install -y --no-install-recommends libcap2-bin \
 && rm -rf /var/lib/apt/lists/* \
 && install -d -o yagra -g yagra -m 0755 /var/lib/yagra/buffer
COPY --from=build /etc/yagra-source-ref /etc/yagra-source-ref
COPY --from=build /etc/yagra-build-profile /etc/yagra-build-profile
COPY --from=build /app/yagra-poller /usr/local/bin/yagra-poller
# File capability: grants CAP_NET_RAW (effective+permitted) on exec without root.
RUN setcap cap_net_raw+ep /usr/local/bin/yagra-poller
USER yagra
ENTRYPOINT ["/usr/local/bin/yagra-poller"]
