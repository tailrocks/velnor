# Velnor fleet unification plan status

Status: **completed / archived**

Completed: 2026-08-17

This is the authoritative final checkpoint. The implementation plan was
committed from the start as `PLAN_STATUS.md`; its completed state and evidence
are preserved here and in Git history.

## Completed phases

1. Fixed the exact 28-repository scope and excluded legacy repositories.
2. Built the generated `velnor` default, optional `github`, and optional
   two-real-lane `both` selector.
3. Unified trusted and public-unmerged execution without weakening the public
   threat model or substituting GitHub for selected Velnor work.
4. Added admission-closure, provenance, cache isolation, request validation,
   deterministic aggregation, and clear fail-closed rejection evidence.
5. Audited workflow topology and retained only generator-authorized distinct
   trusted/public security domains.
6. Repaired preview trigger/event/SHA synchronization, rolling releases,
   source classification, and tap updates with bare SHA-256 values.
7. Repaired GitHub App token planning, owner-bound client IDs, and package
   updater stable/preview behavior.
8. Released runner `v0.1.175`, verified its signed record, deployed it through
   APT, restarted nine daemons, and passed all doctor checks.
9. Released canonical and owner-local mirrored actions at signed tag
   `2026.8.30`, then regenerated and merged all consumers.
10. Corrected branch protection and review requirements for single-maintainer
    repositories while retaining stable required CI gates.
11. Merged all valid in-scope implementation and Renovate work after green
    exact-head CI; closed only superseded PRs whose merge would regress the
    final atomic pin.
12. Ran central tests, generator/fleet audit, remote action-closure validation,
    live selector/rejection/preview/package proofs, and a current-head CI audit
    across all 28 repositories.

## Final gates

- Central test suite: **96/96 passed**.
- Generator inventory: **28 repositories / 5 classes / 5 templates**.
- Remote immutable-action closure: **9 actions / valid**.
- Current fleet main CI: **28/28 completed / success**.
- Sentry runners: **9/9 running; 9/9 doctor success**.
- Open goal pull requests: **0**.
- Remaining ordered implementation work: **none**.

## Preserved hard rules

- Use mise; never Homebrew.
- Velnor remains the automatic default. GitHub and both are explicit options.
- Never replace a selected Velnor lane with GitHub-hosted execution.
- Keep unsafe public-unmerged work pre-execution fail-closed with actionable
  rejection detail.
- Never hand-edit generated consumers; change canonical source and regenerate.
- Never merge red or pending work; verify exact PR head, then post-merge main.
- Do not add excluded legacy repositories without an explicit new goal.
- Private repositories do not require a DCO status producer.

## Archive decision

The completion gates are proved, so this goal is archived as achieved rather
than blocked. The plan is retained as a committed completion record. Future
dependency upgrades, new repositories, workflow changes, or infrastructure
incidents start new maintenance work and do not reopen this plan automatically.
