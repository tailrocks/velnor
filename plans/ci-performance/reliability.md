# Velnor CI reliability execution record

## 2026-09-21 continuation: trusted admission regression

Fresh authenticated refs and Git fetch agree on main `97bac4c4` and PR #979
head `39b67ecd`. The isolated integration checkout is now
`/Users/donbeave/Projects/work/velnor`; unrelated dirty migration work is preserved.
The current cross-repository objective additionally requires every complete
acceptance pipeline to meet 120 seconds. No previous campaign result proves that
target or the six-nines reliability objective.

The PR's generator failure is locally reproduced: 1,830 library tests pass and
`trust_gated_velnor_docker_is_judged_by_the_trusted_class` fails. Its expected
diagnostic text encoded the superseded rule that denied selected work may skip.
The renderer now correctly rejects contradictory selection and admission.
The test architecture allowed that contract disagreement because it checked
strings rather than executing the trusted caller's verdict.

The replacement executes the actual generated trusted-provider verdict for 40
combinations of selection, admission, and job result. Selected work requires
both admission and success; unselected work requires an explicit skipped result.
The test retains assertions tying the actual caller to the trusted provider's
event expression. Independent reviewer `/root/independent_verifier` found no
actionable defect in the repair.

Local verification on the repaired tree:

- `cargo fmt --all --check`: passed.
- `cargo clippy --workspace --all-targets --locked --features velnor-runner/test-support -- -D warnings`:
  passed, 24.02 seconds reported by Cargo.
- `cargo test --locked -p velnor-workflow`: 1,959 tests passed, zero failed or
  ignored. This includes 1,831 library tests and 128 integration tests.
- Candidate `velnor-workflow --plain --dry-run .`: discovered 11 Rust workspace
  packages and 17 total execution units; zero generated files would change.

These are local correctness checks, not complete CI timing or immutable-runtime
acceptance. Fresh pushed CI, source/pin parity, hooks, explicit Rust phases,
consumer adoption, and protected integration remain required.

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
| Emitted final-gate fixture | Selected obligation plus false provider admission plus skipped job can pass. | Locally repaired: planner eligibility already removes ineligible providers before freezing obligations; final gate now rejects contradictory admission. Emitted-shell tests cover false, empty, and invalid admission. Dispatch input normalization and legacy gate semantics remain separate outstanding work. |
| [Main CI 35520255650](https://github.com/tailrocks/velnor/actions/runs/35520255650), Policy job `106102994568` | Policy waits 900 seconds for a PR candidate artifact at a merged main SHA, then fails. Candidate production is downstream of planning, while planning requires a published runtime available only after merge. | Current bootstrap cycle and event mismatch. Reuse an early, source-bound candidate product with equivalent PR/main acquisition and no publication credentials. |
| [Preview 35515840346](https://github.com/tailrocks/velnor/actions/runs/35515840346), amd64 job `106093233951` | Build identity rejects one dirty path after metadata was downloaded into the checkout. | Metadata moved to `runner.temp/release-metadata` by `57e7cafc`. Successor execution remains blocked by earlier policy failure; not yet verified repaired. |
| Same Preview, arm64 job `106093233952` | `aarch64-linux-gnu-gcc` missing on the x64 hosted runner. Current matrix still lacks native ARM routing or a provisioned cross compiler. | Current deterministic package-only escape; repair generic target/provider routing and exercise package preflight before merge. |
| [PR run 35520025700](https://github.com/tailrocks/velnor/actions/runs/35520025700), job `106102422435` | The tested synthetic merge `b9f7e19f` deleted worker state before the unchanged diagnostic-export assertion. | Superseded implementation: final selective integration removed the deletion path. Source comparison and the exact test's PASS in run `35522028476`, job `106107688847`, prove the disposition. This is not evidence of a current flaky test. |

Both linked logs were retrieved with authenticated `gh api`; terminal escape
filtering required `--allow-escape-sequences`. Raw evidence is temporarily in
`/tmp/velnor-failures`; durable inventory artifacts will be incorporated after
collection. The durable [inventory snapshot](observations/reliability-20260920/inventory-summary.md)
contains source/log evidence, complete run and attempt catalogs, and explicit
inspection gaps. Its 28 compressed/Markdown evidence files total 2,296,056 bytes;
`snapshot-manifest.json` records SHA-256 hashes. The catalog contains 7,594
retained runs across 76 pages, from
2026-06-04T22:42:50Z through 2026-09-20T16:23:53Z. All 268 earlier attempts on
182 rerun runs were collected; 30 earlier failed attempts are hidden by a later
successful conclusion. Detailed job/log coverage and dispositions remain
incomplete, so catalog counts are not a completed failure audit.

The snapshot covers 559 run job listings and 17,961 unique jobs. Older retained
non-green runs are being collected separately. Main-push workflow runs in this
historical window passed on the first attempt in 766/1,565 terminal cases
(48.945687%); latest conclusions passed in 770/1,565, with 21 runs retried.
Cancellations and historical workflow names are included. This denominator
counts workflow runs, not combined pipelines per commit, and cannot establish
the 99.9999% objective.

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
Neither manager's default isolation is accepted. Implementation and 31 independent regression fixtures pass; delivery and actual full-workspace hook validation remain pending. No manager is installed in the integration checkout yet.
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
are retained. Strict full-workspace Clippy passed after correcting a newly
introduced documentation lint. The refreshed replay rejects selected-but-not-admitted skips. The original
accepting result remains preserved in the earlier commit history.

Commits `6a208e5` and `ec1bccf2` are pushed in
[draft PR #979](https://github.com/tailrocks/velnor/pull/979). The second commit
refreshes the scan identity after adding tracked evidence files. Rebuilding
the committed source and regenerating produced zero remaining file changes.
The first PR/policy runs (`35523612698`, `35523612583`) were superseded by the
second push and cancelled; retain those outcomes. Current runs are
`35523722608` and `35523720489`; both completed successfully at `ec1bccf2`.
The PR must
not merge until bootstrap/source-pin parity and prospective-main validation
are repaired and verified.

Independent hook replay in the integration checkout passed all 31 fixtures and
118 assertions in 69.63 seconds. This is regression-suite duration, not the
latency of validating Velnor's full workspace. The implementation is queued for
its own increment after the bootstrap repair; actual workspace measurements
follow below.

Actual full-workspace hook validation subsequently passed in a disposable clone
of `39b67ecd` plus the ten hook files. `mise trust`, pinned `mise install`, and
`mise run bootstrap` installed the hook; a normal signed-off Git commit invoked
it successfully and left the clone clean. Empty isolated Cargo caches took
198.0 seconds (one sample; pinned tools already installed). Three warm runs
passed in 8.765, 8.531, and 8.528 seconds with HTTP(S)/ALL proxies pointed at a
closed loopback port. This is prepared offline-transport evidence, not an OS
network sandbox or an optimization comparison. An earlier 99.7-second cold
trial overlapped another compilation and is excluded from controlled comparison.
Team builds were paused for the second cold/warm trials; host-wide exclusivity
was not continuously established. Linux CI and package-specific feature-policy
correspondence remain outstanding.

Additional observed work: adding evidence paths changed only the generated
ownership state's `scan` digest, forcing a correction push and superseding the
first PR runs. `s2/scan/mod.rs` serializes every tracked pathname into
`RepositoryShape::canonical_json`, although generated workflow bytes were
unchanged. Investigate narrowing identity to behavior-relevant scan evidence,
with new-manifest/discovery regressions, before changing this contract.

The admission tightening (`0b74f799`) passed 20 focused generator tests, all 26
actual generated-shell scenarios, and strict full-workspace Clippy. A second
generation was byte-identical across all 21 `.github` files. [Independent review](reviews/reliability-gate-admission.md) confirms no valid
selected-but-unadmitted obligation. It also found dispatch CSV normalization
drift: the planner trims whitespace but workflow admission matches raw inputs;
explicit empty input also differs. Strict verdicts now expose this contradiction
instead of passing skipped validation. Normalize selection consistently next.
Legacy schema gate semantics require a separate applicability audit.

The next hosted run, `35525244762` at `39b67ecd`, exposed one older test
(`trust_gated_velnor_docker_is_judged_by_the_trusted_class`) that still asserted
the removed selected-skip behavior. Its expectation now matches the strict
contract; the emitted-shell regression also covers every non-prerequisite
provider caller. Independent review passed. The full generator suite then
passed all 1,959 tests (42.03 seconds), and strict crate Clippy passed. This
failure remains part of first-attempt evidence; it was not retried away.

A retained artifact-upload failure exposed test HTTP servers that closed before
draining request bodies. A shared framed reader repairs 13 mocks; actual runner
protocol tests pass (131 passed, 2,396 filtered), and deleting the body read makes
two standalone regressions fail. Strict workspace Clippy passes. Reviewed patch
is preserved for a separate increment at
`/tmp/velnor-integration/protocol-fixture-reviewed.patch`; no production code changes.

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

## Stable package channel identity

The package-record emitter compared two distinct vocabularies as raw strings:
release binaries embed `release`, while their package records declare `stable`.
That rejected every otherwise valid stable record. A private typed conversion
now maps release builds to stable packages and preview builds to previews at
the emitter boundary. Source, version, binary digest, development-build and
cross-channel rejection remain enforced. This is source-proven; no retained
historical failure is attributed to it without a matching log.

Independent source review passed. `cargo fmt --all --check`, strict
`cargo clippy -p velnor-runner --all-targets --locked --features test-support -- -D warnings`,
and `cargo test -p velnor-runner --locked --features test-support release::tests::emit_package_record`
passed (five tests). Regression cases cover both package architectures, both
valid channels, every cross-channel pairing, and invalid embedded identities.
These checks establish the emitter repair; native hosted packaging and required
pre-merge packaging remain separate outstanding verification.

## Immediate commit and push checkpoint

At the user’s request, completed source units were committed separately and
pushed on the existing integration branch: package channel mapping `0348729f`,
exact renderer bootstrap `e1c589eb`, protocol request framing `c405d37`, and
isolated staged hooks `4d3e55d9`. Parent bootstrap integration verification
passed formatting, strict generator Clippy and 1,966 tests; the generated-file
comparison awaits the explicit source-pin/regeneration commit.

Unfinished typed-stage and package-preflight patches, their bases, status,
review evidence, cache-collision diagnosis and the expanded inventory are
preserved in [the checkpoint](observations/checkpoint-20260920/README.md).
These patch artifacts are recoverable work in progress, not accepted live
implementation. Queue implementation had no source edits at this checkpoint.
The retained target-run job census covers 2,038 of 3,699 runs, with 56,441
unique jobs and 3,944 logs; 1,661 target runs remain uncollected. Explicit
404/410 gaps remain unavailable evidence, not classified root causes.

The draft remains unmerged. Source pin/generation, hosted hook and package
proof, typed CI ordering, feature correspondence, merge queue, cache workload
identity, and full failure disposition remain active work.

## Exact renderer activation after the source checkpoint

The source checkpoint is pushed through `5293011a`. The active checkout now
has the pinned prek hook installed by `mise run bootstrap`; installation
completed successfully and uses the isolated index launcher.

A detached checkout of `e1c589eb5012a705aa735cb553a5230ff9fec490` built with
`cargo build --locked -p velnor-workflow --no-default-features --features tui`.
Its reported revision is that exact commit, its feature set is `tui`, and its
full debug source closure is
`fa59414c9f36feeccf4df27e41dc23a2ce5d0608c7310d72abb3321970cedadd`.
The current branch reports the identical closure. The repository pin now
names this source commit, and the exact binary scanned the repository before
regenerating the owned workflow surface. A subsequent `--plain --check`
passed without changes.

The attestation-verified published validator from the trusted base revision
`38dbf85e0bbf278cee3ec39ab90a9acc9b5b67a9` accepted all 11 policy rules against
the generated candidate, using the exact renderer above and the live ruleset
contexts `ci-required,DCO,Policy`. Ruleset `19573071` still requires strict
up-to-date validation, GitHub Actions App `15368` for `ci-required` and
`Policy`, and DCO App `974774`. The first local policy invocation omitted
`Policy` from its supplied context list and correctly failed; the corrected
invocation uses the independently re-read server configuration.

Actionlint passed with installed ShellCheck `0.11.0` selected explicitly.
The ordinary local invocation encountered an unrelated unconfigured
ShellCheck mise shim; no lint check was disabled. The regenerated full generator suite passed all 1,967 tests with no
skips in 57.941 seconds (`cargo nextest run --locked -p velnor-workflow`).
Hosted execution remains a separate verification obligation.
