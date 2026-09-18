# d1b-worker verification verdict: CERTIFIED

Branch `feat/d1-worker-lane`, commit `6bd6de311f269f20aa07d9211f92def34e8261a5`
(fetched from origin; SHA matches `/tmp/d1b-worker.md`). Diff vs `34c44fa7`:
13 files, worker lane + allocator + fixture + 2 suites. All work done in
scratch worktree `/tmp/v-d1b-wt` (left clean); independent probe kept at
`/tmp/v-d1b-probe.rs`. No merges, no pushes, no repo edits.

## Checks performed

- **Ledger import**: `cmp` of `velnor-control/src/permit_ledger.rs` against
  `deb9e204` version: BYTE-IDENTICAL. `lib.rs` hunk identical to C2's.
- **No second ledger**: no `CREATE TABLE`/`job_permits` DDL anywhere under
  `scaleset/`; `allocator.rs` only calls `PermitLedger::open` on the
  caller-supplied host-wide path. Holder namespace `scaleset/<set>/<req>`,
  no pid, `advertised_free` None-until-reconciled, two-lane
  `startup_reconcile` + sweep that never frees scale-set rows.
- **PinnedImage rejects latest/tags**: author's unit test
  (`tags_and_latest_are_rejected`) plus my independent 5-test disproof probe
  (17 reject-cases incl. `latest`, bare names, tag+digest combos,
  63/65-hex boundaries, flag-shaped input; positive controls incl.
  registry-port refs). All pass. Only `PinnedImage` constructions outside
  `parse` are the two constant pins in `HomogeneousProfile::pinned()`,
  covered by `production_pins_parse`.
- **Every image reference digest-pinned**: grep over lane + suites finds
  tag-form strings only in doc comments and rejection tests. Every docker
  argv image arg flows from `PinnedImage::reference()` (`repo@sha256:…`).
- **Private DinD**: create argv carries exactly one `-H unix://` listener,
  no `-p/--publish/--expose`, no `tcp://`, no host-socket bind
  (probe-asserted on the argv); private socket `/velnor/scaleset/dind.sock`
  on a per-worker bind at identical absolute paths both sides; runner uses
  `--network container:<dind>` (shared netns, not a bridge);
  `DOCKER_HOST=unix://…` proves daemon identity; JIT blob env-only, no App
  keys; adoption fail-closed on foreign/missing ownership labels.
- **Lifecycle reconciliation**: `legal_edge` table reproduces the spec §5.2
  chain exactly (observed→…→running→terminal→export→cleanup→released,
  plus acquired/uncertain branch, retry edges, terminal-from-anywhere);
  probe-asserted legal + illegal edges. Tick reconciles recorded vs
  observed; DinD restart budget 3; dead runner never restarted (GitHub
  oracle decides); export-before-delete; ordered teardown; `CleanupReport`
  fails on missing evidence; permit releases only on confirmed cleanup,
  else retained uncertain.
- **Ownership + log export**: deterministic names from `OwnershipId`,
  `velnor.scaleset.*` labels on all objects, label round-trip, per-worker
  netns isolation, logs+inspect captured before first deletion.
- **Race + lifecycle suites**: `scaleset_allocator` 6/6 (thundering herd,
  churn bound, cross-lane denial, stale generation, reconcile-before-
  advertise, sweep), `scaleset_worker` 3/3 (pin match, 12-edge lifecycle,
  retain-on-failure).
- **Full gates**: `cargo test -p velnor-runner -p velnor-model` green
  (lib 2271 default / 2339 test-support, all suites ok);
  `cargo test -p velnor-control` 278 green; part-A protocol suite still
  8/8 with fixture in its own dir; clippy CI bar 0 errors; `cargo fmt
  --check` clean.
- **Live pin proof**: ignored `live_tool_content_hook_proves_pinned_images`
  ran (not skipped) against a live daemon: PASS. Registry re-resolution
  via `imagetools inspect` confirms both index digests byte-for-byte
  (runner `e5496277…1ef4`, dind `2a232a42…82ac`, amd64 runner
  `50364809…` also matches).

## Minor findings (non-blocking, all fail-closed)

- F1: `PinnedImage::parse` accepts a *leading-space* input because the
  second-opinion parser trims; the doc comment claims "no whitespace".
  Cannot smuggle tags (tag checks run pre-trim on the raw string); the
  malformed reference fails closed at `docker pull` → hook error → no
  provision. Doc-code discrepancy only; suggest a trim/reject tweak later.
- F2: `FILE_COMMAND_DIR` is defined but not mounted in this part, and
  `_work`/tool-cache mounts are runner-side only. d1b's claimed symmetry
  (socket/state bind) holds; full §5.3 daemon-side bind coverage
  (workspace, TMPDIR, HOME, externals, file-commands) belongs to a later
  part. Note also tool cache shares the workspace volume (one volume at
  two guest paths) — intentional per the ownership doc, but flag for the
  container-job-bind part.
- F3: `DIND_READY_TIMEOUT` is currently unused (`provision_worker` uses a
  probe count); presumably reserved for the listener part.

## Verdict

**CERTIFIED** — commit `6bd6de3` delivers what `/tmp/d1b-worker.md` claims
and satisfies the d1b slice of spec §5.2/§5.3; every disproof attempt
failed (i.e. the code held).
