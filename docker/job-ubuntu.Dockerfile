FROM ubuntu:24.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        bash \
        build-essential \
        ca-certificates \
        curl \
        docker-buildx \
        docker.io \
        git \
        jq \
        libssl-dev \
        openssh-client \
        pkg-config \
        tar \
        unzip \
        xz-utils \
        zip \
        zstd \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /__w
