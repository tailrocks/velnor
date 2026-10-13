# Plan 015: Remove the committed run-log HTML capture with GitHub channel tokens, and add a commit policy

> **Executor instructions**: Follow step by step. **Step 3 rewrites git
> history — it is destructive and must be run by / confirmed with the operator,
> never unilaterally.** Do Steps 1–2 (safe) first. STOP ⇒ report. Update
> `plans/README.md` when done. **Security** plan — never print a token value.
>
> **Drift check (run first)**:
> `git ls-files .velnor-compare/ | grep -i '\.html$'`
> If the HTML capture is gone, this plan is already partially done — verify
> history too (Step 3).

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: MED (history rewrite)
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

`.velnor-compare/velnor-job.html` is a saved, rendered GitHub job page committed
to the repo. It embeds GitHub "Alive"/live-update channel tokens and a
visitor-payload token (JWT-shaped values in `data-channel` / `liveUpdateChannel`
/ `visitor-payload` attributes). These are signed, GitHub-minted session tokens;
committing them leaks operational session context and sets a precedent for
saving raw rendered pages into the repo. They are short-lived and cannot be
"rotated" (they expire on their own), so the correct action is removal from the
working tree **and history**, plus a policy so only sanitized artifacts get
committed going forward.

For contrast (already-good, do not touch): the committed `github-job-*.log`
files have their git `AUTHORIZATION` headers masked to `***`, and `jobs.json`
carries no secret variables — the runner's masking held there. The problem is
specifically the saved **HTML page**.

## Current state

- `.velnor-compare/velnor-job.html` — tracked (`git ls-files` lists it),
  contains JWT-shaped channel/visitor tokens in HTML attributes. (Do not open it
  to quote values; you can confirm token *presence* with
  `git grep -l 'data-channel' -- .velnor-compare/` without printing values.)
- `.gitignore` currently ignores several `.velnor-*` dirs
  (`.velnor-live-evidence/`, `.velnor-work/`, etc.) but **not** `.velnor-compare/`
  (it is intentionally tracked for lane-comparison evidence). Read `.gitignore`
  to confirm.
- The rest of `.velnor-compare/` (`.log`, `.json`, `.md`, `.zip`) is sanitized
  and fine to keep.

## Commands you will need

| Purpose                | Command                                                              | Expected            |
|------------------------|----------------------------------------------------------------------|---------------------|
| Confirm tracked        | `git ls-files .velnor-compare/ \| grep -i '\.html$'`                  | the html path       |
| Confirm token presence | `git grep -l 'data-channel\|liveUpdateChannel' -- .velnor-compare/`   | the html path       |
| Remove from tree       | `git rm .velnor-compare/velnor-job.html`                             | staged deletion     |
| History scan (all html)| `git log --all --diff-filter=A --name-only -- '.velnor-compare/*.html'` | commits that added html |

## Scope

**In scope**:
- Delete `.velnor-compare/velnor-job.html` from the working tree.
- Add a policy: `.gitignore` rule for `*.html` under `.velnor-compare/`, and a
  short note in `AGENTS.md` / `docs/` that only sanitized `.log`/`.json`/`.md`
  artifacts may be committed there (never saved GitHub HTML pages).
- History purge (Step 3) — operator-gated.

**Out of scope**:
- The sanitized `.log`/`.json`/`.md`/`.zip` artifacts — keep them.
- Any change to how the runner masks logs (that masking is working here).

## Git workflow

- Branch: `advisor/015-purge-committed-run-logs-tokens`
- Commit: `chore(security): drop committed run-log HTML capture; forbid committing rendered pages`
- Do NOT push/PR unless instructed. **Do not force-push a rewritten history
  without explicit operator confirmation.**

## Steps

### Step 1: Remove the file from the working tree

```
git rm .velnor-compare/velnor-job.html
```

**Verify**: `git status` shows the deletion staged; `git ls-files
.velnor-compare/ | grep -i '\.html$'` returns nothing.

### Step 2: Add the commit policy

- Append to `.gitignore` a rule ignoring rendered pages under the tracked
  evidence dir, e.g. `.velnor-compare/*.html`.
- Add a short paragraph to `AGENTS.md` (near the fixture/evidence rules) stating:
  only sanitized `.log`/`.json`/`.md` lane-comparison artifacts may be committed
  under `.velnor-compare/`; **never** commit saved/rendered GitHub HTML pages —
  they embed live channel/visitor tokens.

**Verify**: `git check-ignore .velnor-compare/anything.html` → prints the path
(rule matches); `cargo fmt --all --check` unaffected (no Rust change).

### Step 3: Purge from history (OPERATOR-GATED — do not run unilaterally)

The tokens are already published in history. Because they are short-lived and
un-rotatable, history rewrite is a hygiene step, not an emergency. **Present the
following to the operator and get explicit confirmation before running it**, and
coordinate with anyone who has clones (a rewrite invalidates existing clones and
requires a force-push):

- Identify the commits that added any `.velnor-compare/*.html`:
  `git log --all --diff-filter=A --name-only -- '.velnor-compare/*.html'`.
- Use `git filter-repo` (preferred) or BFG to strip the path from all history:
  `git filter-repo --path .velnor-compare/velnor-job.html --invert-paths`.
- Force-push and notify collaborators to re-clone.

If the operator declines the history rewrite, record that decision in the PR and
`plans/README.md` (the working-tree removal + policy still land). Do **not**
force-push without confirmation.

**Verify** (only after an approved rewrite): `git log --all --oneline -- '.velnor-compare/*.html'`
returns nothing.

## Test plan

- No code tests. Verification is the git commands above.

## Done criteria

- [ ] `.velnor-compare/velnor-job.html` removed from the working tree
- [ ] `.gitignore` ignores `*.html` under `.velnor-compare/`; `AGENTS.md`/`docs` states the policy
- [ ] History purge either completed **with operator confirmation** or explicitly deferred with the decision recorded
- [ ] No token value appears in the PR, commit message, or this plan
- [ ] `plans/README.md` row updated (note if history purge deferred)

## STOP conditions

- The HTML file is not where expected, or other `.html` captures exist in
  `.velnor-compare/` — enumerate them all and report before deleting.
- Anyone other than the operator would run the history rewrite — STOP; history
  rewrite requires operator confirmation and coordinated force-push.

## Maintenance notes

- The runner's log masking is correct for `.log`/`.json` artifacts; the gap was
  a human committing a full rendered page. The `.gitignore` + `AGENTS.md` policy
  prevents recurrence.
- Reviewer: confirm no token value is quoted anywhere in the change, and that
  the sanitized evidence artifacts were preserved.
