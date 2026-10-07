FROM rust:1.90-bookworm AS builder

WORKDIR /src
COPY Cargo.toml ./
COPY crates/ ./crates/

RUN cargo build --release --package kvnc-node

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates libgcc-s1 curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /home/kvnc kvnc \
    && install -d --owner=kvnc --group=kvnc /var/lib/kvnc

COPY --from=builder /src/target/release/kvnc-node /usr/local/bin/kvnc-node

ENV KVNC_DATA_DIR=/var/lib/kvnc
WORKDIR /var/lib/kvnc
VOLUME ["/var/lib/kvnc"]
EXPOSE 9000 8545
USER kvnc
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD response="$(curl -fsS http://127.0.0.1:8545/health)" && printf '%s' "$response" | grep -Eq '"status"[[:space:]]*:[[:space:]]*"ok".*"peer_count"[[:space:]]*:[[:space:]]*[0-9]+'

# Keep a plain `docker run` safe: starting a node must be an explicit choice.
ENTRYPOINT ["/usr/local/bin/kvnc-node"]
CMD ["--help"]
