# Checker identity reconciliation

Captured `2026-09-19T19:08:39Z`. Read-only. Effective settings verified as `gpt-5.6-luna` / `max`.

## Authoritative owner

The pushed owner is:

`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-checker`

- branch: `codex/github-first-checker`
- local HEAD: `2ba66b116dd5511f0b4f2a6856cfbed6bd290152`
- remote `refs/heads/codex/github-first-checker`: same SHA
- tree: `b642339db0db0b286c7122e3ab02124b9172b18c`
- worktree: clean

The exact remote resolution was confirmed with `git ls-remote origin refs/heads/codex/github-first-checker`.

## Non-authoritative copies

- `/private/tmp/g1-checker-adversarial-final`: exact current owner commit/tree, clean detached copy.
- `/private/tmp/g2-checker-x8njgs`: exact current owner commit/tree, but dirty with untracked `fixtures/` and `review_fixture_generator.py`; do not use as authoritative.
- `/private/tmp/g1-checker-adversarial-review` and `/private/tmp/g3-checker-host-release-design-8f976b52`: prior `8f976b52e6250deea662cdb23ccd19e7d9ca446c` tree.
- `/private/tmp/g1-checker-review` and `/private/tmp/g2-checker-review.5FXqui`: original `b3b6b2ef5239ff3354f504b8aeb638129fd0504b` tree.
- `/private/tmp/g1-checker-v2-review`: intermediate `017c92e0bf608d42675a0b8e495f0486c7296041` tree.

The pushed candidate is a descendant of `8f976b52`; the old review trees are not alternate current branch heads. Full machine-readable mapping, tree hashes, status, and commit chain: [reconciliation.json](./reconciliation.json).

This reconciles identity only. It does not approve the checker or establish any G0/G1 gate.

