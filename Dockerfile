FROM rust:1.97-bookworm@sha256:77fac8b98f9f46062bb680b6d25d5bcaabfc400143952ebc572e924bcbedc3fa AS build

# sccache: object-level compiler cache in a BuildKit cache mount so source
# changes rebuild from warm objects (estate instant-cache mandate).
RUN ver="v0.15.0" && \
    case "$(uname -m)" in \
      x86_64) arch="x86_64-unknown-linux-musl" ;; \
      aarch64|arm64) arch="aarch64-unknown-linux-musl" ;; \
      *) echo "unsupported arch $(uname -m) for sccache" >&2; exit 1 ;; \
    esac && \
    tmp="$(mktemp -d)" && \
    curl -fsSL "https://github.com/mozilla/sccache/releases/download/${ver}/sccache-${ver}-${arch}.tar.gz" -o "$tmp/sccache.tar.gz" && \
    tar -xzf "$tmp/sccache.tar.gz" -C "$tmp" && \
    install -m 0755 "$tmp/sccache-${ver}-${arch}/sccache" /usr/local/bin/sccache && \
    rm -rf "$tmp" && \
    sccache --version
ENV RUSTC_WRAPPER=sccache \
    SCCACHE_DIR=/sccache

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/sccache \
    cargo build --release --bin velnor-runner --bin velnor-tools \
    && sccache --show-stats

FROM ubuntu:26.04@sha256:3131b4cc82a783df6c9df078f86e01819a13594b865c2cad47bd1bca2b7063bb

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
