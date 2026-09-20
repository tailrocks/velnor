# Independent review: check-profile workflow concurrency

Scope: reviewed only the unstaged diff against the index for `crates/velnor-workflow/src/primitives/check_profiles.rs` and `crates/velnor-workflow/src/s2/primitives/check_profiles.rs`. No source, index, or build changes. No tests run by reviewer.

Verdict: BLOCK on two concurrency-key collisions.

- `crates/velnor-workflow/src/primitives/check_profiles.rs:409` and `crates/velnor-workflow/src/s2/primitives/check_profiles.rs:396`: PR numbers and non-PR `github.run_id` values are placed in the same untagged numeric slot. If they match, a PR run with `cancel-in-progress: true` shares a group with a push, schedule, or dispatch run and can cancel that signal. Namespace the suffix (for example, `pr-<number>` versus `run-<run_id>`).

- `crates/velnor-workflow/src/primitives/check_profiles.rs:366,409` and `crates/velnor-workflow/src/s2/primitives/check_profiles.rs:363,396`: the workflow stem comes from a case-sensitive filename, but GitHub concurrency group names are case-insensitive. The file validators at `crates/velnor-workflow/src/config/mod.rs:1951` and `crates/velnor-workflow/src/s2/config/mod.rs:1719` allow case-variant stems, and rendered-path collision checks compare exact paths only (`crates/velnor-workflow/src/primitives/mod.rs:915`, `crates/velnor-workflow/src/s2/primitives/mod.rs:782`). Declarations `foo.yml` and `Foo.yml` therefore collide on PR runs. Reject case-insensitive stem duplicates or derive a stable group identifier that remains distinct under GitHub's case-insensitive comparison.

The event cancellation condition is otherwise scoped to `pull_request`; the accepted explicit events are `push`, `pull_request`, and `workflow_dispatch`, with schedules added separately. Provider selection and profile rendering are unchanged in this diff.

Official evidence (retrieved 2026-09-21):
- [Control workflow concurrency](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency): one running and one pending run per group; a new pending run replaces the existing pending run even when in-progress cancellation is disabled; group names are case-insensitive; names must be unique across workflows; conditional `cancel-in-progress` is supported.
- [GitHub context](https://docs.github.com/en/actions/reference/workflows-and-actions/contexts#github-context): `github.run_id` is unique among workflow runs in a repository and stable across reruns. The docs do not define it as disjoint from pull-request numbers.

Implementer reports 65 focused tests passed; reviewer did not rerun them.
