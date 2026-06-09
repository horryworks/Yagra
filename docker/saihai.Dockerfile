# Yagra-core — Core/API image.
# Multi-stage: build the workspace binary, ship a slim runtime. Stub — flesh out
# the build cache layers (cargo-chef) once the crate has real dependencies.

FROM rust:1.85-slim AS build
WORKDIR /app
COPY . .
RUN cargo build --release --bin saihai

FROM debian:bookworm-slim AS runtime
RUN useradd -r -u 10001 yagra
COPY --from=build /app/target/release/saihai /usr/local/bin/saihai
USER yagra
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/saihai"]
