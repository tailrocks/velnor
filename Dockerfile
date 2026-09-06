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

# Rust and Mr. Boxington are installed only by the locked project mise contract.
# The mise binary itself is the documented bootstrap exception.
ENV MISE_DATA_DIR=/opt/mise \
    MISE_CACHE_DIR=/opt/mise/cache \
    MISE_CONFIG_DIR=/opt/mise/config \
    MISE_CONFIG_FILE=/opt/mise/config/mise.toml \
    CARGO_HOME=/usr/local/cargo \
    PATH=/opt/mbx/bin:/opt/mise/bin:/opt/mise/shims:$PATH \
    MISE_LOCKFILE=1 \
    MISE_LOCKED=1 \
    MISE_LOCKED_VERIFY_PROVENANCE=1 \
    MBX_CACHE_DIR=/mbx \
    MBX_TARGET_ROOT=/mbx/targets \
    MBX_GC_AUTO=true \
    MBX_GC_MAX_TOTAL_SIZE=50GiB

COPY docker/build-mise.toml /opt/mise/config/mise.toml
COPY docker/build-mise.lock /opt/mise/config/mise.lock
COPY rust-toolchain.toml /opt/mise/config/rust-toolchain.toml
RUN mkdir -p /opt/mise/bin \
    && : > /tmp/mise-empty.toml \
    && cd /opt/mise/config \
    && export MISE_GLOBAL_CONFIG_FILE=/tmp/mise-empty.toml \
    && curl -fsSL https://mise.run | MISE_VERSION="v2026.9.1" MISE_INSTALL_PATH=/opt/mise/bin/mise sh \
    && mise trust /opt/mise/config/mise.toml \
    && mise install --locked --yes rust mr-boxington \
    && mise reshim \
    && mise exec -- rustc --version \
    && XDG_DATA_HOME=/opt mise exec -- mbx setup --yes \
    && mise exec -- mbx --version | grep -F '1.8.3'

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY microvm ./microvm
COPY tools ./tools
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/mbx \
    cd /src \
    && mise exec -- mbx build --manifest-path /src/Cargo.toml --locked --release --bin velnor-runner --bin velnorctl --bin velnor-tools --bin velnor-workflow

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
COPY --from=build /src/target/release/velnor-workflow /usr/local/bin/velnor-workflow
RUN install -d -m 0755 /usr/local/share/velnor \
    && sha256sum /usr/local/bin/velnor-workflow \
        > /usr/local/share/velnor/velnor-workflow.sha256 \
    && chmod 0644 /usr/local/share/velnor/velnor-workflow.sha256 \
    && sha256sum --check --strict --status /usr/local/share/velnor/velnor-workflow.sha256

WORKDIR /work
ENTRYPOINT ["velnorctl"]
