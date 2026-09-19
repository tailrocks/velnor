# G1 hosted-provider exact review — `3aecc6ed0bf83b1f6dd1d99a1a8a29884f3d710a`

## Scope

- Repository: `tailrocks/velnor`, branch `codex/github-first-hosted`.
- Exact detached tree: `/tmp/velnor-hosted-review-3aecc6ed`.
- Parent reviewed baseline: `d70a88a2bfebada05c3ddb1140ceb9066c53a94b`.
- Worktree was clean. No source or generated files were changed.
- Prior G1 blockers reviewed: producer verifiers used a transitive `needs.source` output, and producer admission trusted only display name/conclusion.

## Verdict

**Do not approve this exact commit.** The d70 runtime identity fix is materially complete, and 3ae fixes the tarball publisher's missed `workflow_run` condition plus the producer-event argument. The exact source still has one failing release test, and the producer admission gate does not guard every source-executing native/tarball preview job.

## Corrected behavior

`validate_producer_admission` now checks producer name, completed/success status, repository full name and object ID, head repository full name and object ID, workflow ID/path, producer event (`push`), branch/ref/default branch, positive run ID, and equality of head/run/source SHAs (`s2/runtime.rs:3960-4109`).

Direct CLI replay checks from the exact binary:

| case | result |
| --- | --- |
| trusted identity | admitted |
| foreign repository / repository ID | rejected |
| foreign head repository / head ID | rejected |
| wrong workflow ID/path | rejected |
| wrong producer event | rejected |
| wrong branch/ref | rejected |
| zero run ID | rejected |
| source SHA mismatch | rejected |
| correct `workflow_run` source | resolves the producer head SHA |

The 3ae renderer now emits `--producer-event "$PRODUCER_EVENT"` and the tarball preview publisher condition is `github.ref == refs/heads/main` plus admitted `publish` mode; it no longer requires `github.event_name == 'push'`. Producer verifier/build/publisher source references are direct dependencies and use `needs.source.outputs.sha`.

Fork/manual behavior is fail-closed at the gate: repository/head-repository identity must equal the configured repository; workflow event must be `push`; dispatch resolves a diagnostic mode and cannot publish. No polling or `gh run list` remains.

## Blocking finding — source admission does not gate all producer consumers

The gate is direct for unit verifiers, tarball build, and publishers, but several jobs still consume the producer source before or without `publish-gate`:

- Native preview identity injection adds only `needs: [source]` (`s2/primitives/release.rs:2280-2293`).
- Native guest payload builds from `needs.identity.outputs.commit` with only `needs: [identity]` (`:1323-1327`).
- Native preview metadata builds from source with only `needs: [identity]` (`:1419-1431`).
- Native Debian packaging depends on identity/metadata/guest and has only a default-branch `if` (`:1617-1642`).
- Native preview signing depends on identity/debian and has only a default-branch `if` (`:1721-1731`).
- Tarball guest payload injection adds only `needs: [source]` and checks out `needs.source.outputs.sha` (`:2931-2940`).

On a `workflow_run`, `source` emits the event head SHA before admission. A rejected fork/wrong-workflow event can therefore still cause identity/metadata/guest/debian/sign jobs to checkout and execute producer-controlled source before the full repository/workflow/event/ref/run contract is accepted. This violates the trust/source-pin requirement even though the final publisher is gated. Make every producer-bound source-consuming job directly depend on `publish-gate` and skip unless `needs.publish-gate.outputs.admitted == 'true'`; include the native guest/metadata/debian/sign and tarball guest graph in the regression DAG test.

## Residual run replay contract gap

The runtime validates that `run-id` is a positive canonical decimal, but does not bind that ID to the supplied SHA through a provider lookup or an expected immutable run record. A changed positive ID with otherwise identical SHAs is admitted; a source-SHA mismatch is rejected. That is acceptable only if valid event redelivery/replay is intentionally idempotent. If stale/replayed run IDs must be rejected, add a provider-backed immutable run-ID↔head-SHA check or an explicit monotonic/replay ledger and test it. Current source-only tests do not prove this relationship.

## Exact test evidence

- `rtk cargo test -p velnor-workflow --lib s2::runtime -- --nocapture` — **60 passed**.
- `rtk cargo test -p velnor-workflow --lib s2::config -- --nocapture` — **65 passed**.
- `rtk cargo test -p velnor-workflow --lib s2::primitives::release -- --nocapture` — **96 passed, 1 failed**: `preview_producer_binding_renders_source_gate_and_wired_publish` at `s2/primitives/release.rs:8356` still asserts the pre-3ae build `needs` list and pre-gate push-only publish condition. The rendered output correctly contains `[source, publish-gate, ...]` and the admitted gate expression, so this is stale regression coverage that must be updated before the commit is green.
- Focused `native_preview_binding_rewires_identity_onto_resolved_source` — passed.
- Focused `producer_bound_preview_verification_uses_admitted_source_sha` — passed.
- `rtk cargo fmt --all -- --check` — passed.
- `rtk git diff --check d70a88a2bfebada05c3ddb1140ceb9066c53a94b..HEAD` — passed.
- `rtk cargo clippy --locked -p velnor-workflow --all-targets -- -D warnings` — passed.
- `rtk cargo test -p velnor-workflow --lib s2::tests::checked_in_workflows_match_the_generator_byte_for_byte -- --nocapture` — failed first at `ci-unit-docs.yml` drift. The exact dry run independently reports the same **12 generated files** would change; no generated output was changed, so this is expected source-only drift rather than generated-output approval.
- Exact Velnor dry run: `rtk cargo run -q -p velnor-workflow -- /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-hosted --plain --dry-run` — completed without writes; **12 files would change**. This is expected generator/source drift, not generated-output approval. No generated output was published or adopted.

The actual Velnor `.github-gen/velnor-workflow.toml` remains an unbound native release; producer-binding behavior above is exercised by the generic typed fixtures, not by a generated Velnor producer workflow yet.
