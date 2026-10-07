# Velnor CI reliability execution record

Status: active. Started 2026-09-20. This record covers the Velnor reliability
task; it does not replace or inherit the separate cross-repository performance
campaign's completion contract. The objective is structural prevention of
PR-green/main-red failures, with measured progress toward 99.9999% green main
pipelines. No observed sample currently establishes that reliability rate.

## Baseline and ownership

- Fresh checkout: `97bac4c4582bbe18ee607a1dd7a41b4854345c7e`, initially clean.
- Integration branch: `fix/ci-validation-contract`; parent owns the index,
  branches, commits, generated outputs, pushes, protection changes, and merges.
- Agent capacity: parent plus three children. Independent review is scheduled
  in bounded waves; no concurrent shared builds or hook stashing are permitted.
- `/root/failure_inventory`: retained run/attempt/job inventory and reproduction.
- `/root/integration_architecture`: PR/main parity, generator gates, and merge
  protection; owns generator source/tests during the first increment.
- `/root/hooks_performance`: hk/prek isolation experiments and timing analysis;
  independently reviews the first generator increment before integration.

## Failure-to-prevention ledger

| Evidence | Finding and enabling condition | Disposition / required proof |
| --- | --- | --- |
| [Main Preview 35520255573, job 106102991421](https://github.com/tailrocks/velnor/actions/runs/35520255573/job/106102991421), attempt 1, source `845d4740451541148cc2b00da87f41fe898976eb` | `Enforce workflow policy` fails `generated-tree`; checked-in workflows differ from the renderer pinned to `38dbf85e`. Current policy explicitly accepts candidate generation on PRs but rejects it on main. | Current structural defect. Establish one integration-safe source/pin contract and pre-merge proof; verify Preview after integration. |
| [PR 977 run 35520875130, job 106104648299](https://github.com/tailrocks/velnor/actions/runs/35520875130/job/106104648299), attempt 1, source `b3f9f5d74f2c31134c2f6691243fd26cfa9fbf01` | Clippy in `rust-velnorctl` rejects unused `velnor-runner::GuardError::Contended` under `-D warnings`. Run was ultimately cancelled, but this individual job failed. No formatting failure in this job. | Variant removed by `7308307b` on current main. Current full-workspace Clippy passes. Local early prevention and final CI verification remain required. |
| Live ruleset `19573071` | Required checks allowed a stale base and had no expected App binding. | Repaired settings: strict up-to-date checks; `ci-required` and `Policy` bound to GitHub Actions App `15368`, DCO to App `974774`. API PUT and subsequent GET verified. Existing checks and other rules preserved. Merge queue remains under investigation. |
| Current generated PR `ci-required` | `!cancelled()` can skip the required gate after cancellation. GitHub treats skipped required checks as successful. | Local generator repair uses unconditional `always()` and removes cancellation from the rendering API. Legacy self-hosted modes use a hosted verdict-only job. Independent review, 26 emitted-shell scenarios, graph mutations, and the full 1,958-test generator suite pass. PR/main integration remains pending. |
| Emitted final-gate fixture | Selected obligation plus false provider admission plus skipped job can pass. | Current defect; reconcile planner selection, admission, and explicit nonapplicability before claiming fail-closed aggregation. |
| [Main CI 35520255650](https://github.com/tailrocks/velnor/actions/runs/35520255650), Policy job `106102994568` | Policy waits 900 seconds for a PR candidate artifact at a merged main SHA, then fails. Candidate production is downstream of planning, while planning requires a published runtime available only after merge. | Current bootstrap cycle and event mismatch. Reuse an early, source-bound candidate product with equivalent PR/main acquisition and no publication credentials. |
| [Preview 35515840346](https://github.com/tailrocks/velnor/actions/runs/35515840346), amd64 job `106093233951` | Build identity rejects one dirty path after metadata was downloaded into the checkout. | Metadata moved to `runner.temp/release-metadata` by `57e7cafc`. Successor execution remains blocked by earlier policy failure; not yet verified repaired. |
| Same Preview, arm64 job `106093233952` | `aarch64-linux-gnu-gcc` missing on the x64 hosted runner. Current matrix still lacks native ARM routing or a provisioned cross compiler. | Current deterministic package-only escape; repair generic target/provider routing and exercise package preflight before merge. |
| [PR run 35520025700](https://github.com/tailrocks/velnor/actions/runs/35520025700), job `106102422435` | `scaleset_daemon::crash_with_dead_workers_fails_explicitly` fails the diagnostic-export assertion. | Current assertion remains; a later green run is not a repair. Linux reproduction and causal diagnosis pending. |

Both linked logs were retrieved with authenticated `gh api`; terminal escape
filtering required `--allow-escape-sequences`. Raw evidence is temporarily in
`/tmp/velnor-failures`; durable inventory artifacts will be incorporated after
collection. The catalog contains 7,594 retained runs across 76 pages, from
2026-06-04T22:42:50Z through 2026-09-20T16:23:53Z. All 268 earlier attempts on
182 rerun runs were collected; 30 earlier failed attempts are hidden by a later
successful conclusion. Detailed job/log coverage and dispositions remain
incomplete, so catalog counts are not a completed failure audit.

## Hook decision evidence

Isolated experiments tested hk 2.0.1 and prek 0.5.3. Neither default execution
model satisfies the staged-snapshot contract:

- hk with explicit `stash = "git"` accepted bad staged contents when an
  unstaged fix equalled HEAD. It reported no unstaged files to stash.
- An unstaged hk configuration could suppress the staged failure. Ignored
  dependencies remained visible during checks.
- prek rejected the tested unstaged configuration/helper changes, but both
  untracked and ignored dependencies could make invalid staged contents pass.
- Fixture state was preserved in the tested cases.

Selected direction is pinned prek invoked inside an isolated index snapshot,
before loading validation configuration. hk 2.0.1 and inspected earlier releases
do not publish Darwin x86_64 binaries, while the repository's existing locked
tool platforms include macos-x64 and its Cargo tool policy requires prebuilts.
prek publishes native Darwin x86_64 and arm64 binaries. This concrete platform
gap makes prek the supported choice despite the user's preference for hk.
Neither manager's default isolation is accepted. Implementation and independent
repository-level regression proof remain pending; no manager is installed in
the integration checkout yet.
Warm no-op fixture observations (three each) were approximately 62 ms for hk
and 155 ms for prek. These are not Rust validation latency measurements and
do not establish a repository performance improvement.

Primary references: [hk hooks](https://hk.jdx.dev/hooks),
[hk releases](https://github.com/jdx/hk/releases/tag/v2.0.1),
[prek execution](https://prek.j178.dev/running-hooks/), and
[prek 0.5.3](https://github.com/j178/prek/releases/tag/v0.5.3).

## Verification and remaining work

Verified on baseline:

```sh
rtk cargo fmt --all --check
rtk cargo clippy --workspace --all-targets --locked --features velnor-runner/test-support -- -D warnings
```

First required-gate increment:

```sh
rtk cargo test --locked -p velnor-workflow required_
rtk cargo test --locked -p velnor-workflow
rtk cargo fmt --all --check
rtk mise exec actionlint@1.7.12 shellcheck@0.11.0 -- actionlint
target/debug/velnor-workflow --plain --force .
```

Results: 19 focused tests and 1,958 full generator tests passed. Actionlint
passed with explicit ShellCheck provisioning; the initial invocation exposed
a locally unresolved ShellCheck shim. Two generator passes produced identical
hashes for all 21 files under `.github`. This proves candidate rendering
stability, not exact pinned-renderer acceptance: `--check` still cannot acquire
the locally absent renderer at `38dbf85e`, and the source/pin bootstrap defect
remains open. [Independent review](reviews/reliability-required-gate.md) and
[26 emitted-shell outcomes](observations/reliability-required-gate-replay.json)
are retained. The replay deliberately records the remaining selected-but-not-
admitted skip behavior; it does not certify that behavior as correct.

The installed workflow binary at `a83de766` cannot parse current `trust`
configuration. A read-only dry-run using an existing local binary at
`bbee705d` discovered 11 workspace packages, 13 Rust execution units, four
other units, and 15 dependency edges; all generated outputs matched. The
candidate generator is being built and will be used for final generation
and verification; the older binary is not accepted as candidate proof.

Outstanding: complete failure inventory, source/pin parity, required-gate
admission, integration-event support and enforcement, isolated pinned hooks
and bootstrap, visible formatting/Clippy/test ordering, semantic mutations,
release preflight parity, supported feature/provider coverage, matched cold
and warm measurements, review feedback, small protected PR merges, and
resulting main/preview/release-path verification. No PR is merged for this
task yet. First-attempt/retry reliability denominators remain pending.
