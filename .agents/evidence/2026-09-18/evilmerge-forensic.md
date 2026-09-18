# Evil-merge forensic: converge d3895648 ("Converge everything to main")

READ-ONLY forensic. Method: git object reads + diffs only. No pushes/branches/merges created.

## S0. Refs and merge mechanism (proven)

- Base (merge-base): `dead5ecb` "bump D19 pin to 08ea1b07 after PR994-capabilities merge"
- Main: `231253b3` (PR #934) — Parent2 of converge
- Branch: `8b7b4ac1` "Merge origin/main (dead5ecb) into docs/bastion-final-plan" — Parent1 of converge
- Converge: `d3895648` "Merge origin/main (231253b3) into docs/bastion-final-plan / Converge everything to main; keep branch's plans/bastion-three-provider-ci/."
- Tip: `58a94b12` (PR #912) via `b3ff4626` "refresh generator scan fingerprint after main merge"

Proofs:

- `git diff --name-status 231253b3..d3895648` = exactly 6 added files, all under `plans/bastion-three-provider-ci/`. Excluding that dir, the diff is EMPTY: **converge took main's side everywhere else, byte-for-byte.**
- `git rev-parse b3ff4626^{tree}` == `git rev-parse 58a94b12^{tree}` (`1afd8c26…`): tip == converge branch verbatim. `b3ff4626` touches 1 line of `.github-actions-generator-state` only.
- `git diff --name-status 231253b3..8b7b4ac1` = 213 files; minus the 6 kept plans files = **207 files in scope** below (incl. `plans/2026-09-17-pr994-behavior-ledger.md`, which the branch never touched).

## S1. Full casualty list (207 files)

Lines column = `main-lines/branch-lines` (`wc -l` of each blob; 0 = absent; A-rows show branch-lines in both slots since main lacks the file).
Verdicts: LOST / SUPERSEDED / MAIN-KEPT / DELETION-DISCARDED.

- LOST = branch-unique content absent from ALL of main through 58a94b12 (proven per row).
- SUPERSEDED = main's newer content won (branch-untouched stale files; generated files replaced by main regens — classified by their source, noted per row).
- MAIN-KEPT = main-side file the branch never had (D in main→branch diff); survived.
- DELETION-DISCARDED = branch deleted a base file; converge kept main's copy (content survives; branch intent moot).
- PRESENT-VIA-OTHER-PATH applies at hunk/feature level only (S2: 754c1cd6 hunks, 65beb2ed-producer, 511ffd7e, Prove-step bytes). **No file qualifies wholesale** (all-or-nothing check: 0 A-files present on tip, 0 M-files tip==branch).

Counts: **118 LOST** (63 new + 55 modified: 49 branch-only + 6 both-changed), 63 MAIN-KEPT, 24 SUPERSEDED (16 untouched + 7 generated + 1 state), 2 DELETION-DISCARDED.

| file | lines | verdict | evidence |
|---|---|---|---|
| `.github-gen/sources/actions/setup-velnor-workflow/action.yml` | 186/192 | LOST | tip==main==base b2a17f61; branch +13/-7 via f79ab248 65beb2ed  |
| `.github-gen/velnor-workflow.toml` | 299/308 | SUPERSEDED | branch==base bc688563 (untouched); main evolved: 33eda2c4 7cd66e78 17d4867b bd9952e1  |
| `.github/actionlint.yaml` | 9/10 | SUPERSEDED | branch==base 7aded2d0 (untouched); main evolved: 11b5c1cb  |
| `.github/actions/setup-velnor-workflow/action.yml` | 186/192 | LOST | tip==main==base b2a17f61; branch +13/-7 via f79ab248 65beb2ed  |
| `.github/ci/.github-actions-generator-state` | 27/27 | SUPERSEDED | tip==main fd08bdd0; main: 33eda2c4 7cd66e78 35dad7d5 22cc123c ; NOTE: generated; all four blobs differ; tip = b3ff4626 refresh of main post-flip state. |
| `.github/ci/project.toml` | 415/278 | SUPERSEDED | tip==main 62f3319c; main: 11b5c1cb f66356eb ; NOTE: generated; branch 511ffd7e hunk (GHA flags in github_full_commands) superseded by s2-regen 11b5c1cb (restructured unit/full_commands, GHA flags live) (see S2). |
| `.github/workflows/ci-main.yml` | 2429/2175 | SUPERSEDED | tip==main b013a16f; main: 33eda2c4 7cd66e78 17d4867b bd9952e1 ; NOTE: generated; branch hunks = seed_compat fingerprint only (511ffd7e); superseded by flip-regen chain 11b5c1cb..33eda2c4. |
| `.github/workflows/ci-policy.yml` | 191/178 | SUPERSEDED | branch==base bcb6aaff (untouched); main evolved: 33eda2c4 7cd66e78 17d4867b bd9952e1  |
| `.github/workflows/ci-pr.yml` | 2248/2007 | SUPERSEDED | tip==main 3d82f53f; main: 33eda2c4 7cd66e78 17d4867b bd9952e1 ; NOTE: generated; branch hunks = seed_compat fingerprint only (511ffd7e); superseded by flip-regen chain 11b5c1cb..33eda2c4. |
| `.github/workflows/ci-runtime-products.yml` | 311/384 | SUPERSEDED | tip==main bb962c38; main: 51e635af ; NOTE: generated from runtime_products.rs (LOST-majority source); 754/producer hunks byte-present via #916 regen (Prove-step identical bytes), 403/consumer hunks absent; superseded by 51e635af regen (flip did not touch it). |
| `.github/workflows/ci-unit-bun.yml` | 532/533 | SUPERSEDED | branch==base 55321653 (untouched); main evolved: 33eda2c4 7cd66e78 17d4867b bd9952e1  |
| `.github/workflows/ci-unit-docker.yml` | 574/573 | SUPERSEDED | branch==base 052efc60 (untouched); main evolved: 33eda2c4 7cd66e78 17d4867b bd9952e1  |
| `.github/workflows/ci-unit-docs.yml` | 530/531 | SUPERSEDED | branch==base 46fd51f6 (untouched); main evolved: 33eda2c4 7cd66e78 17d4867b bd9952e1  |
| `.github/workflows/ci-unit-opentofu.yml` | 535/536 | SUPERSEDED | branch==base 0339629e (untouched); main evolved: 33eda2c4 7cd66e78 17d4867b bd9952e1  |
| `.github/workflows/ci-unit-rust.yml` | 912/903 | SUPERSEDED | tip==main 5797dd28; main: 33eda2c4 7cd66e78 17d4867b bd9952e1 ; NOTE: generated; branch regens (A2/pinfetch/provision/security sources, all LOST) superseded by flip-regen chain. |
| `.github/workflows/maintenance.yml` | 360/362 | SUPERSEDED | branch==base 89728b66 (untouched); main evolved: 33eda2c4 7cd66e78 17d4867b bd9952e1  |
| `.github/workflows/nightly.yml` | 110/114 | SUPERSEDED | branch==base e6b6ef7a (untouched); main evolved: 11b5c1cb  |
| `.github/workflows/preview.yml` | 1058/1066 | SUPERSEDED | tip==main 4ffa13f8; main: 33eda2c4 7cd66e78 17d4867b bd9952e1 ; NOTE: generated; branch 6be652b5 guest-payload hunk (source LOST) superseded by flip-regen chain. |
| `.github/workflows/release.yml` | 4030/4332 | SUPERSEDED | tip==main f4874031; main: 33eda2c4 7cd66e78 17d4867b bd9952e1 ; NOTE: generated; branch regens (B1/A2/pinfetch/provision/security/guest sources, all LOST) superseded by flip-regen chain. |
| `Dockerfile` | 206/198 | SUPERSEDED | branch==base bb1408e2 (untouched); main evolved: f66356eb  |
| `content/docs/guides/execution.mdx` | 481/479 | LOST | tip==main==base 626b6d94; branch +7/-9 via deb9e204  |
| `content/docs/guides/operator.mdx` | 350/350 | LOST | tip==main==base 6063183b; branch +4/-4 via deb9e204  |
| `content/docs/operations/storage-and-resources.mdx` | 186/157 | LOST | tip==main==base 1875b5b6; branch +24/-53 via deb9e204  |
| `content/docs/troubleshooting.mdx` | 217/218 | LOST | tip==main==base 73f6e2bc; branch +5/-4 via deb9e204  |
| `crates/velnor-control/src/lib.rs` | 39/40 | LOST | tip==main==base cc8e188a; branch +1/-0 via deb9e204  |
| `crates/velnor-control/src/permit_ledger.rs` | 1276/1276 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: deb9e204  |
| `crates/velnor-control/src/store/migrations.rs` | 2162/2530 | LOST | tip==main==base 70e31a71; branch +372/-4 via 3f935eed 966f3623  |
| `crates/velnor-model/src/lib.rs` | 120/125 | LOST | tip==main==base da45cb68; branch +7/-2 via 966f3623  |
| `crates/velnor-model/src/scheduler.rs` | 171/563 | LOST | tip==main==base e5f01a43; branch +394/-2 via 3f935eed 966f3623  |
| `crates/velnor-model/src/telemetry.rs` | 2309/2310 | LOST | tip==main==base 9b9a2f8b; branch +1/-0 via 915ed072  |
| `crates/velnor-runner/Cargo.toml` | 198/199 | LOST | tip==main==base 642ee2f6; branch +1/-0 via 3f935eed  |
| `crates/velnor-runner/debian/postinst` | 312/283 | LOST | tip==main==base e0f577a4; branch +25/-54 via deb9e204  |
| `crates/velnor-runner/debian/postrm` | 139/133 | LOST | tip==main==base f2ab6a27; branch +18/-24 via deb9e204  |
| `crates/velnor-runner/debian/velnor-daemon.service` | 82/80 | LOST | tip==main==base 9857d9b8; branch +0/-2 via deb9e204  |
| `crates/velnor-runner/debian/velnor-daemon@.service` | 76/74 | LOST | tip==main==base 9f8083cf; branch +0/-2 via deb9e204  |
| `crates/velnor-runner/debian/velnor-jobs.slice` | 19/10 | LOST | tip==main==base f308e99c; branch +5/-14 via deb9e204  |
| `crates/velnor-runner/debian/velnor.env` | 96/99 | LOST | tip==main==base fdc29154; branch +7/-4 via deb9e204  |
| `crates/velnor-runner/src/args.rs` | 418/429 | LOST | tip==main==base 7b3ebf94; branch +15/-4 via 9d7b6dfd deb9e204  |
| `crates/velnor-runner/src/bin/velnor-guest-image.rs` | 245/313 | LOST | tip==main==base 00676432; branch +71/-3 via 6be652b5  |
| `crates/velnor-runner/src/buildkit.rs` | 2549/2301 | LOST | tip==main==base b6167804; branch +45/-293 via deb9e204  |
| `crates/velnor-runner/src/container.rs` | 4909/4572 | LOST | tip==main==base e296c140; branch +200/-537 via 40ca2764 deb9e204  |
| `crates/velnor-runner/src/container/host_budget.rs` | 1283/0 | DELETION-DISCARDED | tip==main==base 884dacae; branch deletion by deb9e204 discarded |
| `crates/velnor-runner/src/docker_lease.rs` | 6248/6313 | LOST | tip==main==base 0217e502; branch +79/-14 via deb9e204  |
| `crates/velnor-runner/src/execution/docker.rs` | 752/574 | LOST | tip==main==base ec1d22b7; branch +83/-261 via deb9e204  |
| `crates/velnor-runner/src/execution/mod.rs` | 774/744 | LOST | tip==main==base a060e3d5; branch +15/-45 via deb9e204  |
| `crates/velnor-runner/src/execution/tests.rs` | 2124/2124 | LOST | tip==main==base dbef515e; branch +40/-40 via deb9e204  |
| `crates/velnor-runner/src/executor.rs` | 31559/31270 | LOST | tip==main==base 6f95ee2b; branch +15/-304 via deb9e204 915ed072  |
| `crates/velnor-runner/src/github_adapter.rs` | 2370/2418 | LOST | tip==main==base 00b3e3de; branch +98/-50 via 40ca2764 deb9e204  |
| `crates/velnor-runner/src/lib.rs` | 187/189 | LOST | tip==main==base dfa649a4; branch +2/-0 via deb9e204 966f3623  |
| `crates/velnor-runner/src/node/controller.rs` | 6644/6642 | LOST | tip==main==base ad877c98; branch +1/-3 via deb9e204  |
| `crates/velnor-runner/src/node/exec.rs` | 142/140 | LOST | tip==main==base 4b339eec; branch +1/-3 via deb9e204  |
| `crates/velnor-runner/src/ops.rs` | 2583/2679 | LOST | tip==main==base 7712e9b7; branch +138/-42 via 915ed072  |
| `crates/velnor-runner/src/permit_guard.rs` | 576/576 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd deb9e204  |
| `crates/velnor-runner/src/plan.rs` | 134/132 | LOST | tip==main==base 55d8ec65; branch +0/-2 via deb9e204  |
| `crates/velnor-runner/src/preflight.rs` | 882/876 | LOST | tip==main==base 97e43532; branch +6/-12 via deb9e204  |
| `crates/velnor-runner/src/runner.rs` | 27878/28582 | LOST | tip==main==base 5cb862ee; branch +826/-122 via 9d7b6dfd deb9e204 915ed072  |
| `crates/velnor-runner/src/scaleset/allocator.rs` | 548/548 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 6bd6de31  |
| `crates/velnor-runner/src/scaleset/backoff.rs` | 276/276 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed 966f3623  |
| `crates/velnor-runner/src/scaleset/capacity.rs` | 507/507 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 3f935eed  |
| `crates/velnor-runner/src/scaleset/client.rs` | 1210/1210 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 40ca2764 966f3623  |
| `crates/velnor-runner/src/scaleset/config.rs` | 214/214 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/src/scaleset/converge.rs` | 329/329 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 3f935eed  |
| `crates/velnor-runner/src/scaleset/credentials.rs` | 398/398 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 40ca2764 966f3623  |
| `crates/velnor-runner/src/scaleset/daemon.rs` | 713/713 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd  |
| `crates/velnor-runner/src/scaleset/demand.rs` | 768/768 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 3f935eed  |
| `crates/velnor-runner/src/scaleset/errors.rs` | 305/305 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/src/scaleset/fixtures.rs` | 316/316 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed 966f3623  |
| `crates/velnor-runner/src/scaleset/intents.rs` | 631/631 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 3f935eed  |
| `crates/velnor-runner/src/scaleset/key_material.rs` | 324/324 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd  |
| `crates/velnor-runner/src/scaleset/lane.rs` | 1244/1244 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd  |
| `crates/velnor-runner/src/scaleset/listener.rs` | 804/804 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 3f935eed  |
| `crates/velnor-runner/src/scaleset/metrics.rs` | 228/228 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed  |
| `crates/velnor-runner/src/scaleset/mod.rs` | 118/118 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 3f935eed 6bd6de31 966f3623  |
| `crates/velnor-runner/src/scaleset/reconcile.rs` | 610/610 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 3f935eed  |
| `crates/velnor-runner/src/scaleset/registration.rs` | 353/353 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd  |
| `crates/velnor-runner/src/scaleset/scale.rs` | 1094/1094 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 3f935eed  |
| `crates/velnor-runner/src/scaleset/session.rs` | 566/566 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed 966f3623  |
| `crates/velnor-runner/src/scaleset/shared_ledger.rs` | 370/370 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd  |
| `crates/velnor-runner/src/scaleset/upstream_pin.rs` | 57/57 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/src/scaleset/worker/dind.rs` | 692/692 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 40ca2764 6bd6de31  |
| `crates/velnor-runner/src/scaleset/worker/mod.rs` | 764/764 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 40ca2764 6bd6de31  |
| `crates/velnor-runner/src/scaleset/worker/ownership.rs` | 328/328 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 40ca2764 6bd6de31  |
| `crates/velnor-runner/src/scaleset/worker/runner.rs` | 1266/1266 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 40ca2764 6bd6de31  |
| `crates/velnor-runner/src/scaleset/worker/supervise.rs` | 910/910 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 40ca2764 6bd6de31  |
| `crates/velnor-runner/src/service.rs` | 827/839 | LOST | tip==main==base 78d3c6f6; branch +25/-13 via 9d7b6dfd deb9e204  |
| `crates/velnor-runner/src/test_support.rs` | 762/762 | LOST | tip==main==base 92993d0c; branch +4/-4 via 915ed072  |
| `crates/velnor-runner/tests/fixtures/scaleset-worker/manifest.json` | 6/6 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 6bd6de31  |
| `crates/velnor-runner/tests/fixtures/scaleset-worker/worker_profile.json` | 1/1 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 6bd6de31  |
| `crates/velnor-runner/tests/fixtures/scaleset/acquire_jobs.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/acquire_partial.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed  |
| `crates/velnor-runner/tests/fixtures/scaleset/admin_connection.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/error_agent_not_found.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/installation_token.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/jit_runner_config.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/manifest.json` | 26/26 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/message_batch.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/message_deferred_offer.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed  |
| `crates/velnor-runner/tests/fixtures/scaleset/message_high_water.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed  |
| `crates/velnor-runner/tests/fixtures/scaleset/message_redelivered.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed  |
| `crates/velnor-runner/tests/fixtures/scaleset/message_reordered.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed  |
| `crates/velnor-runner/tests/fixtures/scaleset/message_stats_only.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/message_unknown_kind.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed  |
| `crates/velnor-runner/tests/fixtures/scaleset/registration_token.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/runner_reference.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/runner_scale_set.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/seed_stale_generation.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed  |
| `crates/velnor-runner/tests/fixtures/scaleset/session_created.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 966f3623  |
| `crates/velnor-runner/tests/fixtures/scaleset/session_refreshed.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed  |
| `crates/velnor-runner/tests/fixtures/scaleset/transcript_nil_polls.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed  |
| `crates/velnor-runner/tests/fixtures/scaleset/transcript_redelivery.json` | 0/0 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed  |
| `crates/velnor-runner/tests/jobs_slice.rs` | 173/174 | LOST | tip==main==base 6040469d; branch +110/-109 via deb9e204  |
| `crates/velnor-runner/tests/node_arch.rs` | 1654/1663 | LOST | tip==main==base adf0f209; branch +18/-9 via deb9e204  |
| `crates/velnor-runner/tests/scaleset_allocator.rs` | 325/325 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 6bd6de31  |
| `crates/velnor-runner/tests/scaleset_daemon.rs` | 2081/2081 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd  |
| `crates/velnor-runner/tests/scaleset_loop.rs` | 820/820 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 3f935eed  |
| `crates/velnor-runner/tests/scaleset_protocol.rs` | 437/437 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 3f935eed 966f3623  |
| `crates/velnor-runner/tests/scaleset_worker.rs` | 332/332 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 9d7b6dfd 40ca2764 6bd6de31  |
| `crates/velnor-workflow-contract/tests/fixtures/pre_parameterization_cache_keys.rs` | 603/591 | SUPERSEDED | branch==base 6797bf8b (untouched); main evolved: 11b5c1cb 9de09111  |
| `crates/velnor-workflow/README.md` | 109/113 | LOST | tip==main==base b3c6c874; branch +6/-2 via a05b0e2a  |
| `crates/velnor-workflow/build.rs` | 138/149 | LOST | tip==main==base 96798e8c; branch +12/-1 via a05b0e2a  |
| `crates/velnor-workflow/src/apt.rs` | 6340/6340 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: ed442855  |
| `crates/velnor-workflow/src/closure.rs` | 509/637 | LOST | tip==main==base 66da631b; branch +128/-0 via a05b0e2a  |
| `crates/velnor-workflow/src/config/mod.rs` | 5993/5503 | LOST | tip==main==base 590a3d6a; branch +41/-0 via ed442855 ; NOTE: both-changed; B1 +41 (ed442855) LOST — survivor 1/15 is a base line (see S2). |
| `crates/velnor-workflow/src/consumer_negatives.rs` | 1651/1651 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 4284f246  |
| `crates/velnor-workflow/src/lib.rs` | 18985/19403 | LOST | tip==main==base 08202224; branch +749/-53 via 40ca2764 ed442855 a05b0e2a 4284f246 511ffd7e f79ab248 65beb2ed 48b49d92 ; NOTE: both-changed; 41/362 survive but 40 are base lines (1 generic sig line); B1/A2-prov/A2-srcid/pinfetch/s1-perf/security hunks LOST; GHA-cache subset PORTED to s2 (see S2). |
| `crates/velnor-workflow/src/policy.rs` | 3098/3150 | LOST | tip==main==base 2b885357; branch +58/-6 via a05b0e2a 48b49d92  |
| `crates/velnor-workflow/src/policy/tests.rs` | 1457/1596 | LOST | tip==main==base a76a86ec; branch +139/-0 via 48b49d92  |
| `crates/velnor-workflow/src/primitives/check_profiles.rs` | 1628/1410 | SUPERSEDED | branch==base 4989383f (untouched); main evolved: 9acc5c87 6d75ec06  |
| `crates/velnor-workflow/src/primitives/ir.rs` | 6472/6398 | LOST | tip==main==base c699ae57; branch +10/-58 via 48b49d92 ; NOTE: both-changed; 48b49d92 pin-fetch hunks LOST (survivor 1/10 is a base line). |
| `crates/velnor-workflow/src/primitives/release.rs` | 9696/10032 | LOST | tip==main==base 145d6cff; branch +668/-30 via 40ca2764 ed442855 65beb2ed 48b49d92 6be652b5 ; NOTE: both-changed; B1/A2-srcid/guest/security hunks LOST (26/222 survive, all 26 are base lines). |
| `crates/velnor-workflow/src/primitives/renovate.rs` | 873/836 | SUPERSEDED | branch==base 96ed3a8b (untouched); main evolved: ca832734  |
| `crates/velnor-workflow/src/primitives/runtime_products.rs` | 1672/2405 | LOST | tip==main==base 910ee72e; branch +1126/-83 via a05b0e2a c3a35c77 f79ab248 754c1cd6 65beb2ed 48b49d92 ; NOTE: both-changed, MIXED: 754c1cd6 + 65beb2ed-producer PRESENT via #916/51e635af (269/274 lines byte-identical); consumer/403/provision/pinfetch (~325 lines) LOST (see S2). |
| `crates/velnor-workflow/src/promote.rs` | 690/690 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 5838b9fe a05b0e2a  |
| `crates/velnor-workflow/src/runtime.rs` | 6903/7543 | LOST | tip==main==base 521e9a7f; branch +650/-10 via ed442855 a05b0e2a  |
| `crates/velnor-workflow/src/s2/capability_tests.rs` | 473/0 | MAIN-KEPT | tip==main b4995d7c; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/closure.rs` | 509/0 | MAIN-KEPT | tip==main e5893e67; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/config/canonical.rs` | 77/0 | MAIN-KEPT | tip==main c2dc6feb; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/config/mod.rs` | 5167/0 | MAIN-KEPT | tip==main 9289bfe0; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/dispatch.rs` | 243/0 | MAIN-KEPT | tip==main c680c85c; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/estate.rs` | 338/0 | MAIN-KEPT | tip==main e1e444f7; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/mod.rs` | 20014/0 | MAIN-KEPT | tip==main 0ec0658f; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/planner.rs` | 497/0 | MAIN-KEPT | tip==main 7f3a6762; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/platform.rs` | 476/0 | MAIN-KEPT | tip==main 1713dfb4; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/policy.rs` | 3087/0 | MAIN-KEPT | tip==main 6c912dbb; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/policy/tests.rs` | 1442/0 | MAIN-KEPT | tip==main 15c7d6fd; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/aggregate.rs` | 68/0 | MAIN-KEPT | tip==main 7475e70e; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/cache.rs` | 288/0 | MAIN-KEPT | tip==main 87305b66; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/check_profiles.rs` | 1390/0 | MAIN-KEPT | tip==main bd62f21c; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/docs_site.rs` | 1376/0 | MAIN-KEPT | tip==main c64de645; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/ir.rs` | 5588/0 | MAIN-KEPT | tip==main d22f481a; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/mod.rs` | 1508/0 | MAIN-KEPT | tip==main 7b18123d; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/pipeline.rs` | 215/0 | MAIN-KEPT | tip==main be00e003; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/plan.rs` | 31/0 | MAIN-KEPT | tip==main f81a104e; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/prepared_tools.rs` | 2751/0 | MAIN-KEPT | tip==main 7f9577e4; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/providers.rs` | 100/0 | MAIN-KEPT | tip==main 0e42263b; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/regen.rs` | 61/0 | MAIN-KEPT | tip==main 1463b48f; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/release.rs` | 8812/0 | MAIN-KEPT | tip==main c04eb23a; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/renovate.rs` | 770/0 | MAIN-KEPT | tip==main bdcabbb2; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/runtime_products.rs` | 1787/0 | MAIN-KEPT | tip==main af93e360; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/snapshot.rs` | 2103/0 | MAIN-KEPT | tip==main 6971e5bb; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/primitives/watch.rs` | 341/0 | MAIN-KEPT | tip==main 72e598c0; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/provider.rs` | 988/0 | MAIN-KEPT | tip==main 374c544e; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/results.rs` | 522/0 | MAIN-KEPT | tip==main ea1ae6d4; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/reuse.rs` | 2746/0 | MAIN-KEPT | tip==main 46b5a886; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/routing.rs` | 284/0 | MAIN-KEPT | tip==main 1cda7f9d; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/runtime.rs` | 7049/0 | MAIN-KEPT | tip==main bb199c7f; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/scan/docker.rs` | 117/0 | MAIN-KEPT | tip==main 361a60ef; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/scan/docs.rs` | 50/0 | MAIN-KEPT | tip==main 9f58ed6c; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/scan/file_walk.rs` | 445/0 | MAIN-KEPT | tip==main 08695e61; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/scan/gradle.rs` | 1005/0 | MAIN-KEPT | tip==main c373f605; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/scan/homebrew.rs` | 22/0 | MAIN-KEPT | tip==main 47d0be97; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/scan/mod.rs` | 325/0 | MAIN-KEPT | tip==main 74948727; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/scan/node.rs` | 422/0 | MAIN-KEPT | tip==main 83601be0; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/scan/opentofu.rs` | 36/0 | MAIN-KEPT | tip==main e5917cb8; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/scan/rust.rs` | 1410/0 | MAIN-KEPT | tip==main 25a4fa39; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/scan/signals.rs` | 26/0 | MAIN-KEPT | tip==main 7648f2ca; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/scan/swift.rs` | 227/0 | MAIN-KEPT | tip==main c3a945f5; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/template_memory.rs` | 378/0 | MAIN-KEPT | tip==main 2209a704; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/trust.rs` | 477/0 | MAIN-KEPT | tip==main 0d216b91; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/tui/mod.rs` | 1383/0 | MAIN-KEPT | tip==main 5c0cc2a5; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/tui/view.rs` | 1064/0 | MAIN-KEPT | tip==main 7b013e02; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/src/s2/watchdog.rs` | 1011/0 | MAIN-KEPT | tip==main 906e2454; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures-s2/polyglot/.github-gen/velnor-workflow.toml` | 9/0 | MAIN-KEPT | tip==main 09db6c06; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures-s2/polyglot/.markdownlint-cli2.yaml` | 4/0 | MAIN-KEPT | tip==main ad7c9400; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures-s2/polyglot/Cargo.toml` | 4/0 | MAIN-KEPT | tip==main 56e8bdc0; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures-s2/polyglot/Dockerfile` | 1/0 | MAIN-KEPT | tip==main c35f1b5f; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures-s2/polyglot/Formula/example.rb` | 2/0 | MAIN-KEPT | tip==main cd4b95b4; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures-s2/polyglot/build.gradle.kts` | 1/0 | MAIN-KEPT | tip==main 6e35612b; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures-s2/polyglot/docs/guide.md` | 1/0 | MAIN-KEPT | tip==main 8c0d02fa; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures-s2/polyglot/infra.tf` | 1/0 | MAIN-KEPT | tip==main 75db7929; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures-s2/polyglot/package-lock.json` | 1/0 | MAIN-KEPT | tip==main da98a5f3; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures-s2/polyglot/package.json` | 1/0 | MAIN-KEPT | tip==main 0ffac1fa; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures-s2/polyglot/rust-toolchain.toml` | 2/0 | MAIN-KEPT | tip==main d6d33819; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/fixtures/release-tasks/velnor-workflow.toml` | 39/0 | MAIN-KEPT | tip==main 81d8c589; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/generic_surface_literals.rs` | 257/255 | SUPERSEDED | branch==base aa1f6214 (untouched); main evolved: eb0303a3  |
| `crates/velnor-workflow/tests/lane_pairing.rs` | 522/517 | SUPERSEDED | branch==base 82704c53 (untouched); main evolved: 24d1df91  |
| `crates/velnor-workflow/tests/promote_atomic.rs` | 394/394 | LOST | absent on 58a94b12 (cat-file exit 128); new at 8b7b4ac1; branch: 5838b9fe a05b0e2a  |
| `crates/velnor-workflow/tests/provider_pairing.rs` | 528/0 | MAIN-KEPT | tip==main 7f02353a; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/release_tasks.rs` | 273/0 | MAIN-KEPT | tip==main 5639194c; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/selection_plan_handoff.rs` | 291/0 | MAIN-KEPT | tip==main ab5f112f; main-added post-base (absent at dead5ecb) |
| `crates/velnor-workflow/tests/velnor_first_ci.rs` | 1335/1322 | LOST | tip==main==base dabe3134; branch +1/-1 via f79ab248 ; NOTE: both-changed; f79ab248 1-line change LOST (0/1; main 24d1df91 R2m-fallout version won). |
| `crates/velnorctl/src/host.rs` | 2047/2048 | LOST | tip==main==base 5e706a40; branch +3/-2 via 9d7b6dfd deb9e204  |
| `crates/velnorctl/src/local_diagnostics.rs` | 1985/1978 | LOST | tip==main==base 28bbc990; branch +11/-18 via deb9e204  |
| `crates/velnorctl/src/runtime.rs` | 932/932 | LOST | tip==main==base 510e49eb; branch +13/-13 via 9d7b6dfd deb9e204  |
| `crates/velnorctl/tests/job_resource_flags_single_source.rs` | 116/0 | DELETION-DISCARDED | tip==main==base 9550fdd3; branch deletion by deb9e204 discarded |
| `plans/2026-09-17-pr994-behavior-ledger.md` | 412/313 | SUPERSEDED | branch==base 600d2a82 (untouched); main evolved: 52c424b1 199617a6  |
| `schemas/velnor.telemetry.v1.json` | 578/579 | LOST | tip==main==base 661e3878; branch +2/-1 via 915ed072  |

## S2. Module mapping (grouped LOST files)

Work-plan deps (from kept `work-plan.md`): A2←A1; B1←A2; B2←B1,A2; B3←B2,A1-A2 (+unbounded work in parallel); B4←B3; C1←B4(APT install); C2←C1; D1←C2; D2←D1; D3←D1,D2,B3/B4; E←D. Sizes = originating-commit insertions (numstat).

### LOST modules (15)

| module | originating commit(s) | size | files (lost) | downstream blockers |
|---|---|---|---|---|
| B1-APT | ed442855 "typed APT feed primitives" | +7473/-41 | apt.rs NEW 6340 (6325+15 merge fixup) + config/mod.rs +41, lib.rs, release.rs, runtime.rs hunks | B1 redo → B2 (needs B1) → B3 → B4 → C1 → C2 → D1 → D2/D3 → E. B1 itself needs A2. |
| A2-PROV | a05b0e2a "atomic pin promotion, product-repo provisioner, fixed producer builders" + 5838b9fe fixup | +1699/+77 | promote.rs NEW 692, promote_atomic.rs NEW 394, closure.rs +128, lib.rs +398, policy.rs, runtime_products.rs +122, runtime.rs +61, README, build.rs, regens | A2 outputs (atomic-promotion proof, provisioner) → B1 (deps A2), B2 (deps A2) |
| A2-SRCID-C (consumer half) | 65beb2ed "carry and verify source revision; pin daemon/runtime identity split" | +360 (split) | MANIFEST_ACCEPT_FILTER revision clause, Velnor consumer probes, consumer-filter tests, daemon/runtime split bits in action.yml×2/release.rs/runtime_products.rs/lib.rs | A2 (source-identity outputs) → B1/B2 |
| A2-SIGNER-C (consumer pin) | f79ab248 "pin consumer attestation verification to default-branch ref" | +52 | setup action --source-ref ×2, Velnor provisioner ×2, all_consumers test, velnor_first_ci 1 line | A2 (trusted signer/ref outputs) → B1/B2 |
| A2-NEG | 4284f246 "executable consumer negative suite + cold-consumer proof" | +1495 | consumer_negatives.rs NEW ~1550 + lib.rs mod decl | A2 (negative suite, cold-consumer proof) → B1/B2 |
| A2-403 | c3a35c77 "converge runtime-product publish on stale-target 403" | +720 | runtime_products.rs +651, ci-runtime-products regen | A2 (publish robustness) → B2 publisher path, B3 |
| A2-PINFETCH | 48b49d92 "self-fetch the D19 pin in the tool" | +250 | policy.rs +56 (ensure_pin_present), policy/tests +139, ir.rs, release.rs, runtime_products.rs, lib.rs +43, regens | A2 (pin machinery) → B1 (publish-before-pin) |
| C2-UNBOUNDED | deb9e204 "unbounded execution plus host-wide max_jobs=N permit ledger" | +3049/-3384 | permit_ledger.rs NEW 1276, permit_guard.rs NEW 590, host_budget.rs DELETED -1283 (deletion discarded), runner/executor/container/buildkit/execution/node/ops/plan rework, debian ×6, docs ×4, velnorctl ×3, tests ×2 | C2 step → D1 (deps C2; shared allocator binds C2 ledger) → D2/D3/E. B3 runs it in parallel. |
| D1-PROTO | 966f3623 "scale-set protocol foundation" | +4127 | scaleset/{mod,client,session,credentials,errors,backoff,config,fixtures,upstream_pin} + scheduler.rs +390, migrations +157, 13 fixtures, scaleset_protocol test | D1 → D2 (deps D1) → D3 → E |
| D1-LANE | 6bd6de31 "homogeneous worker lane plus shared allocator" | +6000 | scaleset/{allocator,worker/dind,worker/mod,worker/ownership,worker/runner,worker/supervise} + 2 fixtures + allocator/worker tests | D1 → D2/D3/E |
| D1-LOOP | 3f935eed "9-step processor loop plus reconcile" | +6161 | scaleset/{capacity,converge,demand,intents,listener,metrics,reconcile,scale} + rusqlite [dependencies] + migrations +225, 10 fixtures, scaleset_loop test | D1 → D2/D3/E |
| D1-WIRE | 9d7b6dfd "wire scale-set adapter into daemon" | +5568 | scaleset/{daemon,lane,key_material,registration,shared_ledger} + runner/service/args wiring + scaleset_daemon test 2037 + velnorctl | D1 → D2/D3/E |
| SEC-B1 | 40ca2764 "harden bastion privileged surfaces (audit batch 1)" | +813 | scaleset client/credentials/worker×4 hardening + container.rs +42, github_adapter, workflow lib.rs +59/release.rs +75, regens, daemon test | D1 (protected credentials = D1 objective) + B-side release hardening |
| OPS-REJECT | 915ed072 "name the failing admission check in opstore rejections" | +450 | ops.rs +180 (AdmissionRejection), runner.rs +337, executor, telemetry.rs, test_support, telemetry schema json | none step-blocking (observability quality) |
| GUEST-PAYLOAD | 6be652b5 "explicit guest-payload file list, scratch out of publish dir" | +103 | guest-image.rs +74 (--work-dir/default_work_dir), release.rs +19, preview/release regens | B3/B4-adjacent (publish correctness; EACCES fix) |

LOST volume: 63 new files = 30,898 lines; 55 modified files branch hunks +6,306/-2,340. Originating lost commits total 38,397 insertions (incl. superseded regens + merge adaptations).

### PRESENT-VIA-OTHER-PATH (feature/hunk level; not casualties)

- A2-SIGNER-P: 754c1cd6 (07:26) runtime_products hunks → main 51e635af #916 (08:12, same author): 269/274 added lines byte-identical (only 4 doc lines + 1 test PINNED const differ); generated Prove-the-default-branch-ref step byte-identical branch-vs-tip (lines 55-61 == 47-53).
- A2-SRCID-P (producer half of 65beb2ed): revision field + --revision probes/spot-checks survive in tip s1 runtime_products (30/53 lines); also ported to s2 (--revision ×6). Consumer half (filter revision clause; tip filter == base text) LOST. Main 51e635af message explicitly defers consumer half ("No consumer accept-filter… those ride the follow-up consumer PR") — no such PR exists through 58a94b12.
- PERF-GHA: 511ffd7e (07:32) → main eb0303a3 R2 (15:59): s2 `docker_seed_full_command` (27 lines) == s1 `docker_hosted_full_command` (23 lines) + exactly 4 lines (github-token secret block); identical doc comment; identical test (modulo schema-2 adaptation); LIVE in tip project.toml docker full_commands (cache-from/to ×2). s1 lib.rs hunks themselves LOST (tip s1 lib.rs: 0 hits).
- Task's "D2 via R2" example is hypothetical for this converge: the branch contains NO D2 module (D2 was built on main: dc890899/52c424b1/06050c9f). The real instances of the pattern are the three above.

### Hunk-level notes (both-changed handwritten files)

- runtime_products.rs: per-commit survival on tip — 754c1cd6 130/135, 65beb2ed 30/53 (producer), f79ab248 4/18 (3 base + 1 coincidental producer-smoke line), c3a35c77 32/274 (all boilerplate; 403/stale: tip 0, branch 12), a05b0e2a 0/49, 48b49d92 0/2. Verdict LOST (majority + all consumer-side gone), producer overlap cited above.
- lib.rs: 41/362 survive; 40 are refactored base lines (verified each against dead5ecb blob), 1 is a generic 31-char signature line also added independently by main #925/#928. All B1/A2-prov/A2-srcid/pinfetch/perf-s1/security hunks LOST.
- release.rs: 26/222 survive, all 26 present in base blob (B1 refactor, not new content). B1 APT integration (AptContract etc.) absent tip-wide incl. s2; base `apt_feed` (1 hit) is the pre-existing untyped feed.
- config/mod.rs 1/15, ir.rs 1/10: sole survivors are base lines. velnor_first_ci.rs 0/1.
- B1 identifiers (40 sampled: AptContract, apt_packages_argv, ChannelUpdateInputs, …): all ABSENT on tip except generic words (as_str/parse) and 2 runner-mirror fns (is_lower_hex etc. in velnor-runner/src/release.rs, pre-existing per apt.rs docs).
- C2/D1 identifiers (permit_ledger, PermitLedger, mod scaleset, ScaleSetClient, DaemonWorkerLane, shared_ledger, permit_guard, acquire_jobs, worker_profile, …): all ABSENT on tip. `scaleset` word hits in scheduler.rs files and `runner_name` in control/store are base-identical (verified blob/lines). `default_work_dir` in tip runner.rs == base (tip==base for runner.rs); branch's guest-image copy LOST (tip guest-image: 0 hits). rusqlite: base+tip have it in [dev-dependencies] only; branch added [dependencies] copy (3f935eed) — LOST.
- promote (run_promote/stamp_pin/PromoteOptions/PromoteReport), pin-fetch (ensure_pin_present), closure builders (binary_dependency_sections), opstore (AdmissionRejection): all ABSENT tip-wide incl. s2.

## S3. Merge hygiene audit (independent replay: `git merge-tree --write-tree P1 P2` vs actual tree)

Labels: CLEAN = tree == auto-merge exactly. RESOLVED = differs only in conflicted files; both sides' content verified preserved line-level (added-lines(P1|base) and added-lines(P2|base) survival checked). ADAPTIVE = contains non-conflict fixups, all additive/adaptive with zero drops (removed lines individually reviewed; test counts compared). EVIL = drops content. Strict task-binary mapping: only CLEAN counts as clean; RESOLVED/ADAPTIVE are listed with their exact deviation.

### Required six

| merge | verdict | evidence |
|---|---|---|
| 08bfa747 (D2 branch update) | RESOLVED | 1 conflict (generator-state); resolved TOOK-P2; only that file differs from auto; proper regen in child 83fb1d0e (parent verified) |
| fa826405 (#927 update) | CLEAN | tree == auto (4367a8f4…); no conflicts |
| d8b4db94 (#929 update) | CLEAN | tree == auto; no conflicts |
| 50c30b22 (R2m update 1) | CLEAN | tree == auto (3ac7dec7…); s2/mod.rs hunk auto-merged |
| a0a15d29 (R2m update 2) | RESOLVED | 1 conflict (generator-state); TOOK-P2; only that file differs; regen in child 17d4867b (parent verified) |
| f7282e31 (R2m update 3) | RESOLVED | 1 conflict (generator-state); TOOK-P2; only that file differs; regen in child 7cd66e78 (parent verified) |

R2m branch merge completeness: `git log --merges 52739e17..ec399527` shows exactly these 3 branch-update merges (+ PR merges #932/#933/#930). No others.

### Main-side sweep (all other origin/main updates in dead5ecb..231253b3)

| merge | verdict | evidence |
|---|---|---|
| 0469e5a5 (scheduled-checks) | RESOLVED | state conflict → TOOK-P1; only that file differs |
| dd8a1cea (#916 branch) | RESOLVED | state conflict → regenerated in-merge (18-line churn) |
| 12b25700 (scheduled-checks) | RESOLVED | state conflict → regenerated in-merge |
| 933239b2 (rendezvous) | RESOLVED | state conflict → regenerated in-merge |
| d4771cc6 (scheduled-checks) | RESOLVED | 13 generated files conflicted; 12 TOOK-P1 (branch regens incl. branch sources; main side was pin-bump churn, re-supplied later); state regenerated (markers removed + fresh scan fp); follow-up regen 0d00195d is direct child (parent verified). No source file affected. |
| a2840748 (#923), b6f53c09 (#921), 0765af35 (#919) | CLEAN (out of scope: PR-into-main merges with fix-style subjects) | tree == auto each |

### Docs-branch sweep (base..8b7b4ac1; matters for casualty attribution)

CLEAN: 00656e0f, 108f43aa, c5a67ed4, 0477e14f (tree == auto).
RESOLVED (conflicts resolved, both sides verified preserved): 7325b9f1, bb27013f, 13096225, 40ff66ec, 96ccc0f1 (state regen each); 1d3eb22e (state + runtime_products; fresh third PINNED a76dfe51); 8f030b7c (8 files; fresh PINNED e3224ae8; manifest_revision coverage kept ×9; 2-arg→1-arg velnor call migrated ×3; only doc-wording/PINNED version misses).
ADAPTIVE (additive-only, 0 drops — every removed line reviewed):

- 8b7b4ac1: 7 conflicted files resolved with both sides preserved (lib.rs P1 363/364 + P2 526/526; release.rs 342/342 + 2432/2432; runtime.rs USAGE truly merged; config 15/15 + 1322/1322; workflows: branch pin-self-fetch mechanism correctly superseded main's hardcoded pin; retention-chain `;` miss = chain extension, content kept) + non-conflict +15/-0 in apt.rs (test-fixture fields for main-extended struct).
- 89773efc: no conflicts; +157/-47 consumer_negatives adaptation (--source-ref enforcement for f79ab248-era branch; removed lines superseded by extended versions); test fns 19/19 identical + state regen.
- 499a7c03: conflicted lib.rs/RP resolved (RP 470/470 + 49/49; lib misses = megaline merges + P2 helpers superseded by revision-carrying versions, verified present) + non-conflict +86/-40 consumer_negatives (fetch-remote → trees-API migration for a05b0e2a provisioner); 19/19 tests, 1 renamed with same assertion goal.
- 04607e6b: 4 conflicted scaleset files resolved (all 12 miss lines are `///` doc-wording picks, 0 functional) + non-conflict +63/-22 scaleset_daemon (import/API migration to OwnershipId/runner_name/WorkerIdentity; JIT-via-env-file assertion UPGRADE; slug via production constructors); tokio tests 11/11 + state regen.
- 411b8a99: conflicted lib.rs resolved (37/38 P2 lines; 1 megaline superseded by pin-self-fetch form, P2's static PINNED_REVISION env subsumed) + non-conflict 1-line sig adaptation in release.rs + state.
- 137ca1f9, 70b1f7d9, bde4d4ec, 34c44fa7, 3bb1e23c: state-only regen-in-merge (1-2 fingerprint lines; bde4d4ec: config+scan fp refresh shown in full).

EVIL (drops): NONE in any audited merge. d3895648 is the sole content-dropping merge in the campaign.

## S4. A3 validity

A3 = "three consecutive full green bootstrap main runs" (deps A1, A2) — a greenness observation on main's tree. The converge dropped only ADDITIVE branch sources; main's tree cannot fail from their absence unless it references them.

35-pattern sweep on 58a94b12 (lost paths, module names, identifiers, fixtures) excluding kept plans dir: every hit disambiguated —

- `mod apt/promote/consumer_negatives/permit_ledger/permit_guard/scaleset`, `crate::apt`, `AptContract`, `run_promote`, `stamp_pin`, `ensure_pin_present`, `AdmissionRejection`, `ScaleSetClient`, `DaemonWorkerLane`, `acquire_jobs`, `worker_profile`, `promote_atomic`, `fixtures/scaleset`, `MANIFEST_ACCEPT_FILTER.*revision`, `guest-payload file list`, `job_resource_flags_single_source`: ALL ABSENT.
- `scaleset(_)` in schedulers, `runner_name` in control/store, `default_work_dir` in runner.rs, `--work-dir` in operator docs, `default-branch ref` in executor.rs: verified base-identical (blobs/lines match dead5ecb; tip==base for those files).
- `default-branch ref` in ci-runtime-products + runtime_products (s1/s2), `PINNED_REVISION: {revision}` provisioner template, MANIFEST_ACCEPT_FILTER (no revision clause): main's own #916/s2/base content, self-consistent (attestation WITHOUT --source-ref; filter WITHOUT revision clause — i.e., pre-A2-consumer main, coherent).
- `host_budget` (+host_budget.rs): branch deletion discarded; main keeps module AND all references — self-consistent.
- Kept `plans/bastion-three-provider-ci/` DOES name the lost modules (B1/D1/C2/A2…) — as planned work steps, not code/test/spec dependencies. No CI impact.

Main is self-consistent: no test/spec/code references lost content. The casualty costs A2-step OUTPUTS (consumer/provisioner/promotion proofs must be redone on main), but A3's verdict (greenness observed on main's tree) is unaffected.

**A3: STANDS.**

---

CASUALTIES: 118 files, 15 modules: B1-APT, A2-PROV, A2-SRCID-C, A2-SIGNER-C, A2-NEG, A2-403, A2-PINFETCH, C2-UNBOUNDED, D1-PROTO, D1-LANE, D1-LOOP, D1-WIRE, SEC-B1, OPS-REJECT, GUEST-PAYLOAD (present-via-other-path, not casualties: A2-SIGNER-P via #916/51e635af, A2-SRCID-P via #916, PERF-GHA via R2/eb0303a3) | OTHER-MERGES: clean: fa826405, d8b4db94, 50c30b22 (sweep-clean: 00656e0f, 108f43aa, c5a67ed4, 0477e14f, a2840748, b6f53c09, 0765af35); resolved: 08bfa747, a0a15d29, f7282e31 (sweep-resolved: 0469e5a5, dd8a1cea, 12b25700, 933239b2, d4771cc6, 40ff66ec, 96ccc0f1, 7325b9f1, bb27013f, 13096225, 1d3eb22e, 8f030b7c); adaptive-additive-zero-drops: 8b7b4ac1, 89773efc, 499a7c03, 04607e6b, 411b8a99, 137ca1f9, 70b1f7d9, bde4d4ec, 34c44fa7, 3bb1e23c; evil: none | A3: STANDS
