FROM ubuntu:24.04@sha256:786a8b558f7be160c6c8c4a54f9a57274f3b4fb1491cf65146521ae77ff1dc54

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
        libbz2-dev \
        libclang-dev \
        libffi-dev \
        liblzma-dev \
        libncurses-dev \
        libreadline-dev \
        libsasl2-dev \
        libsqlite3-dev \
        libssl-dev \
        libzstd-dev \
        mold \
        openssh-client \
        pkg-config \
        protobuf-compiler \
        python3 \
        tar \
        tk-dev \
        uuid-dev \
        zlib1g-dev \
        unzip \
        xz-utils \
        zip \
        zstd \
    && rm -rf /var/lib/apt/lists/*

# Pre-install mise and the Rust CI toolchain at /opt/mise (not bind-mounted by
# Velnor at job time). At runtime Velnor sets MISE_DATA_DIR=/opt/mise so mise
# finds the pre-installed tools and skips extraction (prevents ENOMEM on Docker Desktop).
ENV HOME=/root \
    MISE_DATA_DIR=/opt/mise \
    MISE_CACHE_DIR=/opt/mise/cache \
    MISE_CONFIG_DIR=/opt/mise/config \
    PATH=/opt/mise/bin:/opt/mise/shims:$PATH \
    # Use precompiled python (python-build-standalone) instead of compiling via
    # pyenv. pyenv lacks definitions for brand-new versions (e.g. 3.14) and
    # compiling is slow; GitHub-hosted mise uses precompiled too, so this matches.
    MISE_PYTHON_COMPILE=0

RUN mkdir -p /opt/mise/bin && \
    curl -fsSL https://mise.run | MISE_INSTALL_PATH=/opt/mise/bin/mise sh && \
    mise use --global rust@1.96.0 'cargo:cargo-nextest@latest' 'cargo:rust-script@0.36.0' just@1.51.0 protoc@latest && \
    mise reshim && \
    rustup component add rustfmt clippy && \
    mise exec -- rustc --version && \
    mise exec -- cargo nextest --version && \
    mise exec -- rust-script --version && \
    mise exec -- just --version && \
    mise exec -- protoc --version

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

WORKDIR /__w
