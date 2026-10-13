FROM ubuntu:26.04@sha256:f3d28607ddd78734bb7f71f117f3c6706c666b8b76cbff7c9ff6e5718d46ff64

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
    curl -fsSL https://mise.run | MISE_VERSION="v2026.6.1" MISE_INSTALL_PATH=/opt/mise/bin/mise sh && \
    # gh: GitHub-hosted runner images preinstall the GitHub CLI and estate
    # scripts rely on it (e.g. jackin-role-action's download script) —
    # drop-in parity requires it in the job image too.
    mise use --global rust@1.96.0 'cargo:cargo-nextest@0.9.137' 'cargo:rust-script@0.36.0' just@1.52.0 protoc@35.0 gh@2.93.0 && \
    mise reshim && \
    rustup component add rustfmt clippy && \
    mise exec -- rustc --version && \
    mise exec -- cargo nextest --version && \
    mise exec -- rust-script --version && \
    mise exec -- just --version && \
    mise exec -- protoc --version && \
    mise exec -- gh --version

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

# cosign: backs the native sigstore/cosign-installer adapter and the
# `cosign sign` steps in the agent-role publish workflows.
RUN cosign_ver="v3.1.1" && \
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
