//! Typed rolling package-release handoff.
//
// The producer task remains repository-owned: it builds the package bytes and
// writes the declared verified directory. Velnor owns the boundary around
// that directory: exact manifest/identity binding, payload checksums,
// attestation verification, serialized rolling publication, and the explicit
// consumer updater invocation. No consumer repository or product name is
// embedded here.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use super::{Args, Primitive, RenderCtx, Rendered, PACKAGE_RELEASE};
use crate::s2::provider::ProviderId;
use crate::s2::{
    github_expression, selector_runs_on_yaml, shell_quote, workflow_runtime_setup, ActionPin,
    GeneratorError, ProjectConfig, GENERATED_HEADER,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackageReleaseSpec {
    build_tasks: Vec<String>,
    package_dir: String,
    manifest_schema: String,
    source_repository: String,
    source_ref: String,
    payloads: Vec<String>,
    supporting_assets: Vec<String>,
    channel: String,
    release_tag: String,
    release_title_prefix: String,
    consumer_repository: String,
    consumer_branch: String,
    updater: String,
    updater_token_secret: String,
    update_commit_message: String,
    concurrency_group: String,
}

pub(crate) struct PackageRelease;

impl Primitive for PackageRelease {
    fn id(&self) -> &'static str {
        PACKAGE_RELEASE
    }

    fn schema(&self) -> &'static [&'static str] {
        &[
            "build_tasks",
            "channel",
            "concurrency_group",
            "consumer_branch",
            "consumer_repository",
            "manifest_schema",
            "package_dir",
            "payloads",
            "release_tag",
            "release_title_prefix",
            "source_ref",
            "source_repository",
            "supporting_assets",
            "update_commit_message",
            "updater",
            "updater_token_secret",
        ]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let spec = parse_spec(args)?;
        if !ctx.config.repository.is_empty() && ctx.config.repository != spec.source_repository {
            return Err(GeneratorError::usage(format!(
                "package-release source_repository {} must match scanned repository {}",
                spec.source_repository, ctx.config.repository
            )));
        }
        let content = render_workflow(ctx.config, &spec);
        let file = ctx.file.filter(|file| !file.is_empty()).ok_or_else(|| {
            GeneratorError::usage(
                "package-release renders preview.yml and needs file = preview.yml",
            )
        })?;
        if file != "preview.yml" {
            return Err(GeneratorError::usage(format!(
                "package-release must declare preview.yml, found {file}"
            )));
        }
        Ok(Rendered {
            files: std::iter::once((
                Path::new(".github/workflows/preview.yml").to_owned(),
                content,
            ))
            .collect(),
            ..Rendered::default()
        })
    }
}

fn required_string(args: &Args<'_>, key: &str) -> Result<String, GeneratorError> {
    args.string(key)?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| GeneratorError::usage(format!("package-release needs a non-empty {key}")))
}

fn validate_one_line(key: &str, value: &str) -> Result<(), GeneratorError> {
    if value.is_empty() || value.contains(['\n', '\r']) {
        return Err(GeneratorError::usage(format!(
            "package-release {key} must be one non-empty line"
        )));
    }
    Ok(())
}

fn validate_repository(key: &str, value: &str) -> Result<(), GeneratorError> {
    let mut parts = value.split('/');
    let Some(owner) = parts.next() else {
        return Err(GeneratorError::usage(format!(
            "package-release {key} must be an owner/name repository"
        )));
    };
    let Some(name) = parts.next() else {
        return Err(GeneratorError::usage(format!(
            "package-release {key} must be an owner/name repository"
        )));
    };
    if parts.next().is_some()
        || owner.is_empty()
        || name.is_empty()
        || !owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(GeneratorError::usage(format!(
            "package-release {key} must be an owner/name repository over the GitHub name alphabet"
        )));
    }
    Ok(())
}

fn validate_asset_name(key: &str, value: &str) -> Result<(), GeneratorError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains(['/', '\\', '\n', '\r'])
        || value.bytes().any(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+' | b'~'))
        })
    {
        return Err(GeneratorError::usage(format!(
            "package-release {key} must contain bare portable asset names"
        )));
    }
    Ok(())
}

fn validate_relative_directory(value: &str) -> Result<(), GeneratorError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
        || value.contains(['\n', '\r'])
    {
        return Err(GeneratorError::usage(
            "package-release package_dir must be a relative directory without traversal",
        ));
    }
    Ok(())
}

fn validate_secret_name(value: &str) -> Result<(), GeneratorError> {
    if value.is_empty()
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_uppercase() || byte == b'_' || (index > 0 && byte.is_ascii_digit())
        })
    {
        return Err(GeneratorError::usage(
            "package-release updater_token_secret must be an uppercase GitHub secret name",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn parse_spec(args: &Args<'_>) -> Result<PackageReleaseSpec, GeneratorError> {
    let build_tasks = args.strings("build_tasks")?.unwrap_or_default();
    if build_tasks.is_empty() {
        return Err(GeneratorError::usage(
            "package-release needs at least one build_tasks entry",
        ));
    }
    for task in &build_tasks {
        if !crate::s2::config::valid_check_profile_task(task) {
            return Err(GeneratorError::usage(format!(
                "package-release build_tasks entry {task} is not a plain mise task"
            )));
        }
    }

    let package_dir = required_string(args, "package_dir")?;
    validate_relative_directory(&package_dir)?;
    let manifest_schema = required_string(args, "manifest_schema")?;
    validate_one_line("manifest_schema", &manifest_schema)?;
    if manifest_schema.chars().any(char::is_whitespace) {
        return Err(GeneratorError::usage(
            "package-release manifest_schema must not contain whitespace",
        ));
    }

    let source_repository = required_string(args, "source_repository")?;
    validate_repository("source_repository", &source_repository)?;
    let source_ref = required_string(args, "source_ref")?;
    let source_branch = source_ref.strip_prefix("refs/heads/").ok_or_else(|| {
        GeneratorError::usage("package-release source_ref must be a refs/heads/<branch> reference")
    })?;
    if !crate::s2::runtime::valid_branch(source_branch) {
        return Err(GeneratorError::usage(format!(
            "package-release source_ref has invalid branch {source_branch}"
        )));
    }

    let payloads = args.strings("payloads")?.unwrap_or_default();
    if payloads.len() != 6 {
        return Err(GeneratorError::usage(format!(
            "package-release payloads must declare exactly six entries, found {}",
            payloads.len()
        )));
    }
    for payload in &payloads {
        validate_asset_name("payloads", payload)?;
    }
    let mut names = BTreeSet::from([
        "release-manifest.json".to_owned(),
        "identity.json".to_owned(),
    ]);
    if payloads
        .iter()
        .any(|payload| !names.insert(payload.clone()))
    {
        return Err(GeneratorError::usage(
            "package-release payloads must contain unique names",
        ));
    }

    let supporting_assets = args.strings("supporting_assets")?.unwrap_or_default();
    if supporting_assets.is_empty() {
        return Err(GeneratorError::usage(
            "package-release needs non-empty supporting_assets for sidecars and provenance",
        ));
    }
    for asset in &supporting_assets {
        validate_asset_name("supporting_assets", asset)?;
        if !names.insert(asset.clone()) {
            if matches!(asset.as_str(), "release-manifest.json" | "identity.json") {
                return Err(GeneratorError::usage(format!(
                    "package-release supporting_assets contains reserved metadata name {asset}"
                )));
            }
            return Err(GeneratorError::usage(format!(
                "package-release asset {asset} is declared as both payload and supporting asset"
            )));
        }
    }

    let channel = required_string(args, "channel")?;
    if channel != "preview" {
        return Err(GeneratorError::usage(
            "package-release currently implements the six-payload preview contract only",
        ));
    }
    let release_tag = required_string(args, "release_tag")?;
    if !crate::s2::runtime::valid_package(&release_tag) {
        return Err(GeneratorError::usage(
            "package-release release_tag must be a portable tag token",
        ));
    }
    let release_title_prefix = args
        .string("release_title_prefix")?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Preview".to_owned());
    validate_one_line("release_title_prefix", &release_title_prefix)?;
    let consumer_repository = required_string(args, "consumer_repository")?;
    validate_repository("consumer_repository", &consumer_repository)?;
    let consumer_branch = args
        .string("consumer_branch")?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "main".to_owned());
    if !crate::s2::runtime::valid_branch(&consumer_branch) {
        return Err(GeneratorError::usage(format!(
            "package-release consumer_branch has invalid branch {consumer_branch}"
        )));
    }
    let updater = required_string(args, "updater")?;
    validate_one_line("updater", &updater)?;
    let updater_token_secret = required_string(args, "updater_token_secret")?;
    validate_secret_name(&updater_token_secret)?;
    let update_commit_message = args
        .string("update_commit_message")?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "chore: update verified package metadata".to_owned());
    validate_one_line("update_commit_message", &update_commit_message)?;
    let concurrency_group = args
        .string("concurrency_group")?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "package-release-preview".to_owned());
    validate_one_line("concurrency_group", &concurrency_group)?;

    Ok(PackageReleaseSpec {
        build_tasks,
        package_dir,
        manifest_schema,
        source_repository,
        source_ref,
        payloads,
        supporting_assets,
        channel,
        release_tag,
        release_title_prefix,
        consumer_repository,
        consumer_branch,
        updater,
        updater_token_secret,
        update_commit_message,
        concurrency_group,
    })
}

fn release_runner(config: &ProjectConfig) -> (ProviderId, String) {
    let provider = if config.providers.contains(&ProviderId::GithubHosted) {
        ProviderId::GithubHosted
    } else {
        config
            .providers
            .iter()
            .next()
            .copied()
            .unwrap_or(ProviderId::GithubHosted)
    };
    let runner = config
        .selectors
        .get(&provider)
        .map_or_else(|| "ubuntu-24.04".to_owned(), selector_runs_on_yaml);
    (provider, runner)
}

fn indent_script(script: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    let mut indented = String::new();
    for line in script.lines() {
        let _ = writeln!(indented, "{prefix}{line}");
    }
    indented
}

/// The verified-directory boundary shared by the producer and publisher jobs.
/// It intentionally checks the downloaded directory again: an artifact or
/// release may never be trusted merely because its producer job passed.
#[allow(clippy::too_many_lines)]
fn verification_script(spec: &PackageReleaseSpec) -> String {
    let mut script = String::from(
        r#"set -euo pipefail
dir="$VELNOR_VERIFIED_PACKAGE_DIR"
test -d "$dir"
manifest="$dir/release-manifest.json"
identity="$dir/identity.json"
test -s "$manifest"
test -s "$identity"
expected_files="$(mktemp)"
actual_files="$(mktemp)"
expected_names="$(mktemp)"
actual_names="$(mktemp)"
trap 'rm -f -- "$expected_files" "$actual_files" "$expected_names" "$actual_names"' EXIT
{
  printf '%s\n' "release-manifest.json" "identity.json"
"#,
    );
    for name in &spec.payloads {
        let _ = writeln!(script, "  printf '%s\\n' {}", shell_quote(name));
    }
    for name in &spec.supporting_assets {
        let _ = writeln!(script, "  printf '%s\\n' {}", shell_quote(name));
    }
    script.push_str(
        r#"} | LC_ALL=C sort > "$expected_files"
find "$dir" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort > "$actual_files"
if ! cmp -s "$expected_files" "$actual_files"; then
  echo "::error::verified package directory contains an undeclared or missing file" >&2
  diff -u "$expected_files" "$actual_files" >&2 || true
  exit 1
fi
source_commit="$(jq -er '.source_commit | strings' "$manifest")"
version="$(jq -er '.version | strings' "$manifest")"
[[ "$source_commit" =~ ^[0-9a-f]{40}$ ]] || { echo "::error::manifest source_commit is not 40 lowercase hex" >&2; exit 1; }
[ "$source_commit" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::manifest source_commit is not the checked-out commit" >&2; exit 1; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+-preview\.[0-9]+\+[0-9a-f]{7}$ ]] || { echo "::error::manifest version is not a preview version" >&2; exit 1; }
short_commit="$(printf '%s' "$source_commit" | cut -c1-7)"
version_suffix="$(printf '%s' "$version" | awk -F+ '{print $2}')"
[ "$version_suffix" = "$short_commit" ] || { echo "::error::preview version does not bind its source commit" >&2; exit 1; }
jq -e \
  --arg schema "$EXPECTED_MANIFEST_SCHEMA" \
  --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
  --arg source_ref "$EXPECTED_SOURCE_REF" \
  --arg commit "$source_commit" \
  --slurpfile package_manifest "$manifest" \
  'keys == ["assets","schema","source_commit","source_ref","source_repository","version"] and
   .schema == $schema and .source_repository == $repository and
   .source_ref == $source_ref and .source_commit == $commit and
   (.assets | type == "array" and length == 6)' "$manifest" >/dev/null
jq -e \
  --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
  --arg source_ref "$EXPECTED_SOURCE_REF" \
  --arg commit "$source_commit" \
  --slurpfile package_manifest "$manifest" \
  'keys == ["manifest","source_digest","source_ref","source_repository"] and
   .source_repository == $repository and .source_ref == $source_ref and
   .source_digest == $commit and .manifest == $package_manifest[0]' "$identity" >/dev/null
{
"#,
    );
    for name in &spec.payloads {
        let _ = writeln!(script, "  printf '%s\\n' {}", shell_quote(name));
    }
    script.push_str(
        r#"} | LC_ALL=C sort > "$expected_names"
jq -r '.assets[].name' "$manifest" | LC_ALL=C sort > "$actual_names"
if ! cmp -s "$expected_names" "$actual_names"; then
  echo "::error::manifest asset names do not equal the six declared payloads" >&2
  exit 1
fi
jq -e '(.assets | map(.name) | unique | length == 6)' "$manifest" >/dev/null
for name in \
"#,
    );
    for (index, name) in spec.payloads.iter().enumerate() {
        let suffix = if index + 1 == spec.payloads.len() {
            ""
        } else {
            " \\"
        };
        let _ = write!(script, "  {}{}", shell_quote(name), suffix);
    }
    script.push_str(
        r#"; do
  test -s "$dir/$name"
  expected="$(jq -er --arg name "$name" '[.assets[] | select(.name == $name)] | select(length == 1) | .[0].sha256 | select(test("^[0-9a-f]{64}$"))' "$manifest")"
  actual="$(sha256sum "$dir/$name" | awk '{print $1}')"
  [ "$actual" = "$expected" ] || { echo "::error::payload checksum mismatch: $name" >&2; exit 1; }
done
for name in \
"#,
    );
    for (index, name) in spec.supporting_assets.iter().enumerate() {
        let suffix = if index + 1 == spec.supporting_assets.len() {
            ""
        } else {
            " \\\n"
        };
        let _ = write!(script, "  {}{}", shell_quote(name), suffix);
    }
    script.push_str(
        r#"; do
  test -s "$dir/$name"
done
printf 'version=%s\n' "$version" >> "$GITHUB_OUTPUT"
printf 'source_commit=%s\n' "$source_commit" >> "$GITHUB_OUTPUT"
"#,
    );
    script
}

fn release_asset_names(spec: &PackageReleaseSpec) -> Vec<String> {
    let mut names = vec![
        "release-manifest.json".to_owned(),
        "identity.json".to_owned(),
    ];
    names.extend(spec.payloads.iter().cloned());
    names.extend(spec.supporting_assets.iter().cloned());
    names
}

#[allow(clippy::too_many_lines)]
fn render_workflow(config: &ProjectConfig, spec: &PackageReleaseSpec) -> String {
    let (provider, runner) = release_runner(config);
    let runtime_setup = if provider == ProviderId::GithubHosted {
        workflow_runtime_setup(
            ProviderId::GithubHosted,
            &config.repository,
            &config.workflow_revision,
        )
    } else {
        String::new()
    };
    let checkout = ActionPin::Checkout.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let download = ActionPin::DownloadArtifact.reference();
    let mise = ActionPin::Mise.reference();
    let attest = ActionPin::Attest.reference();
    let source_commit_expr = github_expression("github.sha");
    let workspace_expr = github_expression("github.workspace");
    let build_verify = indent_script(&verification_script(spec), 10);
    let publish_verify = indent_script(&verification_script(spec), 10);
    let updater_token_expr = github_expression(&format!("secrets.{}", spec.updater_token_secret));
    let build_if = github_expression(&format!(
        "github.ref == 'refs/heads/{}'",
        spec.source_ref
            .strip_prefix("refs/heads/")
            .unwrap_or("main")
    ));
    let publish_if = github_expression(&format!(
        "github.event_name == 'push' && github.ref == '{}'",
        spec.source_ref
    ));
    let package_dir_yaml = crate::s2::yaml_scalar(&spec.package_dir);
    let package_dir = spec.package_dir.as_str();
    let channel_yaml = crate::s2::yaml_scalar(&spec.channel);
    let source_repository_yaml = crate::s2::yaml_scalar(&spec.source_repository);
    let source_ref_yaml = crate::s2::yaml_scalar(&spec.source_ref);
    let schema_yaml = crate::s2::yaml_scalar(&spec.manifest_schema);
    let tag_yaml = crate::s2::yaml_scalar(&spec.release_tag);
    let title_yaml = crate::s2::yaml_scalar(&spec.release_title_prefix);
    let consumer_repository_yaml = crate::s2::yaml_scalar(&spec.consumer_repository);
    let consumer_branch_yaml = crate::s2::yaml_scalar(&spec.consumer_branch);
    let updater_yaml = crate::s2::yaml_scalar(&spec.updater);
    let message_yaml = crate::s2::yaml_scalar(&spec.update_commit_message);
    let concurrency_yaml = crate::s2::yaml_scalar(&spec.concurrency_group);
    let source_shell = shell_quote(&spec.source_ref);
    let run_name = format!(
        "Package release · {} · {}",
        github_expression("github.event_name"),
        github_expression("github.ref_name")
    );
    let mut tasks = String::new();
    for task in &spec.build_tasks {
        let _ = writeln!(tasks, "          mise run {}", shell_quote(task));
    }
    let mut attestation_subjects = String::new();
    for name in &spec.payloads {
        let _ = writeln!(
            attestation_subjects,
            "          {workspace_expr}/{package_dir}/{name}"
        );
    }
    let mut publish_attestation_targets = String::new();
    for (index, name) in spec.payloads.iter().enumerate() {
        let suffix = if index + 1 == spec.payloads.len() {
            ""
        } else {
            " \\"
        };
        let _ = writeln!(
            publish_attestation_targets,
            "            \"$PACKAGE_DIR/{name}\"{suffix}",
        );
    }
    let assets = release_asset_names(spec)
        .iter()
        .map(|name| format!("\"$PACKAGE_DIR/{name}\""))
        .collect::<Vec<_>>()
        .join(" \\\n              ");

    let mut output = String::new();
    let _ = writeln!(
        output,
        "{GENERATED_HEADER}name: Package release\nrun-name: {run_name}\n"
    );
    let _ = writeln!(
        output,
        "on:\n  push:\n    branches: [{}]\n  workflow_dispatch:\n",
        crate::s2::yaml_scalar(
            spec.source_ref
                .strip_prefix("refs/heads/")
                .unwrap_or("main")
        )
    );
    let _ = writeln!(
        output,
        "concurrency:\n  group: {concurrency_yaml}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n"
    );
    let _ = writeln!(
        output,
        "jobs:\n  build:\n    name: Verify package release\n    if: {build_if}\n    runs-on: {runner}\n    timeout-minutes: 90\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    outputs:\n      version: {}\n      source_commit: {}\n    env:\n      PACKAGE_DIR: {package_dir_yaml}\n      VELNOR_VERIFIED_PACKAGE_DIR: {workspace_expr}/{package_dir}\n      VELNOR_PACKAGE_CHANNEL: {channel_yaml}\n      EXPECTED_SOURCE_REPOSITORY: {source_repository_yaml}\n      EXPECTED_SOURCE_REF: {source_ref_yaml}\n      EXPECTED_MANIFEST_SCHEMA: {schema_yaml}\n      EXPECTED_SOURCE_COMMIT: {source_commit_expr}\n",
        github_expression("steps.verify.outputs.version"),
        github_expression("steps.verify.outputs.source_commit"),
    );
    let _ = writeln!(
        output,
        "    steps:\n      - name: Checkout source\n        uses: {checkout}\n        with:\n          ref: {source_commit_expr}\n          fetch-depth: 0\n          persist-credentials: false\n{runtime_setup}      - name: Set up Mise\n        uses: {mise}\n        with:\n          install: false\n      - name: Enforce workflow policy\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\n      - name: Build verified package directory\n        env:\n          VELNOR_SOURCE_COMMIT: {source_commit_expr}\n          VELNOR_SOURCE_REF: {source_shell}\n        run: |\n          set -euo pipefail\n          mkdir -p \"$VELNOR_VERIFIED_PACKAGE_DIR\"\n{tasks}      - name: Verify manifest, identity, checksums, and exact file set\n        id: verify\n        run: |\n{build_verify}      - name: Attest declared payloads\n        uses: {attest}\n        with:\n          subject-path: |\n{attestation_subjects}      - name: Upload verified package handoff\n        uses: {upload}\n        with:\n          name: package-release\n          path: {workspace_expr}/{package_dir}\n          if-no-files-found: error\n          retention-days: 2\n",
    );
    let _ = writeln!(
        output,
        "\n  publish:\n    name: Publish rolling package and update consumer\n    needs: build\n    if: {publish_if}\n    runs-on: {runner}\n    timeout-minutes: 30\n    environment: github-preview\n    permissions:\n      contents: write\n    env:\n      PACKAGE_DIR: package\n      VELNOR_VERIFIED_PACKAGE_DIR: {workspace_expr}/package\n      VELNOR_PACKAGE_CHANNEL: {channel_yaml}\n      EXPECTED_SOURCE_REPOSITORY: {source_repository_yaml}\n      EXPECTED_SOURCE_REF: {source_ref_yaml}\n      EXPECTED_MANIFEST_SCHEMA: {schema_yaml}\n      EXPECTED_SOURCE_COMMIT: {source_commit_expr}\n      RELEASE_TAG: {tag_yaml}\n      RELEASE_TITLE_PREFIX: {title_yaml}\n      CONSUMER_REPOSITORY: {consumer_repository_yaml}\n      CONSUMER_BRANCH: {consumer_branch_yaml}\n      UPDATER: {updater_yaml}\n      UPDATER_TOKEN: {updater_token_expr}\n      UPDATE_COMMIT_MESSAGE: {message_yaml}\n    concurrency:\n      group: {concurrency_yaml}\n      cancel-in-progress: false\n    steps:\n      - name: Download verified package handoff\n        uses: {download}\n        with:\n          name: package-release\n          path: package\n          merge-multiple: true\n      - name: Re-verify downloaded handoff\n        id: verify\n        run: |\n{publish_verify}      - name: Publish payload attestations are present\n        env:\n          GH_TOKEN: {}\n        run: |\n          set -euo pipefail\n          for payload in \\\n{publish_attestation_targets}          do\n            gh attestation verify \"$payload\" --repo \"$GITHUB_REPOSITORY\"\n          done\n      - name: Publish rolling release as one verified asset set\n        env:\n          GH_TOKEN: {}\n        run: |\n          set -euo pipefail\n          tag=\"$RELEASE_TAG\"\n          version=\"$(jq -er '.version' \"$PACKAGE_DIR/release-manifest.json\")\"\n          title=\"$RELEASE_TITLE_PREFIX $version\"\n          live=\"$(mktemp)\"\n          trap 'rm -f -- \"$live\"' EXIT\n          if gh api \"repos/$GITHUB_REPOSITORY/releases/tags/$tag\" > \"$live\" 2>/dev/null; then\n            jq -e --arg tag \"$tag\" '(.draft | not) and .tag_name == $tag' \"$live\" >/dev/null\n            live_name=\"$(jq -er '.name | strings' \"$live\")\"\n            live_version=\"$(printf '%s\\n' \"$live_name\" | sed \"s#^$RELEASE_TITLE_PREFIX ##\")\"\n            if command -v dpkg >/dev/null 2>&1 && dpkg --compare-versions \"$live_version\" gt \"$version\"; then\n              echo \"::error::rolling release is newer than candidate; refusing rollback\" >&2\n              exit 1\n            fi\n            if [ \"$live_version\" != \"$version\" ]; then\n              gh release delete \"$tag\" --cleanup-tag --yes\n              gh release create \"$tag\" --target \"$EXPECTED_SOURCE_COMMIT\" --prerelease --title \"$title\" --notes \"Verified package release $version\" {assets}\n            fi\n          else\n            gh release create \"$tag\" --target \"$EXPECTED_SOURCE_COMMIT\" --prerelease --title \"$title\" --notes \"Verified package release $version\" {assets}\n          fi\n          remote=\"$(git ls-remote origin \"refs/tags/$tag^{{}}\" | cut -f1 | head -n1)\"\n          if [ -z \"$remote\" ]; then\n            remote=\"$(git ls-remote origin \"refs/tags/$tag\" | cut -f1 | head -n1)\"\n          fi\n          if [ \"$remote\" != \"$EXPECTED_SOURCE_COMMIT\" ]; then echo \"::error::rolling tag does not resolve to the verified source commit\" >&2; exit 1; fi\n      - name: Download and verify published rolling release\n        env:\n          GH_TOKEN: {}\n        run: |\n          set -euo pipefail\n          rm -rf published-package\n          mkdir -p published-package\n          gh release download \"$RELEASE_TAG\" --dir published-package --clobber\n          export VELNOR_VERIFIED_PACKAGE_DIR=\"$GITHUB_WORKSPACE/published-package\"\n{publish_verify}\n      - name: Checkout consumer repository\n        uses: {checkout}\n        with:\n          repository: {consumer_repository_yaml}\n          ref: {consumer_branch_yaml}\n          token: {updater_token_expr}\n          path: consumer\n          persist-credentials: false\n      - name: Run verified consumer updater and push one commit\n        run: |\n          set -euo pipefail\n          cd consumer\n          git config user.name \"github-actions[bot]\"\n          git config user.email \"41898282+github-actions[bot]@users.noreply.github.com\"\n          VELNOR_PACKAGE_CHANNEL=\"$VELNOR_PACKAGE_CHANNEL\" VELNOR_VERIFIED_PACKAGE_DIR=\"$GITHUB_WORKSPACE/published-package\" bash -c \"$UPDATER\"\n          git diff --check\n          if git diff --quiet; then\n            echo \"consumer already references the verified release\"\n          else\n            git add -A\n            git commit -s -m \"$UPDATE_COMMIT_MESSAGE\"\n            git remote set-url origin \"https://x-access-token:$UPDATER_TOKEN@github.com/$CONSUMER_REPOSITORY.git\"\n            git push origin \"HEAD:$CONSUMER_BRANCH\"\n          fi\n",
        github_expression("github.token"),
        github_expression("github.token"),
        github_expression("github.token"),
    );
    output
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn args() -> BTreeMap<String, toml::Value> {
        toml::from_str(
            r#"
build_tasks = ["release-preview-package"]
package_dir = "dist"
manifest_schema = "example.consumer-manifest-v1"
source_repository = "example/project"
source_ref = "refs/heads/main"
payloads = ["a.tar.gz", "b.tar.gz", "c.tar.gz", "d.tar.gz", "e.tar.gz", "f.tar.gz"]
supporting_assets = ["SHA256SUMS", "a.tar.gz.bundle", "capsule-manifest.json"]
channel = "preview"
release_tag = "preview"
consumer_repository = "example/tap"
updater = "./scripts/package-update.sh"
updater_token_secret = "TAP_TOKEN"
"#,
        )
        .expect("fixture args")
    }

    fn render_config() -> ProjectConfig {
        ProjectConfig {
            repository: "example/project".to_owned(),
            workflow_revision: crate::s2::SOURCE_REVISION.to_owned(),
            profile: "generic".to_owned(),
            analysis: crate::s2::AnalysisSummary {
                method: "test".to_owned(),
                detected: Vec::new(),
                limitations: Vec::new(),
            },
            verified: true,
            workflow_files: vec!["preview.yml".to_owned()],
            notes: Vec::new(),
            version_bump_units: Vec::new(),
            default_branch: "main".to_owned(),
            providers: BTreeSet::from([ProviderId::GithubHosted]),
            automatic_providers: BTreeSet::from([ProviderId::GithubHosted]),
            default_dispatch_providers: BTreeSet::from([ProviderId::GithubHosted]),
            selectors: BTreeMap::from([(
                ProviderId::GithubHosted,
                crate::s2::provider::ProviderSelector {
                    runs_on: vec!["ubuntu-24.04".to_owned()],
                },
            )]),
            release_enabled: false,
            release_reason: String::new(),
            release: None,
            renovate_enabled: false,
            renovate_reason: String::new(),
            renovate: None,
            docs_enabled: false,
            docs_reason: String::new(),
            docs: None,
            check_profiles: Vec::new(),
            maintenance: crate::s2::MaintenanceSpec::default(),
            units: Vec::new(),
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            rust_needs: crate::s2::RustNeeds::Parallel,
            concurrency_group: None,
            serial_stack_groups: false,
            static_files: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
            github_cache: crate::s2::config::CacheGithubSection::default(),
            velnor_host_cache: crate::s2::config::CacheVelnorSection::default(),
        }
    }

    #[test]
    fn package_release_requires_exactly_six_payloads() {
        let mut values = args();
        values.insert(
            "payloads".to_owned(),
            toml::Value::Array(vec![toml::Value::String("one.tar.gz".to_owned())]),
        );
        let error = parse_spec(&Args(&values)).expect_err("one payload must fail");
        assert!(error.to_string().contains("exactly six"));
    }

    #[test]
    fn package_release_rejects_payload_support_collision() {
        let mut values = args();
        values.insert(
            "supporting_assets".to_owned(),
            toml::Value::Array(vec![toml::Value::String("a.tar.gz".to_owned())]),
        );
        let error = parse_spec(&Args(&values)).expect_err("collision must fail");
        assert!(error.to_string().contains("both payload and supporting"));
    }

    #[test]
    fn package_release_rejects_reserved_metadata_names() {
        let mut values = args();
        values.insert(
            "supporting_assets".to_owned(),
            toml::Value::Array(vec![toml::Value::String("identity.json".to_owned())]),
        );
        let error = parse_spec(&Args(&values)).expect_err("metadata collision must fail");
        assert!(error.to_string().contains("reserved metadata"));
    }

    #[test]
    fn verification_binds_manifest_identity_and_all_declared_files() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let script = verification_script(&spec);
        assert!(script.contains(
            "keys == [\"assets\",\"schema\",\"source_commit\",\"source_ref\",\"source_repository\",\"version\"]"
        ));
        assert!(script.contains(".manifest == $package_manifest[0]"));
        assert!(script.contains("cmp -s \"$expected_files\" \"$actual_files\""));
        assert!(script.contains("sha256sum \"$dir/$name\""));
    }

    #[test]
    fn release_asset_set_contains_metadata_payloads_and_supporting_provenance() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        assert_eq!(
            release_asset_names(&spec),
            [
                "release-manifest.json",
                "identity.json",
                "a.tar.gz",
                "b.tar.gz",
                "c.tar.gz",
                "d.tar.gz",
                "e.tar.gz",
                "f.tar.gz",
                "SHA256SUMS",
                "a.tar.gz.bundle",
                "capsule-manifest.json"
            ]
        );
    }

    #[test]
    fn rendered_workflow_rechecks_published_dir_and_attests_declared_payloads() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec);
        assert!(
            workflow.contains(
                "          export VELNOR_VERIFIED_PACKAGE_DIR=\"$GITHUB_WORKSPACE/published-package\""
            ),
            "{workflow}"
        );
        assert!(
            workflow
                .contains("          for payload in \\\n            \"$PACKAGE_DIR/a.tar.gz\" \\"),
            "{workflow}"
        );
        assert!(
            workflow.contains("          ${{ github.workspace }}/dist/a.tar.gz"),
            "{workflow}"
        );
        assert!(
            !workflow
                .contains("VELNOR_VERIFIED_PACKAGE_DIR=\"$GITHUB_WORKSPACE/published-package\" {}"),
            "{workflow}"
        );
        assert!(workflow.contains("cancel-in-progress: false"), "{workflow}");
    }
}
