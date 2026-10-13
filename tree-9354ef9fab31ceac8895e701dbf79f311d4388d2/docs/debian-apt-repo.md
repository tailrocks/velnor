# Velnor runner — Debian package + apt-native repository

**Status: implemented; publication decoupled for coherence (plan 010).**
`git tag vX.Y.Z && git push origin vX.Y.Z` runs the single `release.yml`
coordinator (release-deb.yml is deleted; its binary-presence + size + arch guards
are folded in). It builds both debs and the multi-platform GHCR job image once,
then assembles ONE acyclic `release-record.json` binding source SHA -> crate
version -> per-arch binary/deb digests -> OCI image digest -> compiled-manifest
hash -> APT coordinate, and attaches the record + checksum + tar.gz + deb assets
to the GitHub release. Assets are never clobbered (an existing tag succeeds only
on an exact digest match). The source holds **no** APT credential and does **not**
push to or dispatch `tailrocks/velnor-apt` (N6): the apt repo pulls the published
record itself and verifies schema/source/tag/version/all-hashes/OCI before
`reprepro` (see velnor-apt `verify-release.sh`). The packaged daemon ships
never-exit supervision, sd_notify watchdog, `velnor-daemon@<name>` template
instances, `/etc/velnor/secrets.env` (0600, operator-owned), `velnor-doctor`
timers, and — new in plan 010 — a transactional `preinst`/`postinst` that never
builds/never restarts and a `release verify-installed` ExecStartPre coherence
gate on both daemon units. The design below documents the pieces.

Goal: install and upgrade the Velnor runner daemon with native apt:

```bash
sudo apt update
sudo install -d -m 0750 /run/velnor
sudo env VELNOR_PACKAGE_TRANSACTION_LOCK_HELD=1 \
  /usr/bin/flock --exclusive --nonblock /run/velnor/package-transaction.lock \
  apt-get install velnor-runner=X.Y.Z
```

The `flock` parent must wrap the complete exact-version apt transaction. It
holds the exclusive package lock while `dpkg` unpacks and configures the
package; direct `apt-get install`, `apt upgrade`, or `dpkg` invocation is
refused by the maintainer scripts. First install uses the same wrapper.

Own repository, hosted on GitHub (GitHub Pages), built + signed in CI on tag.

## Pieces

1. **Build the `.deb`** — `cargo-deb` (Rust-native; reads `[package.metadata.deb]`
   in `Cargo.toml`). Both holla and velnor use the exact same approach in their
   `release-deb.yml` (on tag `v*` or workflow_dispatch):
   - `jdx/mise-action` (rust + zig + prebuilt cargo-binstall + cargo-zigbuild)
   - sccache + mold
   - Matrix over `x86_64-unknown-linux-gnu` + `aarch64-unknown-linux-gnu`
   - `cargo zigbuild --release --locked --target $TGT` (modern glibc, no .2.17 shims — those are only for portable tarballs)
   - cargo-deb is pinned by mise and installed only as a verified prebuilt
     cargo-binstall/QuickInstall artifact; CI fails instead of compiling it.
   - `cargo deb -p velnor-runner --target $TGT --no-build --deb-version "$VERSION"`
   Produces `velnor-runner_<version>_<arch>.deb` (amd64 + arm64).
   - Package contents: single product binary → `/usr/bin/velnorctl` (the
     velnorctl command center; the velnor-runner crate is a lib-only runtime); systemd units →
     `/usr/lib/systemd/system/velnor-daemon.service` and
     `velnor-daemon@.service`; default config → `/etc/velnor/velnor.env`;
     operator-owned tokens → `/etc/velnor/secrets.env` or
     `/etc/velnor/<instance>.secrets.env` (0600, never shipped).
   - `maintainer-scripts`: `postinst` creates the state/cache/runtime/log dirs,
     migrates any legacy token out of `velnor.env`, reloads systemd, and leaves
     start/enable under operator control. The daemon currently runs as root
     because it owns Docker-backed job containers and bind mounts.
   - systemd unit: `Restart=always`, separate config and secret environment
     files, so the PAT stays off argv/`/proc`; `WantedBy=multi-user.target` for
     boot start. The default and templated units have the same sandboxing and
     storage contract.

2. **Build the apt repository** — `reprepro` (standard, simple, signs Release).
   - `conf/distributions`:
     ```
     Origin: Velnor
     Label: Velnor
     Codename: stable
     Architectures: amd64 arm64
     Components: main
     SignWith: <GPG key id>
     ```
   - `reprepro includedeb stable velnor-runner_*.deb` → builds `dists/` +
     `pool/`, generates `Packages`, `Release`, and a **GPG-signed** `InRelease` /
     `Release.gpg`.

3. **Sign** — a dedicated GPG signing key.
   - Private key and passphrase are stored securely by maintainers and manually copied into the GitHub repository secrets `APT_GPG_PRIVATE_KEY` / `APT_GPG_PASSPHRASE` (no loading from external secret managers happens inside GitHub Actions). Imported in CI for reprepro `SignWith`.
   - Public key published at `https://velnor-apt.tailrocks.com/velnor.gpg` (and in the repo) for users to install into `/etc/apt/keyrings`.

4. **Host on GitHub Pages** — the isolated signed tree (`dists/`, `pool/`,
   `velnor.gpg`) is deployed through the official Pages actions. Every index
   contains exactly the candidate and its signed rollback predecessor. The
   publisher verifies prior `InRelease`, package hashes, and publication-record
   signature before carrying that pair forward. No state branch is used. Served
   at `https://velnor-apt.tailrocks.com/`.

### Where it lives (storage decision)

- **Store = GitHub Pages.** apt fetches the signed tree over HTTPS directly.
- **Dedicated repo** `tailrocks/velnor-apt` (not the Velnor source repo) so the
  `.deb` binaries do not enter code git history. GitHub Actions deploys the
  generated signed tree directly as a Pages artifact; no `gh-pages` branch is
  used.
- **GitHub Packages does NOT support apt/deb** (npm/Docker/Maven/NuGet/RubyGems
  only) — can't use it.
- **GitHub Releases** is the alternative blob store: keep `.deb` assets in
  Releases (2 GB/asset, no repo bloat) and host only the small `Packages`/
  `Release` index on Pages pointing at the asset URLs. Use this only if the
  `pool/` ever gets large; for now velnor-runner `.deb` ≈ 12 MB and a few
  versions sit comfortably inside Pages' ~1 GB repo / ~100 GB-month limits.
- **Keep it lean**: the index exposes exactly two versions: current candidate
  and its verified rollback predecessor. Older `.deb` files remain in immutable
  historical Releases but are not indexed.

## CI (GitHub Actions, on tag `v*`)

**Policy:** You should always use GitHub Actions for GitHub Pages deployments (never "Deploy from a branch"). Use `actions/configure-pages`, `actions/upload-pages-artifact`, and `actions/deploy-pages`. The index on Pages is always for currently published versions only.

`.github/workflows/release-deb.yml` (unified with holla's):
1. Uses the exact same pattern as holla: `jdx/mise-action` + sccache + mold + `cargo zigbuild` (latest Debian glibc only, matrix for amd64+arm64) → `cargo deb --target $TGT --no-build --deb-version "$VERSION"`.
2. Stages per-arch debs + shas as artifacts.
3. Attaches the `.deb`(s) to the velnor source GitHub Release.
4. If `GH_VELNOR_APT_TOKEN` is present, cross-uploads the .debs to the `velnor-apt` repository's Releases (same tag) and triggers `publish.yml` in the apt repo via `gh workflow run -f version=$TAG`.
5. The apt-repo's `publish.yml` downloads candidate `.deb` files from its own
   Release, recovers the exact prior pair from the signed live repository,
   verifies its signed publication identity, builds a fresh two-version index,
   and deploys it to Pages.

The `.deb` build + attachment to the original release is the responsibility of
the source project. The apt publisher consumes only the apt repository's own
release assets. The Pages index is generated fresh with the requested version
plus its exact signed rollback predecessor; older historical packages remain
in Releases.
6. Also attach the raw `.deb`(s) to the GitHub Release for direct download.

Each new tag → new `.deb` in the pool → regenerated signed `Release` → `apt
upgrade` picks it up. That is the whole upgrade story.

## User install (modern `signed-by` keyring — not deprecated `apt-key`)

```bash
sudo install -m0755 -d /etc/apt/keyrings
curl -fsSL https://velnor-apt.tailrocks.com/velnor.gpg \
  | sudo tee /etc/apt/keyrings/velnor.gpg > /dev/null
echo "deb [signed-by=/etc/apt/keyrings/velnor.gpg] https://velnor-apt.tailrocks.com stable main" \
  | sudo tee /etc/apt/sources.list.d/velnor.list
sudo apt update
sudo install -d -m 0750 /run/velnor
sudo env VELNOR_PACKAGE_TRANSACTION_LOCK_HELD=1 \
  /usr/bin/flock --exclusive --nonblock /run/velnor/package-transaction.lock \
  apt-get install velnor-runner=X.Y.Z
# then set non-secret config and the operator-owned token separately:
sudo nano /etc/velnor/velnor.env      # URL=..., NAME=..., LABELS=..., SLOTS=...
sudo install -m 0600 /dev/null /etc/velnor/secrets.env
sudo nano /etc/velnor/secrets.env     # GITHUB_TOKEN=...
sudo systemctl enable --now velnor-daemon
```

`signed-by=` scopes the key to this repo only (current security best practice;
avoids the deprecated global `apt-key` / `trusted.gpg.d`).

## Notes / decisions

- **cargo-deb vs nfpm**: cargo-deb is the Rust-native fit (config lives in
  `Cargo.toml`); nfpm is fine too if we later want rpm as well.
- **reprepro vs aptly**: reprepro is simpler for a single-arch single-suite repo
  and signs Release out of the box; aptly if we later need snapshots/mirroring.
- **arch**: amd64 + arm64 via matrix (using zigbuild for cross on ubuntu runner).
  Both projects (holla + velnor) use the identical release-deb.yml structure.
- **key rotation**: document a key-rotation procedure; expired signing keys break
  `apt update` for everyone (see the `gh` CLI incident).

## Maintainer release and Sentry deployment

Sentry has one package-deployment path: the configured signed apt repository.
This rule covers first install, upgrade, downgrade, rollback, and forward
recovery.

1. Ensure the release commit is signed off, pushed, and green. Create a signed
   `vX.Y.Z` tag on that exact commit and push only that tag.
2. Monitor Velnor's `Release` workflow. It must build amd64 and arm64, validate
   the packages, publish immutable assets and the release record, and dispatch
   `tailrocks/velnor-apt`'s `Publish apt repo` workflow.
3. Monitor apt publication and Pages deployment. Before touching Sentry, verify
   the `InRelease` signature and signed publication record bind the requested
   tag, source-record digest, exact candidate, and exact rollback predecessor.
   On Sentry, `apt-get update` must complete without a signature warning and
   policy must report the requested exact version from the configured HTTPS
   repository:
   ```bash
   sudo apt-get update
   apt-cache policy velnor-runner
   ```
4. Drain all intended Velnor daemons. Confirm GitHub reports every managed
   runner idle and no Velnor job container remains. Then install the verified
   exact version, never an unpinned candidate:
   ```bash
   sudo install -d -m 0750 /run/velnor
   sudo env VELNOR_PACKAGE_TRANSACTION_LOCK_HELD=1 \
     /usr/bin/flock --exclusive --nonblock /run/velnor/package-transaction.lock \
     apt-get install velnor-runner=X.Y.Z
   dpkg-query -W velnor-runner
   ```
5. Activate the immutable release record, verify the installed package and
   binary plus exact OCI digest and complete labels, inspect
   atomic `active`/`previous` pointers, then start only the intended instance
   units. Run doctor and the fixture smoke before restoring traffic.
6. Rollback uses only the exact signed predecessor already retained in the
   repository: drain, verify its signed identity, `apt-get update`, and
   `install -d -m 0750 /run/velnor` plus the exclusive flock wrapper around
   `apt-get install velnor-runner=<exact-predecessor>`. Prove rollback, then use
   the same exact signed-APT procedure to move forward. Never republish an old
   release merely to roll back.

Never deploy a local or downloaded `.deb`, use direct `dpkg -i`, install an apt
local path, copy a binary, or build/install from a checkout on Sentry. A
checksummed immutable release-record download is activation metadata only; it
does not install executable code and cannot replace any apt step.

Verified 2026-07-18 against both repositories and the live host:
`tailrocks/velnor-apt` publishes amd64+arm64 with a signed reprepro index
through GitHub Actions Pages; Sentry has the scoped keyring and source
configured. The v0.1.58 dual-architecture source package run is
[`29638641012`](https://github.com/tailrocks/velnor/actions/runs/29638641012)
and its signed repository/Pages run is
[`29638791029`](https://github.com/tailrocks/velnor-apt/actions/runs/29638791029).
Those run URLs are historical proof of the chain, not a current version pin;
every later deployment must follow this same tag-to-signed-apt chain and verify
the exact version reported by `apt-cache policy` before changing a host.
