# syntax=docker/dockerfile:1
# Yagra Rust images — builds BOTH the core and poller binaries from one shared build stage, then
# ships each from its own slim runtime stage (`--target core` / `--target poller`).
#
# Why one Dockerfile with two targets (S3): the old split (yagra-core.Dockerfile + yagra-poller.-
# Dockerfile) walked the workspace dependency graph and compiled shared crates once PER image. Here a
# single `cargo build --bin yagra-core --bin yagra-poller` compiles the shared graph once; on the
# serialized CI matrix the poller image build then reuses the cached build stage and only assembles
# its runtime layer.
#
# ── Where the binaries come from: BIN_SRC ──────────────────────────────────────────────────────
# This file has TWO ways to obtain the two binaries, selected by `BIN_SRC`, and exactly one set of
# runtime stages consuming whichever won. BuildKit prunes the stage that is not selected, so the
# unused one is never evaluated — its `COPY` may reference paths that do not exist in the context.
#
#   BIN_SRC=build (default) — compile inside the image, from a full repo-root context. This is the
#     `/release` path and CI's: reproducible, self-contained, `--profile release`.
#
#   BIN_SRC=prebuilt — take binaries `scripts/flash-build.sh` already compiled outside, in a
#     container off THIS FILE's `toolchain` stage. That script runs fmt/clippy/build as ONE compile
#     against one persistent target directory, where the flash path previously compiled the
#     workspace three times over (host clippy at `dev`, in-image build at `ci-fast`, host
#     `cargo test` at `dev` — three target dirs, two toolchains, nothing shared).
#
#     **Its context is the directory holding the two binaries, NOT the repo root** — so the flash
#     image build ships ~150 MB of binary instead of the repo, and reads it off ext4 rather than
#     across the 9p bridge. That is why the `prebuilt` stage's COPY names them at the context root.
#
# The runtime stages are shared on purpose: a second Dockerfile for the fast path would be two
# copies of the wkhtmltopdf layer, the uid/gid pinning and the healthcheck, drifting apart silently.
# `toolchain` is split out for the same reason — `flash-build.sh` builds *that stage* as its builder
# image, so the base pin and the mold install cannot diverge from what the release path uses.
ARG BIN_SRC=build

# ── toolchain — the compiler environment, shared by the in-image build and flash-build.sh ──
# Pin the build base to bookworm so the binary's glibc matches the bookworm runtimes below.
# `rust:1.90-slim` is a moving tag that has rolled to Debian trixie (glibc 2.39); building there
# while the runtimes are `debian:bookworm-slim` (glibc 2.36) yields a binary that fails at startup
# with `GLIBC_2.39 not found`. Keep all three on bookworm. This is also why the flash path compiles
# in a container rather than on the host: a binary built against the dev box's libc would be a
# `GLIBC_x.yz not found` crash loop the moment it landed on the runtime image.
FROM rust:1.90-slim-bookworm AS toolchain
WORKDIR /app

# mold linker (S2): linking is the serial tail of every Rust build and is repeated on every CI run.
# mold is several times faster than the default GNU ld, which is most visible on incremental one-file
# changes (the common dev-cycle case). bookworm ships gcc 12, which resolves `-fuse-ld=mold` by
# finding `mold` on PATH.
RUN apt-get update \
    && apt-get install -y --no-install-recommends mold \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTFLAGS="-C link-arg=-fuse-ld=mold"

# ── flash-toolchain — `toolchain` plus clippy, for scripts/flash-build.sh ──
# The slim rust image ships no clippy component ("'cargo-clippy' is not installed for the toolchain
# '1.90.0-x86_64-unknown-linux-gnu'"), and the release path's build stage has no use for one — it
# compiles, it does not lint. Keeping the component in its own stage means the image `/release`
# builds carries no lint tooling and its layers do not move, while flash-build.sh still gets its
# compiler environment from the same base and cannot drift off the bookworm pin.
FROM toolchain AS flash-toolchain
RUN rustup component add clippy

# ── build — compile in-image (BIN_SRC=build; the release path) ──
FROM toolchain AS build

# Compile profile (S1): `release` (default) is fully optimized and is what `/release` (v* tags)
# publishes. `/flashdeploy` and CI's PR validation builds override it to `ci-fast` so the dev cycle
# stays short. See [profile.ci-fast] in the workspace Cargo.toml. `release` → target/release,
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
# Reuse compiled deps + cargo registry across builds via BuildKit cache mounts. These help wherever
# BuildKit itself persists — a local `docker build` on the build host, and `/flashdeploy`, which
# compiles against its own named volume. They do NOT help in CI any more: CI runs on hosted runners
# and **no cache backend exports cache mounts**, so `type=gha` restores layers but never these
# directories, and a CI image build compiles the workspace from cold. Both binaries are still built
# in one invocation so shared workspace deps compile once, not once per image. Cache mounts aren't
# part of the image filesystem, so copy the finished binaries out of /app/target before the stage
# ends.
RUN --mount=type=cache,target=/app/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --profile ${CARGO_PROFILE} --bin yagra-core --bin yagra-poller \
    && cp target/${CARGO_PROFILE}/yagra-core /app/yagra-core \
    && cp target/${CARGO_PROFILE}/yagra-poller /app/yagra-poller
# The composition and the backup procedure travel WITH the binaries (ADR-050 decision 5). An
# upgrade takes the compose file out of the image it is installing, so the two can never be a
# version apart — which is the failure ADR-044 hit (a new compose publishing 443 with an old web
# container that never listened on it). Staged under /app so both `bins` sources agree on the path.
#
# Only the backup script moves: WORKDIR is /app and `COPY . .` above already put the compose file at
# /app/docker-compose.deploy.yml. Naming it here as well made `cp` a same-file copy, which is a hard
# error — `cp: '...' and '/app/...' are the same file`, exit 1, whole build dead. That shipped
# because this stage is the one `/flashdeploy` prunes (BIN_SRC=prebuilt) and no `main` push runs CI,
# so nothing evaluated the line until `/release`'s pre-flight build did.
RUN cp scripts/yagra-backup.sh /app/yagra-backup.sh

# ── prebuilt — take binaries compiled outside by scripts/flash-build.sh (BIN_SRC=prebuilt) ──
#
# The context for this path is the OUTPUT directory of that script (two files), not the repo — see
# the BIN_SRC note at the top. Running it against the repo root fails loudly on the COPY below,
# which is the desired outcome rather than a silent wrong image.
#
# `--chmod=0755` is load-bearing, not tidiness: the binaries are produced inside a container and
# land on a bind-mounted host directory, and the exec bit does not survive every host filesystem
# faithfully. A binary copied in 0644 produces `permission denied` at container start, which reads
# as a broken build rather than a lost mode bit. (This is the same reason web.Dockerfile chmods its
# entrypoint script.)
#
# busybox rather than scratch because this stage has to `RUN echo` the two provenance markers to
# the same paths the `build` stage writes them to — the runtime stages below copy them from
# whichever stage won, and must not know which that was.
FROM busybox:stable AS prebuilt
ARG SOURCE_REF=unknown
ARG CARGO_PROFILE=ci-fast
RUN echo "${SOURCE_REF}" > /etc/yagra-source-ref \
 && echo "${CARGO_PROFILE}" > /etc/yagra-build-profile
COPY --chmod=0755 yagra-core yagra-poller /app/
# Same two release artifacts the `build` stage stages, put there by scripts/flash-build.sh. Both
# stages must offer them at the same path: the runtime stage below copies from whichever won, and
# must not know which that was. Getting this wrong breaks ONLY the flash path, because BuildKit
# never evaluates the stage it did not select.
COPY docker-compose.deploy.yml docker-compose.poller.yml yagra-backup.sh /app/

# ── The selector. BuildKit builds only the stage this resolves to. ──
FROM ${BIN_SRC} AS bins

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
 && install -d -o yagra -g yagra -m 0750 /var/lib/yagra/tls \
 && install -d -o yagra -g yagra -m 0750 /var/log/yagra \
 && install -d -o yagra -g yagra -m 0770 /data/upgrade
COPY --from=bins /etc/yagra-source-ref /etc/yagra-source-ref
COPY --from=bins /etc/yagra-build-profile /etc/yagra-build-profile
COPY --from=bins /app/yagra-core /usr/local/bin/yagra-core
# Release artifacts an upgrade reads back out of this image (ADR-050 decisions 5 and 9): the
# composition for the version being installed, and the backup procedure of record (ADR-040) for the
# version being replaced. Copied out with `docker cp`, never executed here.
COPY --from=bins /app/docker-compose.deploy.yml /app/yagra-backup.sh /usr/share/yagra/
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
#
# /var/log/yagra is the same trick for the on-disk log (ADR-045). ⚠️ Since Inc.3 this path is what a
# **remote-site** poller uses (docker-compose.poller.yml gives it a volume of its own); the co-located
# poller writes to a `pollers/` subdirectory of core's shared volume instead, and that directory is
# arranged by the `log-init` one-shot rather than here — an image's ownership only gets a vote when
# Docker seeds an *empty* named volume, so it cannot fix an ordering it lost or a volume that already
# exists. This directory losing that vote is exactly how Inc.3 shipped inert: the poller (uid 10002)
# could not mkdir inside core's 0750 10001-owned volume, degraded to stdout as designed, and every
# other signal stayed green.
RUN useradd -r -u 10002 yagra \
 && apt-get update \
 && apt-get install -y --no-install-recommends libcap2-bin \
 && rm -rf /var/lib/apt/lists/* \
 && install -d -o yagra -g yagra -m 0755 /var/lib/yagra/buffer \
 && install -d -o yagra -g yagra -m 0750 /var/log/yagra \
 && install -d -o yagra -g yagra -m 0770 /data/upgrade
COPY --from=bins /etc/yagra-source-ref /etc/yagra-source-ref
COPY --from=bins /etc/yagra-build-profile /etc/yagra-build-profile
# The composition for the version being installed, read back out of this image by a site updater
# (ADR-051, mirroring ADR-050 decision 5 for core). Copied out with `docker cp`, never executed here.
#
# ⚠️ Both `bins` sources must offer it at this path — `build` gets it from `COPY . .`, `prebuilt` from
# scripts/flash-build.sh — because BuildKit never evaluates the stage it did not select, so a miss on
# one side stays invisible until whichever path is that stage's only reader runs.
COPY --from=bins /app/docker-compose.poller.yml /usr/share/yagra/
# The binary arrives through a bind mount rather than a COPY, so that placing it and granting it
# CAP_NET_RAW are ONE layer.
#
# The obvious form — `COPY … ` then `RUN setcap …` — stores the binary TWICE: setcap rewrites the
# file, and a RUN that modifies a file writes the whole file into its own layer. Measured on the
# published v0.1.20 image: 5.1 MB compressed for the COPY layer and 5.1 MB again for the setcap
# layer, and *both* change on every release, so every `docker pull` of a new poller release moved
# 10.2 MB where 5.1 MB was needed. Nothing about the image's behaviour depended on the split.
#
# `setcap` in the `bins` stage instead is not an option: the `prebuilt` stage is busybox and has no
# setcap, so the two BIN_SRC paths would diverge. The mount works for both.
# `install -m0755` rather than `cp`: the mode is stated instead of inherited from the source through
# the process umask, the same reason the `prebuilt` stage spells out `COPY --chmod=0755`.
RUN --mount=type=bind,from=bins,source=/app/yagra-poller,target=/tmp/yagra-poller \
    install -m0755 /tmp/yagra-poller /usr/local/bin/yagra-poller \
 && setcap cap_net_raw+ep /usr/local/bin/yagra-poller
USER yagra
ENTRYPOINT ["/usr/local/bin/yagra-poller"]
