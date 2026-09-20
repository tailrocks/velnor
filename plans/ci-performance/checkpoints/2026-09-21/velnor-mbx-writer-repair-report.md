# MBX PR writer repair

Status: author-side validation complete; parent and an independent reviewer must
still review the patch before integration.

Scratch checkout: `/tmp/velnor-mbx-writer-repair`.
The source patch is `/tmp/velnor-mbx-writer-repair.patch`.
SHA-256: `399a405eefd4a53b3c5893a8e9baf9b232f07ab7f41fd8114511e70cbc0c679c`.
It applies cleanly with `git apply --check` to a fresh `HEAD` archive. It
contains only these five source files (generated outputs are intentionally
omitted):

* `.github-gen/sources/actions/report-velnor-ci-outcomes/action.yml`
* `crates/velnor-workflow/src/lib.rs`
* `crates/velnor-workflow/src/primitives/ir.rs`
* `crates/velnor-workflow/src/s2/mod.rs`
* `crates/velnor-workflow/src/s2/primitives/ir.rs`

The repair removes three enabling conditions. Setup and the optional PR writer
now share a typed GitHub cache mode; only `objects` has a directory-export
implementation and unsupported modes emit no writer. The writer checks the
explicit `MBX_REMOTE_MODE=read-only` boundary before invoking `mbx`, and emits
one grouped `GITHUB_OUTPUT` update. Legacy and S2 renderers both emit export and
save phase markers and pass their local status/outcome into the report action.
The report action keeps local export duration, cache-save step duration, status,
and archive size separate, labels service durability `durable_unknown`, and
censors incomplete or invalid marker pairs.

Focused Rust checks passed:

* `same_repository_pr_mbx_writer_is_post_check_and_fail_closed`: 2 passed.
* `generated_report_action_matches_authoritative_template`: 2 passed.
* `report_action_classifies_cache_outcomes_for_github_and_velnor_lanes`: 2 passed.
* `report_action_` filter: 6 passed.
* `cargo clippy --locked -p velnor-workflow --all-targets --all-features -- -D warnings` passed.
* `cargo fmt --manifest-path crates/velnor-workflow/Cargo.toml --all -- --check` passed.

The generator built with the isolated target and regenerated the scratch tree.
Pinned actionlint 1.7.12 passed the generated Rust workflow; pinned ShellCheck
0.11.0 and both `/bin/bash` 3.2 and `/opt/homebrew/bin/bash` 5.3 passed the
report-action script syntax. Generated writer probes are in
`/tmp/velnor-mbx-writer-shell.qhDu2o`: writable export made one export call and
reported `exported`; export failure reported `export_failed`; explicit
`MBX_REMOTE_MODE=read-only` made no `mbx` call and reported `read_only`.

The report-action probe is in `/tmp/velnor-mbx-report-probe2.ZDCcBI`. A valid
case produced 1 second export and 1 second save durations, `export_local`,
`save_step`, and `durable_unknown`. Removing the export-end marker produced
null durations and `export_local_phase_invalid`/`save_step_phase_invalid`.

This does not prove GitHub cache persistence, remote MBX reuse, or a warm-cache
hit. `actions/cache/save` is recorded as a step outcome; the service receipt is
outside this workflow report boundary. Parent must regenerate generated action
and workflow files from the canonical sources after review.
