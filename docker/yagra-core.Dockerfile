# Yagra-core — Core/API image.
# Multi-stage: build the workspace binary, ship a slim runtime. Stub — flesh out
# the build cache layers (cargo-chef) once the crate has real dependencies.

FROM rust:1.90-slim AS build
WORKDIR /app
COPY . .
RUN cargo build --release --bin yagra-core

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
COPY --from=build /app/target/release/yagra-core /usr/local/bin/yagra-core
USER yagra
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/yagra-core"]
