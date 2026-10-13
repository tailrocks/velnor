FROM ubuntu:26.04@sha256:3131b4cc82a783df6c9df078f86e01819a13594b865c2cad47bd1bca2b7063bb

ARG VELNOR_IMAGE_VERSION=development
LABEL org.opencontainers.image.version="${VELNOR_IMAGE_VERSION}" \
      org.opencontainers.image.source="https://github.com/tailrocks/velnor"

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
        mold \
        openssh-client \
        pipx \
        pkg-config \
        protobuf-compiler \
        python3 \
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
    MISE_PYTHON_COMPILE=0 \
    # crates.io egress from the runner host is intermittently slow/throttled
    # (observed: curl error 28 "<10 bytes/sec" aborting cargo metadata).
    # Retry harder and allow slow transfers instead of failing the job.
    CARGO_NET_RETRY=10 \
    CARGO_HTTP_TIMEOUT=120

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

ARG KACHE_VERSION=v0.10.0
ARG KACHE_SHA256_X86_64=4f78f2897de2a5e40c1ba9cfa983deb8a17ff2d843d13f067ba3fcfa240529fc
ARG KACHE_SHA256_AARCH64=d91090996d9a5af9f348f661dc12ff2dbd4e641016a8f49180a06211a0ae2417
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

# The cargo:* tools compile from source: registry/git cache mounts + sccache
# (scoped to this RUN — runtime jobs choose their own RUSTC_WRAPPER) make a
# version bump rebuild warm instead of cold.
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/sccache-build \
    mkdir -p /opt/mise/bin && \
    curl -fsSL https://mise.run | MISE_VERSION="v2026.7.7" MISE_INSTALL_PATH=/opt/mise/bin/mise sh && \
    # gh: GitHub-hosted runner images preinstall the GitHub CLI and estate
    # scripts rely on it (e.g. jackin-role-action's download script) —
    # drop-in parity requires it in the job image too.
    RUSTC_WRAPPER=sccache SCCACHE_DIR=/sccache-build \
    mise use --global rust@1.97.1 'cargo:cargo-nextest@0.9.140' 'cargo:rust-script@0.36.0' just@1.56.0 protoc@35.1 gh@2.96.0 && \
    mise reshim && \
    rustup component add rustfmt clippy && \
    rustup target add \
      aarch64-apple-darwin \
      aarch64-unknown-linux-gnu \
      x86_64-apple-darwin \
      x86_64-unknown-linux-gnu \
      x86_64-unknown-linux-musl && \
    mise exec -- rustc --version && \
    mise exec -- cargo nextest --version && \
    mise exec -- rust-script --version && \
    mise exec -- just --version && \
    mise exec -- protoc --version && \
    mise exec -- gh --version

# cosign: backs the native sigstore/cosign-installer adapter and the
# `cosign sign` steps in the agent-role publish workflows.
RUN cosign_ver="v3.1.2" && \
    case "$(uname -m)" in \
      x86_64) cs_arch="amd64" ;; \
      aarch64|arm64) cs_arch="arm64" ;; \
      *) echo "unsupported arch $(uname -m) for cosign" >&2; exit 1 ;; \
    esac && \
    curl -fsSL -o /usr/local/bin/cosign \
      "https://github.com/sigstore/cosign/releases/download/${cosign_ver}/cosign-linux-${cs_arch}" && \
    chmod 0755 /usr/local/bin/cosign && \
    cosign version

# hadolint: backs the native hadolint/hadolint-action adapter.
RUN hadolint_ver="v2.14.0" && \
    case "$(uname -m)" in \
      x86_64) hl_arch="x86_64" ;; \
      aarch64|arm64) hl_arch="arm64" ;; \
      *) echo "unsupported arch $(uname -m) for hadolint" >&2; exit 1 ;; \
    esac && \
    curl -fsSL -o /usr/local/bin/hadolint \
      "https://github.com/hadolint/hadolint/releases/download/${hadolint_ver}/hadolint-Linux-${hl_arch}" && \
    chmod 0755 /usr/local/bin/hadolint && \
    hadolint --version

WORKDIR /__w
