FROM ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b AS build

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        git \
        perl \
        pkg-config \
        tar \
    && rm -rf /var/lib/apt/lists/*

# Rust and sccache are installed only by the locked project mise contract.
# The mise binary itself is the documented bootstrap exception.
ENV MISE_DATA_DIR=/opt/mise \
    MISE_CACHE_DIR=/opt/mise/cache \
    MISE_CONFIG_DIR=/opt/mise/config \
    MISE_CONFIG_FILE=/opt/mise/config/mise.toml \
    CARGO_HOME=/usr/local/cargo \
    PATH=/opt/mise/bin:/opt/mise/shims:$PATH \
    MISE_LOCKFILE=1 \
    MISE_LOCKED=1 \
    MISE_LOCKED_VERIFY_PROVENANCE=1

COPY docker/build-mise.toml /opt/mise/config/mise.toml
COPY docker/build-mise.lock /opt/mise/config/mise.lock
COPY rust-toolchain.toml /opt/mise/config/rust-toolchain.toml
RUN mkdir -p /opt/mise/bin \
    && : > /tmp/mise-empty.toml \
    && cd /opt/mise/config \
    && export MISE_GLOBAL_CONFIG_FILE=/tmp/mise-empty.toml \
    && curl -fsSL https://mise.run | MISE_VERSION="v2026.9.1" MISE_INSTALL_PATH=/opt/mise/bin/mise sh \
    && mise trust /opt/mise/config/mise.toml \
    && mise install --locked --yes rust sccache \
    && mise reshim \
    && mise exec -- rustc --version \
    && mise exec -- sccache --version

# sccache: object-level compiler cache in a BuildKit cache mount so source
# changes rebuild from warm objects (estate instant-cache mandate).
ENV RUSTC_WRAPPER=sccache \
    SCCACHE_DIR=/sccache

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY microvm ./microvm
COPY tools ./tools
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/sccache \
    cd /opt/mise/config \
    && mise exec -- cargo build --manifest-path /src/Cargo.toml --release --bin velnor-runner --bin velnorctl --bin velnor-tools \
    && mise exec -- sccache --show-stats

FROM ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b

USER root
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        curl \
        docker-buildx \
        docker.io \
        git \
        jq \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/velnorctl /usr/local/bin/velnorctl
COPY --from=build /src/target/release/velnor-runner /usr/local/bin/velnor-runner
COPY --from=build /src/target/release/velnor-tools /usr/local/bin/velnor-tools

WORKDIR /work
ENTRYPOINT ["velnorctl"]
