//! Read-only self-hosting runtime producer. A source pin must be usable before
//! its merge publishes a release product; publication is never a prerequisite
//! for validating the commit which creates that product.
//!
//! Root control jobs call `prepare`; dependent jobs use the existing verified
//! current-run transport. Consumer repositories retain published-only setup.

use crate::s2::closure::{CLOSURE_PATHS, CLOSURE_VERSION, DEV_FEATURES, PRODUCT_TAG_PREFIX};
use crate::s2::{shell_quote, workflow_setup_action_repository, ActionPin};

pub(crate) fn owns_runtime(repository: &str) -> bool {
    !repository.is_empty() && repository == workflow_setup_action_repository()
}

/// Control jobs have no unit platform. Recognize documented native selector
/// labels; custom routing must include explicit OS and architecture labels.
pub(crate) fn control_platform(
    config: &crate::s2::ProjectConfig,
) -> Option<crate::s2::provider::Platform> {
    hosted_control_platform(&config.selectors)
}

pub(crate) fn hosted_control_platform(
    selectors: &crate::s2::provider::SelectorMap,
) -> Option<crate::s2::provider::Platform> {
    use crate::s2::provider::{Platform, ProviderId};
    // Planning and Policy always use this selector, even in local-only modes.
    // Mirror control_plane_runner's fixed hosted fallback exactly.
    let Some(selector) = selectors.get(&ProviderId::GithubHosted) else {
        return Some(Platform::LinuxX64);
    };
    let labels = &selector.runs_on;
    let labels = labels
        .iter()
        .map(|label| label.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let has = |label: &str| labels.iter().any(|actual| actual == label);
    let linux = has("linux")
        || labels.iter().any(|label| {
            matches!(
                label.as_str(),
                "ubuntu-22.04" | "ubuntu-24.04" | "ubuntu-22.04-arm" | "ubuntu-24.04-arm"
            )
        });
    let macos = has("macos") || has("macos-26");
    let x64 = has("x64") || has("x86_64") || has("ubuntu-22.04") || has("ubuntu-24.04");
    let arm64 = has("arm64")
        || has("aarch64")
        || has("ubuntu-22.04-arm")
        || has("ubuntu-24.04-arm")
        || has("macos-26");
    match (linux, macos, x64, arm64) {
        (true, false, true, false) => Some(Platform::LinuxX64),
        (true, false, false, true) => Some(Platform::LinuxArm64),
        (false, true, false, true) => Some(Platform::MacosArm64),
        _ => None,
    }
}

pub(crate) fn validate_control_platform(
    config: &crate::s2::ProjectConfig,
) -> Result<(), crate::s2::GeneratorError> {
    if owns_runtime(&config.repository) && control_platform(config).is_none() {
        return Err(crate::s2::GeneratorError::usage("owner runtime control jobs need an unambiguous native platform: use ubuntu-24.04, ubuntu-24.04-arm, macos-26, or explicit Linux/X64, Linux/ARM64, macOS/ARM64 selector labels; custom labels alone do not declare a runtime ABI"));
    }
    Ok(())
}

/// One hosted, read-only source producer per native consumer platform.
/// Consumers depend on this job instead of racing the post-merge publisher.
pub(crate) fn render_root_job(
    repository: &str,
    revision: &str,
    platforms: &std::collections::BTreeSet<crate::s2::provider::Platform>,
) -> String {
    render_root(repository, revision, platforms, None)
}

/// Privileged maintenance may consume only a pin already reachable from the
/// protected default branch. A dispatch branch cannot supply new executable code.
pub(crate) fn render_protected_root_job(
    repository: &str,
    revision: &str,
    platform: Option<crate::s2::provider::Platform>,
) -> String {
    render_root(
        repository,
        revision,
        &platform.into_iter().collect(),
        Some("${{ github.event.repository.default_branch }}"),
    )
}

fn render_root(
    repository: &str,
    revision: &str,
    platforms: &std::collections::BTreeSet<crate::s2::provider::Platform>,
    protected_branch: Option<&str>,
) -> String {
    use crate::s2::provider::Platform;
    let runners = platforms
        .iter()
        .map(|platform| match platform {
            Platform::LinuxX64 => "ubuntu-24.04",
            Platform::LinuxArm64 => "ubuntu-24.04-arm",
            Platform::MacosArm64 => crate::s2::MACOS_HOSTED_RUNS_ON,
        })
        .map(crate::s2::yaml_scalar)
        .collect::<Vec<_>>()
        .join(", ");
    let (source_ref, gate, prepare) = match protected_branch {
        Some(branch) => (
            crate::s2::yaml_scalar(&format!("refs/heads/{branch}")),
            "    if: github.event_name == 'schedule' || github.event_name == 'workflow_dispatch'\n",
            prepare_at(repository, revision, ".", false, false),
        ),
        None => (
            "${{ github.event.workflow_run.head_sha || github.sha }}".to_owned(),
            "",
            prepare_revision(repository, revision),
        ),
    };
    format!(
        "  runtime:\n    name: Prepare renderer (${{{{ matrix.runner }}}})\n{gate}    permissions:\n      contents: read\n    timeout-minutes: 40\n    strategy:\n      fail-fast: false\n      matrix:\n        runner: [{runners}]\n    runs-on: ${{{{ matrix.runner }}}}\n    steps:\n      - name: Checkout exact integration source\n        uses: {checkout}\n        with:\n          ref: {source_ref}\n          fetch-depth: 0\n          persist-credentials: false\n{prepare}{upload}",
        checkout = ActionPin::Checkout.reference(),
        upload = crate::s2::workflow_runtime_artifact_upload(revision),
    )
}

pub(crate) fn prepare_revision(repository: &str, revision: &str) -> String {
    prepare_at(repository, revision, ".", false, true)
}

pub(crate) fn prepare_trusted_validator(repository: &str, revision: &str) -> String {
    prepare_at(repository, revision, "policy-checkout", true, false)
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered emitter boundary keeps the credential-free source protocol reviewable"
)]
fn prepare_at(
    repository: &str,
    revision: &str,
    root: &str,
    trusted_validator: bool,
    validate_integration: bool,
) -> String {
    let paths = CLOSURE_PATHS
        .iter()
        .map(|path| shell_quote(path))
        .collect::<Vec<_>>()
        .join(" ");
    let setup = if trusted_validator {
        crate::s2::workflow_runtime_setup_at_checkout_path(
            crate::s2::provider::ProviderId::GithubHosted, repository, revision, "policy-setup-action",
        ).replace(&format!("          rev: {revision}\n"), &format!("          rev: {revision}\n          checkout-path: ${{{{ github.workspace }}}}/policy-checkout\n"))
    } else {
        crate::s2::workflow_runtime_setup(crate::s2::provider::ProviderId::GithubHosted, repository, revision)
    }.replace(
        "        id: runtime\n",
        "        id: runtime\n        if: steps.runtime-source.outputs.published == 'true'\n",
    );
    let audited_head = if validate_integration {
        "${{ github.event.workflow_run.head_sha || github.event.pull_request.head.sha || github.sha }}".to_owned()
    } else {
        revision.to_owned()
    };
    let publish_candidate = validate_integration;
    format!(
        r#"      - name: Resolve exact renderer source and published product
        id: runtime-source
        shell: bash
        working-directory: {root}
        env:
          GH_TOKEN: ${{{{ github.token }}}}
          RENDERER_REVISION: {revision}
          AUDITED_HEAD: {audited_head}
        run: |
          set -euo pipefail
          sha256() {{ if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }}
          git cat-file -e "$RENDERER_REVISION^{{commit}}"
          git merge-base --is-ancestor "$RENDERER_REVISION" HEAD || {{ echo "::error::renderer pin must be reachable from the checked-out integration candidate" >&2; exit 1; }}
          listing="$(git ls-tree -r "$RENDERER_REVISION" -- {paths})"
          test -n "$listing"
          closure() {{ printf '%s\nclosure-version:{version}\nfeatures:%s\nprofile:%s\n' "$(LC_ALL=C sort <<<"$1")" "$2" "$3" | sha256 | awk '{{print $1}}'; }}
          release="$(closure "$listing" '' release)"
          candidate="$(closure "$listing" '{features}' debug)"
          head_listing="$(git ls-tree -r "$AUDITED_HEAD" -- {paths})"
          [[ "$(closure "$head_listing" '{features}' debug)" == "$candidate" ]] || {{ echo "::error::generator source differs from its declared pin; commit the source, pin it, and regenerate before merge" >&2; exit 1; }}
          if [[ '{validate_integration}' == true ]]; then
            integration_listing="$(git ls-tree -r HEAD -- {paths})"
            [[ "$(closure "$integration_listing" '{features}' debug)" == "$candidate" ]] || {{ echo "::error::integration source closure differs from the pinned renderer" >&2; exit 1; }}
          fi
          response="$RUNNER_TEMP/runtime-product-response"
          if gh api --include "repos/$GITHUB_REPOSITORY/releases/tags/{tag}${{release:0:16}}" > "$response"; then
            published=true
          else
            status="$(awk '/^HTTP\// {{print $2; exit}}' "$response")"
            [[ "$status" == 404 ]] || {{ echo "::error::runtime lookup failed with status $status; source fallback is allowed only for a missing product" >&2; exit 1; }}
            published=false
          fi
          {{ echo "published=$published"; echo "candidate=$candidate"; echo "release=$release"; echo "candidate_name=velnor-workflow-candidate-${{candidate:0:16}}-$RUNNER_OS-$RUNNER_ARCH"; }} >> "$GITHUB_OUTPUT"
{setup}      - name: Prepare clean pinned renderer checkout
        if: steps.runtime-source.outputs.published != 'true'
        shell: bash
        working-directory: {root}
        env:
          RENDERER_REVISION: {revision}
          GH_TOKEN: ""
          GITHUB_TOKEN: ""
          RUSTC_WRAPPER: ""
          RUSTC_WORKSPACE_WRAPPER: ""
          RUSTFLAGS: ""
          CARGO_ENCODED_RUSTFLAGS: ""
        run: |
          set -euo pipefail
          sha256() {{ if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }}
          source="$RUNNER_TEMP/renderer-source"
          git worktree add --detach "$source" "$RENDERER_REVISION"
          test -z "$(git -C "$source" status --porcelain --untracked-files=all)"
          cd "$source"
      - name: Check renderer formatting
        if: steps.runtime-source.outputs.published != 'true'
        working-directory: ${{{{ runner.temp }}}}/renderer-source
        env:
          GH_TOKEN: ""
          GITHUB_TOKEN: ""
          RUSTC_WRAPPER: ""
          RUSTC_WORKSPACE_WRAPPER: ""
          RUSTFLAGS: ""
          CARGO_ENCODED_RUSTFLAGS: ""
        run: env -i PATH="$PATH" HOME="$HOME" RUSTUP_HOME="${{RUSTUP_HOME:-$HOME/.rustup}}" CARGO_HOME="$RUNNER_TEMP/renderer-cargo-home" CARGO_TARGET_DIR="$RUNNER_TEMP/renderer-target" cargo fmt --all -- --check
      - name: Check renderer Clippy
        if: steps.runtime-source.outputs.published != 'true'
        working-directory: ${{{{ runner.temp }}}}/renderer-source
        env:
          GH_TOKEN: ""
          GITHUB_TOKEN: ""
          RUSTC_WRAPPER: ""
          RUSTC_WORKSPACE_WRAPPER: ""
          RUSTFLAGS: ""
          CARGO_ENCODED_RUSTFLAGS: ""
        run: env -i PATH="$PATH" HOME="$HOME" RUSTUP_HOME="${{RUSTUP_HOME:-$HOME/.rustup}}" CARGO_HOME="$RUNNER_TEMP/renderer-cargo-home" CARGO_TARGET_DIR="$RUNNER_TEMP/renderer-target" cargo clippy --locked --package velnor-workflow --all-targets --no-default-features --features {features} -- -D warnings
      - name: Build exact renderer source
        if: steps.runtime-source.outputs.published != 'true'
        working-directory: ${{{{ runner.temp }}}}/renderer-source
        env:
          GH_TOKEN: ""
          GITHUB_TOKEN: ""
          RUSTC_WRAPPER: ""
          RUSTC_WORKSPACE_WRAPPER: ""
          RUSTFLAGS: ""
          CARGO_ENCODED_RUSTFLAGS: ""
        run: env -i PATH="$PATH" HOME="$HOME" RUSTUP_HOME="${{RUSTUP_HOME:-$HOME/.rustup}}" CARGO_HOME="$RUNNER_TEMP/renderer-cargo-home" CARGO_TARGET_DIR="$RUNNER_TEMP/renderer-target" cargo build --locked --package velnor-workflow --bin velnor-workflow --no-default-features --features {features}
      - name: Verify source runtime identity
        if: steps.runtime-source.outputs.published != 'true'
        shell: bash
        working-directory: {root}
        env:
          RENDERER_REVISION: {revision}
          EXPECTED_CLOSURE: ${{{{ steps.runtime-source.outputs.candidate }}}}
          GH_TOKEN: ""
          GITHUB_TOKEN: ""
          RUSTC_WRAPPER: ""
          RUSTC_WORKSPACE_WRAPPER: ""
          RUSTFLAGS: ""
          CARGO_ENCODED_RUSTFLAGS: ""
        run: |
          set -euo pipefail
          sha256() {{ if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }}
          source="$RUNNER_TEMP/renderer-source"
          test -z "$(git -C "$source" status --porcelain --untracked-files=all)"
          binary="$RUNNER_TEMP/renderer-target/debug/velnor-workflow"
          [[ "$("$binary" --revision)" == "$RENDERER_REVISION" ]]
          [[ "$("$binary" --closure)" == "$EXPECTED_CLOSURE" ]]
          echo "$RUNNER_TEMP/renderer-target/debug" >> "$GITHUB_PATH"
          echo "VELNOR_WORKFLOW_PINNED_BINARY=$binary" >> "$GITHUB_ENV"
      - name: Bind runtime closure
        shell: bash
        working-directory: {root}
        env:
          EXPECTED_CLOSURE: ${{{{ steps.runtime.outputs.closure || steps.runtime-source.outputs.candidate }}}}
          GH_TOKEN: ""
          GITHUB_TOKEN: ""
          RUSTC_WRAPPER: ""
          RUSTC_WORKSPACE_WRAPPER: ""
          RUSTFLAGS: ""
          CARGO_ENCODED_RUSTFLAGS: ""
        run: |
          set -euo pipefail
          sha256() {{ if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }}
          [[ "$(velnor-workflow --closure)" == "$EXPECTED_CLOSURE" ]]
          echo "VELNOR_WORKFLOW_RUNTIME_CLOSURE=$EXPECTED_CLOSURE" >> "$GITHUB_ENV"
          if [[ "${{{{ steps.runtime-source.outputs.published }}}}" == true ]]; then
            echo "VELNOR_WORKFLOW_RUNTIME_PROFILE=release" >> "$GITHUB_ENV"
            echo "VELNOR_WORKFLOW_RUNTIME_FEATURES=" >> "$GITHUB_ENV"
          else
            echo "VELNOR_WORKFLOW_RUNTIME_PROFILE=debug" >> "$GITHUB_ENV"
            echo "VELNOR_WORKFLOW_RUNTIME_FEATURES={features}" >> "$GITHUB_ENV"
          fi
          echo "VELNOR_WORKFLOW_POLICY_REVISION={revision}" >> "$GITHUB_ENV"
      - name: Prepare source candidate for trusted policy
        if: {publish_candidate} && steps.runtime-source.outputs.published != 'true' && github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository
        shell: bash
        working-directory: {root}
        env:
          CANDIDATE_CLOSURE: ${{{{ steps.runtime-source.outputs.candidate }}}}
          CANDIDATE_HEAD: ${{{{ github.event.pull_request.head.sha }}}}
          GH_TOKEN: ""
          GITHUB_TOKEN: ""
          RUSTC_WRAPPER: ""
          RUSTC_WORKSPACE_WRAPPER: ""
          RUSTFLAGS: ""
          CARGO_ENCODED_RUSTFLAGS: ""
        run: |
          set -euo pipefail
          sha256() {{ if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }}
          stage="$RUNNER_TEMP/renderer-candidate"
          mkdir -p "$stage"
          install -m 0755 "$(command -v velnor-workflow)" "$stage/velnor-workflow"
          digest="$(sha256 "$stage/velnor-workflow" | awk '{{print $1}}')"
          jq -n --arg profile debug --arg platform "$RUNNER_OS-$RUNNER_ARCH" --arg repository "$GITHUB_REPOSITORY" --arg run_id "$GITHUB_RUN_ID" --arg run_attempt "$GITHUB_RUN_ATTEMPT" --arg revision "$CANDIDATE_HEAD" --arg closure "$CANDIDATE_CLOSURE" --arg build_revision '{revision}' --arg binary_sha256 "$digest" '{{profile: $profile, platform: $platform, repository: $repository, run_id: $run_id, run_attempt: $run_attempt, revision: $revision, closure: $closure, build_revision: $build_revision, binary_sha256: $binary_sha256}}' > "$stage/candidate-manifest.json"
      - name: Publish source candidate for trusted policy
        if: {publish_candidate} && steps.runtime-source.outputs.published != 'true' && github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository
        uses: {upload}
        with:
          name: ${{{{ steps.runtime-source.outputs.candidate_name }}}}
          path: ${{{{ runner.temp }}}}/renderer-candidate
          if-no-files-found: error
          retention-days: 7
          overwrite: true
"#,
        version = CLOSURE_VERSION,
        features = DEV_FEATURES,
        tag = PRODUCT_TAG_PREFIX,
        upload = ActionPin::UploadArtifact.reference(),
    )
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_yaml::Value;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Result<Self, Box<dyn Error>> {
            let root = std::env::temp_dir().join(format!(
                "velnor-bootstrap-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("crates/velnor-workflow/src"))?;
            fs::create_dir_all(root.join("mock-bin"))?;
            fs::write(
                root.join("crates/velnor-workflow/src/lib.rs"),
                "pinned source\n",
            )?;
            fs::write(root.join("mock-bin/gh"), "#!/bin/sh\n[ \"$LOOKUP_STATUS\" = network ] && exit 1\nprintf 'HTTP/2 %s\\n' \"$LOOKUP_STATUS\"\n[ \"$LOOKUP_STATUS\" = 200 ]\n")?;
            command(&root, "chmod", &["+x", "mock-bin/gh"])?;
            command(&root, "git", &["init", "-q"])?;
            command(&root, "git", &["config", "user.name", "Fixture"])?;
            command(
                &root,
                "git",
                &["config", "user.email", "fixture@example.invalid"],
            )?;
            command(&root, "git", &["add", "crates"])?;
            command(
                &root,
                "git",
                &["-c", "commit.gpgsign=false", "commit", "-qm", "source"],
            )?;
            Ok(Self(root))
        }

        fn head(&self) -> Result<String, Box<dyn Error>> {
            command(&self.0, "git", &["rev-parse", "HEAD"])
        }

        fn commit(&self, path: &str, value: &str) -> Result<String, Box<dyn Error>> {
            fs::write(self.0.join(path), value)?;
            command(&self.0, "git", &["add", path])?;
            command(
                &self.0,
                "git",
                &["-c", "commit.gpgsign=false", "commit", "-qm", "fixture"],
            )?;
            self.head()
        }

        fn resolve(
            &self,
            pin: &str,
            audited: &str,
            status: &str,
        ) -> Result<Output, Box<dyn Error>> {
            let script = step_run(
                &super::prepare_revision(super::workflow_setup_action_repository(), pin),
                "Resolve exact renderer source and published product",
            )?;
            Ok(Command::new("bash")
                .args(["-c", &script])
                .current_dir(&self.0)
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        self.0.join("mock-bin").display(),
                        std::env::var("PATH")?
                    ),
                )
                .env("RENDERER_REVISION", pin)
                .env("AUDITED_HEAD", audited)
                .env("LOOKUP_STATUS", status)
                .env("RUNNER_TEMP", &self.0)
                .env("RUNNER_OS", "Linux")
                .env("RUNNER_ARCH", "X64")
                .env(
                    "GITHUB_REPOSITORY",
                    super::workflow_setup_action_repository(),
                )
                .env("GITHUB_OUTPUT", self.0.join("outputs"))
                .output()?)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn command(root: &Path, program: &str, args: &[&str]) -> Result<String, Box<dyn Error>> {
        let output = Command::new(program)
            .args(args)
            .current_dir(root)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    fn step_run(steps: &str, name: &str) -> Result<String, Box<dyn Error>> {
        let yaml: Value = serde_yaml::from_str(&format!("steps:\n{steps}"))?;
        yaml["steps"]
            .as_sequence()
            .and_then(|steps| {
                steps
                    .iter()
                    .find(|step| step["name"].as_str() == Some(name))
            })
            .and_then(|step| step["run"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| format!("missing step {name}").into())
    }

    #[test]
    fn source_bootstrap_only_explicit_missing_product_allows_build() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let pin = fixture.head()?;
        for status in ["200", "404", "401", "403", "500", "network"] {
            let output = fixture.resolve(&pin, &pin, status)?;
            assert_eq!(
                output.status.success(),
                matches!(status, "200" | "404"),
                "{status}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    #[test]
    fn source_bootstrap_binds_pin_head_and_prospective_merge_closure() -> Result<(), Box<dyn Error>>
    {
        let fixture = Fixture::new()?;
        let pin = fixture.head()?;
        let generated = fixture.commit("generated.yml", "generated output\n")?;
        assert!(fixture.resolve(&pin, &generated, "404")?.status.success());
        let integration =
            fixture.commit("crates/velnor-workflow/src/lib.rs", "new base source\n")?;
        let output = fixture.resolve(&pin, &generated, "404")?;
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("integration source closure differs")
        );
        let output = fixture.resolve(&pin, &integration, "404")?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("generator source differs"));
        Ok(())
    }

    #[test]
    fn source_bootstrap_protected_mode_rejects_branch_only_pin_before_lookup(
    ) -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let protected_head = fixture.head()?;
        let branch_pin = fixture.commit(
            "crates/velnor-workflow/src/lib.rs",
            "unmerged branch renderer\n",
        )?;
        command(
            &fixture.0,
            "git",
            &["checkout", "--detach", &protected_head],
        )?;
        fs::write(
            fixture.0.join("mock-bin/gh"),
            "#!/bin/sh\ntouch \"$RUNNER_TEMP/LOOKUP_INVOKED\"\nexit 0\n",
        )?;
        let steps = super::prepare_at(
            super::workflow_setup_action_repository(),
            &branch_pin,
            ".",
            false,
            false,
        );
        let script = step_run(
            &steps,
            "Resolve exact renderer source and published product",
        )?;
        let output = Command::new("bash")
            .args(["-c", &script])
            .current_dir(&fixture.0)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    fixture.0.join("mock-bin").display(),
                    std::env::var("PATH")?
                ),
            )
            .env("RENDERER_REVISION", &branch_pin)
            .env("AUDITED_HEAD", &branch_pin)
            .env("RUNNER_TEMP", &fixture.0)
            .output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("pin must be reachable"));
        assert!(!fixture.0.join("LOOKUP_INVOKED").exists());
        Ok(())
    }

    #[test]
    fn source_bootstrap_compiler_has_only_declared_environment() -> Result<(), Box<dyn Error>> {
        let steps = super::prepare_revision(
            super::workflow_setup_action_repository(),
            "0123456789012345678901234567890123456789",
        );
        for name in [
            "Check renderer formatting",
            "Check renderer Clippy",
            "Build exact renderer source",
        ] {
            let run = step_run(&steps, name)?;
            assert!(run.starts_with("env -i "), "{name}: {run}");
            assert!(run.contains("CARGO_HOME=\"$RUNNER_TEMP/renderer-cargo-home\""));
            assert!(run.contains("CARGO_TARGET_DIR=\"$RUNNER_TEMP/renderer-target\""));
            assert!(!run.contains("RUSTFLAGS="));
            assert!(!run.contains("GH_TOKEN="));
        }
        Ok(())
    }

    #[test]
    fn source_bootstrap_transport_checks_every_binding_before_executing(
    ) -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let pin = fixture.head()?;
        let closure = crate::s2::closure::candidate_closure_of_tree(&fixture.0, &pin)?;
        let stage = fixture.0.join("velnor-workflow-runtime-download");
        fs::create_dir_all(&stage)?;
        let binary = b"#!/bin/sh\ntouch EXECUTED\n";
        fs::write(stage.join("velnor-workflow"), binary)?;
        fs::write(stage.join("velnor-workflow-policy"), binary)?;
        let digest = command(&stage, "sha256sum", &["velnor-workflow"])?;
        let digest = digest.split_whitespace().next().ok_or("missing digest")?;
        let manifest = serde_json::json!({"revision":pin,"repository":"example/owner","platform":"Linux-X64","run_id":"123","run_attempt":"2","job_id":"runtime","policy_revision":pin,"profile":"debug","features":"tui","closure":closure,"policy_closure":closure,"binary_sha256":digest,"policy_binary_sha256":digest});
        let script = step_run(
            &crate::s2::workflow_runtime_download_at(
                crate::s2::provider::ProviderId::GithubHosted,
                &pin,
                ".",
                true,
                true,
            ),
            "Verify Velnor workflow runtime",
        )?;
        let mut cases = Vec::new();
        for field in [
            None,
            Some("revision"),
            Some("repository"),
            Some("platform"),
            Some("run_id"),
            Some("run_attempt"),
            Some("job_id"),
            Some("policy_revision"),
            Some("profile"),
            Some("features"),
            Some("closure"),
            Some("policy_closure"),
            Some("binary_sha256"),
            Some("policy_binary_sha256"),
        ] {
            let mut mutated = manifest.clone();
            if let Some(field) = field {
                mutated[field] = serde_json::Value::String("wrong".to_owned());
            }
            cases.push((format!("{field:?}"), mutated, field.is_none()));
        }
        let equivalent = fixture.commit("README.md", "outside renderer closure\n")?;
        let release_closure = crate::s2::closure::closure_of_tree(
            &fixture.0,
            &pin,
            "",
            crate::s2::closure::PROFILE_RELEASE,
        )?;
        assert_eq!(
            release_closure,
            crate::s2::closure::closure_of_tree(
                &fixture.0,
                &equivalent,
                "",
                crate::s2::closure::PROFILE_RELEASE,
            )?
        );
        let mut release = manifest.clone();
        release["profile"] = "release".into();
        release["features"] = "".into();
        release["closure"] = release_closure.clone().into();
        release["policy_closure"] = release_closure.into();
        release["policy_revision"] = equivalent.clone().into();
        cases.push((
            "source-equivalent published revision".to_owned(),
            release,
            true,
        ));
        let mut wrong_debug_revision = manifest;
        wrong_debug_revision["policy_revision"] = equivalent.into();
        cases.push((
            "debug build revision must be exact".to_owned(),
            wrong_debug_revision,
            false,
        ));
        for (label, manifest, expected) in cases {
            fs::write(stage.join("manifest.json"), serde_json::to_vec(&manifest)?)?;
            let output = Command::new("bash")
                .args(["-c", &script])
                .current_dir(&fixture.0)
                .env("EXPECTED_REVISION", &pin)
                .env("GITHUB_REPOSITORY", "example/owner")
                .env("RUNNER_OS", "Linux")
                .env("RUNNER_ARCH", "X64")
                .env("RUNNER_TEMP", &fixture.0)
                .env("GITHUB_RUN_ID", "123")
                .env("GITHUB_RUN_ATTEMPT", "2")
                .output()?;
            assert_eq!(
                output.status.success(),
                expected,
                "{label}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!fixture.0.join("EXECUTED").exists());
        }
        Ok(())
    }
}
