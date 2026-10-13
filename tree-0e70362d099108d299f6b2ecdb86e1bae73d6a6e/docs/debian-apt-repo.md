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
   - Private key stored as a GitHub Actions secret (`APT_GPG_PRIVATE_KEY`,
     `APT_GPG_PASSPHRASE`); imported in CI for reprepro `SignWith`.
   - Public key published at `https://<pages-host>/velnor.gpg` (and in the repo)
     for users to install into `/etc/apt/keyrings`.

4. **Host on GitHub Pages** — publish the reprepro output tree (`dists/`,
   `pool/`, `velnor.gpg`) to the `gh-pages` branch.
   Served at e.g. `https://www.zhokhov.com/velnor-apt/`.

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
- **Keep it lean**: prune old versions from `pool/` on release, or periodically
  squash the `gh-pages` history.

## CI (GitHub Actions, on tag `v*`)

`.github/workflows/release-deb.yml`:
1. `cargo install cargo-deb` → `cargo deb` → the `.deb`.
2. Import `APT_GPG_PRIVATE_KEY`.
3. `reprepro -b apt includedeb stable target/debian/velnor-runner_*.deb`
   (apt repo state kept on the `gh-pages` branch, checked out + updated).
4. Commit/publish the updated `apt/` tree to `gh-pages` (e.g.
   `peaceiris/actions-gh-pages` or a plain `git push`).
5. Also attach the raw `.deb` to the GitHub Release for direct download.

Each new tag → new `.deb` in the pool → regenerated signed `Release` → `apt
upgrade` picks it up. That is the whole upgrade story.

## User install (modern `signed-by` keyring — not deprecated `apt-key`)

```bash
sudo install -m0755 -d /etc/apt/keyrings
curl -fsSL https://www.zhokhov.com/velnor-apt/velnor.gpg \
  | sudo tee /etc/apt/keyrings/velnor.gpg > /dev/null
echo "deb [signed-by=/etc/apt/keyrings/velnor.gpg] https://www.zhokhov.com/velnor-apt stable main" \
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
