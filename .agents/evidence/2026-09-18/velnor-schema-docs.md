Schema documentation for the generic-capability config surface (`crates/velnor-workflow/src/config/mod.rs` unless noted). All snippets use neutral `example/*` names. Line numbers are from the `feat/pr994-generic-ci-capabilities` tree.

## 1. Platform prerequisites (`[[units]]` platform + products)

Keys defined at config/mod.rs:581-637; vocabulary in platform.rs:106-141; executors in platform.rs:218-228.

```toml
schema = 1
[generator]
repository = "example/fixture"

[[units]]                                    # :532 — id override/add row
id = "example-app"
os = "macos"          # :584 — any|linux|macos (platform.rs:110)
arch = "aarch64"      # :588 — any|x86_64|aarch64 (platform.rs:125)
capabilities = ["xcode"]  # :592 — replaces scan-derived set (:1075-1090)
mbx = false           # :602 — Rust-only object-transport opt-out

[[units.products]]    # :606 — this unit builds, named in platform.rs:258
name = "framework"    # :619 — lowercase slug (platform.rs:289)
task = "build-framework"  # :620 — mise task (platform.rs:303)
[units.products.env]  # :622 — task outputs consumers receive
FRAMEWORK_DIR = "dist/framework"

[[units]]             # consumer row
id = "example-consumer"
[[units.prerequisites]]  # :610 — edge; validated :1158-1207
producer = "example-app" # :631 — must name a known unit (:2201-2205)
product = "framework"    # :632 — must be declared by producer
task = "build-framework" # :634 — optional override of product task
[units.prerequisites.env] # :636 — task inputs for prepare step
TARGET = "aarch64"
```

Also `[workflow] macos_runner` (:178, default `macos-15`) selects the Apple-lane label that `requires_apple()` units land on (platform.rs:241-251).

## 2. Prepared-tool handoff (`[[declare]] primitive = "prepared-tool"`)

Unit-contract row: schema `tools` + `recipes` (primitives/prepared_tools.rs:1466-1468); parsing rules :1071-1115. Takes **no `file`** (rejected, primitives/mod.rs:1302-1308); empty `units` = all units (:854-857); renders nothing itself, records `PreparedToolNeed`s (:1470-1494).

```toml
[[declare]]                       # DeclareRow :1241 — primitive :1243, units :1246
primitive = "prepared-tool"
units = ["example-app"]

[declare.args.tools]              # tool id -> authorized producer slugs (all slugs, :1088-1107)
example-runner = ["producer-job"]

[declare.args.recipes]            # tool id -> build recipe; every recipe must name a declared tool (:1080-1086)
example-runner = ["cargo build --locked"]
```

Test reference: `tests/prepared_tool_handoff.rs:104`.

## 3. Closure / reuse — purely automatic, no config surface

- Source closure: fixed inputs + canonical form in closure.rs:8-48; no keys anywhere in config.
- Reuse/selection/aggregate: pure functions over `expected.json`/`results.json` and watch globs (reuse.rs:32-31); `tests/closure_reuse.rs` drives the `aggregate` verb with JSON fixtures, no TOML table exists.
- Only touchpoint: `[docs] docs_paths` (:470) feeds the docs reuse digest (docs_site.rs:322). Otherwise reuse is derived, never declared.

## 4. Docker multi-arch publish

Two equivalent surfaces: `[release]` table (config/mod.rs:372-429) or `[declare.args]` on `primitive = "release"` (schema release.rs:118-154). Minimal via `[release]`:

```toml
[release]
enabled = true            # :373
kind = "docker"           # :375 — one of RELEASE_KINDS (:2256-2264)
image = "ghcr.io/example/app"  # :382 — lowercase OCI ref, validated :2285-2324

[[declare]]
primitive = "release"
file = "release.yml"
```

Full build inputs (all optional; empty `platforms` = both arches natively, `DOCKER_PLATFORMS` :2269):

```toml
[release]
dockerfile = "images/app/Dockerfile"  # :393 — repo-relative, no traversal
context = "images/app"                # :394 — same rule
platforms = ["linux/amd64", "linux/arm64"]  # :396 — validated :2274-2278
image_package = "example-svc"         # :387 — workspace pkg compiled into image; default = package
```

Declare-args equivalent (`migration_contract.rs:352-361`): same key names under `[declare.args]` plus `kind`/`image`. Validation tests: config/mod.rs:3622-3650.

## 5. Release/preview bindings, modes, archives, teardown

Tarball bindings render **only** for `kind = "rust-binary"|"native"` (rejected on other kinds, :3293-3325); shape validated even when disabled (:3180-3183).

```toml
[release]                                   # [release] table :372-429
enabled = true
kind = "rust-binary"
package = "example-svc"     # :376
binary = "example-svc"      # :379
targets = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]  # :381
producer_workflow = "verify.yml"  # :398 — trusted workflow_run producer
producer_conclusion = "success"   # :400 — only accepted value (:3330-3336)
modes = ["validate", "build"]     # :404 — subset of RELEASE_MODES (:3173); `publish` never allowed
archive_members = ["README"]      # :407 — bare portable names (:3397-3404)
archive_checksum = "sha256"       # :409 — only accepted value (:3350-3356)
archive_retention_days = 14       # :411 — 1..90 (:3357-3363)

[[release.credential]]        # :415 — one row per mounted credential; struct :436-440
name = "signing"              # :437 — portable alphabet (shell fn name)
setup = "mise run creds:setup"     # :438 — required
teardown = "mise run creds:teardown" # :439 — required; setup without teardown is an error
```

Preview lane (`primitive = "preview"`, schema release.rs:188-201): a bare row (or only `lanes_input`) reuses the `[release]` contract (`preview_content`, release.rs:75-77, 203-210); a row naming `package`/`binary`/`targets`/bindings renders its own `rust-binary` contract (release.rs:364-410). Previews trigger on push, never tags (no `tag_pattern`), never log in (no registry).

```toml
[[declare]]
primitive = "preview"
file = "preview.yml"
[declare.args]
package = "example-svc"
binary = "example-svc"
targets = ["x86_64-unknown-linux-gnu"]
modes = ["validate"]
```

Flag — asymmetry: the declare-args path hardcodes `credentials: Vec::new()` (release.rs:351, 399) and drops `description`, so **setup/teardown pairs are expressible only via `[[release.credential]]`**, never via `[declare.args]`.

## 6. Docs-site pipeline (`[docs]` + `docs-site`)

Contract :450-471; enabled requires `reason` + `[[declare]] primitive = "docs-site" file = "docs.yml"` (:2909-2926).

```toml
[docs]
enabled = true                # :451
reason = "Example public docs" # :452 — required when enabled
site_url = "https://docs.example.com"  # :453 — https://, no whitespace (:2541-2553)
site_dir = "site"             # :455 — required; repo-relative (:2574-2581)
sitemap_path = "sitemap.xml"  # :455 — optional; default "sitemap.xml" (lib.rs:2208)
build_commands = ["mise run docs:build"]  # :458 — required, single-line shell (:2555-2570)
source_link_commands = ["mise run docs:check-source-links"]  # :460
site_link_commands = ["mise run docs:check-site-links"]      # :462
spell_commands = ["mise run docs:spell"]                     # :464
verify_commands = ["mise run docs:verify-deployed"]          # :466
# At least one of source_link/site_link/spell required (:2603-2611).
# Scheduled-external check comes as a pair (:2612-2625):
schedule = "17 4 * * *"       # :456 — 5-field cron (:2529-2537)
external_link_commands = ["mise run docs:check-live"]  # :468 — requires schedule and vice versa
docs_paths = ["docs/**", "**/*.md"]  # :470 — globs feeding the reuse digest

[[declare]]
primitive = "docs-site"       # primitives/mod.rs:73; schema ["lanes_input"] (docs_site.rs:64-65)
file = "docs.yml"
```

Test reference: `tests/docs_site_pipeline.rs:64-74`; neutral deny-probe: `tests/migration_contract.rs:372-385`.

## 7. Check profiles (`[[check_profile]]` + `scheduled-checks`)

Row keys :350-368; validation :2964-3168. File rendered by `primitive = "scheduled-checks"`, schema `name|profiles|events|lanes_input` (check_profiles.rs:54-56).

```toml
[[check_profile]]             # :350
id = "smoke"                  # :351 — required, job-id alphabet (:2630-2636)
name = "Smoke probe"          # :352 — one non-empty line
schedule = "23 2 * * *"       # :353 — 5-field cron (:2661-2674); omissible only in evented files
runner = "github"             # :354 — github|macos|velnor (:3017-3041)
tools = ["cargo-binstall"]    # :356 — mise lock ids (:3130-3168)
tasks = ["check-smoke"]       # :358 — required, mise task refs (:2642-2649)
needs = ["other-profile"]     # :360 — same-file deps only, acyclic (:3076-3092)
timeout_minutes = 30          # :362 — positive (:3091-3097); render default 30 (check_profiles.rs:39)
artifacts = ["logs/"]         # :364 — upload paths (:3098-3106)
status = "required"           # :365 — required|advisory; advisory → continue-on-error (:3119-3126, render :473-475)
[check_profile.env]           # :367 — thresholds as shell identifiers (:2653-2659)
MAX_SECONDS = "300"

[[declare]]
primitive = "scheduled-checks"
file = "scheduled-daily.yml"
[declare.args]
name = "Daily checks"         # optional; default = file stem
profiles = ["smoke"]          # optional; absent = all profiles (:93-94); one shared cadence per file (:142-180)
events = ["push"]             # optional; push|pull_request|workflow_dispatch (:183-230)
```

## 8. Renovate + policy

Renovate keys :271-307; enabled requires `reason`, a `renovate` declare row, and trusted-runner workflow keys unless `lanes = "github"` (:2680-2768). Both primitives take **zero args** (renovate.rs:69-71, 88-90) — everything comes from `[renovate]`.

```toml
[renovate]
enabled = true                # :272
reason = "Example dependency updates"  # :273 — required
schedule = "13 3 * * *"       # :274 — 5-field cron (:2409-2425)
schedules = ["13 3 * * 1"]    # :277 — extra crons, no dup of primary (:2752-2762)
token = "GH_RENOVATE_TOKEN"   # :278 — uppercase secret name, never GITHUB_TOKEN (:2342-2407)
config = "renovate.json"      # :279 — repo-relative path (:2515-2526)
validate = true               # :280 — requires renovate-validate row (:2700-2709)
cache = true                  # :281 — default true (lib.rs:2176; render renovate.rs:308)
lanes = "velnor"              # :286 — velnor|github|both (:2710-2720)
repositories = ["example/other"]  # :290 — owner/repo slugs (:2427-2449); empty = autodiscover
host_rules_secret = "RENOVATE_HOST_RULES_JSON"  # :294 — secret name (:2371-2377)
author = "Example Bot <bot@example.com>"  # :297 — Name <email> (:2451-2471)
signoff = true                # :302 — requires author (:2359-2363)
allowed_commands = ["^mise run lint$"]  # :306 — single-line regexes (:2473-2482)

[[declare]]
primitive = "renovate"
file = "renovate.yml"
[[declare]]
primitive = "renovate-validate"
file = "renovate-validate.yml"

[policy]                      # :680-701, validated :2794-2818
dco_required = true           # :682 — requires "DCO" in external checks (:2807-2816)
ci_required = true            # :684
action_pin_admission = "reviewed-allowlist"  # :695 — only accepted value (:2800-2806)
ruleset_required_status_checks = ["ci-required"]  # :688 — ci-pr.yml job names
ruleset_external_status_checks = ["DCO"]          # :693 — app-reported contexts
actionlint_config_variables_null = true  # :697
exclude_workflows = ["legacy.yml"]       # :700 — basenames skipped by policy
```

## 9. `lanes_input` (dispatch lane override)

A boolean `[declare.args]` flag on `docs-site` (docs_site.rs:64-65), `preview` (release.rs:188-201), `maintenance` (release.rs:236), and `scheduled-checks` (check_profiles.rs:54-56). Requires `[workflow] runners = "both"` + non-empty `velnor_labels` + **no** `velnor_runner_group` (primitives/mod.rs:663-710); renders a `lanes:` dispatch input defaulting to the family's default leg (:646-661).

```toml
[workflow]
runners = "both"              # :181
velnor_labels = ["self-hosted", "example-lane"]  # :190

[[declare]]
primitive = "maintenance"     # contract from [maintenance]: schedule :317, producers :321, max_deletes :326
file = "maintenance.yml"
[declare.args]
lanes_input = true
```

Note: `lanes_input` is unrelated to `[renovate] lanes` (writer execution lanes, :286) — different key, different layer.

## 10. `tag_pattern`

```toml
[release]
tag_pattern = "example-svc-v*"  # :419 — one non-empty line, no whitespace (lib.rs-side valid_tag_pattern, :3226-3232)
```

Default `v*`; renders into release tag triggers. No declare-args equivalent gap — `release` declare args accept `tag_pattern` too (release.rs:148, parsed :348).

## 11. Registry auth (triple, docker-only)

```toml
[release]
kind = "docker"
registry = "ghcr.io"                    # :424 — lowercase host[:port] (:3263-3268)
registry_username_secret = "REGISTRY_USERNAME"  # :426 — secret name (:3269-3276)
registry_password_secret = "REGISTRY_PASSWORD"  # :428 — secret name (:3277-3284)
```

All-or-nothing triple (:3241-3286); renders only for `kind = "docker"` (:3294-3309); absent keeps GHCR automatic-token login. Declare-args path accepts the same triple (release.rs:144-146, parsed :312-323).

## 12. `runtime-products` — purely automatic, owner-only

No config keys. Renders `ci-runtime-products.yml` (const runtime_products.rs:41) **only** when the repo is the setup-action owner (runtime_products.rs:156-159); a declared row on any other repo fails closed. Follows `[workflow] github_runner`/`macos_runner` for builder labels; ARM64 builder label is fixed (:67).

## Flags

- **Purely automatic (no keys):** closure identity (closure.rs), reuse/selection/aggregate (reuse.rs), `runtime-products` producer (above). `maintenance` content itself has no declare args beyond `lanes_input` — its contract lives in `[maintenance]`.
- **Unclear/undocumented keys — none blocking, two observations:**
  1. `[release] description` (:391) is silently dropped on the declare-args path (`description: String::new()`, release.rs:327) and only recorded into generated `project.toml` (lib.rs:2411-2412), never rendered into workflows. Documented nowhere in the struct (no doc comment, unlike neighbors).
  2. `[[release.credential]]` has no declare-args equivalent (both declared paths hardcode `credentials: Vec::new()`, release.rs:351, 399) — consumers using the `[[declare]]`-args release surface cannot express setup/teardown; they must use the `[release]` table. Asymmetry, not ambiguity, but worth one line in docs.
- Error-message naming drift (cosmetic): validation errors say `[[unit]]`/:2094-2096 and `[[static_file]]`/:2220 while the serde keys are `[[units]]` (:115) and `[[static_files]]` (:117).
