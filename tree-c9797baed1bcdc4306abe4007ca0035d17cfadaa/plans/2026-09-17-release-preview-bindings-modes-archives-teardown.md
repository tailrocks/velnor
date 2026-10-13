# Area E design-challenge memo: release/preview event bindings, modes, archives, teardown

Date: 2026-09-17. Base: velnor2 @ 33688938. Evidence: read-only reference
workflows (rolling-preview + tagged-release + dev-lane + deterministic
archive builder) studied for behavior, not bytes. No reference names, paths,
or copied shell appear below or in the implementation.

Rule applied to every gap: extend the existing typed machinery
(`ReleaseSpec`, `[release]` validation, `release_contract_complete`,
`reconcile absent->create / coherent->noop / different->conflict-fail`,
rolling-preview concurrency, signer/attest template,
`runtime.rs release verify/package`, `version_bump_units`). Anything already
supported is named as such and NOT re-implemented. Every new field is
opt-in: an unset contract renders byte-identical output
(`default_rows_render_the_legacy_release_surface` pins must not move).

## Gap 1 — `workflow_run` trigger binding

Challenge: is a generic binding possible, or is producer-gating inherently
product-specific? Yes, generic: the binding is three declared facts —
producer workflow name, required conclusion, and source branch — plus two
derived behaviors: source-SHA resolution (`workflow_run.head_sha` vs
`github.sha`) and a publish gate that refuses any other producer,
conclusion, or event. Already supported: `workflow_dispatch` inputs,
push-tag triggers, rolling-preview concurrency. NOT supported: no
`workflow_run` block is rendered anywhere, and no runtime command resolves
or admits a producer run, so a `workflow_run`-triggered publish today would
either build the wrong SHA or fail open on an untrusted producer.
Bug class removed: publish-from-unverified-producer (wrong-SHA build,
forged/failed-producer publish, untrusted-PR code reaching a privileged
publish job). The gate is fail-closed: unknown events refuse, and PR
contexts never satisfy the producer predicate.

## Gap 2 — validate / build / rehearse / publish modes

Challenge: are four modes generic, or two (dry-run vs publish) enough?
Four, because the reference behavior distinguishes four externally visible
contracts: `validate` (secret-free assembly + reconciliation fixtures on
any branch), `build` (full compile without publish), `rehearse`
(feature-branch end-to-end assembly that finishes WITHOUT waiting for
default-branch CI), `publish` (tag-triggered external writes only).
Collapsing rehearse into validate reintroduces the bug; collapsing build
into publish removes the secret-free compile drill. Already supported:
per-kind publishers, full-scope validation, `version_bump_units`
(validation runs declared work). NOT supported: no mode input is rendered,
no mode resolver exists in the runtime, and nothing isolates a rehearsal
from the default-branch wait loop. Bug class removed:
rehearsal-blocks-on-main (feature rehearsal inherits the publish gate's
wait-for-CI loop and never finishes) and mode-confusion publish (a
non-tag event reaching external writes). The event×mode matrix is total:
every (tag, dispatch, schedule, workflow_run, PR) × mode pair resolves to
exactly one outcome, and only (tag, publish) + admitted (workflow_run,
publish) write externally.

## Gap 3 — declared archive contents, checksums, manifests, retention

Challenge: generic, or per-product packaging? The mechanism is generic
(deterministic tar flags, per-subject checksum sidecars, one manifest +
one checksum corpus, per-artifact retention); only the file list is
declared. Already supported: `package-binary`/`package-deb`/`package-guest`
emit archives + sidecars, native publish assembles a release record and
verifies `SHA256SUMS --strict`, signer/attest template exists.
NOT supported: archive membership is hard-coded per kind (the binary set
inside the tar is not declared), determinism flags are absent from
`package-binary` (plain `tar -czf`: unsorted, mtime-bearing, gzip
timestamped — rebuilds differ), manifest assembly exists only for the
native kind, and retention is a literal per call site. Bug class removed:
unreproducible-archive + undeclared-membership (consumers cannot verify
what they did not declare; re-runs fail byte-equality forever, which is
exactly why the native publisher verifies coherence instead of bytes).
New: `[release.archive]` declares members, checksum algorithm, manifest
schema URN, and retention days; the runtime gains deterministic packaging
and a manifest assembler shared by all kinds.

## Gap 4 — credential setup/teardown pairing

Challenge: generic without product specifics? Yes: the pattern is
setup-command + teardown-command + always-run pairing, independent of what
the credential is. Already supported: feed mutation notes "GitHub-writer
only", environments gate publish jobs. NOT supported: no declared
credential block exists, no renderer emits paired setup/teardown, and no
test proves teardown runs on cancel/timeout — a leaked credential store is
one forgotten step away. Bug class removed: credential-store leak
(setup without teardown on success/failure/cancel/timeout; sidecars built
while secrets are still mounted). New: `[release.credential]` declares
named setup/teardown pairs; the renderer emits a `trap`-paired
setup step (teardown registered before any secret materializes), an
explicit teardown call before supply-chain sidecars, and an `if: always()`
teardown step so cancellation still restores host state. Validation
refuses setup without teardown and forbids teardown steps that can be
skipped.

## Out of scope (reference-owned, not implemented)

Package schema internals, entitlements, bundle contents, OS-specific
sign/notarize steps, tap/formula mutation, product verifiers. The generic
surface stops at: manifest envelope (schema URN + assets + digests),
credential pairing, mode resolution, producer binding.

## Acceptance

`cargo test -p velnor-workflow`, clippy, fmt green; pinned legacy pins
unmoved; new tests cover the event×mode matrix (tag/dispatch/schedule/
workflow_run/untrusted-PR), rehearsal isolation, immutable-conflict paths,
and teardown-on-cancel; neutral fixtures only.
