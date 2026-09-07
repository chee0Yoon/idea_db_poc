# syntax=docker/dockerfile:1
ARG RUST_IMAGE=rust:1.98.0-trixie@sha256:620dbcd124499c59e2406d3741574b5c5838cf9eb9656f0c3a03948f79b02959
ARG NEO4J_IMAGE=neo4j:5.26.30-community@sha256:037cf5756f0135cbfd66b739b6df7c7c4bb100f9ce11602f6f9538e17e02c74d
FROM ${RUST_IMAGE} AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY docs/mcp-intake.md docs/api.md ./docs/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked && cp target/release/idea-db /idea-db && cp target/release/idea-db-mcp /idea-db-mcp

# Optional API-only deployment; NEO4J_URI points to an existing Neo4j service.
FROM debian:trixie-slim AS api
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget tini \
    && rm -rf /var/lib/apt/lists/* && useradd --system --uid 7474 idea-db
COPY --from=builder /idea-db /usr/local/bin/idea-db
COPY --from=builder /idea-db-mcp /usr/local/bin/idea-db-mcp
COPY static /opt/idea-db/static
ENV IDEA_DB_BIND=0.0.0.0:8080 STATIC_DIR=/opt/idea-db/static
USER 7474
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s --start-period=40s --retries=6 \
  CMD wget -q -O /dev/null http://127.0.0.1:8080/health/ready || exit 1
ENTRYPOINT ["tini", "-g", "--", "/usr/local/bin/idea-db"]

# Default: one independently runnable image containing Neo4j and the Rust API.
FROM ${NEO4J_IMAGE} AS standalone
COPY --from=builder /idea-db /usr/local/bin/idea-db
COPY --from=builder /idea-db-mcp /usr/local/bin/idea-db-mcp
COPY static /opt/idea-db/static
COPY scripts/standalone-entrypoint.sh /opt/idea-db/entrypoint.sh
RUN chmod 755 /opt/idea-db/entrypoint.sh
ENV IDEA_DB_BIND=0.0.0.0:8080 STATIC_DIR=/opt/idea-db/static \
    NEO4J_URI=http://127.0.0.1:7474 NEO4J_USER=neo4j \
    NEO4J_server_default__listen__address=127.0.0.1 \
    NEO4J_dbms_usage__report_enabled=false \
    NEO4J_server_memory_heap_initial__size=256m \
    NEO4J_server_memory_heap_max__size=512m \
    NEO4J_server_memory_pagecache_size=256m
EXPOSE 8080
VOLUME ["/data", "/logs"]
HEALTHCHECK --interval=10s --timeout=3s --start-period=90s --retries=6 \
  CMD wget -q -O /dev/null http://127.0.0.1:8080/health/ready || exit 1
ENTRYPOINT ["tini", "-g", "--", "/opt/idea-db/entrypoint.sh"]
CMD []
