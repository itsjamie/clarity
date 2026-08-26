# syntax=docker/dockerfile:1.7

FROM rust:1.96-bookworm AS protocol-builder
WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY web/src/generated ./web/src/generated
RUN cargo run -p clarity-xtask -- export-protocol

FROM node:24-bookworm-slim AS web-builder
ENV PNPM_HOME=/pnpm
ENV PATH=$PNPM_HOME:$PATH
WORKDIR /workspace/web
RUN corepack enable && corepack prepare pnpm@11.9.0 --activate
COPY web/package.json web/pnpm-lock.yaml web/pnpm-workspace.yaml ./
RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm install --frozen-lockfile
COPY web ./
COPY --from=protocol-builder /workspace/web/src/generated ./src/generated
RUN pnpm lint && pnpm typecheck && pnpm test && pnpm build

FROM rust:1.96-bookworm AS rust-builder
WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY --from=web-builder /workspace/web/dist ./web/dist
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-target,target=/workspace/target \
    cargo build --locked --release -p clarity-server && \
    cp /workspace/target/release/clarity-server /tmp/clarity-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --system --gid 10001 clarity && \
    useradd --system --uid 10001 --gid clarity --home-dir /nonexistent --shell /usr/sbin/nologin clarity
COPY --from=rust-builder /tmp/clarity-server /usr/local/bin/clarity-server
USER 10001:10001
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/clarity-server"]
