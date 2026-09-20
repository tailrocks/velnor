# MBX same-repository PR writer review

Patch: `/private/tmp/velnor-mbx-pr-writer.patch`
SHA-256: `55eb5c61c23e81e74f456d69dcdcfde1301ce8e403e2c7eb1c6fc3bffc497985`
Scratch base: parent integration tree containing `845d4740451541148cc2b00da87f41fe898976eb`, telemetry repairs, and docs; patch applied cleanly.

## Verdict: HOLD

The writer contract is structurally sound after one lint repair, but the current generated workflow fails the repository's pinned actionlint/shellcheck gate. Save reporting also needs a precise attempted-vs-persisted contract.

## Executed checks

- `cargo fmt --all -- --check`: pass.
- Focused legacy/S2 writer tests: 2/2 pass (`same_repository_pr_mbx_writer`).
- `cargo clippy --locked -p velnor-workflow --all-targets --all-features -- -D warnings`: pass.
- `cargo build --locked -p velnor-workflow --bin velnor-workflow`: pass.
- Real generator run against the isolated Velnor tree: 21 files generated; hosted Rust workflow contains the writer and Velnor/local jobs do not.
- Pinned `actionlint 1.7.12` with `shellcheck 0.11.0` fails generated `ci-unit-rust.yml:719` with `SC2129`: the export script writes three separate lines to `$GITHUB_OUTPUT`. Group them in one brace block (`{ echo ...; echo ...; echo ...; } >> "$GITHUB_OUTPUT"`) in both legacy and S2 renderers. A generated scratch workflow with that mechanical grouping passes actionlint without ignores.

## Contract checks that passed

- Hosted setup is `github-cache-mode: objects`, Mr. Boxington `1.12.0`; the pinned action source selects directory bundles for `>=1.12.0`, and the writer uses the same `github-actions-cache-v1` directory.
- The explicit generator key is a v3 snapshot key with provider/platform/trust (S2), compatibility, dependency `hashFiles`, and freshness `hashFiles`; it is passed through `cache-primary-key` unchanged. `cache-hit == 'false'` covers misses and prefix restores; exact hits skip the writer.
- Export/save gate requires `success()`, `pull_request`, same head repository as base, nonempty head SHA, nonempty primary key, and export status `exported`. Fork PRs, dispatch, push, Velnor/local jobs, and MBX opt-outs do not get a writer. No build/test command is duplicated.
- Export uses the action-provided per-run/attempt `MBX_CACHE_EXPORT_GROUP`; MBX's directory export publishes atomically and rejects an empty group. Save uses the pinned `actions/cache/save` with the exact primary key, so concurrent immutable-key races fail without failing checks (`continue-on-error`) and are visible as a failed save step.
- The ordinary Mr. Boxington post action is restore-only on PR events; it does not duplicate this explicit export/save. Push/default and dispatch keep their existing action post-save path because the writer gate excludes them.

## Remaining observability finding

`actions/cache/save` has no success receipt. GitHub's current cache docs state that read-only cache operations are skipped while the action step and job continue; a save step can therefore have `steps.mbx_pr_save.outcome == 'success'` without a cache being persisted. The current summary prints `save=success` and only warns on `failure`, so that field means step outcome, not durable cache creation. Likewise `export=exported` proves only local MBX directory creation, not remote cache upload. At minimum label these as `save_step`/`export_local`; if the campaign needs durable-write truth, add an explicit `ACTIONS_CACHE_MODE`/permission classification and/or a post-run cache API observation. Do not call success a persisted-cache proof.

Primary references: GitHub workflow cache-mode docs (read mode skips save while continuing): <https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching#defaults>; actions/cache read-only behavior: <https://github.com/actions/cache#read-only-access>.

## Additional boundary checks

- The writer's `MBX_PR_CACHE_MODE: objects` is a step-local self-check; it does not configure Mr. Boxington. The pinned renderer currently emits `github-cache-mode: objects`, so the values agree today, but the test only asserts a string and does not prove that coupling. A future `target` renderer would leave the writer with no export group and silently skip. Keep one mode constant/typed argument, emit the writer only for object mode, or add a fixture that mutates the setup input and proves fail-closed behavior.
- The generated PR caller declares `permissions: actions: read`. This is not by itself evidence that the writer cannot save: GitHub's cache access mode is separately trigger-scoped, and `pull_request` caches are not subject to the low-trust default-branch restriction. Same-repository PR writes remain bounded to the merge-ref cache scope; fork PRs are blocked by the repository/head gate. Do not broaden the workflow to `actions: write` without a trust reason. Source: <https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching> and <https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#permissions>.
- `mbx cache export --group` consumes successful group receipts after publishing the local bundle. If `actions/cache/save` then fails, the job keeps checks green but cannot retry the MBX export in its post hook because the PR action is restore-only. This is acceptable only as explicitly best-effort optimization; report the loss as `exported_local/save_step_failed`, never as a durable cache result.
- The writer has no `CACHE_*_SAVE_STARTED/ENDED` markers and the existing phase report only receives `cache_mbx_hit` and `cache_mbx_primary`. Its export/save time and local-vs-remote outcome are therefore absent from telemetry. Add a dedicated bounded phase or explicitly document it as unobserved; do not attribute the existing cache-save timing to this writer.
