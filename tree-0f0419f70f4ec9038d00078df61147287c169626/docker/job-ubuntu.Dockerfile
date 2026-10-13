# Docker CLI + Buildx plugin source. Ubuntu ships no `docker-cli` package: the
# only apt route to the client is `docker.io`, which drags in the full Engine
# (dockerd, containerd, runc, iptables). Velnor jobs never run a daemon inside
# the container — they talk to the *host* Engine through the lease proxy
# (crates/velnor-runner/src/docker_lease.rs, DOCKER_HOST=unix:///var/run/docker.sock
# bind-mounted by crates/velnor-runner/src/container.rs) — so the daemon and its
# runtimes are dead weight. Take the client (and only the client) from the
# official multi-arch CLI image, pinned by manifest-list digest.
FROM docker:29-cli@sha256:3f4743208d2338c934d7b8bcfbe1bb54c0b2355c510ad5e0f31c0c4a54bd704e AS dockercli

# Rust-free view of the committed job toolchain lock.
#
# `docker/job-mise.lock` mirrors the Rust channel resolved from
# rust-toolchain.toml (a plain version mirror — the entry carries no url and no
# checksum, unlike every other tool). Copying that file into the layer that
# installs Node, Python, gh, mold, protoc, just, hadolint, sccache and mbx would
# re-couple the two version axes: a Rust patch bump edits the lock, the lock
# invalidates the layer, and ~600 MB of unrelated toolchain is reinstalled. This
# stage strips the mirror, so the non-Rust toolchain layer below is keyed on a
# file that a Rust bump does not change.
FROM ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b AS toolset-config
COPY docker/job-mise.toml docker/job-mise.lock /in/
RUN mkdir -p /out \
    && cp /in/job-mise.toml /out/mise.toml \
    && awk '/^\[\[tools\.rust\]\]$/ { skip = 1; next } \
            /^\[tools\.rust\./       { skip = 1; next } \
            /^\[/                    { skip = 0 } \
            !skip' /in/job-mise.lock > /out/mise.lock \
    && ! grep -q 'core:rust' /out/mise.lock

FROM ubuntu:26.04@sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b

# Packages the toolchain install itself needs: a C/C++ toolchain and CMake for
# the mise entries that build from source (`cargo:cargo-deny`) and for crates
# with native build scripts, plus the archive and TLS tools mise downloads
# through. This set is near-frozen; the volatile job-runtime package set is a
# separate layer *after* the toolchain, so editing it no longer invalidates the
# ~2.8 GB of installed toolchain the way one combined apt layer did.
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        cmake \
        curl \
        git \
        libssl-dev \
        libzstd-dev \
        pkg-config \
        tar \
        unzip \
        xz-utils \
        zlib1g-dev \
        zstd \
    && rm -rf /var/lib/apt/lists/*

# Pre-install mise and the Rust CI toolchain at /opt/mise (not bind-mounted by
# Velnor at job time). At runtime Velnor sets MISE_DATA_DIR=/opt/mise so mise
# finds the pre-installed tools and skips extraction (prevents ENOMEM on Docker Desktop).
ENV HOME=/root \
    MISE_DATA_DIR=/opt/mise \
    MISE_CACHE_DIR=/opt/mise/cache \
    MISE_CONFIG_DIR=/opt/mise/config \
    PATH=/opt/mbx/bin:/root/.cargo/bin:/opt/mise/bin:/opt/mise/shims:$PATH \
    MISE_LOCKFILE=1 \
    MISE_LOCKED=1 \
    MISE_LOCKED_VERIFY_PROVENANCE=1 \
    # Use precompiled python (python-build-standalone) instead of compiling via
    # pyenv. pyenv lacks definitions for brand-new versions (e.g. 3.14) and
    # compiling is slow; GitHub-hosted mise uses precompiled too, so this matches.
    # Because nothing compiles CPython here, the pyenv build dependencies
    # (libbz2/liblzma/libncurses/libreadline/libsqlite3/libffi/tk/uuid -dev) are
    # deliberately absent from every apt layer above and below.
    MISE_PYTHON_COMPILE=0 \
    # crates.io egress from the runner host is intermittently slow/throttled
    # (observed: curl error 28 "<10 bytes/sec" aborting cargo metadata).
    # Retry harder and allow slow transfers instead of failing the job.
    CARGO_NET_RETRY=10 \
    CARGO_HTTP_TIMEOUT=120

# Baked bootstrap of the mise binary at the fleet-pinned version. This is the
# read-only /opt/mise/bin bootstrap; runtime never rewrites it. Its own layer,
# so it is keyed on MISE_VERSION alone.
RUN mkdir -p /opt/mise/bin \
    && curl -fsSL https://mise.run | MISE_VERSION="v2026.9.1" MISE_INSTALL_PATH=/opt/mise/bin/mise sh \
    && mise --version

# Plan 008: the whole job toolchain is a committed, locked mise config
# (docker/job-mise.toml + docker/job-mise.lock) installed fail-closed. The only
# network bootstrap is the mise binary itself; every language and executable
# comes from `mise install --locked` against pinned URLs/checksums. Native mise
# aliases are preferred; alternate backends are explicit in job-mise.toml only.
#
# Everything except Rust and the `cargo:` backend installs here, against the
# Rust-free lock view. `cargo:cargo-deny` is compiled by the toolchain, so it
# genuinely depends on it and belongs in the Rust layer below.
#
# Cosign (127 MB unpacked) serves exactly one adapter and would be a good
# candidate to install lazily — the adapter already runs `mise install --locked
# --yes cosign` itself. Leaving it out of this list does not work: mise's
# `exec_auto_install` makes any `mise exec` materialise the *whole* configured
# toolset, so the next `mise exec` in this build (and the first one in any job)
# reinstalls it. Making it genuinely lazy means either turning off
# `exec_auto_install` image-wide, which changes tool resolution for every user
# workflow, or dropping cosign from job-mise.toml and giving the adapter its own
# pinned source in crates/velnor-runner/src/executor.rs. Until one of those
# lands, install it here deliberately rather than as an implicit side effect.
COPY --from=toolset-config /out/mise.toml /out/mise.lock /opt/mise/config/
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=secret,id=mise_github_token,required=true \
    : > /tmp/mise-empty.toml \
    && cd /opt/mise/config \
    && export MISE_GLOBAL_CONFIG_FILE=/tmp/mise-empty.toml \
    && mise trust /opt/mise/config/mise.toml \
    && MISE_GITHUB_TOKEN="$(cat /run/secrets/mise_github_token)" \
       mise install --locked --yes \
         'aqua:nextest-rs/nextest/cargo-nextest' \
         'github:fornwall/rust-script' \
         cosign \
         gh \
         hadolint \
         just \
         mold \
         mr-boxington \
         node \
         protoc \
         python \
         sccache \
    && mise reshim

# The Rust toolchain, and only the Rust toolchain, is gated on
# rust-toolchain.toml. The committed config pair is restored to its canonical
# runtime shape here (config.toml is the mise global config read by jobs;
# mise.toml is the same file under the name mise resolves by walking up from
# /opt/mise/config — one COPY, then a copy, rather than the same source COPYed
# twice).
COPY docker/job-mise.toml /opt/mise/config/mise.toml
COPY docker/job-mise.lock /opt/mise/config/mise.lock
COPY rust-toolchain.toml /opt/mise/config/rust-toolchain.toml
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=secret,id=mise_github_token,required=true \
    cp /opt/mise/config/mise.toml /opt/mise/config/config.toml \
    && : > /tmp/mise-empty.toml \
    && cd /opt/mise/config \
    && export MISE_GLOBAL_CONFIG_FILE=/tmp/mise-empty.toml \
    && mise trust /opt/mise/config/mise.toml \
    && MISE_GITHUB_TOKEN="$(cat /run/secrets/mise_github_token)" \
       mise install --locked --yes rust 'cargo:cargo-deny' \
    && mise reshim \
    # Install upstream's stable Cargo launcher without activating an interactive
    # shell. XDG_DATA_HOME places it at /opt/mbx/bin, first on the image PATH.
    && XDG_DATA_HOME=/opt mise exec -- mbx setup --yes \
    && mbx_real="$(mise which mbx)" \
    && printf '#!/bin/sh\nXDG_DATA_HOME=/opt exec %s "$@"\n' "$mbx_real" > /opt/mbx/bin/mbx \
    && chmod 0755 /opt/mbx/bin/mbx \
    && test "$(command -v cargo)" = /opt/mbx/bin/cargo \
    && test "$(command -v mbx)" = /opt/mbx/bin/mbx \
    && test "$(cat /opt/mbx/bin/mbx-target)" = "$mbx_real" \
    && cargo --version \
    && MBX_DISABLE=1 cargo --version \
    && mbx --version | grep -F '1.7.0' \
    && mbx doctor \
    && test -z "${RUSTC_WRAPPER:-}" \
    && mise exec -- sccache --version | grep -F 'sccache 0.16.0' \
    && ! command -v kache \
    && mise exec -- rustc --version \
    && mise exec -- node --version \
    && mise exec -- npm --version \
    && mise exec -- python3 --version \
    && mise exec -- hadolint --version \
    && mise exec -- cargo nextest --version \
    && mise exec -- rust-script --version \
    && mise exec -- just --version \
    && mise exec -- protoc --version \
    && mise exec -- gh --version \
    && mise exec -- mold --version \
    && mise exec -- cosign version

# Job-runtime packages: everything a *workflow* may need but the toolchain
# install does not. This is the volatile half of the package set, so it sits
# after the toolchain layers — editing it no longer reinstalls Rust, Node and
# Python. clang/LLVM is absent on purpose: no crate in Cargo.lock uses bindgen,
# and the mold adapter deliberately links through gcc's -fuse-ld=mold rather
# than requiring clang.
#
# The X11/GTK shared libraries and the font set are Playwright's system
# dependencies. Velnor persists the *browser* payload host-side
# (crates/velnor-runner/src/container.rs, ~/.cache/ms-playwright) but not the
# system libraries, and `playwright install-deps` at job time would need root
# apt with network access, which the one-toolchain contract forbids.
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        bash \
        file \
        fonts-freefont-ttf \
        fonts-ipafont-gothic \
        fonts-liberation \
        fonts-noto-color-emoji \
        fonts-tlwg-loma-otf \
        fonts-unifont \
        fonts-wqy-zenhei \
        git-lfs \
        gnupg \
        jq \
        libasound2t64 \
        libatk-bridge2.0-0t64 \
        libatk1.0-0t64 \
        libatspi2.0-0t64 \
        libcairo2 \
        libcups2t64 \
        libdbus-1-3 \
        libdrm2 \
        libfontconfig1 \
        libfreetype6 \
        libgbm1 \
        libglib2.0-0t64 \
        libnspr4 \
        libnss3 \
        libpango-1.0-0 \
        libx11-6 \
        libxcb1 \
        libxcomposite1 \
        libxdamage1 \
        libxext6 \
        libxfixes3 \
        libxkbcommon0 \
        libxrandr2 \
        openssh-client \
        sudo \
        util-linux \
        xfonts-cyrillic \
        xfonts-scalable \
        xvfb \
        zip \
    && gpg --version \
    && gpgv --version \
    && rm -rf /var/lib/apt/lists/*

# Docker client only. `docker version` and `docker buildx version` are what
# preflight requires inside the job image (crates/velnor-runner/src/preflight.rs);
# the plugin path matches the search list in
# crates/velnor-runner/src/github_adapter.rs and does not collide with the host
# plugin directory Velnor may bind-mount at /usr/local/lib/docker/cli-plugins.
COPY --from=dockercli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=dockercli /usr/local/libexec/docker/cli-plugins/docker-buildx /usr/local/libexec/docker/cli-plugins/docker-buildx
RUN docker --version && docker buildx version

WORKDIR /__w

# Release metadata must remain after every expensive filesystem layer. Putting
# this ARG/LABEL near FROM makes each version bump invalidate the complete
# dual-architecture toolchain build even though no installed byte changed.
ARG VELNOR_IMAGE_VERSION=development
LABEL org.opencontainers.image.version="${VELNOR_IMAGE_VERSION}" \
      org.opencontainers.image.source="https://github.com/tailrocks/velnor"
