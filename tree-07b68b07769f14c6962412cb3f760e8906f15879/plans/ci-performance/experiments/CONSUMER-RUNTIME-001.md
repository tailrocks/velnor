# CONSUMER-RUNTIME-001: converge on an existing attested runtime

Status: candidates pushed in Parallax and Jackin. Independent
diff review and real candidate CI pending. Not a completed optimization iteration.

## Cause and alternatives

Parallax's historical scheduled child failed generated-tree policy after
source-installing its old generator. Source generation, declared pin, and the
runtime product are separate identities; upgrading only one can preserve drift.
Velnor's clean main also fails pinned-render equivalence at its older pin.

Alternatives: rebuild the old generator per job; build an unpublished candidate
once and verify its transfer contract; use the already published, immutable,
attested current-main product. Select the existing product for this baseline
convergence. This does not solve bootstrap for the new typed-stage schema.

## Verified product

- Revision: `e94b48406c4ed206fce2bbf39b788264e72cf39c`.
- Closure: `8f88f2905f853bd9c86b84c1ca308758010270fe5c01a77b2a4eb729824e4999`.
- Release: `velnor-workflow-runtime-v1-8f88f2905f853bd9`.
- Local platform: macOS ARM64; release profile, empty feature set.
- Binary SHA-256: `c11066244c0e9442fc51ac73f7bb580d74b338eef1059e84e20781e0c545b1e8`.
- Manifest, binary self-reports, and GitHub attestation agree. Attestation binds
  the digest to the runtime-products workflow, main push and exact source SHA.
  Raw manifest and attestation are retained under `observations/`.

Atomic `promote` uses the published binary and pristine generator checkout at
that revision. It renders, verifies, and commits with DCO signoff and coauthor.
Both consumers accept `plan` before promotion. Both exact-runtime `--check`
commands pass afterward with `VELNOR_WORKFLOW_PINNED_BINARY` set to that binary.

## Candidate identities and preserved obligations

| Repository | Baseline | Candidate | Generated task graph SHA-256, identical before/after |
| --- | --- | --- | --- |
| Parallax | `6a12bf47a816b63e848b563aaa45ef9694159c79` | `0da45dafabc7a46cf1a5c1ff461e2d193d115cb1` | `0fa68f880b4d206a938ba7ed04c987638835430d9de4aa6a77cb38e96f1385b1` |
| Jackin | `41796158b1e45535ae4e74d5ff048cb5bb4e0488` | `95b437e735aafea5fe9b2e638c122345c5d141c3` | `05b13db49101b212c5b06bbb2de57937db29eb48e6d187da84cd03e5cbcda12f` |

Parallax retains 23 units and 54 dependency edges. Jackin retains 40 units and
146 edges. Both retain source configuration schema 1 and generated runtime
schema 2. Accumulated upstream emitter changes still require independent diff
review and CI; identical task graphs alone do not prove workflow equivalence.

Parallax [PR 112](https://github.com/tailrocks/parallax/pull/112) runs candidate
PR configuration in [35481807065](https://github.com/tailrocks/parallax/actions/runs/35481807065).
Policy run 35481806980 is `pull_request_target`: its base-owned workflow cannot
establish execution of the changed candidate policy workflow.

Jackin [PR 1007](https://github.com/jackin-project/jackin/pull/1007) tracks the
exact candidate above. Its regeneration changes 11 paths; runtime task graph,
desktop workflows and release workflow remain unchanged.

## Demonstrated dispatch access limit

Attempt: `gh workflow run ci-policy.yml --repo tailrocks/parallax --ref codex/ci-performance-campaign`.
Observed: HTTP 401 `Requires authentication`, POST
`/repos/tailrocks/parallax/actions/workflows/358687592/dispatches`.
SSH push and authenticated connector PR creation work. The connector exposes
no discovered dispatch operation. User was asked to refresh CLI authentication
locally; no token requested in chat. Candidate manual-policy and scheduled-path
validation remain pending. PR CI and independent implementation continue.

No production release created. No baseline/candidate timing comparison, speedup,
coverage acceptance, or plateau increment claimed. Functional CI may overlap
across repositories; those observations are not controlled timing samples.
