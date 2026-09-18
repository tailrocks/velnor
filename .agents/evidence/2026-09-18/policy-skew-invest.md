# Policy schema-skew investigation (R2m blocker)

Heads: `origin/main` = 40206d9f, `origin/feat/r2m-flip` = 17d4867b, base product = a6fa8d4a.
Worktrees: `/tmp/r2m-402`, `/tmp/r2m-v3`, `/tmp/r2m-a6` (detached, created this session under /tmp).
Probe dir: `/tmp/skew-probe` (flipped-config copy). Read-only elsewhere; nothing pushed/merged.

Premise correction: main@40206d9f carries TWO loaders — legacy s1 `src/config`
(`CONFIG_SCHEMA = 1`, `src/config/mod.rs:31`) and s2 `src/s2/config`
(`CONFIG_SCHEMA = 2`, `src/s2/config/mod.rs:33`). The s2 loader file is
byte-identical between main and flip (`diff -q` → IDENTICAL), so Q2 citations
from main's `s2/config` apply to the flip's loader verbatim. The a6fa8d4a tree
has no `src/s2/` at all (s1-only binary, no dispatch).

## 1. ALL base-unparseable fields (empirical, a6-built binary)

Method: built the a6 product (`cargo build -p velnor-workflow` in /tmp/r2m-a6)
and ran its real validator entry (`policy --workflow-root`, `policy.rs:219`)
against the flipped config, neutralizing one rejected field per iteration.
serde stops at the first unknown field (TOML tables iterate alphabetically),
so iteration, not a single run, enumerates the set. Cross-checked against the
a6 structs (`WorkflowSection` src/config/mod.rs:172-264,
`UnitSection` :532-611, all `#[serde(deny_unknown_fields)]`).

| # | key (table) | flipped line | s1 counterpart | probe error (verbatim `unknown field`) |
|---|---|---|---|---|
| 1 | `trust` (`[[units]]` docker) | 264 | `requires_trusted` | `unknown field 'trust'` (reproduces CI exactly) |
| 2 | `automatic_providers` (`[workflow]`) | 19 | `automatic` | `unknown field 'automatic_providers'` |
| 3 | `concurrency_group` (`[workflow]`) | 35 | `velnor_concurrency_group` | `unknown field 'concurrency_group'` |
| 4 | `default_dispatch_providers` (`[workflow]`) | 20 | `default_dispatch_runner` | `unknown field 'default_dispatch_providers'` |
| 5 | `providers` (`[workflow]`) | 18 | `runners` | `unknown field 'providers'` |
| 6 | `rust_needs` (`[workflow]`) | 28 | `velnor_rust_needs` | `unknown field 'rust_needs'` |
| 7 | `selectors` (`[workflow.selectors.*]`, incl. `runs_on`) | 38-44 | `github_runner` + `velnor_labels` (+`velnor_runner_group`) | `unknown field 'selectors'` at 38:11 |

Terminal proof of completeness — after neutralizing all 7, struct
deserialization SUCCEEDS and the parser reaches the schema gate
(`src/config/mod.rs:1533-1544`):

```
error: generation config /private/tmp/skew-probe/.github-gen/velnor-workflow.toml
  has schema 2; this generator reads schema 1 only
```

So the full base-unparseable set is the 7 fields above PLUS:

| 8 | `schema = 2` (top level) | 1 | `schema = 1` | parses as i64, then fails the post-parse gate (`schema_error`, :1536-1539). Struct parse runs BEFORE the gate (`parse`, :74-81) — hence `unknown field` precedes any schema error in CI. |

NOT rejected (parse-level): `primitive = "provider-matrix"` (`[[declare]]`,
line 156) — `DeclareRow.primitive` is a free string (`:1241-1252`) and a6
`validate_declare_row` (:1663+) never matches primitive names, so the VALUE is
invisible to the base parser. All of `[cache.*]`, `[policy]`, `[release]`,
`[scan]`, `[generator]`, `[[static_files]]`, other `[[units]]`/`[[declare]]`
rows parse (proven by reaching the schema gate).

Other validator-parsed configs: none strict. `configured_velnor_policy`
(`policy.rs:2221-2307`) reads `.github/ci/project.toml` (`RUNTIME_CONFIG`,
:59) and the generation `[workflow]` table as generic `toml::Value` (:2227,
`generation_workflow` :2125-2145) with lenient `.get()` lookups — new keys are
harmless, and the flip only REMOVES the keys it touches (`runners`,
`velnor_labels`, `pull_request_on_velnor`) → `None` → defaults, no type error
possible. The generation config struct is the SOLE strict gate in
`DeclaredTree::read` (`policy.rs:623-668`).

## 2. Per-field: does the s2 loader accept any base-parseable spelling?

No. Every field REQUIRES s2-only spelling. Main's s2 `WorkflowSection`
(`src/s2/config/mod.rs:173-238`) and `UnitSection` (:500-563) both carry
`#[serde(deny_unknown_fields)]` (:173, :500) with ZERO s1 fields and ZERO
`#[serde(alias)]` (grep `serde(alias|alias =` → empty; s1 names
`requires_trusted`/`velnor_labels`/`github_runner`/`default_dispatch_runner`/`runners`/`automatic`
→ zero hits in the file). Omission also changes meaning in every case:

| field | s1 spelling in s2? | omission = identical meaning? | result |
|---|---|---|---|
| `providers` | rejected (no `runners`) | No: absent = `ProviderId::ALL` = all 3 incl. `github-self-hosted` (`s2/config/mod.rs:1583`, `s2/provider.rs:30`) ≠ declared 2 | BLOCKING-A2 |
| `automatic_providers` | rejected (no `automatic`) | No: absent = full universe (:181) ≠ declared 2 | BLOCKING-A2 |
| `default_dispatch_providers` | rejected (no `default_dispatch_runner`) | No: absent = full universe (:186) ≠ declared 2 | BLOCKING-A2 |
| `rust_needs` | rejected (no `velnor_rust_needs`) | No: absent = parallel starts (:219-223) ≠ `dependency-closure` | BLOCKING-A2 |
| `concurrency_group` | rejected (no `velnor_concurrency_group`) | No: absent = no group ≠ group (:233) | BLOCKING-A2 |
| `selectors`/`runs_on` | rejected (no `github_runner`/`velnor_labels`); `ProviderSelector` = `{runs_on}` only (`s2/provider.rs:131-135`, deny_unknown_fields) | No: labels live ONLY here (:189-192); s1 keys cannot carry them | BLOCKING-A2 |
| `trust` | rejected (no `requires_trusted`); `trust: Option<String>` (:539), typed `untrusted-ok`/`trusted-only`, validation :1862 | No: default `untrusted-ok` (:535-536) ≠ `trusted-only` | BLOCKING-A2 |
| `schema = 2` | `schema = 1` fails s2 gate (requires exactly 2, :1382-1388; `CONFIG_SCHEMA=2`, :33) AND routes to the s1 pipeline (`s2/dispatch.rs:123-133`), which cannot render the s2 tree | n/a (gate, not default) | BLOCKING-A2 |

Value-level too: `lane-matrix` is absent from s2 (grep → empty) while s2
registers only `PROVIDER_MATRIX` (`s2/primitives/mod.rs:57`, registry test
:1422-1444); s2 `generate()` fails closed on unknown primitives
(`s2/primitives/mod.rs:682-684`). Keeping the s1 primitive VALUE would break
s2 render. A2 intersection is empty at struct, value, AND default level.

No s2 source change could fix A2 without a shim: accepting any s1 spelling
means adding back a removed field/alias/value to the s2 structs — a
compatibility shim, forbidden by AGENTS.md ("no compatibility shims, aliases,
or deprecation periods"). Per the task rule, that reclassifies to A1-REQUIRED
regardless.

## 3. Candidate-path design (main policy code)

Order in BOTH pipelines (s1 `src/policy.rs` is byte-identical a6↔main;
s2 `src/s2/policy.rs` mirrors it, also byte-identical main↔flip):

- `evaluate()` (`policy.rs:332-343`, same lines in s2): `DeclaredTree::read(&root)?`
  (:337) runs FIRST with `?` — hard error before anything else; `pin_rules`
  (:340) runs after.
- `DeclaredTree::read` (:623-668): strict `config::discover` (:624) + lenient
  policy contract (:639) + entrypoint text scan. No candidate involvement.
- `pin_rules` (:381-446) → `regenerate_and_compare` (:1425-1456): pin resolve
  (:1441) → pin render (:1443) → ONLY on differences, `render_with_candidate`
  (:1448).
- Candidate identity checks (`render_with_candidate`, :1469-1545): manifest
  load + `manifest.closure == wanted` (:1489-1501, `wanted` computed locally
  from git, :1483); digest-before-exec (:1524-1530); `--closure` self-report
  tripwire (:1532-1537); candidate render-compare (:1538-1542).

Could a pinned-parse failure fall back to the verified candidate WITHOUT
weakening fail-closed? NO as sequenced — at `read` time no candidate check has
passed (identity checks need only git+manifest+env, but they run strictly
later). Delegation would require: (a) reordering identity verification ahead
of `read`, (b) a NEW candidate→policy IPC surface (e.g. `candidate
policy-describe` emitting pin/repository/excludes/checks as JSON), (c) trusted
handling of candidate-reported SEMANTIC inputs — `excludes` and
`required_checks` from candidate bytes could suppress rules (fail-OPEN for
semantics; only `generated-tree` stays sound via byte-compare). The pin is
recoverable trustworthily (entrypoint-literal text scan,
`entrypoint_policy_revision`, :696-709), but excludes/checks have no trusted
fallback. Verdict on delegation: HARD, and it expands trusted IPC for no gain —
because the bridge already solves it:

`run_from_env` (`lib.rs:5377`) → `s2::dispatch::run_if_s2()` peeks at the raw
TOML `schema` (`dispatch.rs:123-133`, no struct parse) and routes schema-2
`policy --workflow-root` invocations to `s2::policy` (:66-90). The Enforce step
passes `--workflow-root` (ci-policy.yml:176). s2/policy then parses the s2
config natively and the EXISTING candidate exception applies.

Proven end-to-end (local replay of post-bump `pull_request_target`):
bridge binary (40206d9f build) `policy --workflow-root /tmp/r2m-v3` →
parses (no skew error), 4 pin rules PASS, semantic rules PASS,
`generated-tree` FAIL-Differences with exactly the §6 11-file set (no
candidate bound); then with env-slot candidate (17d4867b debug build,
self-report `ae268173…` == locally computed `--candidate` closure ==
CI candidate name) + crafted manifest → **11/11 PASS** (`generated-tree …
matches the candidate render (ae268173…) … generator change in flight`).
Zero source changes.

## 4. VERDICT

A1-REQUIRED. A2 is impossible (all 8 skew points BLOCKING-A2: s2 rejects every
s1 spelling, every omission changes meaning, and any s2 compat acceptance is a
forbidden shim). No candidate-parse delegation is needed — the minimal A1 is a
base-side pin-bump PR, no source change at all:

1. Pin-bump PR on main: `[generator] revision` a6fa8d4a→40206d9f + regen (s1
   tree; greens under the a6 base binary by PIN-MATCH, no candidate needed —
   the 40206d9f runtime product already exists per §1 triple-link).
2. After merge + runtime-product publish, flip rebases, repoints pin to the new
   HEAD, regens. `pull_request_target` then runs the bridge base binary, which
   dispatches the schema-2 tree to s2/policy → parses → candidate exception →
   green (replayed locally 11/11 above; `s2/policy.rs` identical main↔flip, so
   zero evaluation skew).

What must change: policy BINARY version only (via the existing pin mechanism +
workflow regen); no policy/workflow SOURCE change, no delegation IPC, no shim.
Note (out of scope): the §6 post-merge all-skip shape (pin-check drift from
flip emitter changes) still needs its follow-up pin-bump after the flip lands;
it is a different mechanism from this skew and is unaffected by this verdict.

VERDICT: A1-REQUIRED + base pin-bump to a bridge binary unblocks with zero source changes (proven 11/11 green in replay); A2 has empty spelling intersection.
