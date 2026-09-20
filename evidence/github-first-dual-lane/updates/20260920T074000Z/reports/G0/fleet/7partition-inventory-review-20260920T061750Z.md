# G0 seven-partition distribution inventory review

Review date: 2026-09-20 UTC  
Scope: read-only source/inventory verification. This is not a G0/G2 gate or an execution, release, install, or publication approval.

## Exact evidence

The submitted inventory and its companion report were rehashed:

| Object | SHA-256 | Result |
|---|---|---|
| `g0-distribution-inventory-20260920T061750Z.json` | `502c5368612b06669e4524075456c08a3d27c5b027279f190eb612849b1f08c7` | exact |
| `g0-distribution-inventory-20260920T061750Z.md` | `495fd21dd1d939a5cb34e4adf997ad83e21a16931c3a2721125d380d05bafe1e` | exact |
| `distribution-workload-contract/workload-contract-20260920T001239Z-corrected.json` | `1132d45af4a583c88925302d06d2c0d330b5eee0e01131a4cc41df94840359d4` | exact |

The six raw-capture objects were also rehashed against the inventory metadata: capture metadata `f8d073509f3c11471e878a596f48f312af57598f0e676493996f6a6e98c590bc`, workflow sources `96b2ed5becf5028b40c57217cb0ec193a891566c8f1924cf66f7dd56b6356c97`, workflow dedup `55d6751ecb179520fac926d232e3bdf8c3946682cf20c13e5964300af21f554d`, rulesets `563e37d4ebc5e1ea74d18f73a82fda1f08d4dc436010d08382097145a61ce447b`, check targets `9b10bdcfa2280d1671c9f7e4e7d8198790aa2969c3013fc0cc5be1b09033b26b`, and identity/churn `b44e0a4e4e0dc0563bb96484fc8e3d36839e724a35a756ec8e0031b93060a00b`.

## What is proven

- Scope is exactly seven repositories: `velnor-apt`, `holla-apt`, `homebrew-tablerock`, `homebrew-ruxel`, `homebrew-parallax`, `homebrew-holla`, and `homebrew-velnor`.
- The seven pinned repository commits resolve, and every source-file git-blob listed by the inventory resolves to the recorded blob at that commit. This verifies source identity, not execution.
- There are 23 nonempty expected workload IDs: 4 + 4 Apt workloads, 3 each for the four generic Homebrew repositories, and 3 for `homebrew-velnor`. Platform/provider fields are present in the rows.
- The inventory retains uncertainty instead of promoting it: 13 rows explicitly carry `unknown`, `missing`, `future`, or `not_wired` states. Examples include Apt clean-client proof, generic macOS-native proof, and all `homebrew-velnor` workflow/native proof.
- Source-declared producer→feed/tap→install/release edges are recorded. `velnor-apt` has a source release graph; `holla-apt` lacks a publisher/release workflow; generic taps have static source/unit workflow evidence only; `homebrew-velnor` has a workflow-directory 404 and only formula/README source.

## Boundary and failures still required

The raw capture contains workflow/ruleset/check-target source objects (1304 workflow rows and 171 ruleset/check references), but no jobs, run attempts, artifacts, logs, dispatches, installers, or published feeds. Required-context names are not app/provider bindings. Therefore the inventory cannot prove expected-job execution, terminal conclusions, child graph, artifact/digest binding, provider/runner identity, release assets, feed/tap publication, or install/upgrade behavior.

The `homebrew-velnor` 404 is retained as an unknown/failure state; it must not become a successful workflow inventory. Static source edges must not become live distribution edges. Any checker consuming this object must fail closed on missing execution/provenance fields and must not treat `gate_status=not_evaluated` or `source_derived_not_execution` as success.

## Verdict

Evidence integrity and source/workload inventory shape: **verified, bounded**.  
G0/G2 distribution acceptance: **not proven / not approved**.  
Next proof must bind independently acquired API run/check/artifact/feed/install records to these exact repository revisions and expected workloads, with absence/unknown states remaining failures where the contract requires proof.
