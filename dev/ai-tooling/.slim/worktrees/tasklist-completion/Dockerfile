# syntax=docker/dockerfile:1.7

# ---------------------------------------------------------------------------
# KVNC full-node image.
#
#   docker build -t kvnc-node:local .
#
# The image only carries the node binary and a tiny runtime; keys are never
# baked in. See ops/docker/README.md for the local devnet.
# ---------------------------------------------------------------------------

FROM rust:1.90-bookworm AS builder

WORKDIR /src

# Copy the workspace manifests first so dependency resolution stays cached
# across source-only changes. Every workspace member's manifest is required.
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/

# Build the node from the committed lockfile. Cargo's registry/git caches use
# BuildKit cache mounts and are not part of the final image; the compiled
# target/ is kept (we copy the binary out below).
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo build --release --locked -p kvnc-node \
    && strip target/release/kvnc-node

# ---------------------------------------------------------------------------

FROM debian:bookworm-slim AS runtime

# ca-certificates is needed for outbound DNS/TLS to seeds; libgcc-s1 is the
# unwinder used by Rust; curl backs the healthcheck. Nothing else.
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates libgcc-s1 curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 kvnc \
    && useradd --system --uid 10001 --gid 10001 \
         --home-dir /var/lib/kvnc --no-create-home --shell /usr/sbin/nologin kvnc \
    && install -d -o kvnc -g kvnc /var/lib/kvnc

COPY --from=builder /src/target/release/kvnc-node /usr/local/bin/kvnc-node
COPY ops/docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENV KVNC_DATA_DIR=/var/lib/kvnc
WORKDIR /var/lib/kvnc
VOLUME ["/var/lib/kvnc"]
EXPOSE 9000 8545

USER kvnc

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${KVNC_RPC_PORT:-8545}/health" >/dev/null || exit 1

# The entrypoint provisions a persistent validator seed inside the data volume
# (never in the image) and then execs the node.
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]

# Keep a plain `docker run` safe: it prints usage instead of booting a node.
CMD ["--help"]