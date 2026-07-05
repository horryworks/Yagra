# syntax=docker/dockerfile:1
# Yagra-core — Core/API image.
# Multi-stage: build the workspace binary, ship a slim runtime.

# Pin the build base to bookworm so the binary's glibc matches the bookworm runtime below.
# `rust:1.90-slim` is a moving tag that has rolled to Debian trixie (glibc 2.39); building there
# while the runtime is `debian:bookworm-slim` (glibc 2.36) yields a binary that fails at startup
# with `GLIBC_2.39 not found`. Keep both on bookworm.
FROM rust:1.90-slim-bookworm AS build
WORKDIR /app
COPY . .
# Reuse compiled deps + cargo registry across builds via BuildKit cache mounts. On the persistent
# self-hosted CI runner these survive between runs, so a one-line source change recompiles only the
# changed crates instead of every dependency from scratch. The same target/registry mounts carry
# into the poller image build (identical mount paths, serialised `images` matrix), so shared
# workspace deps compile once, not once per image. Cache mounts aren't part of the image filesystem,
# so copy the finished binary out of /app/target before the stage ends.
RUN --mount=type=cache,target=/app/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --bin yagra-core \
    && cp target/release/yagra-core /app/yagra-core

FROM debian:bookworm-slim AS runtime
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
RUN useradd -r -u 10001 yagra
COPY --from=build /app/yagra-core /usr/local/bin/yagra-core
USER yagra
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/yagra-core"]
