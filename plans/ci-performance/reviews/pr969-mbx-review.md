# PR969 per-member MBX review

Scope: exact source diff from `origin/main` `325719f1e05d3d46322c9fd3eeb9ad545e175638` to PR969 head `2445c647dafa43acbc5285c873d3b04174ff75c6`. Read-only source review; no source or Git mutation.

## Verdict

**PASS for the PR969 per-member transport change, with acceptance constraints. No throughput claim.** The legacy and S2 engines carry the selected member's typed `mbx_enabled` value across the reusable-workflow boundary. Mixed Rust jobs gate the MBX block on `inputs.mbx_enabled` and the hosted sccache block plus all three sccache environment assignments on `inputs.mbx_enabled == false`. Unanimous MBX jobs render only MBX; unanimous opt-out jobs render only hosted sccache. The exact diff passes `git diff --check`.

The review does not prove an MBX or sccache speedup. It proves branch selection and setup exclusivity from generated source.

## Source facts checked

- Both engines add the typed boolean to their input namespace, facts struct, caller `input_values()`, and reusable declaration. False is omitted, so the reusable default is false.
- Both engines derive the flag from `tools_for_unit`, which is `MrBoxington` only when global `mr_boxington` is true and `Unit::uses_mbx()` is true. `Unit::uses_mbx()` defaults Rust to true and honors `mbx = false`.
- Both engines remove `MrBoxington` and `Sccache` from the kind-level tool union. Their setup blocks are selected from per-member facts instead of being emitted once for the whole collapsed job.
- `Github`/`GithubHosted` receives MBX snapshot facts and hosted object-cache setup. `Velnor` and S2 local providers receive the local MBX action without hosted snapshot facts. S2 treats `github-self-hosted` as local too, so it receives no hosted sccache action or GitHub MBX cache.
- The current Velnor image explicitly has no `sccache` binary (`docker/job-ubuntu.Dockerfile` checks `! command -v sccache`); local providers therefore correctly run an opted-out member with plain Cargo and no sccache wrapper setup. This is a capability boundary, not a measured cache result.
- The generated Velnor PR/main caller diff adds `mbx_enabled: true` to all current Rust callers, matching the current config where every Rust unit uses the default MBX path. Non-Rust reusable declarations receive the default-false input only; no non-Rust setup or installation is introduced.

## Tests present in PR969

Both engine test modules cover mixed MBX plus `mbx = false`, all-MBX, and all-disabled Rust members. They assert typed input values, hosted-only snapshot facts, gated setup text, and absence/presence of sccache. S2 also checks generated Rust reusable declarations and every generated Rust provider caller.

The present tests set `ir.mr_boxington = true` for the three transport fixtures. The repository integration fixture `mbx = false` checks plain Cargo and no MBX setup, but does not assert the local-provider environment. Add the following acceptance cases before calling the provider contract complete:

1. Run the mixed fixture through both engines and every provider. Assert exactly one transport branch is executable for each caller: hosted MBX or hosted sccache; local MBX or plain Cargo. Assert no local `Set up sccache`, `RUSTC_WRAPPER`, or `SCCACHE_GHA_ENABLED` is emitted.
2. Run an all-opted-out fixture with `mr_boxington = false`/`mbx = false`. Assert no MBX action, MBX cache inputs, or MBX command rewrite. Hosted gets one sccache setup and the three environment assignments; local gets plain Cargo with no sccache action or wrapper.
3. Round-trip generated callers against facts: a true caller must carry compatibility/dependency/freshness inputs; a false caller must omit all MBX inputs and rely on the typed default. Run pinned actionlint on the generated YAML.
4. Keep an all-MBX fixture asserting no sccache setup or environment in either engine, including the aggregate path used by the consumer.

## Separate legacy caveat

The old `WorkflowIr::render` aggregate path still emits workflow-level sccache environment whenever `self.tools` contains `Sccache`, regardless of runner lane. If that legacy aggregate is used with `mr_boxington = false` and a Velnor provider, it can export `RUSTC_WRAPPER=sccache` to a runner image that explicitly lacks sccache. PR969 does not touch this code, and the current generated nested aggregate path does not emit that workflow-level block. Treat it as a separate pre-existing provider-capability defect: either prove the legacy path is unreachable for emitted consumer workflows or fix it before enabling global no-MBX Velnor jobs. Do not attribute it to PR969 or hide it with an installation.

## Acceptance boundary

Accept PR969 after the focused generated-output tests above and provider capability check pass. Verify exact action/runtime pins and generated state together. Keep timing work separate; setup exclusivity alone is not evidence of duplicate-work removal or wall-clock improvement.
