# syntax=docker/dockerfile:1
#
# meta-whatsapp-server, built from this repository. A test deployment's
# image: the published one (public GHCR, its contents and licences listed)
# is roadmap M4. Build from the repository root:
#
#   docker build -f deploy/compose/Dockerfile -t meta-whatsapp-server:local .

FROM debian:trixie-slim AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl build-essential cmake perl pkg-config \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
# The toolchain itself comes from rust-toolchain.toml (1.98.1) on the
# first cargo call.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain none
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p meta-whatsapp-server \
    && cp target/release/meta-whatsapp-server /usr/local/bin/meta-whatsapp-server

FROM debian:trixie-slim
# ca-certificates: Meta's TLS roots, for the Graph API.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin wa
COPY --from=build /usr/local/bin/meta-whatsapp-server /usr/local/bin/meta-whatsapp-server
USER wa
# Inside the container both listeners bind every interface; compose decides
# what reaches them (the public one through Caddy, the internal one, with
# the admin API, on the host's loopback only).
ENV WA_SERVER_PUBLIC_BIND=0.0.0.0:8080 \
    WA_SERVER_INTERNAL_BIND=0.0.0.0:8081 \
    WA_SERVER_LOG_FORMAT=json
EXPOSE 8080 8081
HEALTHCHECK --interval=15s --timeout=6s --start-period=30s \
    CMD ["meta-whatsapp-server", "healthcheck"]
ENTRYPOINT ["meta-whatsapp-server"]
CMD ["serve"]
