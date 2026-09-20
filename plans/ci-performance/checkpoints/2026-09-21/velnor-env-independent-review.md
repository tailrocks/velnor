# Environment partition independent review

Reviewed `/tmp/velnor-env-parent-snapshot.patch` and `/tmp/velnor-env-signature.patch`; both SHA256 `c16fedc00d8d7c3202febea7290d6b50c2db32f2ad2cdcc0d556e6890aafb3cd`, so no author delta was present.

Verdict: HOLD.

1. The test-only `render_kind_unit_workflow` compatibility method in both legacy and S2 calls `render_kind_unit_workflows(...).into_iter().next()`. `render_kind_units` delegates to it. Once one kind has multiple effective-env partitions, tests inspect only the first BTreeMap partition and can miss the others. Production callers use the full vector, but the test shim weakens proof. Remove the lossy shim or make tests select/iterate explicit partition paths; add a two-partition assertion covering both emitted callees and caller references.
2. `validate_config_unit_workflow_file_budget` counts `config.units` paths before the actual generated file/caller graph exists. The limit is per caller's unique local reusable-workflow references, so this is not the measured contract. It can reject adopted surfaces (no generated unit callees) or any future caller graph that omits config units. Validate actual emitted `uses: ./.github/workflows/*.yml` references per caller after rendering, including static/adopted templates, or explicitly prove/document the stricter global policy and test its intended scope.
3. Parent nextest evidence shows two real fixtures still read unsuffixed filenames after partitioning: `tests/platform_prerequisites.rs:291` (`ci-unit-swift.yml`, consumer inherits product env) and `:351` (`ci-unit-rust.yml`, declared env). They fail with `No such file or directory`; update tests to derive the emitted partition path and retain assertions.

Effective env propagation itself is coherent: product output env is inherited into the consumer map, prerequisite task env remains task-local, and `nested_unit_workflow_file`, `kind_from_unit_workflow_file`, caller construction, prepare-cargo selection, and required-caller construction all use the unit's exact partition path. Absent and explicit nonempty maps are separated by the digest; an explicitly empty table remains equivalent to absent, which is correct if it exports no variables.

Evidence: `/tmp/velnor-env-parent-nextest.log`, 1905 passed (3 leaky), 2 failed; exact failures at `platform_prerequisites.rs:28` from the two unsuffixed reads. Current integration source was read-only at `07bc3e23` with the env patch unstaged.
