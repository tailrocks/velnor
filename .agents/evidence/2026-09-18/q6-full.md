Q6 decision. All reads done; no files touched.

## Correction to the brief

`lanes` input bytes are **not** identical everywhere: `/tmp/jd-preview.yml:15-19` is `github (default) | velnor | both`, default `github` — inverted polarity vs the other five copies (velnor default). This matters: it matches the generic default for that family (see below), so it's precedent, not drift.

## Core finding: `both` is already dead in every conditional copy

All conditional runs-ons test only `inputs.lanes == 'github'` (docs, hygiene, cache-cleanup, renovate copies) — `both` matches no branch and silently takes the velnor else-leg. `both` ≡ velnor there today, provably, by reading the expressions. Only the two matrix copies (preview build jobs, reuse-compliance) implement real fan-out. And the generic code has already ruled twice: renovate renders `[velnor, github]` with **no `both`** (`renovate.rs:174-180`, conditional shape), while versioned-tool keeps `both` **only** with matrix fan-out (`release.rs:2669-2748`). So the `both` ruling follows the shape, and the shape follows whether anything lane-specific varies per leg.

## Per-family shape: conditional runs-on everywhere, `both` dropped everywhere

Shared expression (renovate's exact shape, generalized by polarity — **no `|| push || PR` event terms**; those exist in PR copies only because their static default is velnor while PR/push go github; generic statics are event-independent, and Q6 fixes "default-lane behavior unchanged"):

- Default-github family: `runs-on: ${{ (github.event_name == 'workflow_dispatch' && inputs.lanes == 'velnor') && fromJSON('<velnor>') || '<github>' }}`
- Default-velnor file: renovate-identical, `== 'github'` then-branch. Quoting follows renovate's `fromJSON('…')` single-quote convention, labels from config.

**(a) Preview — conditional.** Generic preview has nothing lane-varying to fan out over: no per-lane `mise_data_dir` split (cf. jd-preview matrix configs), fixed lane-neutral artifact names (`debian-packages`, `preview-metadata`), push-gated publish (dispatch builds only). Matrix would force lane-suffixed names plus consumer-pattern surgery (the machinery at jd-preview:552/679). Input (Both repos; `selected_runner` maps Both→github, matching the PR copy's github default):

```
  workflow_dispatch:
    inputs:
      lanes:
        description: github (default) | velnor
        type: choice
        default: github
        options: [github, velnor]
```

Thread the conditional into **every** job uniformly (PR copy threads source/assemble/publish too, jd-preview:37/474/537/658), including push-gated publish — one substitution point, harmless dead expression there. Matrix truth table collapses to two rows (dispatch→velnor, else static github); single leg per run keeps lane-neutral names collision-free. This answers the doc's open artifact question: stay lane-neutral.

**(b) Docs-site — conditional.** All jobs take one `runner: &str` (`docs_site.rs:304+`) — single substitution point. Same github-default input bytes as preview (`docs_runner` maps Both→github, `docs_site.rs:112-114`). No `CI_LANE` env added (generic docs has none; out of scope). **Migration note (pre-existing, not Q6's):** PR docs copy defaults velnor with schedule→velnor, but generic Both-static is github everywhere — the override preserves the generic default, so Jackin's docs schedule/dispatch-default lane reconciles at migration time regardless.

**(c) Scheduled-checks — conditional, per-profile, with a homogeneity rule.** Profiles already declare their own lane (`profile_runs_on`, `check_profiles.rs:412`). Rule: `lanes_input = true` requires all non-macos profiles in the file to share one lane L (usage error naming the file otherwise — same fail-closed style as `select_profiles`); input default = L; dispatch flips github↔velnor per job; **macos profiles always render static runs-on** (desktop-cadence precedent: `lanes` input present but unthreaded into `macos-26` jobs — jd-desktop-cadence:42/70). Reuse-compliance's matrix is overkill to preserve: its `writer` flag has no consumer (no `if: matrix.config.writer` in any step), the job publishes nothing, dual-run asserts one predicate twice. Migration note: required status contexts become lane-neutral (no `(Velnor)`/`(GitHub)` legs).

**(d) Maintenance — conditional, input inserted above existing inputs.** `render_maintenance` has two runner placeholders (`__MAINTENANCE_PRUNE_RUNNER__`/`__CACHE_RUNNER__`, both `configured_runner(config, cache_lane)`) — both go conditional. Lanes block renders **first**, above `pull_request_number` (lanes-first ordering precedent: jd-cache-cleanup:10-18). Both→github default, same bytes as preview. Safe by construction: the github-lane setup block is already `runner.environment == 'github-hosted'`-gated, so a dispatch-to-velnor run skips it. Dual-leg cache deletion would race (delete-bound + retention-gate assume one runner) — another reason `both` must go here.

## `both` ruling: drop from options in all four families

- Preview: dropped behavior = dual-lane build that publishes GitHub-only anyway (assemble pattern jd-preview:552 and lane input :679 both resolve `both`→GitHub; the Velnor leg's artifacts upload and are never downloaded). No load-bearing consumer; fleet-down recovery needs one selectable lane.
- Docs-site / maintenance: `both` ≡ velnor today (expression text); dropping the option changes zero executable behavior, removes a misleading alias.
- Scheduled-checks: `both` dual-run has no consumer (dead `writer`, no lane-specific outputs).

## Declare-arg surface

Uniform per-row bool **`lanes_input = true`**, default false/absent = today's bytes exactly. Plumbing:

- New `Args::flag("lanes_input")` in `primitives/mod.rs` (no bool accessor exists today — only string/strings/integer/string_tables). Add `"lanes_input"` to each family's schema list.
- Scheduled-checks: slots into existing row args next to `name`/`profiles`/`events`.
- Preview: read the flag **before** the `args.keys().is_empty()` default-row branch (`release.rs:199`) — a row declaring only `lanes_input` must not be forced down the full-contract declared-spec path.
- Docs-site / maintenance: schemas are `&[]` with ignored `_args` today — add the key, thread into `docs_runner` call + `render_triggers`, and into `render_maintenance`'s trigger + two runner placeholders.
- Shared helper (new, next to `Args` or a small `primitives/lanes.rs`): `lanes_dispatch_input(default) -> &'static str` + `lanes_runs_on(config, default) -> String`, subsuming renovate's `writer_dispatch_inputs`/`writer_runs_on`/`velnor_labels_json`/`json_string`. Renovate migrates onto it in-slice **only if byte-identical** (shapes match exactly for default=Velnor); otherwise leave it.
- Fail-closed rule (renovate's no-escalation law, `renovate.rs:171-173`): `lanes_input = true` with `config.runners != Both` is a usage error — a dispatch must never select an undeclared lane.

## Byte-identical when undeclared

Trigger blocks (bare `workflow_dispatch:` for preview/docs/checks; maintenance keeps only `pull_request_number`), every `runs-on`, artifact names, concurrency, gates. No `CI_LANE`, no writer flags, no matrix scaffolding added anywhere.

## Not decided

1. Lanes-vs-`mode` input ordering in preview when drill modes are also declared — I didn't read `inject_dispatch_modes`. Recommendation: lanes first (PR-copy lanes-first precedent); implementer confirms.
2. `Args::flag` vs `Args::bool` naming — implementer's call.
3. Renovate consolidation if not byte-identical — leave the private copies; do not force it.