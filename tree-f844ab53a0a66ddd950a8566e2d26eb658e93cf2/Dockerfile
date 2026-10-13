FROM rust:1.96-bookworm@sha256:13c186980fa33cc12759b429662a1322939dbe697484b7c33b47dd2698d28460 AS build

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --bin velnor-runner --bin velnor-tools

FROM ubuntu:24.04@sha256:786a8b558f7be160c6c8c4a54f9a57274f3b4fb1491cf65146521ae77ff1dc54

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
