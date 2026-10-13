# Upstream 9e5 integration review

Status: local deterministic checks and final independent review pass; exact-head CI pending.
No performance acceptance.

Parent integrates upstream `9e5c0eb215d4169578d6f064806e89fe4c793e85`
with campaign `56017bac6a04823d456be9deb6405468e90b87c5`.
Existing package transaction, MBX quota, and sccache grouping changes remain.

## Runtime provenance

Independent reviewer: `/root/velnor_inventory`; verdict: adopt exact runtime
`4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d`.
Manifest binds release profile, empty features, and Linux x64/ARM64 plus macOS
ARM64 products. Parent verified manifest and downloaded macOS binary attestations
against repository, main ref, exact source/signer digest, producer workflow, and
GitHub-hosted execution. Raw verification output is retained under observations.
Release metadata is not immutable; source identity and verified digests are the
contract. Producer run: <https://github.com/tailrocks/velnor/actions/runs/35503134829>.

## Rollback process boundary

Root cause: running verification inside a conditional subshell suppresses shell
errexit; a failed command followed by a successful command can look successful.
A separate `bash -euo pipefail -c` process owns verifier failure semantics while
parent rollback/finalizer traps retain transaction ownership. Exported verified
package inputs cross the process boundary; parent-local helpers are not needed.

Independent reviewer: `/root/jackin_inventory`; verdict: behavior approved.
Fixtures cover Bash 3.2/5 failure, explicit exit, pipeline failure, success,
child TERM, process-group TERM, rollback count, lock retention, and cleanup.
Parent independently replayed ten direct child-process cases and ten conditional
child-process cases; raw results retained. Group cancellation preserves lock
fencing for manual recovery rather than claiming successful rollback.

Full all-target Clippy subsequently found four fixture panic calls and one
oversized fixture helper. Repaired with typed helper errors, fixture extraction, generic repository input,
and bounded TERM-to-KILL cleanup. No new lint allowances or weakened assertions.
Parent final checks: strict all-target/all-feature Clippy, formatting, and all
1,942 nextest tests pass (21 binaries; 38.148 seconds local execution).
Actionlint and generated-tree checks pass; exact committed rebuild follows.
Final reviewer `/root/velnor_inventory` approved the repaired fixture diff,
independently ran 44 package tests plus strict all-target Clippy, and confirmed
TERM/KILL cleanup, lock retention, and single rollback assertions.
Real CI cancellation remains unvalidated; local shell fixtures do not prove it.

## Completion limits

No speedup, warm-cache, release-success, or final campaign completion claim.
Upstream preview run 35504500719 failed ARM linker setup and dirty source identity;
its successful main workflow does not validate preview products. Candidate DAG,
Mise installer/platform contracts, and consumer integration remain separate work.
