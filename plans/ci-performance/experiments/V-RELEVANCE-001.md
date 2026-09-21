# V-RELEVANCE-001: isolate documentation and unknown evidence inputs

Status: diagnosis observed; independent review pending. Not counted yet.

Hypothesis: an unrelated documentation change should select documentation
coverage, excluding Rust and Docker execution. The first real campaign run
appeared to contradict this, selecting all 17 units. Isolate the changed input
class before treating it as a dependency-closure defect.

## Controlled inputs

Repository: `tailrocks/velnor`. Base source:
`e94b48406c4ed206fce2bbf39b788264e72cf39c`.

- Control head `aab1d75838820b1e6e6c6ea29a1a8e5147cf397c`: one added
  `plans/ci-performance/README.md`.
- Treatment head `c17c6ad09c4eb9617fbf8372914f50b9195a67ec`: that README,
  `baseline.md`, and two raw evidence JSON files under the same directory.
- Same local runtime: built from `aab1d758`, Rust 1.98.1, Darwin ARM64,
  dev profile, no default features. Closure:
  `b44cee623cdcc58f209ff37b3e2c915d875ab0ff5964009e38ef8598dafd5724`.
- Same `.github/ci/project.toml` and event/provider/trust inputs for both
  local plans. No cache or compiled-product experiment is claimed.

Replay, substituting each full head above:

```sh
env EVENT_NAME=pull_request \
  BASE_SHA=e94b48406c4ed206fce2bbf39b788264e72cf39c \
  HEAD_SHA=<control-or-treatment> \
  VELNOR_PROVIDERS=github-hosted VELNOR_EVENT_TRUSTED=true \
  target/debug/velnor-workflow plan --config .github/ci/project.toml
```

## Measurements and actual CI

| Condition | Planned units | Rust selected | Docker selected |
| --- | ---: | --- | --- |
| Markdown-only local control | 1 (`docs`) | no | no |
| Markdown + JSON local treatment | 17 (all) | yes | yes |
| Markdown + JSON real PR treatment | 17 (all) | yes | yes |

Saved local outputs:
[control](../observations/velnor-markdown-control-plan.txt),
[treatment](../observations/velnor-evidence-json-plan.txt).

Real [run 35480462500](https://github.com/tailrocks/velnor/actions/runs/35480462500),
attempt 1, planning [job 105997217349](https://github.com/tailrocks/velnor/actions/runs/35480462500/job/105997217349),
checks out merge `b935b24514f18eb8f69bebfbfd1d3707f02e4154` and uses published
runtime pin `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`, closure
`8b96d5108550dfa61a57ff65c6c357b4119493c660417bf3beb4af2b03742269`.
This differs from the local binary, so local/remote agreement is corroboration,
not a same-runtime performance comparison. The real run has a generated-state
failure; it is not a successful workflow baseline. A real Markdown-only
control and UI inspection remain pending.

## Structural explanation and alternatives

`s2/runtime.rs::selection_for_diff` falls back to full coverage as soon as a
changed path matches no unit watch set. Markdown matches `docs`; the evidence
JSON files match none. The fallback is necessary while irrelevance is unproven.

Alternatives to evaluate separately:

1. Keep conservative full fallback with an explicit unmatched-path explanation.
2. Prove complete source/product input closure and declare known non-product
   evidence paths through a typed configuration contract.
3. Broaden documentation inputs only if an actual documentation/data check
   consumes and validates them; do not relabel unknown files merely to skip work.

Conclusion awaiting review: the initial treatment was not a Markdown-only
negative test. Do not remove the fallback or claim a Docker false positive from
this run. Next experiment must test complete typed irrelevance and unknown-input
fallback independently, including deletion/rename and incomplete diffs.

No performance candidate accepted, no speedup calculated, no plateau increment.
