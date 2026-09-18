# D1 part B implementation evidence — homogeneous worker lane + shared allocator

Branch: `feat/d1-worker-lane` (from `34c44fa7`, which merges `origin/feat/d1-scaleset-protocol @ 966f3623`).
Commit: `6bd6de311f269f20aa07d9211f92def34e8261a5` (signed off, pushed to origin).
Diff: 13 files, worker lane + allocator + fixture + 2 integration suites. No legacy shims.

## What landed

- `scaleset/worker/ownership.rs` — deterministic identity: stable `OwnershipId(set, runner-name)`,
  derived container/network/volume names, `velnor.scaleset.*` labels, label round-trip. Retried
  provisions converge on the same names; same inner names/ports isolated by per-worker netns.
- `scaleset/worker/dind.rs` — private DinD: `--privileged` daemon with ONE `-H unix://` listener,
  no `-p/--publish/--expose` (test-proven on the argv), no host socket bind, per-worker bridge
  network with ownership labels, exec-based readiness probe, adopt-on-retry with foreign-label
  fail-closed.
- `scaleset/worker/runner.rs` — `PinnedImage` (`repo@sha256:<64hex>` only; tags incl. `latest`
  unconstructable), `HomogeneousProfile` (runner 2.337.0 + dind 28.5.2, x86_64/aarch64), runner
  spec (`--network container:<dind>`, identical-absolute-path binds, JIT blob env-only, versions
  recorded), `Connected/Starting/Down` detection, `DockerToolContentHook`
  (pull → RepoDigests match → provenance-label match → attestation).
- `scaleset/worker/supervise.rs` — tick reconciles recorded vs observed: DinD restart within
  budget (3), runner death never restarted (GitHub oracle decides), export-before-delete,
  ordered teardown (stop runner → export → rm runner → stop/rm dind → network → volumes),
  failure-collecting `CleanupReport` (missing evidence fails the report; missing objects are clean).
- `scaleset/worker/mod.rs` — `ScaleSetWorkerState` transition table + `ScaleSetWorker` record
  (edge sink seam for the journal), `WorkerRunner` public seam (blanket impl over the internal
  runner), `provision_worker` orchestration (verify → network → dind → ready loop → runner →
  connection; injected sleep).
- `scaleset/allocator.rs` — ScaleSet binding to the ONE C2 ledger: `scaleset/<set>/<request>`
  holders, fenced acquire w/ duplicate-holds-once, `advertised_free` (None until reconciled),
  guard (release-on-drop, uncertain-on-cleanup-failure), two-lane `startup_reconcile` + sweep.
  No second ledger, no per-scope N (test: set 7 can spend all of N, set 9 waits).
- `velnor-control/src/permit_ledger.rs` — imported BYTE-IDENTICAL from
  `origin/feat/c2-unbounded-global-n @ deb9e204` (`cmp` clean after fmt); control `lib.rs` hunk
  identical to C2's, so both merge cleanly. `permit_guard.rs` deliberately NOT imported (native
  wiring absent here would leave dead code); allocator takes the ledger path as a parameter and
  post-merge callers pass C2's resolved host-wide path.

## Pins (resolved live 2026-09-17, verified by the hook test against pulled images)

- Runner `ghcr.io/actions/actions-runner:2.337.0` = index `sha256:e5496277…1ef4`
  (amd64 `50364809…97`, arm64 `f5a0d9a3…0d`); labels prove `source=https://github.com/actions/runner`.
- DinD `docker:28.5.2-dind` = index `sha256:2a232a42…82ac`
  (amd64 `9a06753d…5a`, arm64 `14518479…28`); index labels `null`, digest-only proof.

## Fixture + tests

- New verified bundle `tests/fixtures/scaleset-worker/` (`worker_profile.json` + manifest;
  own dir so part A's 11-file bundle contract is untouched — verified: protocol suite still 8/8).
- Unit (in-module, scripted runners): ownership 5, dind 11, runner 12, machine/provision 11,
  supervise 8, allocator 9. Live `live_tool_content_hook_proves_pinned_images` (ignored, ran
  explicitly here): PASS against real pulled images.
- Integration `scaleset_allocator` (6): thundering-herd exact grants (16 racers, N=2, dual
  barrier), churn occupancy bound, native↔scaleset mutual denial, stale-generation grants
  nothing, reconcile-before-advertise, sweep never frees scale-set rows.
- Integration `scaleset_worker` (3): fixture→const pin match, full lifecycle (acquire →
  provision → tick → dead-runner fail → export → cleanup → release, 12 edges), cleanup
  failure retains uncertain.

## Gates (final tree state)

- `cargo test -p velnor-runner -p velnor-model`: all green (runner lib 2271 + suites).
- `cargo test -p velnor-runner --features test-support`: all green (lib 2339, allocator 6/6,
  protocol 8/8, worker 3/3, all others green). `cargo test -p velnor-control`: 278 incl.
  9 ledger tests.
- `cargo clippy --locked --profile test --all-targets --all-features -p velnor-runner
  -p velnor-model -p velnor-control -- -D warnings`: 0 errors (CI bar).
- `cargo fmt --check` on the three crates: clean.

## Merge notes for parent

- Post-merge daemon startup should call ONE `startup_reconcile` with native markers + worker
  registry (two separate reconciles would mark the other lane uncertain); C2's
  `reconcile_and_sweep` and this fn overlap there — collapse to one call site.
- `AllocatorError` mirrors C2 `GuardError` semantics with lane-specific messages; unify or keep.
- Listener (poll→Scale→ACK), journal `ScaleSetWorkerEdge` sink, and `scheduler.rs` activation
  gate are later parts; seams ready (`EdgeSink`, `provision_worker`, `Supervision`, guards).
