FROM ubuntu:24.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        bash \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        docker-buildx \
        docker.io \
        git \
        git-lfs \
        jq \
        libclang-dev \
        libsasl2-dev \
        libssl-dev \
        libzstd-dev \
        openssh-client \
        pkg-config \
        protobuf-compiler \
        python3 \
        tar \
        unzip \
        xz-utils \
        zip \
        zstd \
    && rm -rf /var/lib/apt/lists/*

# Pre-install mise and Rust stable + cargo-nextest at /opt/mise (not bind-mounted
# by Velnor at job time). At runtime Velnor sets MISE_DATA_DIR=/opt/mise so mise
# finds the pre-installed tools and skips extraction (prevents ENOMEM on Docker Desktop).
ENV HOME=/root \
    MISE_DATA_DIR=/opt/mise \
    MISE_CACHE_DIR=/opt/mise/cache \
    MISE_CONFIG_DIR=/opt/mise/config \
    PATH=/opt/mise/bin:/opt/mise/shims:$PATH

RUN mkdir -p /opt/mise/bin && \
    curl -fsSL https://mise.run | MISE_INSTALL_PATH=/opt/mise/bin/mise sh && \
    mise use --global rust@stable 'cargo:cargo-nextest@latest' && \
    mise reshim && \
    mise exec -- rustc --version && \
    mise exec -- cargo nextest --version

WORKDIR /__w
