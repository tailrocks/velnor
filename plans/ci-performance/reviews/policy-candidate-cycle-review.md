# Policy and candidate graph review

Scope: generated policy/candidate graph at `e717de39b97558117e7e4f55c20216c8a4e4b1fc`, plus the merged package fragment at `9307861d2668424d27012c3c736d48ab47f9012e`. Read-only review; no workflow or renderer edits here.

## Verdict

**HOLD.** The candidate wait is a graph defect. It is not a slow artifact-indexing case and must not be fixed by increasing the 15-minute poll.

In `ci-main.yml`, every unit caller, including `github-hosted-rust-velnor-workflow`, has `needs: [plan, policy]`. That caller is the only candidate publisher. Its generated publish step is gated by `github.event_name == 'pull_request'`, so a main push cannot publish a candidate at all. The policy job polls only `ci-pr.yml` runs with `event=pull_request`; it does not consume the current main run. A main run whose checked-in pin differs from the base policy closure therefore waits for an artifact that this graph cannot produce. The observed upstream failure was `no candidate product ... was published within 15 minutes`.

The PR path is different: `ci-pr.yml` has no policy dependency, so its hosted generator caller can publish; `ci-policy.yml` may consume that sibling run. The main path still has an unsatisfied producer, and the same generated graph must support push and dispatch events.

## Minimal structural repair

Keep the hosted generator caller as the pre-policy producer on `ci-main`; do not run Velnor/self-hosted code before policy. Generate these edges:

```text
plan ───────────────▶ hosted generator/candidate publisher ─▶ policy ─▶ ordinary checks
  └──────────────────────────────────────────────────────────────▶ ci-required
```

The hosted generator caller must omit `policy` from its `needs` only on the main aggregate, while ordinary callers retain the policy gate. The policy job must declare `needs: [plan, hosted-generator]`. `ci-required` keeps both policy and every expected caller in its final required set. This removes the cycle without making policy advisory.

The candidate renderer must support the trusted main producer's source identity (`github.sha`/the frozen plan head), artifact name, manifest revision, and build revision. For main push or dispatch, policy first consumes the artifact from the exact current run ID. For `pull_request_target`, retain the same-repository `ci-pr` producer path and exact head/artifact binding. Never search `latest` or correlate an unrelated run. If a same-closure run intentionally skips publication, policy may use the declared pin path; if a differing closure has no producer, fail immediately with the missing-producer reason.

Manual dispatch with a provider subset that excludes the hosted producer needs an explicit outcome: either the hosted candidate producer remains an automatic policy prerequisite, or a differing pin is rejected before any selected provider job starts. Silently waiting for a Velnor candidate would weaken the trust boundary.

## Required renderer tests

- Main graph: hosted generator caller has no policy edge; policy waits for that caller; every other caller still waits for policy; no dependency cycle.
- PR graph: existing sibling `ci-pr` candidate producer remains policy-free; policy workflow consumes its exact run/head artifact.
- Main push with differing pin: current-run candidate is uploaded, then policy downloads that exact run artifact and renders it.
- Main push with equal base/pin closure: candidate publication skips and policy takes the declared-pin check without waiting.
- Dispatch with hosted excluded and differing closure: deterministic fail-closed diagnostic, no local-provider candidate execution.
- Candidate manifest source/revision/build-revision and artifact name remain head-bound; fork candidates remain rejected.

The Bash 3 rollback fix is separate: `/tmp/velnor-rollback-fix.patch` changes only the generated package-release verifier guard and its trap fixture. It passes the focused trap test and all 42 package-release tests on Bash 3.2.57.

## Parent review of candidate DAG patch (2026-09-20, 11:39 UTC)

Patch `/tmp/velnor-candidate-dag-final.patch` remains HOLD pending independent
review and repair. Source inspection found the bootstrap runs a default-feature
Cargo build before testing whether the pin suffices, but emits an empty feature
identity. The declared target is hardcoded x86_64 GNU Linux while runner
placement remains configurable. These are compatibility and unnecessary-work
questions, not accepted performance improvements. Review must also prove actual
job dependencies (textual YAML order proves none), new-branch base handling,
Velnor-only behavior, and receipt agreement with the policy manifest parser.
Reviewer `/root/jackin_inventory` has the exact patch and these findings.

## Independent DAG review (2026-09-20, 12:xx UTC)

Verdict remains **HOLD**. The candidate patch does not apply cleanly to
`3a7da430`; `git apply --check` fails in `lib.rs`, both primitive IR files,
and `s2/mod.rs`. Partial inspection confirmed these blocking defects:

- The producer writes `source_head_sha`, while the current Rust
  `CandidateManifest` requires `revision`.
- The producer exports the untrusted candidate through
  `VELNOR_WORKFLOW_PINNED_BINARY`, collapsing the candidate and trusted
  runtime identities. The current contract uses the separate candidate slot.
- `download_artifact` assigns its arguments to global shell variables. The
  receipt download overwrites `artifact_id`, so the subsequent candidate
  download can request the receipt artifact while checking the candidate name.
- The default-feature Cargo build is stamped as `features=""`; the package
  currently defaults to `tui`.
- The producer and consumer hardcode `x86_64-unknown-linux-gnu` while runner
  placement remains configurable.
- Velnor-only/provider-restricted graphs can omit the producer, while the
  push/dispatch policy branch still requires its outputs.
- Consumer policy needs `actions: read` for the jobs/artifacts API, but the
  generated permission block only grants `contents: read`.
- `config_sha256` is shape-checked but never compared with the exact audited
  checkout config.
- Dispatch base refs are fetched without canonicalizing `FETCH_HEAD` to a
  commit; first pushes can pass an all-zero `github.event.before`.
- The legacy event predicate broadens arbitrary branch push/dispatch
  admission. Textual job order also does not make bootstrap precede plan;
  those jobs remain parallel.

The patch also builds unconditionally before deciding whether the declared
pin suffices, and artifact names omit `run_attempt`. Both need explicit
correctness and rerun handling before timing claims.
