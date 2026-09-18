# B1 implementation evidence: typed APT primitives

- Branch: `feat/b1-apt-primitives` (from `origin/docs/bastion-final-plan`)
- Commit: `ed442855` (signed off, pushed to `origin/feat/b1-apt-primitives`)
- Oracle: `verify-release.sh` + `publication-previous.jq` + `package-update.sh`
  fetched read-only via `gh api repos/tailrocks/velnor-apt/contents/scripts/...`
  (this repo ships no such script). Ported line-by-line into typed Rust.

## What changed (5 files, +7473/−41)

- `crates/velnor-workflow/src/apt.rs` (new, 6325 lines): the typed core.
  Validators (slug/package/binary/secret-ref/keyring/origin/description/
  feed-url/staging/pool-version/commit/digest/fingerprint), `Suite`,
  `vX.Y.Z` + `X.Y.Z~preview.N+7hex` grammars, dpkg-order preview
  comparator, `Retention`, `AptContract::resolve`, fetch (resolve-commit +
  exact-pattern allowlists), `.deb` IO (`dpkg-deb` with `ar`+`tar`
  fallback, fixed argv), stable + preview verification arming the
  `.reprepro-ok` sentinel, stable + preview-strict + preview-bootstrap
  publication, previous-pointer derivation (the jq rules), publication
  records (stable + preview variant), the no-rollback deploy guard, and
  channel-state emission.
- `config/mod.rs` + `lib.rs`: eight new `[release]` fields
  (`apt_arches`, `signer_fingerprint`, `passphrase_secret`, `keyring_path`,
  `apt_origin`, `apt_identity_dir`, `apt_feed_url`, `retention`);
  `apply_release` validates complete apt contracts at config-load time and
  fails generation loudly on malformed values. New fields are
  generation-time only (never emitted to pinned runtimes), like
  `manifest_schema`.
- `primitives/release.rs`: declared schema + parsing for the new fields,
  the widened apt completeness contract, and the rewritten
  `render_apt_release`: a real five-job feed workflow (verify, publish,
  deploy, `feed-result`, `admit-runner`) with single-writer concurrency,
  `package-feed`/`github-pages` environments, and Pages deployment.
  `render_package_feed` (homebrew) is untouched.
- `runtime.rs`: seven narrow commands (`apt-resolve-commit`, `apt-fetch`,
  `apt-verify`, `apt-publish`, `apt-previous-pointer`,
  `apt-channel-update`, `apt-deploy-guard`); the `update-feed`/`verify-feed`
  apt stub arms are removed (homebrew-only now; apt through those commands
  is rejected).

## Spec §7 capability coverage

source+identity, package, arch set (exactly {amd64,arm64}), stable/preview
suites, signer+secrets, coherence verification, assembly (staging-only,
deterministic pool, `apt-ftparchive`, `gpg` fixed argv), records (stable +
preview variant), retention (typed, policy 1), Pages artifact/deployment
with the no-rollback guard, channel tasks — all implemented, rendered, and
tested. Config executes no shell/YAML: every value reaching a command line
is a validated scalar (hostile-shape rejection tests included); secrets
travel on stdin / environment, never argv (asserted from stub argv logs).

## Verification (all observed this session)

- `cargo test -p velnor-workflow`: 549 lib + 55 integration (2+6+5+9+33),
  0 failures. Includes: 50+ apt unit tests (grammars, malformed rejection,
  every negative class: bad source/digest/key/arch/incoherent inputs —
  each asserted to leave no sentinel, i.e. rejected pre-mutation),
  renamed-fixture genericity at both the verifier and generator layers,
  malformed/incomplete declaration rejection, no-exec tests, config-path
  tests, runtime arg-validation tests, and stub-backed end-to-end publish
  tests (hermetic: no network, no dpkg/gpg/apt required).
- `cargo clippy --workspace --all-targets --locked
  --features velnor-runner/test-support -- -D warnings`: clean.
- `cargo fmt -p velnor-workflow`: applied; `fmt --check` posture clean.
- Generator-only coverage: fixture repo renders `release.yml` (real feed,
  no omission) via `velnor-workflow --plain --dry-run` → exit 0, and via a
  real generate; `actionlint` on the rendered `release.yml` → exit 0;
  `actionlint -shellcheck` (warning severity) on it → exit 0.
- Built-binary probes: `apt-deploy-guard` accepts forward / refuses
  rollback; `apt-previous-pointer` emits the prior pointer and the
  `"preview"`/`null` consts; `update-feed --kind apt` is rejected.
- Genericity gate `generic_surface_literals` passes (no consumer literals;
  schema URNs stay config parameters; record schemas mirror the runner).

## Deliberate deviations from the oracle (all documented in code)

- Runner claim checks are mirrored, not linked: the shipped generator
  never links the runner crate, and the runner's `release` module is
  crate-private, so `ReleaseRecord::verify` parity is by identical
  semantics. Full parity is intentionally impossible (runner pins its own
  source repo; the generic engine must not name it).
- Legacy stable previous-pointer string eliminated (final schema only).
- The interim second-binary (`velnorctl`) extraction check is dropped:
  product estate, not coherence; the generic engine checks `usr/bin/<binary>`.
- `dpkg` ordering for previews is a pure comparator restricted to the
  validated preview grammar (cross-checked against real `dpkg` when
  present); publish requires `dpkg-deb` OR `ar`+`tar` (capability-gated).
- Stable has no bootstrap (oracle parity): the first stable publication
  needs a live prior; the workflow fails closed otherwise. This is a B4
  execution concern, flagged, not solved here.
- Preview `suite:"preview"` record variant: this engine parses it; the
  current runner `PublicationRecord` (deny_unknown_fields, no suite field)
  does not. Runner-side follow-up for a later step if the runner must read
  preview records.

## Pre-existing bug fixed along the way

`update-feed --channel X` always failed: it delegates its full argv to
`verify-feed`, which rejected `--channel` as unknown. `verify-feed` now
accepts (and ignores) `channel`.
