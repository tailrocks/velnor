# Plan note 2026-09-13 — isolate explicit sccache, remove from default image (branch `r0-sccache-isolate`)

Base: `origin/main` at `f5165c55` (fetch time).

Goal 22: one compiler-acceleration architecture on the default path
(transparent mbx); explicit sccache as an alternative mode, never a
simultaneous default — and not baked into the default job image.

## What changed

New single-boundary module `crates/velnor-runner/src/sccache_compat.rs`:

- `is_explicit(job)` is the single decision point (enabled
  `mozilla-actions/sccache-action` step). Planning calls it once per job;
  off means sccache is absent from planning, mounts, env, and PATH.
- Owns the store path (`store_host`, via the shared rust-store layout),
  the container mount point + env (`CONTAINER_DIR`, `container_env`), and
  the step scripts (`setup_script`, `post_script`, `guest_script`).
- Owns provisioning: the default image no longer ships sccache, so the
  first explicit-mode step reuses a PATH binary at `LOCKED_VERSION`
  (`0.16.0`) or fetches the pinned musl release, verifies SHA-256, and
  installs to `/usr/local/bin` (`$HOME/.local/bin` fallback for non-root
  lanes, surfaced via the cosign-style `PATH_MARKER`). Checksums were
  copied verbatim from the deleted `docker/job-mise.lock` stanza.

Call-site rewiring (behavior-preserving for both modes):

- `github_adapter`: one decision call; mbx gated on `!explicit`; sccache
  store via compat. Deleted `github_sccache_store_host` (moved).
- `container`: sccache mount/env rendered from compat consts; default mbx
  branch byte-identical. Deleted dead `sccache_host` (only its own test
  used it; planning never called it).
- `executor`: `native_sccache` runs the compat setup script and parses the
  fallback PATH marker; post step runs the compat post script (moved
  verbatim); persistent-cache exemption uses the compat mount const.
- `guest_actions`: sccache arm runs the compat guest script (ensure +
  start-server; documented live MicroVM behavior, kept working).
- `manifest::declares_sccache` kept as a thin alias over `is_explicit` so
  admission and the `r0-f4-warm` stable-workspace gating
  (`wants_stable_workspace`) keep compiling and behaving identically.

Default image (no sccache presence):

- `docker/job-mise.toml`: dropped the `sccache` pin; `docker/job-mise.lock`:
  dropped the stanza; `docker/job-ubuntu.Dockerfile`: dropped the install
  entry and the version assertion, added a fail-closed
  `! command -v sccache` build assertion (mirrors `! command -v kache`).
- `config/version-pins.json`: deleted the sccache entry (single source of
  truth needs no cross-file check; input-rule tie covered by unit test).
- `renovate.json`: deleted the Dockerfile-assertion entry (no version-only
  automation: a bump without fresh checksums would fail closed at install;
  same stance as the cosign lock).
- Docs: `execution.mdx` "Choosing sccache instead" documents provisioning.

## Regression tests (smallest focused set)

- `sccache_compat::tests::*`: decision on/off (+disabled-step negative),
  repo-namespaced + slot-shared store, setup provisions pinned release
  (URLs, both SHAs, install, start-server; no "must be preinstalled"),
  post stats/stop, guest ensure/start, `sh -n` validity of all scripts.
- `github_adapter::docker_swaps_mbx_for_sccache_only_on_explicit_request`:
  default plans mbx-only; explicit plans sccache-only.
- `container::default_job_mounts_only_mbx_with_bounded_gc` extended: no
  `RUSTC_WRAPPER`, no case-insensitive `sccache` in default args.
- `manifest::sccache_version_input_matches_compat_lock`: admitted
  `version:` input must equal the compat lock.
- `mise::job_image_places_lock_at_mise_config_root` flipped: Dockerfile
  asserts sccache absence; mise toml + lock contain no sccache.

## Verification

- `cargo fmt -p velnor-runner -- --check`: clean.
- `cargo clippy -p velnor-runner --all-targets -- -D warnings`: clean.
- `cargo test -p velnor-runner --lib`: 1872 passed, 0 failed with 2
  pre-existing flakes skipped
  (`checkout_emits_the_four_bench_phase_spans`,
  `checkout_reader_lease_blocks_mirror_repair` — untouched files, fail
  identically on the pristine base, pass in isolation).
- `node scripts/pin_integrity.mjs`: no sccache findings; the 4 remaining
  failures (root `mise.lock` rust copy, cosign template copy) reproduce
  identically on the pristine base.

## F4 compatibility

`r0-f4-warm` (`fad11543`) keys stable workspaces off
`manifest::declares_sccache` / `wants_stable_workspace`. Both names and
their semantics are preserved, so the F4 merge keeps working and
explicit-sccache jobs keep their stable workspaces.
