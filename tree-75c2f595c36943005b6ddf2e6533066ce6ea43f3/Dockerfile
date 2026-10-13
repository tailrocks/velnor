FROM rust:1.96-bookworm AS build

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --bin velnor-runner --bin velnor-tools

FROM ubuntu:24.04

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
COPY --from=build /src/target/release/velnor-runner /usr/local/bin/velnor-runner
COPY --from=build /src/target/release/velnor-tools /usr/local/bin/velnor-tools

WORKDIR /work
ENTRYPOINT ["velnor-runner"]
