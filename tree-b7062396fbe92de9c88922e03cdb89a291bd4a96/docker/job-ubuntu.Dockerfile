FROM ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b

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
        file \
        fonts-freefont-ttf \
        fonts-ipafont-gothic \
        fonts-liberation \
        fonts-noto-color-emoji \
        fonts-tlwg-loma-otf \
        fonts-unifont \
        fonts-wqy-zenhei \
        git \
        git-lfs \
        gnupg \
        jq \
        libbz2-dev \
        libasound2t64 \
        libatk-bridge2.0-0t64 \
        libatk1.0-0t64 \
        libatspi2.0-0t64 \
        libcairo2 \
        libclang-dev \
        libcups2t64 \
        libdbus-1-3 \
        libdrm2 \
        libffi-dev \
        libfontconfig1 \
        libfreetype6 \
        libgbm1 \
        libglib2.0-0t64 \
        liblzma-dev \
        libncurses-dev \
        libnspr4 \
        libnss3 \
        libpango-1.0-0 \
        libreadline-dev \
        libsasl2-dev \
        libsqlite3-dev \
        libssl-dev \
        libzstd-dev \
        libx11-6 \
        libxcb1 \
        libxcomposite1 \
        libxdamage1 \
        libxext6 \
        libxfixes3 \
        libxkbcommon0 \
        libxrandr2 \
        openssh-client \
        pkg-config \
        sudo \
        tar \
        tk-dev \
        uuid-dev \
        zlib1g-dev \
        unzip \
        util-linux \
        xz-utils \
        xfonts-cyrillic \
        xfonts-scalable \
        xvfb \
        zip \
        zstd \
    && gpg --version \
    && gpgv --version \
    && rm -rf /var/lib/apt/lists/*

# Pre-install mise and the Rust CI toolchain at /opt/mise (not bind-mounted by
# Velnor at job time). At runtime Velnor sets MISE_DATA_DIR=/opt/mise so mise
# finds the pre-installed tools and skips extraction (prevents ENOMEM on Docker Desktop).
ENV HOME=/root \
    MISE_DATA_DIR=/opt/mise \
    MISE_CACHE_DIR=/opt/mise/cache \
    MISE_CONFIG_DIR=/opt/mise/config \
    PATH=/root/.cargo/bin:/opt/mise/bin:/opt/mise/shims:$PATH \
    MISE_LOCKFILE=1 \
    MISE_LOCKED=1 \
    MISE_LOCKED_VERIFY_PROVENANCE=1 \
    # Use precompiled python (python-build-standalone) instead of compiling via
    # pyenv. pyenv lacks definitions for brand-new versions (e.g. 3.14) and
    # compiling is slow; GitHub-hosted mise uses precompiled too, so this matches.
    MISE_PYTHON_COMPILE=0 \
    # crates.io egress from the runner host is intermittently slow/throttled
    # (observed: curl error 28 "<10 bytes/sec" aborting cargo metadata).
    # Retry harder and allow slow transfers instead of failing the job.
    CARGO_NET_RETRY=10 \
    CARGO_HTTP_TIMEOUT=120

<<<<<<< origin/main
=======
# GitHub-hosted Ubuntu exposes Node to ordinary run steps independently of an
# action's bundled runtime. Type-aware Oxlint plugins and other estate tools
# rely on that base-runner contract even when Bun is their package manager.
# Install the latest stable Node release as an architecture-pinned, verified
# system tool; jobs never download or compile it.
ARG NODE_VERSION=v26.5.0
ARG NODE_SHA256_X86_64=9f619528f1db5ddc41dccf54211066fb42228d69a156733c69cb9d6cc92e358c
ARG NODE_SHA256_AARCH64=036df0b49662ebb350eb56f1cac603699b1e9ed1e2603ee129fefda473479030
RUN ver="$NODE_VERSION" && \
    case "$(uname -m)" in \
      x86_64) arch="x64"; sha="$NODE_SHA256_X86_64" ;; \
      aarch64|arm64) arch="arm64"; sha="$NODE_SHA256_AARCH64" ;; \
      *) echo "unsupported arch $(uname -m) for Node.js" >&2; exit 1 ;; \
    esac && \
    tmp="$(mktemp -d)" && \
    curl -fsSL "https://nodejs.org/dist/${ver}/node-${ver}-linux-${arch}.tar.xz" -o "$tmp/node.tar.xz" && \
    echo "$sha  $tmp/node.tar.xz" | sha256sum -c - && \
    tar -xJf "$tmp/node.tar.xz" -C /usr/local --strip-components=1 && \
    rm -rf "$tmp" && \
    node --version && \
    npm --version

ARG SCCACHE_VERSION=v0.16.0
ARG SCCACHE_SHA256_X86_64=aec995a83ad3dff3d14b6314e08858b7b73d35ca85a5bcf3d3a9ec07dee35588
ARG SCCACHE_SHA256_AARCH64=f73a5c39f96bb6ebb89cc7915cf182260d4cbf30765322c5e793d0fe8bd80784
RUN ver="$SCCACHE_VERSION" && \
    case "$(uname -m)" in \
      x86_64) arch="x86_64-unknown-linux-musl"; sha="$SCCACHE_SHA256_X86_64" ;; \
      aarch64|arm64) arch="aarch64-unknown-linux-musl"; sha="$SCCACHE_SHA256_AARCH64" ;; \
      *) echo "unsupported arch $(uname -m) for sccache" >&2; exit 1 ;; \
    esac && \
    tmp="$(mktemp -d)" && \
    curl -fsSL "https://github.com/mozilla/sccache/releases/download/${ver}/sccache-${ver}-${arch}.tar.gz" -o "$tmp/sccache.tar.gz" && \
    echo "$sha  $tmp/sccache.tar.gz" | sha256sum -c - && \
    tar -xzf "$tmp/sccache.tar.gz" -C "$tmp" && \
    install -m 0755 "$tmp/sccache-${ver}-${arch}/sccache" /usr/local/bin/sccache && \
    rm -rf "$tmp" && \
    sccache --version

ARG KACHE_VERSION=v0.16.0
ARG KACHE_SHA256_X86_64=caee657c662379475af2a0a7611ad32a6d053822036c1ec191bb8fd1c826d54b
ARG KACHE_SHA256_AARCH64=6eabb67867022eecdbfffe43c16e75b5c5b561983742bda3c900a4cb4c50e4a7
RUN ver="$KACHE_VERSION" && \
    case "$(uname -m)" in \
      x86_64) arch="x86_64-unknown-linux-musl"; sha="$KACHE_SHA256_X86_64" ;; \
      aarch64|arm64) arch="aarch64-unknown-linux-musl"; sha="$KACHE_SHA256_AARCH64" ;; \
      *) echo "unsupported arch $(uname -m) for kache" >&2; exit 1 ;; \
    esac && \
    tmp="$(mktemp -d)" && \
    curl -fsSL "https://github.com/kunobi-ninja/kache/releases/download/${ver}/kache-${arch}.tar.gz" -o "$tmp/kache.tar.gz" && \
    echo "$sha  $tmp/kache.tar.gz" | sha256sum -c - && \
    tar -xzf "$tmp/kache.tar.gz" -C "$tmp" && \
    install -m 0755 "$tmp/kache" /usr/local/bin/kache && \
    rm -rf "$tmp" && \
    kache --version

>>>>>>> b0b5f75
# Plan 008: the whole job toolchain is a committed, locked mise config
# (docker/job-mise.toml + docker/job-mise.lock) installed fail-closed. The only
# network bootstrap is the mise binary itself; every language and executable
# comes from `mise install --locked` against pinned URLs/checksums. Native mise
# aliases are preferred; alternate backends are explicit in job-mise.toml only
# where the native registry has no suitable tool (for example Kache).
COPY docker/job-mise.toml /opt/mise/config/config.toml
COPY docker/job-mise.toml /opt/mise/config/mise.toml
COPY docker/job-mise.lock /opt/mise/config/mise.lock
COPY rust-toolchain.toml /opt/mise/config/rust-toolchain.toml
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/sccache-build \
    --mount=type=secret,id=mise_github_token,required=true \
    mkdir -p /opt/mise/bin && \
    : > /tmp/mise-empty.toml && \
    cd /opt/mise/config && \
    export MISE_GLOBAL_CONFIG_FILE=/tmp/mise-empty.toml && \
    # Baked bootstrap of the mise binary at the fleet-pinned version. This is
    # the read-only /opt/mise/bin bootstrap; runtime never rewrites it.
    curl -fsSL https://mise.run | MISE_VERSION="v2026.8.14" MISE_INSTALL_PATH=/opt/mise/bin/mise sh && \
    mise trust /opt/mise/config/mise.toml && \
    # Install the native compiler cache first. Cargo-backed mise tools may
    # compile during installation, so the wrapper must already be executable.
    MISE_GITHUB_TOKEN="$(cat /run/secrets/mise_github_token)" \
    mise install --locked --yes sccache && \
    mise reshim && \
    # Fail-closed, non-interactive install of the remaining locked toolchain.
    MISE_GITHUB_TOKEN="$(cat /run/secrets/mise_github_token)" \
    RUSTC_WRAPPER=sccache SCCACHE_DIR=/sccache-build \
    mise install --locked --yes && \
    mise reshim && \
    mise exec -- rustc --version && \
    mise exec -- node --version && \
    mise exec -- npm --version && \
    mise exec -- python3 --version && \
    mise exec -- sccache --version && \
    mise exec -- kache --version && \
    mise exec -- hadolint --version && \
    mise exec -- cargo nextest --version && \
    mise exec -- rust-script --version && \
    mise exec -- just --version && \
    mise exec -- protoc --version && \
    mise exec -- gh --version && \
    mise exec -- mold --version && \
    mise exec -- cosign version

WORKDIR /__w

# Release metadata must remain after every expensive filesystem layer. Putting
# this ARG/LABEL near FROM makes each version bump invalidate the complete
# dual-architecture toolchain build even though no installed byte changed.
ARG VELNOR_IMAGE_VERSION=development
LABEL org.opencontainers.image.version="${VELNOR_IMAGE_VERSION}" \
      org.opencontainers.image.source="https://github.com/tailrocks/velnor"
