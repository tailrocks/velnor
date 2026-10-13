# Velnor runner — Debian package + apt-native repository (design)

Goal: install and upgrade the Velnor runner daemon with native apt:

```bash
sudo apt update && sudo apt install velnor-runner   # install
sudo apt upgrade                                     # upgrade later
```

Own repository, hosted on GitHub (GitHub Pages), built + signed in CI on tag.

## Pieces

1. **Build the `.deb`** — `cargo-deb` (Rust-native; reads `[package.metadata.deb]`
   in `Cargo.toml`). Produces `velnor-runner_<version>_amd64.deb`.
   - Package contents: binary → `/usr/bin/velnor-runner`; systemd unit →
     `/lib/systemd/system/velnor-daemon.service`; default config →
     `/etc/velnor/velnor.env` (conffile, token NOT shipped — operator fills it);
     state dir `/var/lib/velnor` owned by a `velnor` service user.
   - `maintainer-scripts`: `postinst` creates the `velnor` user + dirs,
     `systemctl daemon-reload`, `enable` (start gated on the operator having set
     the token); `prerm`/`postrm` stop + disable on remove.
   - systemd unit: `Restart=always`, `EnvironmentFile=/etc/velnor/velnor.env`
     (URL, name, labels, slots, work-dir, `GITHUB_TOKEN`) so the **PAT stays off
     argv/`/proc`** (current deploy leaks it on the command line), `WantedBy=
     multi-user.target` for boot start.

2. **Build the apt repository** — `reprepro` (standard, simple, signs Release).
   - `conf/distributions`:
     ```
     Origin: Velnor
     Label: Velnor
     Codename: stable
     Architectures: amd64
     Components: main
     SignWith: <GPG key id>
     ```
   - `reprepro includedeb stable velnor-runner_*.deb` → builds `dists/` +
     `pool/`, generates `Packages`, `Release`, and a **GPG-signed** `InRelease` /
     `Release.gpg`.

3. **Sign** — a dedicated GPG signing key.
   - Private key and passphrase are stored securely by maintainers and manually copied into the GitHub repository secrets `APT_GPG_PRIVATE_KEY` / `APT_GPG_PASSPHRASE` (no loading from external secret managers happens inside GitHub Actions). Imported in CI for reprepro `SignWith`.
   - Public key published at `https://velnor-apt.tailrocks.com/velnor.gpg` (and in the repo) for users to install into `/etc/apt/keyrings`.

4. **Host on GitHub Pages** — the reprepro output tree (`dists/`, `pool/`, `velnor.gpg`) is deployed via a GitHub Actions workflow (using the official `actions/deploy-pages`). The `apt-state` branch is used internally as a state store for `reprepro` (to keep old package versions). GitHub Pages is deployed via GitHub Actions (recommended; never "Deploy from a branch"). Served at `https://velnor-apt.tailrocks.com/`.

### Where it lives (storage decision)

- **Store = GitHub Pages.** apt fetches the signed tree over HTTPS directly.
- **Dedicated repo** `donbeave/velnor-apt` (NOT the velnor source repo) so the
  `.deb` binaries don't bloat the code git history; the signed tree lives on its
  `gh-pages` branch.
- **GitHub Packages does NOT support apt/deb** (npm/Docker/Maven/NuGet/RubyGems
  only) — can't use it.
- **GitHub Releases** is the alternative blob store: keep `.deb` assets in
  Releases (2 GB/asset, no repo bloat) and host only the small `Packages`/
  `Release` index on Pages pointing at the asset URLs. Use this only if the
  `pool/` ever gets large; for now velnor-runner `.deb` ≈ 12 MB and a few
  versions sit comfortably inside Pages' ~1 GB repo / ~100 GB-month limits.
- **Keep it lean**: the index on Pages includes only current versions. Old .debs remain in historical Releases (for manual download if needed) but are not part of the current apt repo.

## CI (GitHub Actions, on tag `v*`)

**Policy:** You should always use GitHub Actions for GitHub Pages deployments (never "Deploy from a branch"). Use `actions/configure-pages`, `actions/upload-pages-artifact`, and `actions/deploy-pages`. The index on Pages is always for currently published versions only.

`.github/workflows/release-deb.yml` (in the original velnor repo):
1. Uses `jdx/mise-action` + `cargo zigbuild` (latest Debian glibc only) → `cargo deb`.
2. Attaches the `.deb`(s) to the velnor source GitHub Release.
3. If `GH_VELNOR_APT_TOKEN` is present, cross-uploads the .deb(s) to the `velnor-apt` repository's Releases (same tag) and triggers `publish.yml` in the apt repo via repository_dispatch or `gh workflow run`.
4. The apt-repo's `publish.yml` then downloads the .deb from *its own* Releases (default GITHUB_TOKEN is sufficient) and runs reprepro.

The `.deb` build + attachment to the original release is the responsibility of the source project. The apt publisher only consumes from the apt-repo's own releases.
   The index on Pages is generated fresh each time with only current versions (no state branch; old versions forgotten from index per maintainer preference).
5. Also attach the raw `.deb` to the GitHub Release for direct download.

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
sudo apt install velnor-runner
# then set the token + config:
sudo nano /etc/velnor/velnor.env      # GITHUB_TOKEN=..., URL=..., LABELS=...
sudo systemctl enable --now velnor-daemon
```

`signed-by=` scopes the key to this repo only (current security best practice;
avoids the deprecated global `apt-key` / `trusted.gpg.d`).

## Notes / decisions

- **cargo-deb vs nfpm**: cargo-deb is the Rust-native fit (config lives in
  `Cargo.toml`); nfpm is fine too if we later want rpm as well.
- **reprepro vs aptly**: reprepro is simpler for a single-arch single-suite repo
  and signs Release out of the box; aptly if we later need snapshots/mirroring.
- **arch**: amd64 first (Sentry + targets are amd64). Add arm64 later via a
  matrix build if needed.
- **key rotation**: document a key-rotation procedure; expired signing keys break
  `apt update` for everyone (see the `gh` CLI incident).

## Status

Future work — not started. Tracked in
`prompts/chainargos-migration-outstanding.checklist.md` (Priority 1).
