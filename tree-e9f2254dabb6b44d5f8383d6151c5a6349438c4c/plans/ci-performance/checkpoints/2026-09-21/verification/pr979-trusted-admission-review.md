# PR 979 trusted-admission verdict review

## Result

The focused test repair is correct. The old failure was a stale test expectation, not a generator runtime defect: `s2::tests::trust_gated_velnor_docker_is_judged_by_the_trusted_class` required the emitted selected-caller branch to accept `skipped` when `PROVIDER_ADMITTED_VELNOR_TRUSTED` was false. PR head `39b67ecd9ecc47462a32b337c056f8eabe28080e` deliberately changed that branch to reject a selected obligation that contradicts provider admission.

The updated test executes the extracted generated verdict block for 40 combinations: selected/unselected, trusted admission `true`/`false`/empty/unknown, and result `success`/`skipped`/`failure`/`cancelled`/empty. It accepts only selected + admitted + success, or unselected + skipped. It retains assertions that this caller reads `PROVIDER_ADMITTED_VELNOR_TRUSTED` and that the gate evaluates the same expression as the Velnor trusted caller. This gives behavioral coverage for the actual trust class and would fail under the old selected-skip behavior.

The fixture stubs only `plan_expects` and `result_for_job`; it exercises the generated per-caller shell branch, while the adjacent generator tests cover full gate rendering, plan/result parsing, and dependency graph. The matrix does not claim to exercise GitHub expression evaluation or live Actions scheduling.

## Failure evidence and root cause

- Local baseline evidence: [pr979-trust-gate-baseline.log](/Users/donbeave/Projects/work/ci-evidence/verification/pr979-trust-gate-baseline.log) shows the old test failing at `crates/velnor-workflow/src/s2/mod.rs:9916` because generated output contained `selected CI job velnor-docker contradicts provider admission ...` instead of the asserted `skipped) ;;` branch. The parent also reported the full locked library run as 1,830 passed and this one failed.
- Parent's focused post-fix run: [pr979-trust-gate-fixed.log](/Users/donbeave/Projects/work/ci-evidence/verification/pr979-trust-gate-fixed.log) shows 1 passed, 0 failed, 1,830 filtered out, in 0.23 seconds.
- Live Actions run [35525244762, job 106116179376](https://github.com/tailrocks/velnor/actions/runs/35525244762/job/106116179376) failed in `Run unit checks`; the visible annotation reports exit code 1. GitHub's unauthenticated page does not expose the test output. Authenticated run/job/log and annotation API requests returned HTTP 403 rate-limit responses despite `/rate_limit` reporting core quota available. The captured page is [run35525244762-job106116179376.md](/Users/donbeave/Projects/work/ci-evidence/verification/run35525244762-job106116179376.md).
- The same Actions run reports `ci-required` failed in 4 seconds. Detailed gate output was not available from the public page, so this review does not attribute that failure beyond the visible status. It may reflect the failing dependency; a fresh fully green run must validate the live gate.

Architecture allowed this test-only regression because the trusted-provider test asserted implementation text for the old admission policy. It had no executable behavior check for the selected + unadmitted case in this provider class. The repair removes that blind spot by executing the generated branch and asserting the result contract.

## Scope and verification limits

This was a read-only review at PR head `39b67ecd9ecc47462a32b337c056f8eabe28080e`, base `97bac4c4582bbe18ee607a1dd7a41b4854345c7e`. The only checkout change observed during review was the integration owner's edit to `crates/velnor-workflow/src/s2/mod.rs`; no source implementation was changed by this reviewer.

I did not execute tests or build artifacts. The focused post-fix test and Clippy results above were run by the parent and are identified as such. The full generator suite was still running at the time of review, so this report does not mark it passed. Separate review of live Actions gate behavior remains pending a run with successful unit checks and accessible gate output.

The live page was retrieved with the `firecrawl-scrape` workflow ([SKILL.md](/Users/donbeave/.agents/skills/firecrawl-scrape/SKILL.md)); it exposes job status and annotations, but not the raw logs without an authenticated browser session.
