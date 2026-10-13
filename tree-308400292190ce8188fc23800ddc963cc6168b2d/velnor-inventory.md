# Velnor generator and workflow inventory

Status: read-only inventory, refreshed 2026-09-20. No implementation or Git mutation is included in this record.

## Revisions and scope

- Current upstream `main`: `e94b48406c4ed206fce2bbf39b788264e72cf39c` (`fix(package-release): verify source-bound release tasks safely`). The requested historical snapshot was `d20d4d1d17590cca85b501d982cbaad70d42c641`; the cited run used PR #966's merge checkout `45f18aa8c078bebbe9b39d64a1487a0ff59fd4a7` and workflow head `eaae46c48dd45479f1eb7a1314708a38c16d6cb5`.
- Source-owned `.github-gen/velnor-workflow.toml` pins runtime/generator revision `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`; the published runtime tag at current main is `velnor-workflow-runtime-v1-8f88f2905f853bd9`. The source revision, generated pin, published product, and run checkout are different identities and must remain separate in evidence.
- Current Velnor config declares `github-hosted` and `velnor` providers, selects hosted automatically, dispatches both by default, and contains Rust, Bun, Docker, docs, and OpenTofu units. The generated Rust workflow has one reusable unit job per selected Rust unit and one opaque `Run unit checks` step.
- Applicable rules: `crates/velnor-workflow/AGENTS.md` requires generic scanner/IR/runtime fixes, deterministic generation, and no repository-specific generator logic. `.github/AGENTS.md` and `.github/workflows/AGENTS.md` mark generated output as read-only. The generator's source and generated output must be changed together and regenerated with the documented command.

## Architecture and enabling conditions

The generator scans repository facts into typed units, builds an IR, and emits provider workflows/runtime configuration. The scanner intentionally does not execute project commands. Project commands and policy facts live in `.github/ci/project.toml` and source-owned `.github-gen/velnor-workflow.toml`; the schema-2 provider path under `crates/velnor-workflow/src/s2/` emits the active schema-3 runtime table. The crate-root runtime is a legacy schema-1 path and is not the active consumer for current Velnor.

The active Rust scanner (`crates/velnor-workflow/src/s2/scan/rust.rs:470-715`) currently constructs one ordered string vector for every Rust unit:

1. `cargo fmt ... -- --check`
2. `cargo nextest run ...` (or `cargo test ...`)
3. `cargo clippy ... --profile test ... -- -D warnings`

The source comment says nextest precedes Clippy to reuse test-profile artifacts. This is a measured hypothesis only; it is not proof that the order is best for fast failure or for all workspace/profile combinations. The scanner serializes `pr_commands` and `full_commands` into the unit but leaves `products` and `prerequisites` empty for Rust (`rust.rs:590-615`), so the graph cannot express exact compiled products, generated bindings, or stage prerequisites.

The active schema-2 runtime deserializes only `pr_commands`/`full_commands` (`s2/runtime.rs:115-185`). It topologically runs units in parallel layers (`s2/runtime.rs:2612-2661`), but `run_unit` executes every command in one grouped shell-facing runtime invocation (`s2/runtime.rs:2851-2860`). Thus Actions sees setup followed by one `Run unit checks` step, while format, compile, tests, and lint are hidden inside it. A dependency that is selected only as a prerequisite is silently transformed by `prerequisite_commands` (`s2/runtime.rs:2663-2678`): Rust Clippy is rewritten to `cargo/mbx check`, warning flags and `--no-deps` are removed. This implicit command transformation is an enabling condition for hidden compilation, unclear coverage, and possible duplicate work.

The active pipeline primitive (`s2/primitives/pipeline.rs:15-60,127-166`) models a unit job and cache contract, not its validation stages. The active IR (`s2/primitives/ir.rs:4785-4790`) has command digests and cache identities, but no typed stage/profile/product contract. The watch scanner has a useful Docker closure implementation (`s2/primitives/watch.rs:164-252`), including Dockerfile sources and transitive unit dependencies; it correctly selects Docker for PR #966 because that PR changed generator source copied by the Dockerfile. A genuinely unrelated-change fixture is still needed before changing relevance logic.

### Structural diagnosis

The primary defect is a boundary problem: validation stages are represented as opaque command strings at the runtime boundary, while products and prerequisites are represented as optional empty fields. Adding YAML step splitting alone would preserve hidden setup, implicit Clippy rewrites, and duplicate compilation. The generator needs a typed, stage-aware validation/product model. The model must retain each command's exact package, target, features, profile, flags, tool requirements, and source/product inputs; a generic classifier must reject ambiguous commands rather than guessing.

## Historical baseline: run 35477566106

Evidence URL: <https://github.com/tailrocks/velnor/actions/runs/35477566106>. Job URLs below contain raw job timestamps and should be retained with any rerun evidence. The run was a PR event, created `2026-09-19T23:59:32Z`, completed `2026-09-20T00:05:35Z`; trigger-to-final-required-result was approximately 363 seconds. This is a baseline, not a current-main success claim.

| Job | Job URL | Raw interval | Wall duration | Reported phases / observation |
| --- | --- | ---: | ---: | --- |
| Planning | <https://github.com/tailrocks/velnor/actions/runs/35477566106/job/105989385633> | 00:00:01–00:00:11 | 10 s | Selection and plan transport |
| Docker GitHub | <https://github.com/tailrocks/velnor/actions/runs/35477566106/job/105989414811> | 00:00:14–00:01:32 | 78 s | BuildKit seed fallback was empty; report total 71 s, checks 61 s |
| Rust production topology | <https://github.com/tailrocks/velnor/actions/runs/35477566106/job/105989414957> | 00:00:15–00:01:23 | 68 s | Product/topology check |
| Rust runner GitHub | <https://github.com/tailrocks/velnor/actions/runs/35477566106/job/105989415008> | 00:01:22–00:04:50 | 208 s | Exact outer caches, but 77 compiler lines and MBX bypass/not-looked-up actions |
| Rust workflow GitHub | <https://github.com/tailrocks/velnor/actions/runs/35477566106/job/105989415068> | 00:00:15–00:04:49 | 274 s | Slowest job; candidate build is mislabeled as cleanup |
| Required control | <https://github.com/tailrocks/velnor/actions/runs/35477566106/job/105990006269> | 00:05:32–00:05:35 | 3 s | Final required gate |

The slowest retained job was `rust-velnor-workflow`, 274 seconds wall time (report total 265 seconds). A literal 10x job target is 27.4 seconds. The full trigger-to-final-required baseline is about 363 seconds, giving a 36.3-second 10x target. The `rust-velnor-runner` baseline is 208 seconds, giving a 20.8-second target. These are engineering targets, not achieved results; all required checks and product behavior remain in scope.

### Rust workflow evidence

The workflow job restored a 6.4 GiB MBX state archive from a restore-key fallback (`VELNOR_CACHE_MBX_HIT=false`) and reported it as `mbx:cold`. Rustup, Cargo, and mold had exact cache hits. The report was:

```text
runner_setup_seconds=4, tool_bootstrap_seconds=80, cache_prep_seconds=6,
cargo_fetch_seconds=1, checks_wall_seconds=108, cleanup_seconds=65,
total_seconds=265
mbx=cold, rustup=exact, cargo=exact, mold=exact
mbx object cache: 1 hits, 2 misses, 1 bypassed; 366.8 MiB local
mbx object cache: 5 hits, 21 misses, 1 bypassed; 937.2 MiB local
mbx object cache: 6 hits, 20 misses, 1 bypassed; 8.5 MiB local
finished dev=20.96s, test=37.80s, test=18.04s; compiling_lines=7
```

The runtime then prepared and published a candidate generator product after `Run unit checks`; that build finished in roughly 56.69 seconds in the job log. The current report places candidate preparation/publication inside `cleanup_seconds=65`, so the telemetry does not identify the real critical path. `cleanup` must end before candidate build and a separate `candidate_build_seconds`/`candidate_publish_seconds` phase must cover those markers. A restore-key archive must be reported as `prefix_restore` (with bytes and compatibility state), not as a cold cache layer when bytes were imported; “outer archive hit” and compiler reuse are separate facts.

The first actionable failure is currently delayed by all setup and cache bootstrap. Format is the first command inside the opaque runtime step, but not the first visible Actions step. A formatting failure therefore waits for approximately 80 seconds of tool/bootstrap work in this sample. The test profile and Clippy order also need controlled comparison before changing: preserving `--profile test` is mandatory, and Clippy's test-profile artifacts are not automatically interchangeable with nextest products.

### Rust runner evidence

This job restored exact rustup, Cargo, mold, mise, and workspace state caches, but compiler reuse was poor:

```text
total_seconds=202, tool_bootstrap_seconds=22, checks_wall_seconds=173
mbx object cache: 3 hits, 0 misses, 1879 not looked up, 131 bypassed
finished test profile in 2m52s; compiling_lines=77
```

The progress lines show the `not looked up` and `bypassed` counts growing throughout the 173-second check. These statistics disprove “exact outer cache hit means no recompilation.” The cause still needs command/profile/action-level attribution: inspect MBX supported compiler actions, bypass reasons, path/environment fingerprints, native build scripts, proc macros, and whether Cargo target state was compatible. Do not hide `Compiling` or `Downloaded` lines.

### Docker evidence

The Docker job restored no mutable seed bytes: `Docker build seed empty: the build starts from cold cache mounts`. It ran:

```text
docker buildx build --load --target ci --file Dockerfile --tag local-ci:dockerfile .
  --build-context velnor-cache-seed=.velnor-docker-cache/seed
  --cache-from type=gha,scope=docker,mode=max
  --cache-to type=gha,scope=docker,mode=max
```

The nested build logged `Updating crates.io index`, `Updating git repository`, and `Downloading crates`; the report recorded one occurrence of each. BuildKit layer cache presence cannot prove mutable `RUN --mount=type=cache` persistence on an ephemeral hosted runner. The generator's seed transport must be measured as its own product with producer, consumer, key, bytes, import time, and fallback reason. PR #966 is a valid Docker selection because it changes copied generator source; use a docs-only or unrelated source fixture to test a negative selection.

## Relevant open PRs and conflict boundaries

- **#967**, head `69899af0db610a647a36bb3bc129a87c8d4195e0`, changes MBX to directory-form bundle/import staging, bumps MBX/action versions, and adds MBX version to the compatibility digest. It directly addresses large bundle/quota behavior. Any cache transport experiment must either include this head or explicitly measure the current implementation without duplicating its mechanism.
- **#966**, head `eaae46c48dd45479f1eb7a1314708a38c16d6cb5`, refuses legacy rolling package-release mutation. The cited Docker run used this PR; its changed generator source is copied by the Dockerfile, so that Docker job is not a false-positive relevance result.
- **#965**, head `9c198d07ea36747bbb4023d4eac072ba1bb6ee3e`, commits changed package consumers. **#964** (closed) added source-bound package verification. These affect release correctness and generated state, not the first Rust-stage experiment.
- **#962**, head `ea9686f0eb522e402441ecd461bd1f458b731d06`, routes ARM64 release producers; **#960**, head `c8f7a2b353f9d2d6a60d3ad8bb2dc6299a108ceb`, keeps release metadata out of checkout; **#952**, head `a5c1c0bd5c92c4c52d58ccb21042b1b2c0b08637`, restores Docker seed/fetch history. Treat these as release/product graph changes and inspect before integration; do not implement their mechanisms twice.
- **#963**, head `c440d4db3fd59a9e4abd396d7a75e670c4f3d862`, and **#961**, head `5b9a16a620951b65bbfe0a5cf7b1ffe04a317303`, contain generic scanner/audit changes with overlapping history. Compare actual diffs and current heads before modifying relevance or scanner behavior.
- **#948**, head `ab82b08746ec312558981f8461a32a1e3350d802`, is authoring-only bastion provisioning. User scope explicitly excludes turning it into live infrastructure.

## First implementation proposal and independent challenge

### Preferred design: typed generic validation stages

Add a versioned typed stage model to the generator IR/runtime, for example `ValidationStage { id, kind, command, profile, products, prerequisites, failure_policy }`, with separate affected/full vectors. Keep the command as a structured argv/profile declaration where possible; validate shell fragments strictly when a repository must provide one. Rust scanning should emit `fmt`, `clippy`, and `nextest` stages from generic detected policy, retaining exact package, target, features, lock mode, `--profile test`, warning policy, and test selection. Product/prerequisite edges must be explicit, not inferred from command text at runtime.

Render each stage as a named Actions step within the existing unit job after only the setup it actually requires. Put formatting before compiler/tool/bootstrap prerequisites when it can run with the checked-out source and the pinned formatter. Run Clippy before nextest only after a controlled comparison proves the command remains semantically complete and the `--profile test` product contract is preserved; otherwise keep nextest first but expose both stages. For local runtime execution, use the same stage graph and visible markers as Actions so local and generated behavior cannot diverge. The candidate generator build gets separate phase markers and a product contract, allowing an already validated compatible product to be consumed rather than rebuilt.

This removes the enabling condition (opaque command vector plus implicit prerequisite rewrite), supports non-Rust stages, and gives cache/product planning enough structure to avoid duplicate work. It must not add repository-specific unit IDs or shell/YAML escape hatches.

### Three alternatives to challenge independently

1. **Typed stage model (preferred, structural).** Add stage/profile/product fields to the shared IR and runtime, with generic scanners for Rust and existing declarative config for facts that cannot be inferred. Pros: correct visible boundaries, explicit prerequisites, reusable product contracts, consistent local/Actions execution. Risks: schema migration, generated fixture breadth, and proving command/profile identity. Reject if compatibility or output determinism cannot be established with fixtures and all three consumers.
2. **Command-stage adapter (smaller intervention).** Keep serialized command vectors but introduce a strict generic stage classifier in the Rust scanner/runtime, requiring an explicit stage label for ambiguous commands; render each classified command as a separate step while preserving the current order. Pros: small generated diff and quick feedback experiment. Risks: leaves products/prerequisites opaque, cannot safely plan stage-specific setup, and text classification can misclassify wrappers. This is useful as a diagnostic control, not a final architecture unless tests prove the missing product semantics are unnecessary.
3. **Separate fast-check and compile workflows (graph boundary).** Generate a minimal formatter/lint job and a dependent compile/test job, sharing only checked-out source and pinned tool setup/artifacts where compatible. Pros: fastest actionable failure and independent scheduling. Risks: new queue/bootstrap/transfer cost can worsen success-path latency, duplicate tool setup, and required-gate complexity; Clippy/nextest product sharing must be proven. Keep as an experiment, not a default, until end-to-end and aggregate-work measurements beat the in-job stage model.

Independent challenge required before implementation: verify that the current Clippy `--profile test` invocation really reuses the same artifacts as nextest for the target workspace; enumerate commands where `prerequisite_commands` rewriting changes behavior; challenge whether fmt requires any bootstrap currently installed; compare one-job visible stages against split jobs including queue and cache transfer; and inspect MBX bypass reasons before attributing the 208-second runner job to missing cache keys.

## Controlled first experiments

Each item is a substantive experiment candidate and needs its own ledger entry, raw run IDs/attempts, independent reviewer, and accepted/rejected/inconclusive status.

- **V-RUST-001:** Baseline versus typed visible stages with identical commands, toolchain, runner, source, and cache namespace. Inject a deliberate format failure in a fixture to measure time-to-actionable-result; verify no required command is omitted and exit codes propagate.
- **V-RUST-002:** Current nextest→Clippy order versus fmt→Clippy→nextest, preserving `--profile test`, all features/targets, and warning policy. Measure cold, repeated identical, source-only, lockfile, and toolchain-change scenarios. Capture Cargo fingerprints/timing and MBX hit/miss/not-looked-up/bypass output.
- **V-RUST-003:** Candidate generator product built once versus current post-check fresh worktree build. Validate source/configuration digest, exact runtime revision, artifact digest, trusted producer, and fallback build. Attribute candidate build time separately from cleanup.
- **V-CACHE-004:** Restore-key MBX bundle versus exact-key and directory-form transport (including PR #967 where applicable). Measure archive bytes, import/export time, quota failures, compiler reuse, and bypass reasons; do not treat a restore-key import as cold without recording compatibility.
- **V-DOCKER-005:** Cold mutable BuildKit dependency cache seed versus current empty fallback using a source-independent Docker change. Measure nested registry/git/crate downloads, BuildKit layer hits, seed transfer cost, and final CI target coverage. Include seed producer work in aggregate and end-to-end totals.
- **V-TELEMETRY-006:** Correct phase markers/reports for fallback caches, candidate preparation/publication, cleanup, and compiler evidence. Replay against historical logs and a controlled run; require report totals to reconcile with raw timestamps within documented marker overhead.

The implementation must be regenerated from one exact generator revision into Velnor, Jackin, and Parallax only after the stage schema has fixture coverage. The next independent reviewer should inspect this inventory, the current generated YAML, and the exact source at the historical run SHA before accepting any ordering or cache conclusion.
