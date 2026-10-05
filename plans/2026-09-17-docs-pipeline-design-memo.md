# Docs pipeline design-challenge memo (area F)

Date: 2026-09-17. Scope: composable documentation pipeline in `velnor-workflow`.

## Challenge 1 — Is this generic?

No repository name, path, URL, tool, or content rule enters the generator.
Evidence shape (826-line consumer workflow) decomposes into generic stages:

- gate (change filter + result reuse lookup)
- source-link check (local, PR/push/dispatch only)
- build + built-site check (local, PR/push/dispatch only, built-site reuse)
- spelling (local, PR/push/dispatch only)
- Pages deploy (bounded retry, main only)
- post-deploy verify (internal, after deploy)
- scheduled live check (external, schedule only)
- required aggregator (result publish)

All consumer-owned values stay in `[docs]` config + named tasks:
site URL, sitemap path, output directory, build/check/spell/verify/external
commands, path filters, schedule. Generator renders structure, gates, reuse
keys, retry ladder, failure reporting — never addresses, mappings, or rules.

Verdict: generic composition over opaque consumer inputs. No estate knowledge.

## Challenge 2 — Is this already supported?

- `scan/docs.rs`: markdownlint unit only. No linkcheck, no spelling, no build,
  no deploy, no post-verify.
- `release.rs` `kind="pages"`: single `deploy` job (`Bun` + fixed
  `scripts/generate-docs.ts` + Configure/Upload/Deploy) plus a policy-only
  `verify` job. No source/built-site split, no spell, no reuse, no retry, no
  URL/health post-check. Fixed script path is the consumer-owned violation
  this work removes from the new path (legacy renderer untouched).
- No scheduled-external vs PR-local split anywhere in generator.

Verdict: not supported. New primitive + config section required.

## Challenge 3 — What bug class does this remove?

1. Silent link rot: source renames bypass docs-only filters; deploy without
   post-verify ships broken site. Fixed by always-on source-link gate,
   built-site gate before deploy, mandatory post-verify + scheduled live gate.
2. Flaky-deploy false red: opaque Pages handoff fails valid artifact. Fixed by
   bounded retry (3 attempts, backoff) scoped to deploy handoff only, with
   final fail-closed reporting. Validation never retries.
3. PR/external conflation: live external checks on PRs flake and leak secrets;
   local-only checks on schedule miss drift. Fixed by event-split gates:
   `!= schedule` local/internal, `== schedule` external.
4. Hardcoded consumer generator: fixed script path breaks every non-matching
   repo. Fixed by config-owned commands; empty optional stage omits its job.

Structural fix: one `docs-site` primitive renders the whole DAG from one
`DocsSpec`, so stages cannot drift. Symptom patches (per-repo static files)
remain possible but no longer required.
