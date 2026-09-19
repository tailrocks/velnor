#!/usr/bin/env bash
set -euo pipefail

# External review harness. It creates only temporary JSON inputs and invokes
# the exact detached checker binary; it never edits the implementation tree.
CHECKER="${CHECKER:-/tmp/g2-checker-review.5FXqui/target/debug/velnor-tools}"
OUT="${1:-$(pwd)/checker-g2-fixtures}"
mkdir -p "$OUT"

repeat() {
  local character="$1" count="$2"
  printf '%*s' "$count" '' | tr ' ' "$character"
}

SOURCE="$(repeat a 40)"
OTHER_SOURCE="$(repeat b 40)"
D_A="sha256:$(repeat 1 64)"
D_B="sha256:$(repeat 2 64)"
D_C="sha256:$(repeat 3 64)"
D_MANIFEST="sha256:$(repeat 4 64)"
D_ALT_MANIFEST="sha256:$(repeat 5 64)"

jq -n \
  ' {
      schema_version: 1,
      manifest_id: "checker-g2-review",
      repositories: [range(0; 32) as $i |
        {
          repository: ("owner/repo-" + ($i | tostring)),
          repository_role: "library",
          default_branch: "main",
          expected_workload_ids: ["ci"],
          required_check_contexts_and_apps: [{context: "ci / required", app: "github-actions"}],
          workload_platform_architecture: [{workload_id: "ci", platform: "linux", architecture: "amd64"}],
          provider_eligibility: {github: "eligible", velnor: "eligible"},
          release_applicability: (if $i == 0 then "required" else "not-applicable" end)
        }
      ]
    }' > "$OUT/manifest.json"

jq -n --arg source "$SOURCE" \
  ' {
      schema_version: 1,
      observed_at_utc: "2026-09-20T00:00:00Z",
      repositories: [range(0; 32) as $i |
        {
          repository: ("owner/repo-" + ($i | tostring)),
          default_branch: "main",
          default_branch_sha: $source,
          open_prs: []
        }
      ]
    }' > "$OUT/snapshot.json"

jq -n \
  --arg source "$SOURCE" \
  --arg da "$D_A" \
  --arg db "$D_B" \
  --arg dc "$D_C" \
  --arg dm "$D_MANIFEST" \
  ' {
      schema_version: 1,
      manifest_id: "checker-g2-review",
      snapshot_observed_at_utc: "2026-09-20T00:00:00Z",
      stage: "G2",
      records: [range(0; 32) as $i |
        {
          repository: ("owner/repo-" + ($i | tostring)),
          repository_role: "library",
          default_branch: "main",
          default_branch_sha: $source,
          observed_at_utc: "2026-09-20T00:01:00Z",
          generator_revision: ("b" * 40),
          runtime_product_id: "velnor",
          generator_artifact_digest: ("sha256:" + ("a" * 64)),
          configuration_digest: ("sha256:" + ("b" * 64)),
          generated_tree_digest: ("sha256:" + ("c" * 64)),
          scan_state_digest: ("sha256:" + ("d" * 64)),
          runtime_release_version: "0.1.1",
          runtime_source_sha: ("c" * 40),
          job_image_digest: ("sha256:" + ("e" * 64)),
          expected_workload_ids: ["ci"],
          required_check_contexts_and_apps: [{context: "ci / required", app: "github-actions"}],
          workload_platform_architecture: [{workload_id: "ci", platform: "linux", architecture: "amd64"}],
          provider_eligibility: {github: "eligible", velnor: "eligible"},
          justified_exclusions: [],
          PR_number: null,
          PR_head_sha: null,
          PR_base_sha: null,
          tested_merge_sha: null,
          merge_group_sha: null,
          workflow_path: ".github/workflows/ci.yml",
          workflow_revision: ("d" * 40),
          event: "push",
          run_id: 1,
          run_attempt: 1,
          run_url: "https://github.com/owner/repo/actions/runs/1",
          trigger_source_sha: $source,
          actual_checkout_sha: $source,
          provider: "github",
          runner_name: "ubuntu-24.04",
          host_id: "github-hosted",
          expected_jobs: [{job_id: "ci", workload_id: "ci", provider: "github", platform: "linux", architecture: "amd64"}],
          actual_job_ids: ["ci"],
          actual_job_conclusions: {ci: "success"},
          logs: ["https://github.com/owner/repo/actions/runs/1"],
          child_run_links: [],
          required_checks: [{context: "ci / required", app: "github-actions", job_id: "ci", conclusion: "success"}],
          release: (if $i != 0 then
            {applicability: "not-applicable", justification: "no release surface"}
          else
            {
              applicability: "required",
              release_channel: "stable",
              release_version: "0.1.1",
              tag_target_sha: $source,
              release_id: "release-1",
              asset_digests: {
                "velnorctl-linux": $da,
                "velnor-runner-linux": $db,
                "velnor-workflow-linux": $dc
              },
              apt_feed_revision_suite_and_candidate: ("feed-sha=feed-1 suite=stable version=0.1.1 manifest=" + $dm),
              homebrew_tap_revision_and_formula: ("tap-sha=tap-1 formula=velnorctl version=0.1.1 manifest=" + $dm),
              manifest: {
                schema: "velnor.application-manifest.v1",
                product_id: "velnor",
                channel: "stable",
                version: "0.1.1",
                source_repository: "tailrocks/velnor",
                source_ref: "refs/tags/v0.1.1",
                source_commit: $source,
                release_tag: "v0.1.1",
                release_id: "release-1",
                manifest_sha256: $dm,
                artifacts: [
                  {name: "velnorctl-linux", target: "linux-amd64", kind: "archive", sha256: $da, size: 1},
                  {name: "velnor-runner-linux", target: "linux-amd64", kind: "archive", sha256: $db, size: 1},
                  {name: "velnor-workflow-linux", target: "linux-amd64", kind: "archive", sha256: $dc, size: 1}
                ],
                components: [
                  {name: "velnorctl", crate: "velnorctl", version: "0.1.1", binary: "velnorctl", targets: ["linux-amd64"]},
                  {name: "velnor-runner", crate: "velnor-runner", version: "0.1.1", binary: "velnor-runner", targets: ["linux-amd64"]},
                  {name: "velnor-workflow", crate: "velnor-workflow", version: "0.1.1", binary: "velnor-workflow", targets: ["linux-amd64"]}
                ]
              }
            }
          end),
          install: (if $i != 0 then
            {applicability: "not-applicable", justification: "no installed product"}
          else
            {
              applicability: "required",
              install_upgrade_test_environment: {
                os_image: "ubuntu-24.04",
                platform: "linux",
                architecture: "amd64",
                runner: "ubuntu-24.04",
                workspace: "/tmp/clean",
                path: "/usr/bin"
              },
              installed_binary_identity: {
                product_id: "velnor",
                channel: "stable",
                version: "0.1.1",
                source_sha: $source,
                manifest_sha256: $dm,
                binaries: [
                  {name: "velnorctl", path: "/usr/bin/velnorctl", sha256: $da},
                  {name: "velnor-runner", path: "/usr/bin/velnor-runner", sha256: $db},
                  {name: "velnor-workflow", path: "/usr/bin/velnor-workflow", sha256: $dc}
                ]
              },
              upgrade_from: null,
              switch_from: null,
              service_manager_result: "systemd success",
              functional_result: "success"
            }
          end),
          owner: "distribution",
          reviewer: "independent",
          gate_status: "pass",
          blocker: null,
          next_action: null
        }
      ]
    }' > "$OUT/evidence-base.json"

run_case() {
  local name="$1" expected="$2" filter="$3"
  local evidence="$OUT/evidence-$name.json" report="$OUT/report-$name.json"
  jq --arg source "$SOURCE" --arg dm "$D_MANIFEST" --arg alt "$D_ALT_MANIFEST" "$filter" \
    "$OUT/evidence-base.json" > "$evidence"
  set +e
  "$CHECKER" evidence-check --stage G2 --manifest "$OUT/manifest.json" \
    --snapshot "$OUT/snapshot.json" --evidence "$evidence" --json > "$report" 2>&1
  local status=$?
  set -e
  local report_status
  report_status="$(jq -r '.status // "checker-error"' "$report" 2>/dev/null || printf 'checker-error')"
  printf '%s\texpected=%s\texit=%s\treport=%s\n' "$name" "$expected" "$status" "$report_status" \
    | tee -a "$OUT/results.tsv"
}

: > "$OUT/results.tsv"

# Expected pass: control baseline.
run_case baseline pass '.'

# Hostile mutations which should be rejected by a complete product checker.
# These currently expose false-green paths in this candidate.
run_case manifest-component-missing fail \
  '.records[0].release.manifest.components |= .[:-1] | .records[0].install.installed_binary_identity.binaries |= .[:-1]'
run_case digest-binding fail \
  '(.records[0].release.manifest.manifest_sha256 = $alt) |
   (.records[0].release.apt_feed_revision_suite_and_candidate |= gsub($dm; $alt)) |
   (.records[0].release.homebrew_tap_revision_and_formula |= gsub($dm; $alt)) |
   (.records[0].install.installed_binary_identity.manifest_sha256 = $alt)'
run_case arch-mismatch fail \
  '.records[0].install.install_upgrade_test_environment.architecture = "arm64"'
run_case service-unsupported fail \
  '.records[0].install.service_manager_result = "not-applicable: systemd unavailable"'
run_case upgrade-paths-absent fail \
  '.records[0].install.upgrade_from = null | .records[0].install.switch_from = null'
run_case same-version-replacement fail \
  '.records[0].install.upgrade_from = "stable/0.1.1" | .records[0].install.switch_from = "stable/0.1.1"'
run_case source-tag-identity fail \
  '.records[0].release.manifest.source_repository = "other/repo" |
   .records[0].release.manifest.source_ref = "refs/heads/evil" |
   .records[0].release.manifest.release_tag = "v9.9.9"'
run_case producer-consumer-chain fail \
  '.records[0].release.apt_feed_revision_suite_and_candidate = ("0.1.1 " + $dm) |
   .records[0].release.homebrew_tap_revision_and_formula = ("0.1.1 " + $dm)'
run_case stale-schema fail \
  '.records[0].release.manifest.schema = "velnor.package-release.v1"'

echo "fixtures written to $OUT"
