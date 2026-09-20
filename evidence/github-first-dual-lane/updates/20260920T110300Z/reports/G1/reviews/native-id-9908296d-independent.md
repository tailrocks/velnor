# Independent native product release-id review

Status: bounded PASS for the `9908296d` canonical release-id fix. This is not publication approval.

Review date: 2026-09-20

## Exact scope and pins

- Source repository: `/private/tmp/dual-lane-native-product-v3`
- Branch: `codex/github-first-native-product-v3`
- Reviewed commit: `9908296d28d27e0d5b993d1e48ea7a96bc31db83`
- Reviewed tree: `6c2dcb0805f81cf044cec8016dfd7f58aee2537b`
- Parent comparison: `a8fd18e61edd02fd0db741a7edf13ecafdf4b0e1`
- Remote branch resolved to the same `9908296d28d27e0d5b993d1e48ea7a96bc31db83`
- Isolated detached review tree: `/private/tmp/velnor-native-990-review`
- Review tree was clean; no source files were edited.

The commit changes only `crates/velnor-workflow/src/s2/primitives/release.rs` (51 insertions, 54 deletions). It removes the renderer's post-construction string replacement and puts the canonical admission expression in the stable rendered shell itself.

## Source findings

- `release.rs:2866-2876`: the stable existing-product path reads `product-manifest.json` with
  `jq -er '.release_id | strings | select(test("^[1-9][0-9]*$"))'` and compares that exact string to the provider ID. JSON numbers are no longer coerced.
- `release.rs:2993-2994`: renderer substitutions are now only `{binary}` and `__RUNTIME_ASSETS__`; the prior `.replace()` that masked an unsafe template is gone.
- `release.rs:7304-7314`: rendered-output assertions require the string-only expression and forbid the old `numbers | tostring` product-manifest expression.
- `release.rs:7408-7463`: the test locates the exact rendered line from the generated publish job and executes that line through Bash. It does not rewrite the generated shell before execution.
- The rendered cases are: canonical string `12345` (pass/output preserved), JSON number (reject), leading-zero string (reject), numeric overflow `18446744073709551616` (reject), and provider mismatch (reject).
- Preview source remains unchanged in this diff. Its provider extraction is at `release.rs:2138-2139`; preview manifest/provider checks remain at `release.rs:2216-2223`. The diff contains no preview-path edit.

## Verification evidence

All commands ran against the exact detached tree.

- `rtk cargo test --locked -p velnor-workflow --lib native_identity_release_wires_release_build_and_deb_publishing -- --test-threads=1`: PASS, `2 passed; 1744 filtered out`.
- `rtk cargo test --locked -p velnor-workflow --lib rendered_native_product_census_binds_source_ref_under_nounset -- --test-threads=1`: PASS, `1 passed; 1745 filtered out`.
- `rtk cargo test --locked -p velnor-workflow --lib rendered_native_product_preview_census_splits_targets_nominally -- --test-threads=1`: PASS, `1 passed; 1745 filtered out`.
- The first concurrent census attempt was externally SIGKILLed by resource contention; serial reruns passed. No test assertion failed.
- `rtk cargo fmt --all -- --check`: PASS.
- `rtk git diff --check a8fd18e61edd02fd0db741a7edf13ecafdf4b0e1..9908296d28d27e0d5b993d1e48ea7a96bc31db83`: PASS.
- `rtk cargo clippy --locked -p velnor-workflow --lib --tests -- -D warnings`: PASS, `cargo clippy: No issues found`.
- `rtk actionlint`: PASS, exit 0. This validates checked-in workflows; it does not constitute a live publication test.

## Numeric boundary check

Local `jq` is `jq-1.7.1-apple`. The exact provider expression `.id | numbers | tostring` preserved these values byte-for-byte:

```text
9007199254740991
9007199254740992
9223372036854775807
18446744073709551615
18446744073709551616
```

An independent shell check using the two exact source expressions compared provider `18446744073709551615` with manifest string `"18446744073709551615"` and produced:

```text
provider=18446744073709551615 product=18446744073709551615
```

No arithmetic was applied to the jq values. This is an environment-level jq check, not a claim that a provider transaction is atomic. The generated product admission is string-only, so a numeric manifest cannot be silently rounded into an accepted ID.

## Residual boundary

This commit proves canonical release-id type/admission in the rendered stable rerun path and preserves preview behavior. It does not create producer/native attestation authority, nor does it prove a live provider publication. Those remain outside this bounded fix and require the separate native signer/provider-boundary work.

Verdict: PASS for this exact source fix; no publication/install/dispatch approval.
