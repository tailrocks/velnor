# Investigation queue

These are unaccepted hypotheses and correctness investigations. They do not
count as completed experiments. Keep independent review and real measurements
attached before accepting a remedy. Ranking remains provisional until full
cross-repository timing collection completes.

| Item | Evidence / enabling condition | Next controlled test | Owner / dependency |
| --- | --- | --- | --- |
| Rust stages | Scanner emits fmt → nextest → Clippy command strings; one visible Actions step | Typed stage equivalence, deliberate failures, cold/warm ordering comparison | Velnor agent; independent Jackin review |
| Desktop tool boundary | Historical Jackin broad mise task setup and nested desktop PR task | Verify transitive required tools and distinct assertions before changing task graph | Jackin inventory then implementation |
| Embedded UI prerequisite | Parallax #109 builds Bun inside Cargo; no declared product transfer | Isolated producer/consumer run with exact UI artifact and inputs | Parallax inventory then product model |
| MBX transport | Historical generator job spends 65 s in setup; #967 implements directory transport | Inspect exact PR diff, measure before/after bytes/import/quota/compiler statistics | Queue; do not duplicate #967 |
| Candidate generator product | Historical preparation 59 s and publication 6 s categorized as cleanup | Preserve compatibility guards, measure reuse and separate telemetry | Queue; depends on stage/product graph |
| Candidate runtime bootstrap | New runtime schema cannot use old published pin; candidate artifact is currently produced after unit checks | Establish verified build-once pre-release runtime before planning the new schema | Blocking dependency for stage rollout; author/reviewer notified |
| Docker mutable seed | Historical job starts with explicitly empty dependency cache mounts | Cold vs compatible seed including producer cost | Queue; inspect #952 actual diff first |
| Scheduler outcome | Parallax scheduler succeeds while dispatched child fails policy | Correlate exact child, wait for terminal result, align provider policy | Queue |
| Unknown-file fallback | PR #968 adds Markdown plus evidence JSON; all 17 units selected | Pure Markdown control vs unmatched JSON with recorded plan reasons | Parent diagnosis; collector evidence |
| Scan metadata churn | `--check` says workflow bytes match but scan fingerprint differs after campaign additions | Compare unchanged-main archive and single input additions before narrowing identity | Queue; do not weaken drift checks |
| Generic scan exclusions | `s2/scan/file_walk.rs` hardcodes `config/fleet/velnor-host.env` as owned output | Derive exclusions from actual output ownership, fixture generic roots | Queue; crate genericity rule |
| Primitive watch regression | Parallax source-2 migration drops Bun `ui/` prefixes and adds unrelated Cargo inputs to Maple | Compare scanner-owned closure with emitted primitive watches; root/nested fixtures and negative plans | Velnor agent after gate validation; migration remains uncommitted |
| Candidate versus pinned renderer | f8 policy downloaded candidate then rejected its closure as the declared pin | Separate verified renderer identities; reject tampering before execution | Parallax agent implementing policy tests |
| Dirty generator identity | Local source build reports HEAD closure despite dirty generator source | Reproduce content identity and Cargo rerun behavior; preserve local development | Velnor agent queued |
| Per-member cache selection | New Velnor PR #969 head 2445c647 fixes collapsed reusable MBX/sccache selection | Independently review actual source, execute mixed-member scenarios, integrate without duplicate mechanism | Parent inspected diff; independent review queued |
| Parallax release obligations | PR #111 removes references to deleted preview/release/SDK workflows and replaces ten assertions with rehearsal checks | Map obligations across clean-room regeneration 8419de70 and current product declarations before accepting changed coverage | Independent review queued; do not blindly cherry-pick test removal |

## Relevance diagnostic under way

The first pushed treatment is documentation **and JSON evidence**, source
`c17c6ad09c4eb9617fbf8372914f50b9195a67ec`, run `35480462500`.
The plan selected every unit. The exact source condition in
`selection_for_diff` is conservative full selection for an unmatched file.
This is not evidence that an unknown input can safely be ignored.

Local control at source `aab1d75838820b1e6e6c6ea29a1a8e5147cf397c`, compared
with `e94b48406c4ed206fce2bbf39b788264e72cf39c`, contains only the Markdown
README addition and selects only `docs`. The same current local runtime
selects all units for the JSON treatment. Plans are saved under observations.
The control has not yet been executed as a separate real CI run; no UI or
latency claim follows from local planning alone.

## Clean-main generation control

A detached checkout at `/tmp/velnor-ci-baseline-e94b4840` isolates unchanged
main from active implementation edits. It uses the same built generator as
the campaign checkout. `--check --plain` passes the initial output/scan check,
then reports no provisioned renderer for pin `0dc79895`. Local-only retry
with `--pin-build` builds that exact pin (36.44 s reported Cargo release build)
and fails D19 on:

- `.github/actionlint.yaml`
- `.github/ci/.github-actions-generator-state`
- `.github/workflows/ci-runtime-products.yml`

Thus main has a pre-existing pinned-render mismatch, separate from the new
campaign's scan-fingerprint mismatch. Neither failure is waived. Candidate
runtime distribution and pin convergence must make the final generated tree
reproducible before acceptance. The detached checkout creates no additional
working branch and has no modifications.
