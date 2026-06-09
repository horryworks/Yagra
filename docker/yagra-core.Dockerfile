# Yagra-core — Core/API image.
# Multi-stage: build the workspace binary, ship a slim runtime. Stub — flesh out
# the build cache layers (cargo-chef) once the crate has real dependencies.

FROM rust:1.90-slim AS build
WORKDIR /app
COPY . .
RUN cargo build --release --bin yagra-core

FROM debian:bookworm-slim AS runtime
RUN useradd -r -u 10001 yagra
COPY --from=build /app/target/release/yagra-core /usr/local/bin/yagra-core
USER yagra
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/yagra-core"]
