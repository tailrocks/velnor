# G0 inventory

Observed 2026-09-19 UTC from the Luna/max worker. `rtk` version: 0.49.0.

## Execution settings

The local thread ledger `/Users/donbeave/.codex-chainargos/state_5.sqlite` records worker thread `01a0ba72-3925-7141-b1f7-5529a5cf6c98` as model `gpt-5.6-luna`, reasoning effort `max`. The orchestrator thread `01a0ba6f-f806-7d31-9abb-c828b3dc9e4e` is `gpt-6-astra`, `low`, as authorized by the user.

## Input revision

Repository `tailrocks/velnor` default branch is `main`. Local `main`, `origin/main`, and live GitHub `main` all resolve to:

`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

The source checkout had only the untracked specification file `velnor-github-first-dual-lane-goal.md`.

## Existing PRs

| PR | state | head | base | result |
| --- | --- | --- | --- | --- |
| [952](https://github.com/tailrocks/velnor/pull/952) | open / blocked | `a5c1c0bd5c92c4c52d58ccb21042b1b2c0b08637` | `3123f8ae63a1fe98fe1d4dc86fb25b212defcd59` | Seed and generator-pin release-leg repair. Velnor jobs failed closed at operational-store admission; aggregate required check failed. |
| [953](https://github.com/tailrocks/velnor/pull/953) | open / blocked | `af31b644aa01eb352ba6240fca957e900174d067` | `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` | Typed schema-2 desktop release/MBX work. Policy failed generated-tree/candidate acquisition; CI run had no jobs and remained pending. |
| [954](https://github.com/tailrocks/velnor/pull/954) | open / unstable | `856a222bd551f71de49468d70952e91a724060bc` | stacked branch at `8964876f6b6b6eca624b2fb26f7eff1d5dc5d575` | Typed six-payload package-release contract. Same generated-tree/candidate acquisition failure; CI run had no jobs and remained pending. |

## Failure evidence

- [Main run 35430875046](https://github.com/tailrocks/velnor/actions/runs/35430875046): generator policy reported scan-input drift (`bab3e77b3257adc8` to `d503cfb3b27c320e`) and candidate product timeout; all workload jobs were skipped and required aggregation failed.
- [Preview run 35338351916](https://github.com/tailrocks/velnor/actions/runs/35338351916): `generated-tree` failed because the tree differed from the render at pinned generator `fdeed261bd2247a38db6922a7726cd45d3d6f31e`.
- [Release run 35332416794](https://github.com/tailrocks/velnor/actions/runs/35332416794): aarch64 guest build failed; hosted Docker lacked `.velnor-docker-cache/seed`; shallow checkout could not resolve `fdeed261...`; Velnor jobs were rejected by the operational store.
- [PR 952 CI run 35338047632](https://github.com/tailrocks/velnor/actions/runs/35338047632): Velnor-lane jobs failed closed and `ci-required` reported an expected Velnor job failure.
- [PR 953 policy run 35453599647](https://github.com/tailrocks/velnor/actions/runs/35453599647): candidate `velnor-workflow-candidate-128ec58bb341e639-Linux-X64` was not published within 15 minutes.
- [PR 954 policy run 35453337652](https://github.com/tailrocks/velnor/actions/runs/35453337652): candidate `velnor-workflow-candidate-3dc51026416c2a1d-Linux-X64` was not published within 15 minutes.

## Reusable work and next tasks

PR 952 source changes are reusable after integration: `crates/velnor-workflow/src/s2/primitives/{ir.rs,mod.rs,release.rs}`. Its generated state and release workflow must be regenerated from the integrated generator; generated files are not copied manually.

The current configuration still has `automatic_providers = ["github-hosted", "velnor"]` and `default_dispatch_providers = ["github-hosted", "velnor"]`, so G1 requires a staged hosted-only configuration and a clean regeneration. Bootstrap must be made acyclic: a policy check cannot require a candidate artifact whose producing CI is blocked by that same policy check. The staged pin must also work in shallow checkout and record current scan inputs without self-drift. PRs 953/954 require rebase/reintegration only after this baseline is fixed.
