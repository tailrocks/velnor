# Bastion APT Repository Structure & Release Architecture

Authoritative specification for Debian APT packaging, signing, and host deployment on Bastion.

- **Primary Signing Key**: `7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801`
- **Signing Subkey**: `CD4693750A4BA4F12BC9ABFD857FCD279679A34B`
- **Official Public URL**: `https://velnor-apt.tailrocks.com/`
- **Target Bastion**: `root@37.27.110.241` (Debian 13 trixie x86_64, AMD EPYC 9454P 48c/96t, ~128 GB RAM)
- **Mandatory Invariant**: Secondary NVMe (`/dev/nvme1n1`, 3.5 TB) is strictly UNTOUCHED.

---

## 1. Debian Package Hierarchy

The signed repository strictly adheres to Debian multi-architecture standards:

```text
https://velnor-apt.tailrocks.com/
├── velnor.gpg                                      <- Exported public key ring
├── pool/
│   └── main/
│       └── v/
│           └── velnor-runner/
│               ├── velnor-runner_0.1.273_amd64.deb <- Prior retained release (rollback)
│               ├── velnor-runner_0.1.273_arm64.deb
│               ├── velnor-runner_0.1.274_amd64.deb <- Current live release (SHA256: 3a35d3aba1b9...)
│               └── velnor-runner_0.1.274_arm64.deb
└── dists/
    ├── stable/ (and trixie/)
    │   ├── InRelease                               <- In-line clearsigned release index
    │   ├── Release                                 <- Plain checksum index
    │   ├── Release.gpg                             <- Detached signature
    │   └── main/
    │       ├── binary-amd64/
    │       │   ├── Packages                        <- Plain stanza index
    │       │   └── Packages.gz                     <- Gzip compressed stanza index
    │       └── binary-arm64/
    │           ├── Packages
    │           └── Packages.gz
```

---

## 2. GPG Signing Mechanism

Repository indices are signed during automated release using GnuPG:
1. `InRelease`: Generated via `gpg --batch --yes --pinentry-mode loopback --passphrase-fd 0 --local-user 7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801 --output dists/trixie/InRelease --clearsign dists/trixie/Release`
2. `Release.gpg`: Generated via `gpg --batch --yes --pinentry-mode loopback --passphrase-fd 0 --local-user 7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801 --output dists/trixie/Release.gpg --detach-sign --armor dists/trixie/Release`

### Security Boundary:
- The private signing key and passphrase are preserved as protected GitHub Actions secrets (`APT_GPG_PRIVATE_KEY` and `APT_GPG_PASSPHRASE`).
- The private key is deliberately excluded from host and runner environments.
- Clients configure repository-scoped `Signed-By=/etc/apt/keyrings/velnor.gpg` authenticated against fingerprint `7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801`.

---

## 3. Deployment Locking Protocol

To guarantee zero race conditions and protect active runners during maintenance:
- Root transaction path: `/run/velnor/package-transaction.lock`
- Invocation command:
  ```bash
  install -d -m 0750 /run/velnor
  apt-get update
  apt-cache policy velnor-runner
  /usr/bin/flock --exclusive --nonblock --no-fork /run/velnor/package-transaction.lock \
    apt-get install -y --no-install-recommends "velnor-runner=${VERSION}"
  dpkg-query -W velnor-runner
  ```
