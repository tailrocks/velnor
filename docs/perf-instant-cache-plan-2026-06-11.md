# Instant-cache plan — root causes and fixes (2026-06-11)

Operator directive: re-running a warm pipeline must download nothing,
compile no dependencies, and rebuild no unchanged docker layers — "almost
instant". Three java-monorepo Velnor jobs from run-set 29a197e (09:39 UTC)
were analyzed end-to-end. Every slowdown traced to a structural defect.
This document is the design record; master-plan §3a/P3 items reference it.

## Measured state (before)

| Job | Wall | Dominant cost |
|---|---|---|
| Rust / Format and lint | 1m58s | Clippy 1m42s: full crate-graph walk + **fresh registry download (~280 crates) + git-dep updates** despite warm host; sccache itself 100% hits, 0.001s reads |
| Rust Docker / bake | 2m44s | bake 2m29s: 99.5s workspace release build (deps recompiled that chef cook should own) + ~40s exporting layer cache to GitHub's cache API from the self-hosted host |
| Kestra / docker-jvm-base | 48s | 32s "Check if image exists": mise downloads + extracts GraalVM 25 + Node 24 + Python 3.14 per job; rust-script/registry caches missed |

## Root causes (verified in code/logs, not guessed)

1. **Run-step env prelude lies about HOME** (`script_step.rs::script_with_path_prelude`,
   `executor.rs::native_shell`): every step gets
   `export HOME=/root; export CARGO_HOME=/root/.cargo` — an OrbStack/macOS
   dev workaround. Container `/root` is **not bind-mounted**, so:
   - cargo writes the registry to container-local `/root/.cargo` → lost at
     job end; the actions/cache adapter resolves `~/...` to the job-home
     mount (`<job>/home`) → save reports *"no paths exist"* → the
     cargo-registry cache **never saved once** on any slot; every job
     re-downloads every crate. (Restore is equally invisible to steps.)
   - mise-action *does* export `CARGO_HOME=/github/home/.cargo` to
     GITHUB_ENV — the prelude overrides it. Adapters and steps disagree
     about HOME (docker login lands in `/root/.docker`, etc.).
   - kestra workflow worked around it with a lane-split
     `/root/.cache/rust-script` path, which via the resolver's absolute-path
     fallthrough **reads/writes the daemon host's real /root** (host-leak).
   Why the prelude existed: with `CARGO_HOME=/github/home/.cargo` the mise
   `cargo` shim errors ("not a valid shim") because mise's rust backend
   computes its bin path as `$CARGO_HOME/bin`, which is an empty bind at
   job start. Verified live on Sentry. The fix is env-layering, not lying:
   rustup proxies preinstalled at `/root/.cargo/bin` (PATH-first) honor
   `$CARGO_HOME` for registry/git, so steps work with the truthful HOME.

2. **Cache store was slot-fragmented** (fixed by 0.1.17's
   `daemon_shared_root`, but Sentry still ran 0.1.16 with stale apt lists):
   10 slots × ~2 GB duplicate sccache stores; a key saved on slot-2 was
   invisible on slot-5 → restore roulette.

3. **Registry/tool bytes round-trip through tarballs at all.** Even with
   1+2 fixed, copying ~1.5 GB of registry per job in/out of the store is
   the GitHub-hosted model on local disk. The Velnor model is **mount the
   warm store directly**.

4. **bake exports layer cache to GitHub's cache API from Sentry**
   (`type=gha` cache-to, mode=max): ~40 s per run of pure upload latency,
   while the named buildx builder (`chainargos-rust-workspace-builder`,
   up 6 days) already holds a persistent local cache. Import is redundant,
   export is waste — on the Velnor lane only.

5. **cargo-chef cook/build feature mismatch** (java-monorepo
   `backend-rust/Dockerfile`): cook uses the whole-workspace recipe,
   the final build uses `--package`×8 → different feature unification →
   sentry/tracing/regex/git-dep chains recompile in the final layer on
   every commit. No sccache inside the builder stage either, and
   `rust-toolchain.toml` + minimal-profile `rust:1.96-trixie` triggers a
   rustup clippy download inside the build.

6. **Job image lacks the kestra toolset** (GraalVM/Node/Python) and mise
   installs are container-ephemeral → re-downloaded per job.

## Fixes (Velnor 0.1.18)

A. **Truthful step env, layered** — drop HOME/RUSTUP_HOME/CARGO_HOME from
   the script prelude; provide them as *defaults* in the exec env
   (`HOME=/github/home`, `RUSTUP_HOME=/root/.rustup`,
   `CARGO_HOME=/github/home/.cargo`) that GITHUB_ENV/step env can
   override. PATH keeps `/root/.cargo/bin` ahead of mise shims (rustup
   proxies bypass the broken mise cargo-shim path). Docker client state
   unifies on the job home (`/github/home/.docker`) for adapters and run
   steps alike; CLI plugins move to a HOME-independent mount
   (`/usr/local/lib/docker/cli-plugins`).

B. **Host-persistent stores mounted into every job container** (under the
   daemon-shared work root, sibling of `_velnor_sccache`):
   - `_velnor_cargo/registry` → `/github/home/.cargo/registry`
   - `_velnor_cargo/git`      → `/github/home/.cargo/git`
   - `_velnor_mise`           → `/opt/mise` (seeded from the job image's
     baked `/opt/mise` once per image digest, then accretes job-installed
     tools: GraalVM installs once per fleet, not per job)
   - `_velnor_targets/<trust-scope>/<repo>/<workflow>/<job-bucket>` →
     `CARGO_TARGET_DIR` (per-trust-scope, per-job-class persistent incremental
     state; cargo's own build-dir lock serializes the rare same-bucket
     concurrency)
   Cargo registry/git are concurrency-safe by cargo's own file locking.

C. **Cache adapter: persistent-path no-op** — `~/.cargo/registry|git`,
   `/opt/mise`, sccache and target paths resolve to host-persistent
   mounts; restore/save print "host-persistent on Velnor, skipped" instead
   of tar round-trips. Workflow YAML stays identical on both lanes
   (drop-in mandate); the GitHub lane keeps real cache semantics.

D. **Host-leak fix** — unknown absolute paths no longer fall through to
   daemon-host paths; they resolve to None with a visible warning.

E. **gha strip on the Velnor lane** — the native bake/build-push adapters
   drop `type=gha` cache-from/cache-to entries (persistent local builder
   already covers them); other cache types pass through.

## Fixes (java-monorepo PR)

- `backend-rust/Dockerfile`: chef stage gains pinned sccache install +
  `rustup component add clippy rustfmt`; cook gets the same `--package`×8
  set as the final build; both compile RUNs get
  `RUSTC_WRAPPER=sccache` + `--mount=type=cache,target=/sccache`.
- `kestra-build-publish.yml`: revert the `/root/.cache/rust-script`
  lane-split to plain `~/.cache/rust-script` (correct after fix A).

## Measured results (verified live, 2026-06-11)

0.1.18 (truthful HOME + store mounts + gha strip) and 0.1.19 (mtime pinning,
shared `.cargo/bin`, image-build caches) deployed to Sentry; java-monorepo
PR #1405 merged. Same-commit reruns on the warm fleet:

| Job | Before | After (steady state) | Evidence |
|---|---|---|---|
| Rust / Format and lint | 1m58 solo (clippy 1m41, ~280 crate downloads every run) | **26s under 9-way concurrency — clippy `Finished in 0.61s`, 0 compiles, 0 downloads** (0.1.24 pins directory mtimes too: `rerun-if-changed=<dir>` build scripts no longer re-run per checkout) | run 27348311958 |
| Rust / test jobs ×8 | 1.1–2.3 min solo; 5m54–7m42 when 9 ran concurrently | **1m30–3m21 with all 9 concurrent** (faster than the old solo numbers under full fan-out) | run 27344555706 |
| Rust Docker / bake | 2m44 (≈100s rebuild + ~40s gha cache export) | **24s** (manifest pushes only; `[velnor] dropped 2 type=gha cache option(s)`) | run 27341437078 |
| Kestra / docker-jvm-base | 48s (32s of GraalVM+Node+Python downloads per job) | **29s** first warm pass; exists-check seconds once the mise store holds the toolset | run 27341438948 |
| actions/cache cargo-registry | *never saved once* ("no paths exist") | "Cache paths live on Velnor host-persistent storage (always warm)" both directions, zero tar I/O | run 27341203467 |

Residual structural findings fixed in 0.1.21–0.1.22 (incidents #10/#11 in
master-plan §3): graceful SIGTERM drain (an apt upgrade restart killed 7
in-flight jobs) and mise-store image seeding (fresh shared store dangled
image-baked shims, observed as `gh is not a valid shim` on brown).

## Next performance wave (ranked, from the runner-source review)

1. **Async job finalization**: completion RPC currently gates on log/timeline
   publishers (up to 30 s best-effort timeouts) and cleanup runs serially on
   the critical path — report completion, then finalize in the background.
2. **Parallel cleanup + service start** (needs CommandRunner concurrency).
3. **Host-side git mirror pool** for checkout (zero-fetch warm clones).
4. **Per-step docker exec batching** / persistent exec shell.
5. **Bind-mount verify cache** (per-daemon, not per-job).
6. Native HTTP client (P3.1), zero-copy log pipeline (P3.2) — unchanged.

## Deploy + verification gates

1. velnor v0.1.18 tag → deb → apt; Sentry upgraded (all three daemons);
   per-slot sccache/cache stores migrated into the shared root (largest
   store wins; ~19 GB reclaimed).
2. Re-dispatch all three pipelines twice on the same commit. Gate
   (master-plan §3a rerun-idempotency): second run shows **zero
   `Downloading`/`Updating` lines, zero dependency `Compiling` lines,
   zero layer rebuilds**; target walls: fmt+lint ≤ 30 s, bake ≤ 60 s
   (push-bound), jvm-base exists-check ≤ 10 s.
3. Lane comparison rerun for the record (docs/perf numbers updated).
