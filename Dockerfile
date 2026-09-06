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
COPY crates/velnor-model/Cargo.toml ./crates/velnor-model/Cargo.toml
COPY crates/velnor-control/Cargo.toml ./crates/velnor-control/Cargo.toml
COPY crates/velnor-client/Cargo.toml ./crates/velnor-client/Cargo.toml
COPY crates/velnor-render/Cargo.toml ./crates/velnor-render/Cargo.toml
COPY crates/velnorctl/Cargo.toml ./crates/velnorctl/Cargo.toml
COPY crates/velnor-runner/Cargo.toml ./crates/velnor-runner/Cargo.toml
COPY crates/velnor-workflow/Cargo.toml ./crates/velnor-workflow/Cargo.toml
COPY crates/velnor-tools/Cargo.toml ./crates/velnor-tools/Cargo.toml
COPY crates/velnor-bench/Cargo.toml ./crates/velnor-bench/Cargo.toml
COPY tools/unit-collector/Cargo.toml ./tools/unit-collector/Cargo.toml
RUN mkdir -p \
        crates/velnor-model/src \
        crates/velnor-control/src \
        crates/velnor-client/src \
        crates/velnor-render/src \
        crates/velnorctl/src \
        crates/velnor-runner/src/bin \
        crates/velnor-workflow/src \
        crates/velnor-tools/src \
        crates/velnor-bench/src \
        tools/unit-collector/src \
    && touch \
        crates/velnor-model/src/lib.rs \
        crates/velnor-control/src/lib.rs \
        crates/velnor-client/src/lib.rs \
        crates/velnor-render/src/lib.rs \
        crates/velnorctl/src/lib.rs \
        crates/velnorctl/src/main.rs \
        crates/velnor-runner/build.rs \
        crates/velnor-runner/src/lib.rs \
        crates/velnor-runner/src/main.rs \
        crates/velnor-runner/src/bin/velnor-guest-agent.rs \
        crates/velnor-runner/src/bin/velnor-guest-image.rs \
        crates/velnor-workflow/src/lib.rs \
        crates/velnor-workflow/src/main.rs \
        crates/velnor-tools/src/main.rs \
        crates/velnor-bench/src/lib.rs \
        crates/velnor-bench/src/main.rs \
        tools/unit-collector/src/lib.rs \
        tools/unit-collector/src/main.rs
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cd /opt/mise/config \
    && mise exec -- cargo fetch --manifest-path /src/Cargo.toml --locked
COPY crates/velnor-model ./crates/velnor-model
COPY crates/velnor-control ./crates/velnor-control
COPY crates/velnor-client ./crates/velnor-client
COPY crates/velnor-render ./crates/velnor-render
COPY crates/velnorctl ./crates/velnorctl
COPY crates/velnor-runner ./crates/velnor-runner
COPY crates/velnor-workflow ./crates/velnor-workflow
COPY crates/velnor-tools ./crates/velnor-tools
COPY microvm ./microvm
COPY tools/unit-collector ./tools/unit-collector

# Pull-request Docker validation stops here. It validates the locked build
# toolchain, dependency fetch, and complete source context without rebuilding
# the release binaries for every Rust source edit. Full/default builds below
# remain the release-image guardrail used by trusted main and release flows.
RUN cd /opt/mise/config \
    && mise exec -- mbx --version | grep -F '1.8.3' \
    && test -f /src/Cargo.lock \
    && test -f /src/crates/velnor-workflow/src/lib.rs \
    && touch /tmp/velnor-ci-inputs-validated

FROM ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b AS ci
COPY --from=build /tmp/velnor-ci-inputs-validated /usr/local/share/velnor/ci-inputs-validated
RUN test -f /usr/local/share/velnor/ci-inputs-validated

FROM build AS release
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/mbx \
    cd /opt/mise/config \
    && CARGO_TARGET_DIR=/src/target mise exec -- mbx build --manifest-path /src/Cargo.toml --locked --release --bin velnor-runner --bin velnorctl --bin velnor-tools --bin velnor-workflow

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
COPY --from=release /src/target/release/velnorctl /usr/local/bin/velnorctl
COPY --from=release /src/target/release/velnor-runner /usr/local/bin/velnor-runner
COPY --from=release /src/target/release/velnor-tools /usr/local/bin/velnor-tools
COPY --from=release /src/target/release/velnor-workflow /usr/local/bin/velnor-workflow
RUN install -d -m 0755 /usr/local/share/velnor \
    && sha256sum /usr/local/bin/velnor-workflow \
        > /usr/local/share/velnor/velnor-workflow.sha256 \
    && chmod 0644 /usr/local/share/velnor/velnor-workflow.sha256 \
    && sha256sum --check --strict --status /usr/local/share/velnor/velnor-workflow.sha256

WORKDIR /work
ENTRYPOINT ["velnorctl"]
