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

const GLOBAL_PUBLISH_CONCURRENCY_GROUP: &str = "package-release-global-publish";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VersionFormat {
    Channel,
    Semver,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManifestFormat {
    CoreV1,
    SupportingAssetsV1,
}

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
    manifest_format: ManifestFormat,
    version_format: VersionFormat,
    release_tag: String,
    github_release_type: String,
    publish_environment: String,
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
            "github_release_type",
            "manifest_schema",
            "manifest_format",
            "package_dir",
            "payloads",
            "publish_environment",
            "release_tag",
            "release_title_prefix",
            "source_ref",
            "source_repository",
            "supporting_assets",
            "update_commit_message",
            "updater",
            "updater_token_secret",
            "version_format",
        ]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let spec = parse_spec(args)?;
        if !ctx.config.providers.contains(&ProviderId::GithubHosted) {
            return Err(GeneratorError::usage(
                "package-release requires the github-hosted provider for GitHub release and attestation APIs",
            ));
        }
        validate_release_runner(ctx.config)?;
        if !ctx.config.repository.is_empty() && ctx.config.repository != spec.source_repository {
            return Err(GeneratorError::usage(format!(
                "package-release source_repository {} must match scanned repository {}",
                spec.source_repository, ctx.config.repository
            )));
        }
        let file = validate_workflow_file(ctx.file)?;
        let content = render_workflow(ctx.config, &spec, &file);
        Ok(Rendered {
            files: std::iter::once((Path::new(".github/workflows").join(&file), content)).collect(),
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

fn default_release_title_prefix(channel: &str) -> String {
    let mut prefix = channel.to_owned();
    if let Some(first) = prefix.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    prefix
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
    let valid_segment = |segment: &str| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    };
    if value.is_empty()
        || path.is_absolute()
        || value.contains(['\\', ':', '\n', '\r'])
        || value.chars().any(char::is_whitespace)
        || value.split('/').any(|segment| !valid_segment(segment))
    {
        return Err(GeneratorError::usage(
            "package-release package_dir must be a portable relative directory with simple path segments",
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

fn validate_workflow_file(file: Option<&str>) -> Result<String, GeneratorError> {
    let file = file
        .filter(|file| !file.is_empty())
        .ok_or_else(|| GeneratorError::usage("package-release needs a declared workflow file"))?;
    let path = Path::new(file);
    let workflow_extension = path.extension().and_then(|extension| extension.to_str());
    let valid_segment = |segment: &str| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    };
    if path.is_absolute()
        || file.contains(['\\', ':', '\n', '\r'])
        || file.split('/').any(|segment| !valid_segment(segment))
        || !matches!(workflow_extension, Some("yml" | "yaml"))
    {
        return Err(GeneratorError::usage(
            "package-release file must be a safe relative .yml/.yaml workflow path",
        ));
    }
    Ok(file.to_owned())
}

#[cfg(test)]
fn valid_channel_version(value: &str, channel: &str) -> bool {
    let Some((base, source_suffix)) = value.split_once('+') else {
        return false;
    };
    if source_suffix.len() != 7
        || !source_suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return false;
    }
    let Some((release, version_channel_and_sequence)) = base.split_once('-') else {
        return false;
    };
    let Some((version_channel, sequence)) = version_channel_and_sequence.rsplit_once('.') else {
        return false;
    };
    if version_channel != channel {
        return false;
    }
    if sequence.is_empty() || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let mut components = release.split('.');
    (0..3).all(|_| {
        components.next().is_some_and(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
    }) && components.next().is_none()
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
    if payloads.is_empty() {
        return Err(GeneratorError::usage(
            "package-release needs at least one payload",
        ));
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
    if !crate::s2::runtime::valid_package(&channel) {
        return Err(GeneratorError::usage(
            "package-release channel must be a portable release-channel token",
        ));
    }
    let version_format = match required_string(args, "version_format")?.as_str() {
        "channel" => VersionFormat::Channel,
        "semver" => VersionFormat::Semver,
        _ => {
            return Err(GeneratorError::usage(
                "package-release version_format must be `channel` or `semver`",
            ));
        }
    };
    let manifest_format = match required_string(args, "manifest_format")?.as_str() {
        "core-v1" => ManifestFormat::CoreV1,
        "supporting-assets-v1" => ManifestFormat::SupportingAssetsV1,
        _ => {
            return Err(GeneratorError::usage(
                "package-release manifest_format must be `core-v1` or `supporting-assets-v1`",
            ));
        }
    };
    let release_tag = required_string(args, "release_tag")?;
    if !crate::s2::runtime::valid_package(&release_tag) {
        return Err(GeneratorError::usage(
            "package-release release_tag must be a portable tag token",
        ));
    }
    let github_release_type = required_string(args, "github_release_type")?;
    if !matches!(github_release_type.as_str(), "prerelease" | "release") {
        return Err(GeneratorError::usage(
            "package-release github_release_type must be `prerelease` or `release` from the target repository policy",
        ));
    }
    match (github_release_type.as_str(), version_format) {
        ("prerelease", VersionFormat::Channel) | ("release", VersionFormat::Semver) => {}
        ("prerelease", VersionFormat::Semver) => {
            return Err(GeneratorError::usage(
                "package-release prerelease lanes require version_format = `channel`",
            ));
        }
        ("release", VersionFormat::Channel) => {
            return Err(GeneratorError::usage(
                "package-release release lanes require version_format = `semver`",
            ));
        }
        _ => {
            return Err(GeneratorError::usage(
                "package-release release type and version format are not a supported pair",
            ));
        }
    }
    match (manifest_format, supporting_assets.is_empty()) {
        (ManifestFormat::CoreV1, true) | (ManifestFormat::SupportingAssetsV1, false) => {}
        (ManifestFormat::CoreV1, false) => {
            return Err(GeneratorError::usage(
                "package-release manifest_format = `core-v1` requires an empty supporting_assets list",
            ));
        }
        (ManifestFormat::SupportingAssetsV1, true) => {
            return Err(GeneratorError::usage(
                "package-release manifest_format = `supporting-assets-v1` requires supporting_assets",
            ));
        }
    }
    if github_release_type == "prerelease" && manifest_format != ManifestFormat::SupportingAssetsV1
    {
        return Err(GeneratorError::usage(
            "package-release prerelease lanes require manifest_format = `supporting-assets-v1`",
        ));
    }
    if channel == "preview" && github_release_type != "prerelease" {
        return Err(GeneratorError::usage(
            "package-release preview channel requires github_release_type = `prerelease`",
        ));
    }
    if channel == "stable" && github_release_type != "release" {
        return Err(GeneratorError::usage(
            "package-release stable channel requires github_release_type = `release`",
        ));
    }
    let publish_environment = required_string(args, "publish_environment")?;
    if publish_environment.len() > 255
        || publish_environment.chars().any(char::is_control)
        || publish_environment.contains("${{")
    {
        return Err(GeneratorError::usage(
            "package-release publish_environment must be a literal GitHub environment name of at most 255 bytes",
        ));
    }
    let release_title_prefix = args
        .string("release_title_prefix")?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_release_title_prefix(&channel));
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
    let concurrency_group = format!("package-release-{release_tag}");
    if args
        .string("concurrency_group")?
        .filter(|value| !value.is_empty())
        .is_some_and(|configured| configured != concurrency_group)
    {
        return Err(GeneratorError::usage(
            "package-release concurrency_group is derived from release_tag and cannot be overridden",
        ));
    }
    if concurrency_group.eq_ignore_ascii_case(GLOBAL_PUBLISH_CONCURRENCY_GROUP) {
        return Err(GeneratorError::usage(
            "package-release release_tag maps to the reserved global publish concurrency group",
        ));
    }

    Ok(PackageReleaseSpec {
        build_tasks,
        package_dir,
        manifest_schema,
        source_repository,
        source_ref,
        payloads,
        supporting_assets,
        channel,
        manifest_format,
        version_format,
        release_tag,
        github_release_type,
        publish_environment,
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

fn validate_release_runner(config: &ProjectConfig) -> Result<(), GeneratorError> {
    let Some(selector) = config.selectors.get(&ProviderId::GithubHosted) else {
        return Ok(());
    };
    let is_supported_ubuntu = matches!(
        selector.runs_on.as_slice(),
        [label]
            if matches!(
                label.as_str(),
                "ubuntu-latest"
                    | "ubuntu-22.04"
                    | "ubuntu-24.04"
                    | "ubuntu-26.04"
                    | "ubuntu-22.04-arm"
                    | "ubuntu-24.04-arm"
                    | "ubuntu-26.04-arm"
            )
    );
    if !is_supported_ubuntu {
        return Err(GeneratorError::usage(
            "package-release requires one standard GitHub-hosted Ubuntu runner label for Bash and GNU package verification tools",
        ));
    }
    Ok(())
}

fn indent_script(script: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    let mut indented = String::new();
    for line in script.lines() {
        if line.trim().is_empty() {
            indented.push('\n');
        } else {
            let _ = writeln!(indented, "{prefix}{line}");
        }
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
verification_temp_dir="${VELNOR_VERIFICATION_TEMP_DIR:-${TMPDIR:-/tmp}}"
expected_files="$(mktemp "$verification_temp_dir/expected-files.XXXXXX")"
actual_files="$(mktemp "$verification_temp_dir/actual-files.XXXXXX")"
expected_names="$(mktemp "$verification_temp_dir/expected-names.XXXXXX")"
actual_names="$(mktemp "$verification_temp_dir/actual-names.XXXXXX")"
checksum_names="$(mktemp "$verification_temp_dir/checksum-names.XXXXXX")"
expected_supporting_names="$(mktemp "$verification_temp_dir/expected-supporting-names.XXXXXX")"
actual_supporting_names="$(mktemp "$verification_temp_dir/actual-supporting-names.XXXXXX")"
trap 'rm -f -- "$expected_files" "$actual_files" "$expected_names" "$actual_names" "$checksum_names" "$expected_supporting_names" "$actual_supporting_names"' EXIT
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
if find "$dir" -mindepth 1 -maxdepth 1 ! -type f -print -quit | grep -q .; then
  echo "::error::verified package directory contains a non-file entry" >&2
  exit 1
fi
if ! cmp -s "$expected_files" "$actual_files"; then
  echo "::error::verified package directory contains an undeclared or missing file" >&2
  diff -u "$expected_files" "$actual_files" >&2 || true
  exit 1
fi
source_checkout="${VELNOR_SOURCE_CHECKOUT_DIR:-$GITHUB_WORKSPACE}"
actual_source_commit="$(git -C "$source_checkout" rev-parse HEAD)"
[[ "$actual_source_commit" =~ ^[0-9a-f]{40}$ ]] || { echo "::error::source checkout HEAD is not 40 lowercase hex" >&2; exit 1; }
[ "$actual_source_commit" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::source checkout HEAD does not match the expected source commit" >&2; exit 1; }
source_remote="$(git -C "$source_checkout" remote get-url origin)"
case "$source_remote" in
  https://github.com/*|http://github.com/*) actual_source_repository="${source_remote#*github.com/}" ;;
  ssh://git@github.com/*) actual_source_repository="${source_remote#ssh://git@github.com/}" ;;
  git@github.com:*) actual_source_repository="${source_remote#git@github.com:}" ;;
  *) echo "::error::source checkout origin is not a GitHub repository URL" >&2; exit 1 ;;
esac
actual_source_repository="${actual_source_repository%.git}"
[ "$actual_source_repository" = "$EXPECTED_SOURCE_REPOSITORY" ] || { echo "::error::source checkout repository does not match the declared repository" >&2; exit 1; }
source_commit="$(jq -er '.source_commit | strings' "$manifest")"
__MANIFEST_VERSION_GUARD__
version="$(jq -er '.version | strings' "$manifest")"
[[ "$source_commit" =~ ^[0-9a-f]{40}$ ]] || { echo "::error::manifest source_commit is not 40 lowercase hex" >&2; exit 1; }
[ "$source_commit" = "$actual_source_commit" ] || { echo "::error::manifest source_commit is not the checked-out commit" >&2; exit 1; }
__VERSION_VALIDATION__
jq -e \
  --arg schema "$EXPECTED_MANIFEST_SCHEMA" \
  --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
  --arg source_ref "$EXPECTED_SOURCE_REF" \
  --arg commit "$source_commit" \
  --slurpfile package_manifest "$manifest" \
  '__MANIFEST_SHAPE_VALIDATION__ and
   .schema == $schema and .source_repository == $repository and
   .source_ref == $source_ref and .source_commit == $commit' "$manifest" >/dev/null
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
  echo "::error::manifest asset names do not equal the declared payloads" >&2
  exit 1
fi
jq -e '(.assets | map(.name)) as $names | ($names | unique | length) == ($names | length)' "$manifest" >/dev/null
for name in \
"#,
    );
    for (index, name) in spec.payloads.iter().enumerate() {
        let suffix = if index + 1 == spec.payloads.len() {
            ""
        } else {
            " \\\n"
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
"#,
    );
    if !spec.supporting_assets.is_empty() {
        script.push_str("{\n");
        for name in &spec.supporting_assets {
            let _ = writeln!(script, "  printf '%s\\n' {}", shell_quote(name));
        }
        script.push_str(
            r#"} | LC_ALL=C sort > "$expected_supporting_names"
jq -r '.supporting_assets[].name' "$manifest" | LC_ALL=C sort > "$actual_supporting_names"
if ! cmp -s "$expected_supporting_names" "$actual_supporting_names"; then
  echo "::error::manifest supporting assets do not equal the declared supporting assets" >&2
  exit 1
fi
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
  expected="$(jq -er --arg name "$name" '[.supporting_assets[] | select(.name == $name)] | select(length == 1) | .[0].sha256 | select(test("^[0-9a-f]{64}$"))' "$manifest")"
  actual="$(sha256sum "$dir/$name" | awk '{print $1}')"
  [ "$actual" = "$expected" ] || { echo "::error::supporting asset checksum mismatch: $name" >&2; exit 1; }
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
"#,
        );
    }
    if spec
        .supporting_assets
        .iter()
        .any(|asset| asset == "SHA256SUMS")
    {
        script.push_str(
            r#"if ! awk '
  NF == 2 {
    name = $2
    sub(/^\*/, "", name)
    if (length($1) != 64 || $1 !~ /^[0-9a-f]+$/ || name == "" || name ~ /[\\/[:space:]]/) {
      exit 1
    }
    print name
    next
  }
  { exit 1 }
' "$dir/SHA256SUMS" | LC_ALL=C sort > "$checksum_names"; then
  echo "::error::SHA256SUMS has an invalid line" >&2
  exit 1
fi
if ! cmp -s "$expected_names" "$checksum_names"; then
  echo "::error::SHA256SUMS does not name exactly the declared payloads" >&2
  exit 1
fi
if ! (cd "$dir" && sha256sum --check --strict SHA256SUMS) >/dev/null; then
  echo "::error::SHA256SUMS does not verify the downloaded payload bytes" >&2
  exit 1
fi
"#,
        );
    }
    let checksum_sidecars = spec
        .supporting_assets
        .iter()
        .filter_map(|sidecar| {
            let payload = sidecar.strip_suffix(".sha256")?;
            spec.payloads
                .iter()
                .find(|name| name.as_str() == payload)
                .map(|_| (sidecar.as_str(), payload))
        })
        .collect::<Vec<_>>();
    if !checksum_sidecars.is_empty() {
        script.push_str(
            r#"verify_sha256_sidecar() {
  local sidecar="$1"
  local payload="$2"
  local expected_name="${payload##*/}"
  local digest
  test -s "$sidecar"
  test "$(wc -c < "$sidecar" | tr -d ' ')" -le 4096
  if ! digest="$(awk -v expected_name="$expected_name" '
    NR == 1 && (NF == 1 || NF == 2) {
      if (length($1) != 64 || $1 !~ /^[0-9a-f]+$/) exit 1
      if (NF == 2) {
        name = $2
        sub(/^\*/, "", name)
        if (name != expected_name) exit 1
      }
      print $1
      next
    }
    { exit 1 }
    END { if (NR != 1) exit 1 }
  ' "$sidecar")"; then
    echo "::error::checksum sidecar is not one strict digest line: $sidecar" >&2
    exit 1
  fi
  actual="$(sha256sum -- "$payload" | awk '{print $1}')"
  [ "$digest" = "$actual" ] || {
    echo "::error::checksum sidecar does not match payload: $sidecar" >&2
    exit 1
  }
}
"#,
        );
        for (sidecar, payload) in checksum_sidecars {
            let _ = writeln!(
                script,
                "verify_sha256_sidecar \"$dir/{sidecar}\" \"$dir/{payload}\""
            );
        }
    }
    script.push_str(
        r#"printf 'version=%s\n' "$version" >> "$GITHUB_OUTPUT"
printf 'source_commit=%s\n' "$source_commit" >> "$GITHUB_OUTPUT"
"#,
    );
    let version_checks =
        version_validation_script(spec.version_format, "version", "source_commit", "manifest");
    script = script.replace("__VERSION_VALIDATION__", &version_checks);
    script = script.replace(
        "__MANIFEST_SHAPE_VALIDATION__",
        &manifest_shape_validation(spec),
    );
    script = script.replace("__MANIFEST_VERSION_GUARD__", manifest_version_line_guard());
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

fn manifest_shape_validation(spec: &PackageReleaseSpec) -> String {
    let mut query = format!("{} and\n", manifest_key_validation(spec));
    query.push_str(
        r#"   (.assets | type == "array" and
    all(.[]; type == "object" and (keys == ["name","sha256"]) and
      (.name | strings | length > 0) and
      (.sha256 | strings | test("^[0-9a-f]{64}$"))))
"#,
    );
    if spec.manifest_format == ManifestFormat::CoreV1 {
        query.push_str(
            r"   and ((.assets | map(.name)) as $names |
    ($names | unique | length) == ($names | length))",
        );
    } else {
        query.push_str(
            r#"   and (.supporting_assets | type == "array" and
    all(.[]; type == "object" and (keys == ["name","sha256"]) and
      (.name | strings | length > 0) and
      (.sha256 | strings | test("^[0-9a-f]{64}$"))))
   and ((.assets + .supporting_assets | map(.name)) as $names |
    ($names | unique | length) == ($names | length))"#,
        );
    }
    query
}

fn manifest_key_validation(spec: &PackageReleaseSpec) -> &'static str {
    match spec.manifest_format {
        ManifestFormat::CoreV1 => {
            r#"keys == ["assets","schema","source_commit","source_ref","source_repository","version"]"#
        }
        ManifestFormat::SupportingAssetsV1 => {
            r#"keys == ["assets","schema","source_commit","source_ref","source_repository","supporting_assets","version"]"#
        }
    }
}

fn latest_manifest_identity_validation() -> &'static str {
    r".schema == $schema and .source_repository == $repository and
   .source_ref == $source_ref and .source_commit == $commit"
}

fn latest_identity_envelope_validation() -> &'static str {
    r#"keys == ["manifest","source_digest","source_ref","source_repository"] and
   .source_repository == $repository and .source_ref == $source_ref and
   .source_digest == $commit and .manifest == $latest_manifest[0]"#
}

/// Reject line breaks on the raw JSON string before command substitution can
/// strip trailing newlines and turn an invalid version into a valid one.
fn manifest_version_line_guard() -> &'static str {
    r#"jq -e '.version | type == "string" and ((contains("\n") or contains("\r") or contains("\u0000")) | not)' "$manifest" >/dev/null || { echo "::error::manifest version must be a single-line JSON string without NUL bytes" >&2; exit 1; }"#
}

fn known_manifest_shape_validation() -> &'static str {
    r#"((keys == ["assets","schema","source_commit","source_ref","source_repository","version"]) or
    (keys == ["assets","schema","source_commit","source_ref","source_repository","supporting_assets","version"])) and
   (.assets | type == "array" and
    all(.[]; type == "object" and (keys == ["name","sha256"]) and
      (.name | strings | length > 0) and
      (.sha256 | strings | test("^[0-9a-f]{64}$")))) and
   (if has("supporting_assets") then
      (.supporting_assets | type == "array" and
        all(.[]; type == "object" and (keys == ["name","sha256"]) and
          (.name | strings | length > 0) and
          (.sha256 | strings | test("^[0-9a-f]{64}$")))) and
      ((.assets + .supporting_assets | map(.name)) as $names |
        ($names | unique | length) == ($names | length))
    else
      ((.assets | map(.name)) as $names |
        ($names | unique | length) == ($names | length))
    end)"#
}

fn version_validation_script(
    format: VersionFormat,
    version_variable: &str,
    commit_variable: &str,
    error_context: &str,
) -> String {
    let mut script = String::new();
    match format {
        VersionFormat::Channel => {
            let _ = writeln!(
                script,
                r#"[[ "${version_variable}" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-([A-Za-z0-9_-]+)\.(0|[1-9][0-9]*)\+([0-9a-f]{{7}})$ ]] || {{ echo "::error::{error_context} version is not a source-bound channel version" >&2; exit 1; }}"#
            );
            let _ = writeln!(
                script,
                r#"[ "${{BASH_REMATCH[4]}}" = "$VELNOR_PACKAGE_CHANNEL" ] || {{ echo "::error::{error_context} version channel does not match the configured channel" >&2; exit 1; }}"#
            );
            let _ = writeln!(
                script,
                r#"[ "${{{version_variable}##*+}}" = "${{{commit_variable}:0:7}}" ] || {{ echo "::error::{error_context} version does not bind its source commit" >&2; exit 1; }}"#
            );
        }
        VersionFormat::Semver => {
            let _ = writeln!(
                script,
                r#"[[ "${version_variable}" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || {{ echo "::error::{error_context} version is not strict semantic versioning" >&2; exit 1; }}"#
            );
        }
    }
    script
}

fn version_validation_return_script(
    format: VersionFormat,
    version_variable: &str,
    commit_variable: &str,
    error_context: &str,
) -> String {
    version_validation_script(format, version_variable, commit_variable, error_context)
        .replace("exit 1", "return 1")
}

#[allow(clippy::too_many_lines)]
fn render_workflow(
    config: &ProjectConfig,
    spec: &PackageReleaseSpec,
    workflow_file: &str,
) -> String {
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
    let publish_source_commit_expr = github_expression("needs.build.outputs.source_commit");
    let workspace_expr = github_expression("github.workspace");
    let build_verify = indent_script(&verification_script(spec), 10);
    let publish_verify = indent_script(&verification_script(spec), 10);
    let updater_token_expr = github_expression(&format!("secrets.{}", spec.updater_token_secret));
    let github_token_expr = github_expression("github.token");
    let build_if = github_expression(&format!(
        "github.ref == 'refs/heads/{}'",
        spec.source_ref
            .strip_prefix("refs/heads/")
            .unwrap_or("main")
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
    let mut attested_assets = spec.payloads.clone();
    attested_assets.extend(spec.supporting_assets.iter().cloned());
    attested_assets.push("release-manifest.json".to_owned());
    attested_assets.push("identity.json".to_owned());
    for name in &attested_assets {
        let _ = writeln!(
            attestation_subjects,
            "            {workspace_expr}/{package_dir}/{name}"
        );
    }
    let mut artifact_upload_paths = String::new();
    for name in release_asset_names(spec) {
        let _ = writeln!(
            artifact_upload_paths,
            "            {workspace_expr}/{package_dir}/{name}"
        );
    }
    let mut publish_attestation_targets = String::new();
    for (index, name) in attested_assets.iter().enumerate() {
        let suffix = if index + 1 == attested_assets.len() {
            ""
        } else {
            " \\"
        };
        let _ = writeln!(
            publish_attestation_targets,
            "            \"$PACKAGE_DIR/{name}\"{suffix}",
        );
    }
    let attestation_flags = format!(
        "--repo \"$GITHUB_REPOSITORY\" --signer-workflow \"$GITHUB_REPOSITORY/.github/workflows/{workflow_file}\" --source-ref \"$EXPECTED_SOURCE_REF\" --source-digest \"$EXPECTED_SOURCE_COMMIT\""
    );

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
        "jobs:\n  build:\n    name: Verify package release\n    if: {build_if}\n    runs-on: {runner}\n    defaults:\n      run:\n        shell: bash\n    timeout-minutes: 90\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    outputs:\n      version: {}\n      source_commit: {}\n    env:\n      PACKAGE_DIR: {package_dir_yaml}\n      VELNOR_VERIFIED_PACKAGE_DIR: {workspace_expr}/{package_dir}\n      VELNOR_PACKAGE_CHANNEL: {channel_yaml}\n      EXPECTED_SOURCE_REPOSITORY: {source_repository_yaml}\n      EXPECTED_SOURCE_REF: {source_ref_yaml}\n      EXPECTED_MANIFEST_SCHEMA: {schema_yaml}\n      EXPECTED_SOURCE_COMMIT: {source_commit_expr}\n",
        github_expression("steps.verify.outputs.version"),
        github_expression("steps.verify.outputs.source_commit"),
    );
    output = output.replace(
        &format!("      EXPECTED_SOURCE_COMMIT: {source_commit_expr}\n"),
        &format!(
            "      EXPECTED_SOURCE_COMMIT: {source_commit_expr}\n      VELNOR_SOURCE_CHECKOUT_DIR: {workspace_expr}\n"
        ),
    );
    let _ = writeln!(
        output,
        "    steps:\n      - name: Checkout source\n        uses: {checkout}\n        with:\n          ref: {source_commit_expr}\n          fetch-depth: 0\n          persist-credentials: false\n{runtime_setup}      - name: Set up Mise\n        uses: {mise}\n        with:\n          install: false\n      - name: Enforce workflow policy\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\n      - name: Build verified package directory\n        env:\n          VELNOR_SOURCE_COMMIT: {source_commit_expr}\n          VELNOR_SOURCE_REF: {source_shell}\n        run: |\n          set -euo pipefail\n          mkdir -p \"$VELNOR_VERIFIED_PACKAGE_DIR\"\n{tasks}      - name: Verify manifest, identity, checksums, and exact file set\n        id: verify\n        run: |\n{build_verify}      - name: Attest declared payloads\n        uses: {attest}\n        with:\n          subject-path: |\n{attestation_subjects}      - name: Upload verified package handoff\n        uses: {upload}\n        with:\n          name: package-release\n          path: |\n{artifact_upload_paths}          include-hidden-files: true\n          if-no-files-found: error\n          retention-days: 2\n",
    );
    output = output.replace("Attest declared payloads", "Attest declared package assets");
    output = output.replace(
        "          install: false\n      - name: Enforce workflow policy",
        "          install: false\n      - name: Install locked build tools\n        run: mise --yes install --locked --include-task-tools\n      - name: Enforce workflow policy",
    );
    output.push('\n');
    output.push_str(&render_publish_job(
        spec,
        &runner,
        checkout,
        download,
        &publish_verify,
        &publish_attestation_targets,
        &attestation_flags,
        &workspace_expr,
        &updater_token_expr,
        &github_token_expr,
        &publish_source_commit_expr,
        &channel_yaml,
        &source_repository_yaml,
        &source_ref_yaml,
        &schema_yaml,
        &tag_yaml,
        &title_yaml,
        &consumer_repository_yaml,
        &consumer_branch_yaml,
        &updater_yaml,
        &message_yaml,
    ));
    output = output.replace(
        "git -C source ls-remote origin",
        "git -C source -c \"http.extraheader=AUTHORIZATION: bearer $GH_TOKEN\" ls-remote origin",
    );
    output = output.replace(
        "remote_branch_sha=\"$(git ls-remote origin",
        "remote_branch_sha=\"$(git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" ls-remote origin",
    );
    output = output.replace(
        "          response_status=0\n          gh api --repo \"$GITHUB_REPOSITORY\" -i \"repos/$GITHUB_REPOSITORY/releases/tags/$tag\" > \"$release_json\" 2>/dev/null || response_status=$?\n",
        "          if ! gh api --repo \"$GITHUB_REPOSITORY\" -i \"repos/$GITHUB_REPOSITORY/releases/tags/$tag\" > \"$release_json\" 2>/dev/null; then\n            :\n          fi\n",
    );
    output = output.replace(
        "git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" push --force-with-lease=refs/heads/$automation_branch:$remote_branch_sha origin",
        "git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" push \"--force-with-lease=refs/heads/$automation_branch:$remote_branch_sha\" origin",
    );
    output
}

struct PublishVerification<'a> {
    script: &'a str,
    attestation_flags: &'a str,
}

#[allow(clippy::too_many_lines)]
fn render_rolling_refresh_script(
    spec: &PackageReleaseSpec,
    published_assets: &str,
    expected_asset_names: &str,
    attested_asset_names: &str,
    verification: &PublishVerification<'_>,
) -> String {
    let inline_verification = verification.script.replace(
        r#"trap 'rm -f -- "$expected_files" "$actual_files" "$expected_names" "$actual_names" "$checksum_names" "$expected_supporting_names" "$actual_supporting_names"' EXIT"#,
        "# temporary files are inside transaction_dir; on_exit removes them",
    );
    let mut script = String::from(
        r#"set -Eeuo pipefail
case "$RELEASE_PRERELEASE" in
  true|false) ;;
  *) echo "::error::invalid configured GitHub release type" >&2; exit 1 ;;
esac
ROLLING_PREFLIGHT_ONLY="${ROLLING_PREFLIGHT_ONLY:-false}"
case "$ROLLING_PREFLIGHT_ONLY" in
  true|false) ;;
  *) echo "::error::invalid rolling release preflight mode" >&2; exit 1 ;;
esac
rolling_tag="$RELEASE_TAG"
staged_tag="$RELEASE_TAG-$EXPECTED_SOURCE_COMMIT"
if [ "$ROLLING_PREFLIGHT_ONLY" = true ]; then
  published_dir="$GITHUB_WORKSPACE/$PACKAGE_DIR"
else
  published_dir="$GITHUB_WORKSPACE/published-package"
fi
transaction_dir="$(mktemp -d)"
rolling_response="$transaction_dir/rolling-response"
rolling_body="$transaction_dir/rolling.json"
latest_response="$transaction_dir/latest-response"
expected_assets="$transaction_dir/expected-assets"
old_assets="$transaction_dir/old-assets"
staged_assets="$transaction_dir/staged-assets"
rolling_staged_assets="$transaction_dir/rolling-staged-assets"
rolling_published_assets="$transaction_dir/rolling-published-assets"
rollback_dir="$transaction_dir/old-package"
rolling_release_id=""
old_tag_sha=""
old_tag_ref_sha=""
old_name=""
old_body_file="$transaction_dir/old-release-body"
old_draft=""
old_prerelease=""
old_make_latest=false
old_source_commit=""
old_version=""
manifest="$published_dir/release-manifest.json"
__MANIFEST_VERSION_GUARD__
candidate_version="$(jq -er '.version | strings' "$manifest")"
had_release=0
mutated=0

remote_tag_sha() {
  local tag_name="$1"
  local result sha
  if ! result="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name^{}")"; then
    echo "::error::failed to query remote tag: $tag_name" >&2
    return 1
  fi
  sha="$(awk 'NR == 1 {print $1}' <<<"$result")"
  if [ -z "$sha" ]; then
    if ! result="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name")"; then
      echo "::error::failed to query remote tag: $tag_name" >&2
      return 1
    fi
    sha="$(awk 'NR == 1 {print $1}' <<<"$result")"
  fi
  if [ -n "$sha" ] && ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    echo "::error::remote tag returned an invalid object id: $tag_name" >&2
    return 1
  fi
  printf '%s\n' "$sha"
}

remote_tag_ref_sha() {
  local tag_name="$1"
  local ref="refs/tags/$tag_name"
  local result sha
  if ! result="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "$ref")"; then
    echo "::error::failed to query remote tag ref: $tag_name" >&2
    return 1
  fi
  sha="$(awk -v ref="$ref" '$2 == ref {print $1}' <<<"$result")"
  if [ -n "$sha" ] && ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    echo "::error::remote tag ref returned an invalid object id: $tag_name" >&2
    return 1
  fi
  printf '%s\n' "$sha"
}

verify_latest_floor() {
  if [ "$RELEASE_LATEST" != true ]; then
    return 0
  fi
  local latest_dir="$transaction_dir/latest-floor"
  local manifest identity latest_tag_sha latest_attested_asset
  mkdir -p "$latest_dir"
  if ! gh release download "$latest_tag" --repo "$GITHUB_REPOSITORY" --dir "$latest_dir" \
      --pattern release-manifest.json --pattern identity.json; then
    echo "::error::current GitHub Latest release has no readable package identity; refusing to publish stable release" >&2
    return 1
  fi
  manifest="$latest_dir/release-manifest.json"
  identity="$latest_dir/identity.json"
  test -s "$manifest"
  test -s "$identity"
  jq -e '__KNOWN_MANIFEST_SHAPE_VALIDATION__' "$manifest" >/dev/null
  __MANIFEST_VERSION_GUARD__
  latest_version="$(jq -er '.version | strings' "$manifest")"
  latest_source_commit="$(jq -er '.source_commit | strings' "$manifest")"
  [[ "$latest_source_commit" =~ ^[0-9a-f]{40}$ ]] || {
    echo "::error::current GitHub Latest manifest source_commit is not 40 lowercase hex" >&2
    return 1
  }
  jq -e --arg schema "$EXPECTED_MANIFEST_SCHEMA" \
    --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
    --arg source_ref "$EXPECTED_SOURCE_REF" \
    --arg commit "$latest_source_commit" \
    '__LATEST_MANIFEST_IDENTITY_VALIDATION__' "$manifest" >/dev/null || {
    echo "::error::current GitHub Latest manifest does not match the configured schema and source identity" >&2
    return 1
  }
  jq -e --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
    --arg source_ref "$EXPECTED_SOURCE_REF" \
    --arg commit "$latest_source_commit" \
    --slurpfile latest_manifest "$manifest" \
    '__LATEST_IDENTITY_ENVELOPE_VALIDATION__' "$identity" >/dev/null || {
    echo "::error::current GitHub Latest identity envelope does not bind its manifest and source" >&2
    return 1
  }
  if ! latest_tag_sha="$(remote_tag_sha "$latest_tag")"; then
    echo "::error::could not verify the current GitHub Latest tag; refusing stable publication" >&2
    return 1
  fi
  [ "$latest_tag_sha" = "$latest_source_commit" ] || {
    echo "::error::current GitHub Latest tag does not match its package manifest" >&2
    return 1
  }
  for latest_attested_asset in "$manifest" "$identity"; do
    gh attestation verify "$latest_attested_asset" __LATEST_ATTESTATION_FLAGS__
  done
  __LATEST_VERSION_VALIDATION__
  if [ "$latest_version" = "$candidate_version" ]; then
    [ "$latest_source_commit" = "$EXPECTED_SOURCE_COMMIT" ] || {
      echo "::error::candidate version already belongs to a different GitHub Latest source commit" >&2
      return 1
    }
  elif [ "$(printf '%s\n' "$latest_version" "$candidate_version" | LC_ALL=C sort -V | tail -n 1)" != "$candidate_version" ]; then
    echo "::error::candidate stable version is older than current GitHub Latest" >&2
    return 1
  fi
}

validate_existing_rolling_release() {
  local body="$1"
  local source_dir="$2"
  local manifest="$source_dir/release-manifest.json"
  local identity="$source_dir/identity.json"
  local manifest_assets="$transaction_dir/live-manifest-assets"
  local manifest_digests="$transaction_dir/live-manifest-digests"
  local downloaded_assets="$transaction_dir/live-downloaded-assets"
  local github_digests="$transaction_dir/live-github-digests"
  local name expected_digest actual_digest api_digest

  if ! jq -e --arg tag "$rolling_tag" --argjson prerelease "$RELEASE_PRERELEASE" '
    (.draft | type == "boolean") and .prerelease == $prerelease and .tag_name == $tag and
    (.id | type == "number") and
    (.assets | type == "array" and length > 0) and
    ((.assets | map(.name)) as $names |
      ($names | unique | length) == ($names | length)) and
    (.assets | all(.[];
        type == "object" and
        (.name | type == "string") and
        (.digest | type == "string" and test("^sha256:[0-9a-f]{64}$"))))
  ' <<<"$body" >/dev/null; then
    echo "::error::existing rolling release metadata is invalid" >&2
    return 1
  fi
  if ! test -s "$manifest" || ! test -s "$identity"; then
    echo "::error::existing rolling release is missing its manifest or identity envelope" >&2
    return 1
  fi

  if ! jq -e '__MANIFEST_SHAPE_VALIDATION__' "$manifest" >/dev/null; then
    echo "::error::existing rolling release manifest does not match the configured schema" >&2
    return 1
  fi

  if ! old_source_commit="$(jq -er '.source_commit | strings' "$manifest")"; then
    echo "::error::existing rolling release manifest has no source commit" >&2
    return 1
  fi
  if ! jq -e '.version | type == "string" and ((contains("\n") or contains("\r") or contains("\u0000")) | not)' "$manifest" >/dev/null; then
    echo "::error::existing rolling manifest version must be a single-line JSON string without NUL bytes" >&2
    return 1
  fi
  if ! old_version="$(jq -er '.version | strings' "$manifest")"; then
    echo "::error::existing rolling release manifest has no version" >&2
    return 1
  fi
  [[ "$old_source_commit" =~ ^[0-9a-f]{40}$ ]] || {
    echo "::error::existing rolling manifest source_commit is not 40 lowercase hex" >&2
    return 1
  }
  [ "$old_source_commit" = "$old_tag_sha" ] || {
    echo "::error::existing rolling manifest source_commit does not match its tag" >&2
    return 1
  }
  __EXISTING_VERSION_VALIDATION__
  if ! jq -e \
    --arg schema "$EXPECTED_MANIFEST_SCHEMA" \
    --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
    --arg source_ref "$EXPECTED_SOURCE_REF" \
    --arg commit "$old_source_commit" \
    --slurpfile rolling_manifest "$manifest" \
    '.schema == $schema and .source_repository == $repository and
     .source_ref == $source_ref and .source_commit == $commit' "$manifest" >/dev/null; then
    echo "::error::existing rolling manifest does not match the configured source identity" >&2
    return 1
  fi
  if ! jq -e \
    --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
    --arg source_ref "$EXPECTED_SOURCE_REF" \
    --arg commit "$old_source_commit" \
    --slurpfile rolling_manifest "$manifest" \
    'keys == ["manifest","source_digest","source_ref","source_repository"] and
     .source_repository == $repository and .source_ref == $source_ref and
     .source_digest == $commit and .manifest == $rolling_manifest[0]' "$identity" >/dev/null; then
    echo "::error::existing rolling release identity envelope does not bind its manifest and source" >&2
    return 1
  fi
  if ! jq -e --arg name "$RELEASE_TITLE_PREFIX $old_version" '.name == $name' <<<"$body" >/dev/null; then
    echo "::error::existing rolling release title does not match its manifest version" >&2
    return 1
  fi

  if ! {
    printf '%s\n' "release-manifest.json" "identity.json"
    jq -r '(.assets[] | .name), ((.supporting_assets // [])[] | .name)' "$manifest"
  } | LC_ALL=C sort > "$manifest_assets"; then
    echo "::error::existing rolling manifest asset list could not be read" >&2
    return 1
  fi
  if ! jq -r '.assets[].name' <<<"$body" | LC_ALL=C sort > "$old_assets"; then
    echo "::error::existing rolling release asset list could not be read" >&2
    return 1
  fi
  if ! find "$source_dir" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort > "$downloaded_assets"; then
    echo "::error::existing rolling release download inventory could not be read" >&2
    return 1
  fi
  cmp -s "$manifest_assets" "$old_assets" || {
    echo "::error::existing rolling release assets are not exactly covered by its manifest" >&2
    return 1
  }
  cmp -s "$old_assets" "$downloaded_assets" || {
    echo "::error::existing rolling release download differs from its live asset set" >&2
    return 1
  }

  if ! jq -r '(.assets[] | [.name, .sha256] | @tsv), ((.supporting_assets // [])[] | [.name, .sha256] | @tsv)' "$manifest" > "$manifest_digests"; then
    echo "::error::existing rolling manifest digests could not be read" >&2
    return 1
  fi
  while IFS=$'\t' read -r name expected_digest; do
    if ! test -s "$source_dir/$name"; then
      echo "::error::existing rolling release is missing asset: $name" >&2
      return 1
    fi
    if ! actual_digest="$(sha256sum -- "$source_dir/$name" | awk '{print $1}')"; then
      echo "::error::existing rolling release asset could not be hashed: $name" >&2
      return 1
    fi
    [ "$actual_digest" = "$expected_digest" ] || {
      echo "::error::existing rolling manifest digest mismatch: $name" >&2
      return 1
    }
  done < "$manifest_digests"
  if ! jq -r '.assets[] | [.name, .digest] | @tsv' <<<"$body" > "$github_digests"; then
    echo "::error::existing rolling GitHub asset digests could not be read" >&2
    return 1
  fi
  while IFS=$'\t' read -r name api_digest; do
    if ! actual_digest="$(sha256sum -- "$source_dir/$name" | awk '{print $1}')"; then
      echo "::error::existing rolling release asset could not be hashed: $name" >&2
      return 1
    fi
    [ "$api_digest" = "sha256:$actual_digest" ] || {
      echo "::error::existing rolling GitHub asset digest mismatch: $name" >&2
      return 1
    }
  done < "$github_digests"

  if ! git -C source cat-file -e "${old_source_commit}^{commit}"; then
    echo "::error::existing rolling source commit is not present in the checked-out history" >&2
    return 1
  fi
  if ! git -C source merge-base --is-ancestor "$old_source_commit" "$EXPECTED_SOURCE_COMMIT"; then
    echo "::error::candidate source commit is not a descendant of the live rolling source" >&2
    return 1
  fi
  return 0
}

reject_stale_rolling_draft() {
  echo "::error::existing rolling draft is incomplete or incompatible; repair or remove it before retrying" >&2
  exit 1
}

verify_restored_assets() {
  local source_dir="$1"
  local expected_names="$2"
  local restored_dir="$transaction_dir/restored-package"
  local restored_assets="$transaction_dir/restored-assets"
  local restored_body asset_name source_digest restored_digest remote_digest
  if ! rm -rf -- "$restored_dir"; then
    return 1
  fi
  if ! mkdir -p "$restored_dir"; then
    return 1
  fi
  if ! gh release download "$rolling_tag" --repo "$GITHUB_REPOSITORY" --dir "$restored_dir" --clobber >/dev/null; then
    echo "::error::rollback release could not be downloaded for byte verification" >&2
    return 1
  fi
  if ! restored_body="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id")"; then
    echo "::error::rollback release could not be read for digest verification" >&2
    return 1
  fi
  if ! jq -r '.assets[].name' <<<"$restored_body" | LC_ALL=C sort > "$restored_assets"; then
    return 1
  fi
  if ! cmp -s "$expected_names" "$restored_assets"; then
    echo "::error::rollback release asset set is not exact" >&2
    return 1
  fi
  while IFS= read -r asset_name; do
    if [ -z "$asset_name" ] || ! test -s "$source_dir/$asset_name" || ! test -s "$restored_dir/$asset_name"; then
      echo "::error::rollback release is missing restored asset: $asset_name" >&2
      return 1
    fi
    if ! source_digest="$(sha256sum -- "$source_dir/$asset_name" | awk '{print $1}')"; then
      return 1
    fi
    if ! restored_digest="$(sha256sum -- "$restored_dir/$asset_name" | awk '{print $1}')"; then
      return 1
    fi
    if [ "$source_digest" != "$restored_digest" ]; then
      echo "::error::rollback restored bytes differ: $asset_name" >&2
      return 1
    fi
    if ! remote_digest="$(jq -er --arg name "$asset_name" '
      [ .assets[] | select(.name == $name) ]
      | select(length == 1)
      | .[0].digest
      | strings
      | select(test("^sha256:[0-9a-f]{64}$"))
    ' <<<"$restored_body")"; then
      echo "::error::rollback release has no valid GitHub digest: $asset_name" >&2
      return 1
    fi
    if [ "$remote_digest" != "sha256:$restored_digest" ]; then
      echo "::error::rollback GitHub digest differs: $asset_name" >&2
      return 1
    fi
  done < "$expected_names"
}

rollback() {
  local status="$1"
  trap - ERR EXIT
  if [ "$mutated" = 1 ]; then
    set +e
    rollback_status=0
    if [ "$had_release" = 1 ]; then
      # Hide the release before replacing any public assets. A lost PATCH
      # response is resolved by reading server state, not by assuming failure.
      asset_restore_status=0
      assets_restored=0
      if ! gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" -F draft=true >/dev/null; then
        echo "::warning::draft transition response was uncertain; checking remote release state" >&2
      fi
      if ! current_release_body="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id")"; then
        asset_restore_status=1
      elif ! jq -e --arg tag "$rolling_tag" '.tag_name == $tag and .draft == true' <<<"$current_release_body" >/dev/null; then
        asset_restore_status=1
      fi
      if [ "$asset_restore_status" -eq 0 ]; then
        if ! current_assets="$(gh api --paginate --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id/assets" --jq '.[] | [.id, .name] | @tsv')"; then
          asset_restore_status=1
        else
          while IFS=$'\t' read -r asset_id asset_name; do
            if ! grep -Fqx -- "$asset_name" "$old_assets"; then
              gh api --method DELETE --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/assets/$asset_id" >/dev/null || asset_restore_status=1
            fi
          done <<<"$current_assets"
        fi
        while IFS= read -r asset_name; do
          if ! gh release upload "$rolling_tag" --repo "$GITHUB_REPOSITORY" --clobber "$rollback_dir/$asset_name"; then
            asset_restore_status=1
          fi
        done < "$old_assets"
        if verify_restored_assets "$rollback_dir" "$old_assets"; then
          assets_restored=1
        else
          asset_restore_status=1
        fi
      fi

      # Restore the ref independently of release API state; a tag restore must
      # still be attempted when the release PATCH or asset download failed.
      if [ -z "$old_tag_sha" ] || [ -z "$old_tag_ref_sha" ]; then
        rollback_status=1
      else
        if ! gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag" -f "sha=$old_tag_ref_sha" -F force=true >/dev/null; then
          echo "::warning::tag restore response was uncertain; checking remote ref state" >&2
        fi
        if ! restored_tag_ref_sha="$(remote_tag_ref_sha "$rolling_tag")"; then
          rollback_status=1
        elif [ "$restored_tag_ref_sha" != "$old_tag_ref_sha" ]; then
          rollback_status=1
        fi
        if ! restored_tag_sha="$(remote_tag_sha "$rolling_tag")"; then
          rollback_status=1
        elif [ "$restored_tag_sha" != "$old_tag_sha" ]; then
          rollback_status=1
        fi
      fi

      restore_draft="$old_draft"
      restore_latest="$old_make_latest"
      restore_metadata="$transaction_dir/restore-release-metadata.json"
      if [ "$assets_restored" != 1 ]; then
        # Keep a release with unverified bytes private, while restoring fields
        # that do not expose those bytes.
        restore_draft=true
        restore_latest=false
        rollback_status=1
      fi
      if ! jq -n --rawfile body "$old_body_file" --arg name "$old_name" \
          --argjson draft "$restore_draft" --argjson prerelease "$old_prerelease" \
          --argjson make_latest "$restore_latest" \
          '{name:$name, body:$body, draft:$draft, prerelease:$prerelease, make_latest:$make_latest}' \
          > "$restore_metadata"; then
        echo "::warning::could not encode the original release metadata" >&2
        rollback_status=1
      elif ! gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" \
          --input "$restore_metadata" >/dev/null; then
        echo "::warning::release metadata restore response was uncertain; checking remote release state" >&2
      fi
      if ! restored_body="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id")"; then
        rollback_status=1
      elif ! jq -e --rawfile expected_body "$old_body_file" --arg tag "$rolling_tag" --arg name "$old_name" \
          --argjson draft "$restore_draft" --argjson prerelease "$old_prerelease" \
          '.tag_name == $tag and .name == $name and (.body // "") == $expected_body and .draft == $draft and .prerelease == $prerelease' \
          <<<"$restored_body" >/dev/null; then
        rollback_status=1
      elif ! jq -j '.body // ""' <<<"$restored_body" > "$transaction_dir/restored-release-body" || \
           ! cmp -s "$old_body_file" "$transaction_dir/restored-release-body"; then
        rollback_status=1
      fi
      if [ "$assets_restored" = 1 ]; then
        if ! verify_restored_assets "$rollback_dir" "$old_assets"; then
          rollback_status=1
        fi
        if gh api --repo "$GITHUB_REPOSITORY" -i "repos/$GITHUB_REPOSITORY/releases/latest" > "$latest_response" 2>/dev/null; then
          :
        fi
        restored_latest_http="$(awk 'NR == 1 {print $2; exit}' "$latest_response")"
        case "$restored_latest_http" in
          200)
            restored_latest_body="$(awk 'body {print; next} /^\r?$/ {body = 1}' "$latest_response")"
            restored_latest_tag="$(jq -er '.tag_name | strings' <<<"$restored_latest_body")"
            [ "$restored_latest_tag" = "$latest_tag" ] || rollback_status=1
            ;;
          404)
            [ -z "$latest_tag" ] || rollback_status=1
            ;;
          *) rollback_status=1 ;;
        esac
      fi
    else
      if [ -z "$rolling_release_id" ]; then
        if gh api --repo "$GITHUB_REPOSITORY" -i "repos/$GITHUB_REPOSITORY/releases/tags/$rolling_tag" > "$rolling_response" 2>/dev/null; then
          :
        fi
        discovered_http="$(awk 'NR == 1 {print $2; exit}' "$rolling_response")"
        case "$discovered_http" in
          200)
            discovered_body="$(awk 'body {print; next} /^\r?$/ {body = 1}' "$rolling_response")"
            if ! rolling_release_id="$(jq -er '.id | select(type == "number")' <<<"$discovered_body")"; then
              rollback_status=1
            fi
            ;;
          404) ;;
          *) rollback_status=1 ;;
        esac
      fi
      if [ -n "$rolling_release_id" ]; then
        gh api --method DELETE --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" >/dev/null || rollback_status=1
      fi
      if gh api --repo "$GITHUB_REPOSITORY" -i "repos/$GITHUB_REPOSITORY/releases/tags/$rolling_tag" > "$rolling_response" 2>/dev/null; then
        :
      fi
      absent_release_http="$(awk 'NR == 1 {print $2; exit}' "$rolling_response")"
      [ "$absent_release_http" = 404 ] || rollback_status=1

      if ! current_tag_sha="$(remote_tag_sha "$rolling_tag")"; then
        rollback_status=1
      elif [ -n "$current_tag_sha" ]; then
        gh api --method DELETE --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag" >/dev/null || rollback_status=1
      fi
      if ! absent_tag_sha="$(remote_tag_sha "$rolling_tag")"; then
        rollback_status=1
      elif [ -n "$absent_tag_sha" ]; then
        rollback_status=1
      fi
    fi
    if [ "$rollback_status" -ne 0 ]; then
      echo "::error::rolling package release publication failed and rollback was incomplete" >&2
      status=1
    else
      echo "::warning::rolling package release publication failed; previous state restored" >&2
    fi
  fi
  rm -rf -- "$transaction_dir"
  exit "$status"
}
on_exit() {
  local status="$1"
  trap - EXIT
  if [ "$status" -ne 0 ] && [ "$mutated" = 1 ]; then
    rollback "$status"
  fi
  rm -rf -- "$transaction_dir"
  exit "$status"
}
trap 'on_exit "$?"' EXIT

{
"#,
    );
    script.push_str(expected_asset_names);
    script.push_str(
        r#"
} | LC_ALL=C sort > "$expected_assets"

if [ "$ROLLING_PREFLIGHT_ONLY" = true ]; then
  if ! find "$published_dir" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort > "$staged_assets"; then
    echo "::error::candidate package inventory could not be read during release preflight" >&2
    exit 1
  fi
  cmp -s "$expected_assets" "$staged_assets" || {
    echo "::error::candidate package asset set is not exact during release preflight" >&2
    exit 1
  }
else
  if ! staged_body="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/tags/$staged_tag")"; then
    echo "::error::verified immutable staging release is missing" >&2
    exit 1
  fi
  jq -e --arg tag "$staged_tag" --argjson prerelease "$RELEASE_PRERELEASE" '(.draft | not) and .prerelease == $prerelease and .tag_name == $tag' <<<"$staged_body" >/dev/null
  jq -r '.assets[].name' <<<"$staged_body" | LC_ALL=C sort > "$staged_assets"
  cmp -s "$expected_assets" "$staged_assets" || { echo "::error::immutable staging release asset set is not exact" >&2; exit 1; }
  staged_tag_sha="$(remote_tag_sha "$staged_tag")"
  [ "$staged_tag_sha" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::immutable staging tag does not resolve to the verified source commit" >&2; exit 1; }
fi

latest_tag=""
latest_id=""
latest_version=""
latest_source_commit=""
if gh api --repo "$GITHUB_REPOSITORY" -i "repos/$GITHUB_REPOSITORY/releases/latest" > "$latest_response" 2>/dev/null; then
  :
fi
latest_http="$(awk 'NR == 1 {print $2; exit}' "$latest_response")"
case "$latest_http" in
  200)
    latest_body="$(awk 'body {print; next} /^\r?$/ {body = 1}' "$latest_response")"
    jq -e '(.id | type == "number") and (.tag_name | type == "string" and length > 0)' <<<"$latest_body" >/dev/null
    latest_tag="$(jq -er '.tag_name | strings' <<<"$latest_body")"
    latest_id="$(jq -er '.id' <<<"$latest_body")"
    [ "$latest_tag" != "$rolling_tag" ] || old_make_latest=true
    verify_latest_floor
    ;;
  404) ;;
  *)
    echo "::error::current GitHub Latest release could not be determined; refusing package publication" >&2
    exit 1
    ;;
esac
manifest="$published_dir/release-manifest.json"

if gh api --repo "$GITHUB_REPOSITORY" -i "repos/$GITHUB_REPOSITORY/releases/tags/$rolling_tag" > "$rolling_response" 2>/dev/null; then
  :
fi
rolling_http="$(awk 'NR == 1 {print $2; exit}' "$rolling_response")"
case "$rolling_http" in
  200)
    awk 'body {print; next} /^\r?$/ {body = 1}' "$rolling_response" > "$rolling_body"
    had_release=1
    jq -e --arg tag "$rolling_tag" '(.draft | type == "boolean") and .tag_name == $tag and (.id | type == "number")' "$rolling_body" >/dev/null
    rolling_release_id="$(jq -er '.id' "$rolling_body")"
    old_name="$(jq -er '.name | strings' "$rolling_body")"
    if ! jq -e '(.body == null) or (.body | type == "string")' "$rolling_body" >/dev/null || \
       ! jq -j '.body // ""' "$rolling_body" > "$old_body_file"; then
      echo "::error::existing rolling release body is not a readable string" >&2
      exit 1
    fi
    old_draft="$(jq -er '.draft | tostring' "$rolling_body")"
    old_prerelease="$(jq -er '.prerelease | tostring' "$rolling_body")"
    old_tag_sha="$(remote_tag_sha "$rolling_tag")"
    old_tag_ref_sha="$(remote_tag_ref_sha "$rolling_tag")"
    if ! [[ "$old_tag_sha" =~ ^[0-9a-f]{40}$ ]]; then
      if [ "$old_draft" = true ]; then
        reject_stale_rolling_draft
      else
        echo "::error::rolling release tag is not a commit ref" >&2
        exit 1
      fi
    elif ! [[ "$old_tag_ref_sha" =~ ^[0-9a-f]{40}$ ]]; then
      if [ "$old_draft" = true ]; then
        reject_stale_rolling_draft
      else
        echo "::error::rolling release tag ref object could not be verified" >&2
        exit 1
      fi
    elif [ "$old_draft" = true ] && [ "$old_prerelease" != "$RELEASE_PRERELEASE" ]; then
      reject_stale_rolling_draft
    elif ! mkdir -p "$rollback_dir" || ! gh release download "$rolling_tag" --repo "$GITHUB_REPOSITORY" --dir "$rollback_dir" --clobber; then
      if [ "$old_draft" = true ]; then
        reject_stale_rolling_draft
      else
        echo "::error::existing rolling release could not be downloaded" >&2
        exit 1
      fi
    elif jq -e '__KNOWN_MANIFEST_SHAPE_VALIDATION__' "$rollback_dir/release-manifest.json" >/dev/null &&
         ! jq -e '__MANIFEST_KEY_VALIDATION__' "$rollback_dir/release-manifest.json" >/dev/null; then
      echo "::error::existing rolling release manifest shape differs from the configured package contract; explicit manifest migration is required before publication" >&2
      exit 1
    elif ! validate_existing_rolling_release "$rolling_body" "$rollback_dir"; then
      if [ "$old_draft" = true ]; then
        reject_stale_rolling_draft
      else
        echo "::error::existing rolling release failed immutable validation" >&2
        exit 1
      fi
    fi
    if [ "$had_release" = 1 ]; then
      if ! cmp -s "$expected_assets" "$old_assets"; then
        echo "::error::existing rolling release asset set differs from the configured package contract; explicit asset migration is required before publication" >&2
        exit 1
      fi
      old_version_order="${old_version%%+*}"
      candidate_version_order="${candidate_version%%+*}"
      if [ "$old_version" = "$candidate_version" ] && [ "$old_source_commit" = "$EXPECTED_SOURCE_COMMIT" ]; then
        :
      elif [ "$old_version_order" = "$candidate_version_order" ] || [ "$(printf '%s\n' "$old_version_order" "$candidate_version_order" | LC_ALL=C sort -V | tail -n 1)" != "$candidate_version_order" ]; then
        echo "::error::candidate version is not newer than the live rolling version" >&2
        exit 1
      fi
    fi
"#,
    );
    script.push_str(
        r#"
    ;;
  404)
    if ! rolling_tag_sha="$(remote_tag_sha "$rolling_tag")"; then
      echo "::error::could not verify that the rolling tag is absent" >&2
      exit 1
    fi
    [ -z "$rolling_tag_sha" ] || { echo "::error::rolling tag exists without a release; refusing to overwrite it" >&2; exit 1; }
    ;;
  *)
    echo "::error::rolling release preflight failed with HTTP $rolling_http" >&2
    exit 1
    ;;
esac

if [ "$ROLLING_PREFLIGHT_ONLY" = true ]; then
  exit 0
fi

if [ "$had_release" = 0 ]; then
  mutated=1
  create_json="$(gh api --method POST --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases" \
    -f "tag_name=$rolling_tag" -f "target_commitish=$EXPECTED_SOURCE_COMMIT" \
    -f "name=$RELEASE_TITLE_PREFIX $candidate_version" \
    -f "body=Verified package release from $EXPECTED_SOURCE_COMMIT" \
    -F draft=true -F "prerelease=$RELEASE_PRERELEASE" -F make_latest=false)"
  rolling_release_id="$(jq -er '.id' <<<"$create_json")"
else
  mutated=1
  gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" -F draft=true >/dev/null
fi

# The immutable source-bound release is the candidate staging record.  Copy its
# already-verified files only while the rolling release remains a draft; draft
# publication is the visibility boundary, so readers never see mixed assets.
gh release upload "$rolling_tag" --repo "$GITHUB_REPOSITORY" --clobber \
"#,
    );
    script.push_str(published_assets);
    script.push_str(
        r#"

rolling_stage_json="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id")"
jq -e --arg tag "$rolling_tag" --argjson prerelease "$RELEASE_PRERELEASE" '.draft == true and .prerelease == $prerelease and .tag_name == $tag' <<<"$rolling_stage_json" >/dev/null
jq -r '.assets[].name' <<<"$rolling_stage_json" | LC_ALL=C sort > "$rolling_staged_assets"
cmp -s "$expected_assets" "$rolling_staged_assets" || { echo "::error::staged rolling release asset set is not exact" >&2; exit 1; }

if [ "$had_release" = 1 ]; then
  gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag" -f "sha=$EXPECTED_SOURCE_COMMIT" -F force=true >/dev/null
fi
gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" \
  -f "name=$RELEASE_TITLE_PREFIX $candidate_version" \
  -f "body=Verified package release from $EXPECTED_SOURCE_COMMIT" \
  -F draft=false -F "prerelease=$RELEASE_PRERELEASE" -F make_latest=false >/dev/null

new_tag_sha="$(remote_tag_sha "$rolling_tag")"
[ "$new_tag_sha" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::rolling tag does not resolve to the verified source commit" >&2; exit 1; }
rolling_post_json="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/tags/$rolling_tag")"
jq -e --arg tag "$rolling_tag" --argjson prerelease "$RELEASE_PRERELEASE" '(.draft | not) and .prerelease == $prerelease and .tag_name == $tag' <<<"$rolling_post_json" >/dev/null
jq -r '.assets[].name' <<<"$rolling_post_json" | LC_ALL=C sort > "$rolling_published_assets"
cmp -s "$expected_assets" "$rolling_published_assets" || { echo "::error::rolling release asset set is not exact after publication" >&2; exit 1; }
rm -rf -- "$transaction_dir/rolling-published"
mkdir -p "$transaction_dir/rolling-published"
gh release download "$rolling_tag" --repo "$GITHUB_REPOSITORY" --dir "$transaction_dir/rolling-published" --clobber
export VELNOR_VERIFIED_PACKAGE_DIR="$transaction_dir/rolling-published"
export VELNOR_VERIFICATION_TEMP_DIR="$transaction_dir"
"#,
    );
    script.push_str(&inline_verification);
    script.push_str("\nfor payload in \\\n");
    script.push_str(attested_asset_names);
    script.push_str("do\n  gh attestation verify \"$transaction_dir/rolling-published/$payload\" ");
    script.push_str(verification.attestation_flags);
    script.push_str("\ndone\n");
    script = script.replace(
        "__MANIFEST_SHAPE_VALIDATION__",
        &manifest_shape_validation(spec),
    );
    script = script.replace("__MANIFEST_VERSION_GUARD__", manifest_version_line_guard());
    script = script.replace("__MANIFEST_KEY_VALIDATION__", manifest_key_validation(spec));
    script = script.replace(
        "__KNOWN_MANIFEST_SHAPE_VALIDATION__",
        known_manifest_shape_validation(),
    );
    script = script.replace(
        "__EXISTING_VERSION_VALIDATION__",
        &version_validation_return_script(
            spec.version_format,
            "old_version",
            "old_source_commit",
            "existing rolling manifest",
        ),
    );
    script = script.replace(
        "__LATEST_VERSION_VALIDATION__",
        &version_validation_script(
            VersionFormat::Semver,
            "latest_version",
            "latest_source_commit",
            "current GitHub Latest manifest",
        ),
    );
    script = script.replace(
        "__LATEST_MANIFEST_IDENTITY_VALIDATION__",
        latest_manifest_identity_validation(),
    );
    script = script.replace(
        "__LATEST_IDENTITY_ENVELOPE_VALIDATION__",
        latest_identity_envelope_validation(),
    );
    script = script.replace(
        "__LATEST_ATTESTATION_FLAGS__",
        &verification
            .attestation_flags
            .replace("\"$EXPECTED_SOURCE_COMMIT\"", "\"$latest_source_commit\""),
    );
    script
}

/// Common verification helpers and initialization for immutable publication.
fn immutable_publish_script_prelude() -> &'static str {
    r#"set -euo pipefail
case "$RELEASE_PRERELEASE" in
  true|false) ;;
  *) echo "::error::invalid configured GitHub release type" >&2; exit 1 ;;
esac
case "$RELEASE_LATEST" in
  true|false) ;;
  *) echo "::error::invalid configured latest-release policy" >&2; exit 1 ;;
esac
if [ "$RELEASE_PRERELEASE" = true ] && [ "$RELEASE_LATEST" = true ]; then
  echo "::error::prerelease lanes cannot mark an immutable release latest" >&2
  exit 1
fi
release_flags=(--latest=false)
if [ "$RELEASE_PRERELEASE" = true ]; then
  release_flags+=(--prerelease)
fi
tag="$RELEASE_TAG-$EXPECTED_SOURCE_COMMIT"
manifest="$PACKAGE_DIR/release-manifest.json"
__MANIFEST_VERSION_GUARD__
version="$(jq -er '.version | strings' "$manifest")"
title="$RELEASE_TITLE_PREFIX $version"
transaction_dir="$(mktemp -d)"
release_json="$transaction_dir/release-response"
expected_assets="$transaction_dir/expected-assets"
existing_assets="$transaction_dir/existing-assets"
download_dir="$transaction_dir/download"
immutable_public=0
trap 'rm -rf -- "$transaction_dir"' EXIT

remote_tag_sha() {
  local tag_name="$1"
  local result sha
  if ! result="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name^{}")"; then
    echo "::error::failed to query remote tag: $tag_name" >&2
    return 1
  fi
  sha="$(awk 'NR == 1 {print $1}' <<<"$result")"
  if [ -z "$sha" ]; then
    if ! result="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name")"; then
      echo "::error::failed to query remote tag: $tag_name" >&2
      return 1
    fi
    sha="$(awk 'NR == 1 {print $1}' <<<"$result")"
  fi
  if [ -n "$sha" ] && ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    echo "::error::remote tag returned an invalid object id: $tag_name" >&2
    return 1
  fi
  printf '%s\n' "$sha"
}

verify_asset_bytes() {
  local release_tag="$1"
  local names_file="$2"
  local source_dir="$3"
  local target_dir="$4"
  local downloaded_assets name expected actual
  rm -rf -- "$target_dir"
  mkdir -p "$target_dir"
  gh release download "$release_tag" --repo "$GITHUB_REPOSITORY" --dir "$target_dir"
  downloaded_assets="$transaction_dir/downloaded-assets"
  find "$target_dir" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort > "$downloaded_assets"
  cmp -s "$names_file" "$downloaded_assets" || {
    echo "::error::immutable release downloaded asset set differs from its API asset set" >&2
    return 1
  }
  while IFS= read -r name; do
    test -n "$name"
    test -s "$source_dir/$name"
    test -s "$target_dir/$name"
    expected="$(sha256sum -- "$source_dir/$name" | awk '{print $1}')"
    actual="$(sha256sum -- "$target_dir/$name" | awk '{print $1}')"
    [ "$actual" = "$expected" ] || {
      echo "::error::immutable release asset bytes differ: $name" >&2
      return 1
    }
  done < "$names_file"
}

{
"#
}

fn render_immutable_publish_script(spec: &PackageReleaseSpec) -> String {
    let mut script = String::from(immutable_publish_script_prelude());
    for name in release_asset_names(spec) {
        let _ = writeln!(script, "  printf '%s\\n' {}", shell_quote(&name));
    }
    script.push_str(
        r#"} | LC_ALL=C sort > "$expected_assets"

tag_sha="$(remote_tag_sha "$tag")"
if [ -n "$tag_sha" ] && [ "$tag_sha" != "$EXPECTED_SOURCE_COMMIT" ]; then
  echo "::error::immutable release tag resolves to an unexpected source commit" >&2
  exit 1
fi

if ! gh api --repo "$GITHUB_REPOSITORY" -i "repos/$GITHUB_REPOSITORY/releases/tags/$tag" > "$release_json" 2>/dev/null; then
  :
fi
response_http="$(awk 'NR == 1 {print $2; exit}' "$release_json")"
case "$response_http" in
  404)
    if [ -n "$tag_sha" ]; then
      gh release create "$tag" --repo "$GITHUB_REPOSITORY" --verify-tag --draft "${release_flags[@]}" --title "$title" --notes "Verified immutable package release $version from $EXPECTED_SOURCE_COMMIT"
    else
      gh release create "$tag" --repo "$GITHUB_REPOSITORY" --target "$EXPECTED_SOURCE_COMMIT" --draft "${release_flags[@]}" --title "$title" --notes "Verified immutable package release $version from $EXPECTED_SOURCE_COMMIT"
    fi
    tag_sha="$(remote_tag_sha "$tag")"
    [ "$tag_sha" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::new immutable release tag does not resolve to the verified source commit" >&2; exit 1; }
    ;;
  200)
    live_body="$(awk 'body {print; next} /^\r?$/ {body = 1}' "$release_json")"
    jq -e --arg tag "$tag" --arg title "$title" --argjson prerelease "$RELEASE_PRERELEASE" \
      '.tag_name == $tag and .name == $title and .prerelease == $prerelease' <<<"$live_body" >/dev/null \
      || { echo "::error::immutable release identity does not match the verified candidate" >&2; exit 1; }
    test -n "$tag_sha" || { echo "::error::existing immutable release has no exact source tag" >&2; exit 1; }
    [ "$tag_sha" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::existing immutable release tag moved" >&2; exit 1; }
    jq -r '.assets[].name' <<<"$live_body" | LC_ALL=C sort > "$existing_assets"
    if comm -23 "$existing_assets" "$expected_assets" | grep -q .; then
      echo "::error::immutable release contains an undeclared asset" >&2
      exit 1
    fi
    if jq -e '.draft == true' <<<"$live_body" >/dev/null; then
      if [ -s "$existing_assets" ]; then
        verify_asset_bytes "$tag" "$existing_assets" "$PACKAGE_DIR" "$download_dir"
      fi
    else
      cmp -s "$expected_assets" "$existing_assets" || {
        echo "::error::immutable release is already public but incomplete; refusing to mutate it" >&2
        exit 1
      }
      verify_asset_bytes "$tag" "$expected_assets" "$PACKAGE_DIR" "$download_dir" || {
        echo "::error::immutable release is already public but its bytes differ; refusing to mutate it" >&2
        exit 1
      }
      immutable_public=1
    fi
    ;;
  *)
    echo "::error::release preflight failed; refusing publication" >&2
    exit 1
    ;;
esac

if [ "$immutable_public" = 0 ]; then
  while IFS= read -r asset_name; do
    if ! grep -Fqx -- "$asset_name" "$existing_assets"; then
      gh release upload "$tag" --repo "$GITHUB_REPOSITORY" "$PACKAGE_DIR/$asset_name"
    fi
  done < "$expected_assets"

  staged_body="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/tags/$tag")"
  jq -e --arg tag "$tag" --arg title "$title" --argjson prerelease "$RELEASE_PRERELEASE" \
    '.draft == true and .prerelease == $prerelease and .tag_name == $tag and .name == $title' <<<"$staged_body" >/dev/null \
    || { echo "::error::immutable staging release is not still a draft with the expected identity" >&2; exit 1; }
  jq -r '.assets[].name' <<<"$staged_body" | LC_ALL=C sort > "$existing_assets"
  cmp -s "$expected_assets" "$existing_assets" || {
    echo "::error::immutable staging release asset set is not exact" >&2
    exit 1
  }
  verify_asset_bytes "$tag" "$expected_assets" "$PACKAGE_DIR" "$download_dir"
  gh api --method PATCH --repo "$GITHUB_REPOSITORY" \
    "repos/$GITHUB_REPOSITORY/releases/$(jq -er '.id' <<<"$staged_body")" \
    -F draft=false -F "prerelease=$RELEASE_PRERELEASE" -F make_latest=false >/dev/null
fi

immutable_body="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/tags/$tag")"
jq -e --arg tag "$tag" --arg title "$title" --argjson prerelease "$RELEASE_PRERELEASE" \
  '(.draft | not) and .prerelease == $prerelease and .tag_name == $tag and .name == $title' <<<"$immutable_body" >/dev/null \
  || { echo "::error::immutable release was not published with the expected identity" >&2; exit 1; }
jq -r '.assets[].name' <<<"$immutable_body" | LC_ALL=C sort > "$existing_assets"
cmp -s "$expected_assets" "$existing_assets" || {
  echo "::error::immutable release asset set is not exact after publication" >&2
  exit 1
}
final_tag_sha="$(remote_tag_sha "$tag")"
[ "$final_tag_sha" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::immutable tag does not resolve to the verified source commit" >&2; exit 1; }
printf 'immutable_tag=%s\n' "$tag" >> "$GITHUB_OUTPUT"
"#
    );
    script.replace("__MANIFEST_VERSION_GUARD__", manifest_version_line_guard())
}

/// Render the publication half separately from the build half.  The release
/// tag is source-bound and immutable: the configured tag is only a namespace
/// prefix.  This keeps a failed candidate from deleting or partially replacing
/// the previously published preview.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_publish_job(
    spec: &PackageReleaseSpec,
    runner: &str,
    checkout: &str,
    download: &str,
    publish_verify: &str,
    publish_attestation_targets: &str,
    attestation_flags: &str,
    workspace_expr: &str,
    updater_token_expr: &str,
    github_token_expr: &str,
    publish_source_commit_expr: &str,
    channel_yaml: &str,
    source_repository_yaml: &str,
    source_ref_yaml: &str,
    schema_yaml: &str,
    tag_yaml: &str,
    title_yaml: &str,
    consumer_repository_yaml: &str,
    consumer_branch_yaml: &str,
    updater_yaml: &str,
    message_yaml: &str,
) -> String {
    let publish_environment_yaml = crate::s2::yaml_scalar(&spec.publish_environment);
    let release_prerelease_yaml =
        crate::s2::yaml_scalar(if spec.github_release_type == "prerelease" {
            "true"
        } else {
            "false"
        });
    let release_latest_yaml = crate::s2::yaml_scalar(if spec.github_release_type == "release" {
        "true"
    } else {
        "false"
    });
    let mut attested_asset_names = String::new();
    let mut attested_names = spec.payloads.clone();
    attested_names.extend(spec.supporting_assets.iter().cloned());
    attested_names.push("release-manifest.json".to_owned());
    attested_names.push("identity.json".to_owned());
    for (index, name) in attested_names.iter().enumerate() {
        let suffix = if index + 1 == attested_names.len() {
            ""
        } else {
            " \\"
        };
        let _ = writeln!(attested_asset_names, "  {}{}", shell_quote(name), suffix);
    }
    let mut expected_asset_names = String::new();
    for name in release_asset_names(spec) {
        let _ = writeln!(
            expected_asset_names,
            "  printf '%s\\n' {}",
            shell_quote(&name)
        );
    }
    let mut published_assets = String::new();
    for (index, name) in release_asset_names(spec).iter().enumerate() {
        let suffix = if index + 1 == release_asset_names(spec).len() {
            ""
        } else {
            " \\"
        };
        let _ = writeln!(published_assets, "  \"$published_dir/{name}\"{suffix}");
    }
    let verification = PublishVerification {
        script: publish_verify,
        attestation_flags,
    };
    let rolling_refresh = indent_script(
        &render_rolling_refresh_script(
            spec,
            &published_assets,
            &expected_asset_names,
            &attested_asset_names,
            &verification,
        ),
        10,
    );
    let mut output = String::new();
    let _ = writeln!(output, "  publish:");
    output.push_str("    name: Publish immutable package and update consumer\n");
    output.push_str("    needs: build\n");
    output.push_str("    concurrency:\n      group: ");
    output.push_str(GLOBAL_PUBLISH_CONCURRENCY_GROUP);
    output.push_str("\n      queue: max\n      cancel-in-progress: false\n");
    output.push_str("    if: ");
    output.push_str(&github_expression(&format!(
        "github.event_name == 'push' && github.ref == '{}'",
        spec.source_ref
    )));
    output.push('\n');
    output.push_str("    runs-on: ");
    output.push_str(runner);
    output.push_str("\n    defaults:\n      run:\n        shell: bash\n    timeout-minutes: 30\n    environment: ");
    output.push_str(&publish_environment_yaml);
    output.push('\n');
    output.push_str(
        "    permissions:\n      contents: write\n      pull-requests: write\n      attestations: read\n",
    );
    output.push_str("    outputs:\n      release_tag: ");
    output.push_str(&github_expression("steps.publish.outputs.immutable_tag"));
    output.push_str("\n      consumer_pr_url: ");
    output.push_str(&github_expression("steps.consumer-pr.outputs.pr_url"));
    output.push_str("\n    env:\n      PACKAGE_DIR: package\n      VELNOR_VERIFIED_PACKAGE_DIR: ");
    output.push_str(workspace_expr);
    output.push_str("/package\n      VELNOR_PACKAGE_CHANNEL: ");
    output.push_str(channel_yaml);
    output.push_str("\n      EXPECTED_SOURCE_REPOSITORY: ");
    output.push_str(source_repository_yaml);
    output.push_str("\n      EXPECTED_SOURCE_REF: ");
    output.push_str(source_ref_yaml);
    output.push_str("\n      EXPECTED_MANIFEST_SCHEMA: ");
    output.push_str(schema_yaml);
    output.push_str("\n      EXPECTED_SOURCE_COMMIT: ");
    output.push_str(publish_source_commit_expr);
    output.push_str("\n      VELNOR_SOURCE_CHECKOUT_DIR: ");
    output.push_str(workspace_expr);
    output.push_str("/source");
    output.push_str("\n      RELEASE_TAG: ");
    output.push_str(tag_yaml);
    output.push_str("\n      RELEASE_PRERELEASE: ");
    output.push_str(&release_prerelease_yaml);
    output.push_str("\n      RELEASE_LATEST: ");
    output.push_str(&release_latest_yaml);
    output.push_str("\n      RELEASE_TITLE_PREFIX: ");
    output.push_str(title_yaml);
    output.push_str("\n      CONSUMER_REPOSITORY: ");
    output.push_str(consumer_repository_yaml);
    output.push_str("\n      CONSUMER_BRANCH: ");
    output.push_str(consumer_branch_yaml);
    output.push_str("\n      UPDATER: ");
    output.push_str(updater_yaml);
    output.push_str("\n      UPDATER_TOKEN: ");
    output.push_str(updater_token_expr);
    output.push_str("\n      UPDATE_COMMIT_MESSAGE: ");
    output.push_str(message_yaml);
    output.push_str("\n    steps:\n");

    output.push_str("      - name: Checkout verified source for publication\n        uses: ");
    output.push_str(checkout);
    output.push_str("\n        with:\n          repository: ");
    output.push_str(source_repository_yaml);
    output.push_str("\n          ref: ");
    output.push_str(publish_source_commit_expr);
    output.push_str("\n          fetch-depth: 0\n          path: source\n          persist-credentials: false\n");

    output.push_str("      - name: Download verified package handoff\n        uses: ");
    output.push_str(download);
    output.push_str("\n        with:\n          name: package-release\n          path: package\n          merge-multiple: true\n");
    output.push_str(
        "      - name: Re-verify downloaded handoff\n        id: verify\n        run: |\n",
    );
    output.push_str(publish_verify);

    output.push_str("      - name: Verify build attestations\n        env:\n          GH_TOKEN: ");
    output.push_str(github_token_expr);
    output.push_str("\n        run: |\n          set -euo pipefail\n          for payload in \\\n");
    output.push_str(publish_attestation_targets);
    output.push_str("          do\n            gh attestation verify \"$payload\" ");
    output.push_str(attestation_flags);
    output.push_str("\n          done\n");

    output.push_str(
        "      - name: Preflight rolling package release\n        env:\n          GH_TOKEN: ",
    );
    output.push_str(github_token_expr);
    output.push_str("\n          ROLLING_PREFLIGHT_ONLY: \"true\"\n        run: |\n");
    output.push_str(&rolling_refresh);

    output.push_str("      - name: Publish immutable source-bound release\n        id: publish\n        env:\n          GH_TOKEN: ");
    output.push_str(github_token_expr);
    output.push_str("\n        run: |\n");
    output.push_str(&indent_script(&render_immutable_publish_script(spec), 10));

    let immutable_tag_output = github_expression("steps.publish.outputs.immutable_tag");
    output.push_str("      - name: Download and re-verify published release\n        env:\n          GH_TOKEN: ");
    output.push_str(github_token_expr);
    output.push_str("\n          RELEASE_ASSET_TAG: ");
    output.push_str(&immutable_tag_output);
    output.push_str(
        "\n        run: |\n          set -euo pipefail\n          rm -rf published-package\n          mkdir -p published-package\n          gh release download \"$RELEASE_ASSET_TAG\" --repo \"$GITHUB_REPOSITORY\" --dir published-package\n          export VELNOR_VERIFIED_PACKAGE_DIR=\"$GITHUB_WORKSPACE/published-package\"\n",
    );
    output.push_str(publish_verify);

    output.push_str(
        "      - name: Verify published release attestations\n        env:\n          GH_TOKEN: ",
    );
    output.push_str(github_token_expr);
    output.push_str("\n          PACKAGE_DIR: published-package\n          RELEASE_ASSET_TAG: ");
    output.push_str(&immutable_tag_output);
    output.push_str("\n        run: |\n          set -euo pipefail\n          for payload in \\\n");
    output.push_str(publish_attestation_targets);
    output.push_str("          do\n            gh attestation verify \"$payload\" ");
    output.push_str(attestation_flags);
    output.push_str("\n          done\n");

    output.push_str(
        "      - name: Refresh rolling package release\n        env:\n          GH_TOKEN: ",
    );
    output.push_str(github_token_expr);
    output.push_str("\n        run: |\n");
    output.push_str(&rolling_refresh);
    if spec.github_release_type == "release" {
        output.push_str(
            "      - name: Promote immutable release to GitHub Latest after rolling refresh\n        env:\n          GH_TOKEN: ",
        );
        output.push_str(github_token_expr);
        output.push_str(
            "\n        run: |\n          set -euo pipefail\n          immutable_tag=\"$RELEASE_TAG-$EXPECTED_SOURCE_COMMIT\"\n          release_body=\"$(gh api --repo \"$GITHUB_REPOSITORY\" \"repos/$GITHUB_REPOSITORY/releases/tags/$immutable_tag\")\"\n          jq -e --arg tag \"$immutable_tag\" '.tag_name == $tag and .draft == false and .prerelease == false and (.id | type == \"number\")' <<<\"$release_body\" >/dev/null\n          release_id=\"$(jq -er '.id' <<<\"$release_body\")\"\n          gh api --method PATCH --repo \"$GITHUB_REPOSITORY\" \"repos/$GITHUB_REPOSITORY/releases/$release_id\" -F make_latest=true >/dev/null\n          latest_body=\"$(gh api --repo \"$GITHUB_REPOSITORY\" \"repos/$GITHUB_REPOSITORY/releases/latest\")\"\n          jq -e --arg tag \"$immutable_tag\" '.tag_name == $tag' <<<\"$latest_body\" >/dev/null\n",
        );
    }
    output.push_str("      - name: Checkout consumer repository\n        uses: ");
    output.push_str(checkout);
    output.push_str("\n        with:\n          repository: ");
    output.push_str(consumer_repository_yaml);
    output.push_str("\n          ref: ");
    output.push_str(consumer_branch_yaml);
    output.push_str("\n          token: ");
    output.push_str(updater_token_expr);
    output.push_str("\n          path: consumer\n          persist-credentials: false\n");

    output.push_str("      - name: Run updater and create or update consumer PR\n        id: consumer-pr\n        env:\n          RELEASE_ASSET_TAG: ");
    output.push_str(&immutable_tag_output);
    output.push_str("\n          GH_TOKEN: ");
    output.push_str(updater_token_expr);
    output.push_str(
        "\n        run: |\n          set -euo pipefail\n          cd consumer\n          git config user.name \"github-actions[bot]\"\n          git config user.email \"41898282+github-actions[bot]@users.noreply.github.com\"\n          automation_branch=\"automation/package-release-$RELEASE_TAG\"\n          remote_branch_sha=\"$(git ls-remote origin \"refs/heads/$automation_branch\" | awk 'NR == 1 {print $1}')\"\n          git switch --force-create \"$automation_branch\" \"origin/$CONSUMER_BRANCH\"\n          VELNOR_PACKAGE_CHANNEL=\"$VELNOR_PACKAGE_CHANNEL\" VELNOR_PACKAGE_RELEASE_TAG=\"$RELEASE_ASSET_TAG\" VELNOR_VERIFIED_PACKAGE_DIR=\"$GITHUB_WORKSPACE/published-package\" bash -c \"$UPDATER\"\n          git diff --check\n          if git diff --quiet; then\n            echo \"consumer already references the verified release\"\n          else\n            git add -A\n            git commit -s -m \"$UPDATE_COMMIT_MESSAGE\"\n            if [ -n \"$remote_branch_sha\" ]; then\n              git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" push --force-with-lease=refs/heads/$automation_branch:$remote_branch_sha origin \"HEAD:refs/heads/$automation_branch\"\n            else\n              git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" push origin \"HEAD:refs/heads/$automation_branch\"\n            fi\n          fi\n          pr_url=\"$(gh pr list --repo \"$CONSUMER_REPOSITORY\" --head \"$automation_branch\" --base \"$CONSUMER_BRANCH\" --state open --json url --jq '.[0].url // empty')\"\n          if [ -z \"$pr_url\" ] && ! git diff --quiet HEAD \"origin/$CONSUMER_BRANCH\"; then\n            pr_url=\"$(gh pr create --repo \"$CONSUMER_REPOSITORY\" --head \"$automation_branch\" --base \"$CONSUMER_BRANCH\" --title \"$UPDATE_COMMIT_MESSAGE ($RELEASE_ASSET_TAG)\" --body \"Automated verified package update. Review and merge this PR; the publisher never merges consumer changes.\")\"\n          fi\n          printf 'pr_url=%s\\n' \"$pr_url\" >> \"$GITHUB_OUTPUT\"\n          if [ -n \"$pr_url\" ]; then echo \"::notice::Consumer update PR: $pr_url\"; else echo \"::notice::Consumer update PR: none\"; fi\n",
    );
    output = output.replace(
        "          automation_branch=\"automation/package-release-$RELEASE_TAG\"\n          remote_branch_sha=\"$(git ls-remote origin \"refs/heads/$automation_branch\" | awk 'NR == 1 {print $1}')\"\n          git switch --force-create \"$automation_branch\" \"origin/$CONSUMER_BRANCH\"\n",
        "          stale_branch=\"automation/package-release-$RELEASE_TAG\"\n          automation_branch=\"automation/package-release-$RELEASE_ASSET_TAG\"\n          while IFS= read -r stale_pr_url; do\n            if [ -n \"$stale_pr_url\" ]; then\n              gh pr close \"$stale_pr_url\" --repo \"$CONSUMER_REPOSITORY\" --comment \"Superseded by immutable package release $RELEASE_ASSET_TAG\"\n            fi\n          done < <(gh pr list --repo \"$CONSUMER_REPOSITORY\" --head \"$stale_branch\" --base \"$CONSUMER_BRANCH\" --state open --json url --jq '.[].url')\n          remote_branch_sha=\"$(git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" ls-remote origin \"refs/heads/$automation_branch\" | awk 'NR == 1 {print $1}')\"\n          if [ -n \"$remote_branch_sha\" ]; then\n            git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" fetch origin \"refs/heads/$automation_branch:refs/remotes/origin/$automation_branch\"\n            git switch --detach \"origin/$automation_branch\"\n          else\n            git switch --create \"$automation_branch\" \"origin/$CONSUMER_BRANCH\"\n          fi\n",
    );
    output = output.replace(
        "          git diff --check\n          if git diff --quiet; then",
        "          untracked_files=\"$(git ls-files --others --exclude-standard)\"\n          if [ -n \"$untracked_files\" ]; then\n            echo \"::notice::consumer updater produced untracked files; staging them\"\n            printf '%s\\n' \"$untracked_files\"\n          fi\n          git diff --check\n          if [ -n \"$(git status --porcelain --untracked-files=all)\" ]; then",
    );
    output = output.replace(
        "            if [ -n \"$remote_branch_sha\" ]; then\n              git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" push --force-with-lease=refs/heads/$automation_branch:$remote_branch_sha origin \"HEAD:refs/heads/$automation_branch\"\n            else\n              git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" push origin \"HEAD:refs/heads/$automation_branch\"\n            fi",
        "            git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" push origin \"HEAD:refs/heads/$automation_branch\"",
    );
    output = output.replace(
        "          else\n            git add -A\n            git commit -s -m \"$UPDATE_COMMIT_MESSAGE\"\n            git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" push origin \"HEAD:refs/heads/$automation_branch\"\n          fi",
        "          else\n            if [ -n \"$remote_branch_sha\" ]; then\n              echo \"::error::immutable consumer branch already exists and would need rewriting; refusing to mutate it\" >&2\n              exit 1\n            fi\n            git add -A\n            git commit -s -m \"$UPDATE_COMMIT_MESSAGE\"\n            git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" push origin \"HEAD:refs/heads/$automation_branch\"\n          fi",
    );
    output = output.replace(
        "VELNOR_PACKAGE_RELEASE_TAG=\"$RELEASE_ASSET_TAG\"",
        "VELNOR_PACKAGE_RELEASE_TAG=\"$RELEASE_TAG\"",
    );
    output
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
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
version_format = "channel"
manifest_format = "supporting-assets-v1"
release_tag = "preview"
github_release_type = "prerelease"
publish_environment = "github-preview"
consumer_repository = "example/tap"
consumer_branch = "main"
updater = "./scripts/package-update.sh"
updater_token_secret = "TAP_TOKEN"
update_commit_message = "chore: update verified preview"
"#,
        )
        .expect("fixture args")
    }

    fn stable_args() -> BTreeMap<String, toml::Value> {
        let mut values = args();
        values.insert(
            "channel".to_owned(),
            toml::Value::String("stable".to_owned()),
        );
        values.insert(
            "version_format".to_owned(),
            toml::Value::String("semver".to_owned()),
        );
        values.insert(
            "release_tag".to_owned(),
            toml::Value::String("stable".to_owned()),
        );
        values.insert(
            "github_release_type".to_owned(),
            toml::Value::String("release".to_owned()),
        );
        values.remove("supporting_assets");
        values.insert(
            "manifest_format".to_owned(),
            toml::Value::String("core-v1".to_owned()),
        );
        values
    }

    fn consumer_fixture_path(lane: &str, file: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/package-release")
            .join(lane)
            .join(file)
    }

    fn rolling_refresh_script_for_test(spec: &PackageReleaseSpec, workflow_file: &str) -> String {
        let attestation_flags = format!(
            "--repo \"$GITHUB_REPOSITORY\" --signer-workflow \"$GITHUB_REPOSITORY/.github/workflows/{workflow_file}\" --source-ref \"$EXPECTED_SOURCE_REF\" --source-digest \"$EXPECTED_SOURCE_COMMIT\""
        );
        let assets = release_asset_names(spec);
        let mut expected_assets = String::new();
        let mut published_assets = String::new();
        for (index, name) in assets.iter().enumerate() {
            let _ = writeln!(expected_assets, "  printf '%s\\n' {}", shell_quote(name));
            let suffix = if index + 1 == assets.len() { "" } else { " \\" };
            let _ = writeln!(published_assets, "  \"$published_dir/{name}\"{suffix}");
        }
        let mut attested_names = spec.payloads.clone();
        attested_names.extend(spec.supporting_assets.iter().cloned());
        attested_names.push("release-manifest.json".to_owned());
        attested_names.push("identity.json".to_owned());
        let mut attested_assets = String::new();
        for (index, name) in attested_names.iter().enumerate() {
            let suffix = if index + 1 == attested_names.len() {
                ""
            } else {
                " \\"
            };
            let _ = writeln!(attested_assets, "  {}{suffix}", shell_quote(name));
        }
        let verification = PublishVerification {
            script: "",
            attestation_flags: &attestation_flags,
        };
        render_rolling_refresh_script(
            spec,
            &published_assets,
            &expected_assets,
            &attested_assets,
            &verification,
        )
    }

    fn script_region<'a>(script: &'a str, start: &str, end: &str) -> &'a str {
        let start = script.find(start).expect("script region start");
        let end = script[start..].find(end).expect("script region end") + start;
        &script[start..end]
    }

    fn run_latest_floor_case(
        expected_schema: &str,
        expected_repository: &str,
        expected_ref: &str,
        fail_attestation: bool,
        candidate_version: &str,
    ) -> (bool, String, String) {
        use std::process::Command;

        let spec = parse_spec(&Args(&stable_args())).expect("stable fixture config");
        let script = rolling_refresh_script_for_test(&spec, "stable.yml");
        let verify_latest = script_region(
            &script,
            "verify_latest_floor() {",
            "\nvalidate_existing_rolling_release() {",
        );
        let root = std::env::temp_dir().join(format!(
            "velnor-package-latest-floor-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create latest-floor fixture");
        let log = root.join("gh.log");
        let mut harness = format!(
            "set -Eeuo pipefail\ntransaction_dir={}\nGITHUB_REPOSITORY=example/project\nEXPECTED_MANIFEST_SCHEMA={}\nEXPECTED_SOURCE_REPOSITORY={}\nEXPECTED_SOURCE_REF={}\nEXPECTED_SOURCE_COMMIT=fedcba9876543210fedcba9876543210fedcba98\nRELEASE_LATEST=true\nlatest_tag=stable-0123456789abcdef0123456789abcdef01234567\ncandidate_version={}\nlatest_version=\"\"\nlatest_source_commit=\"\"\nremote_tag_sha() {{ printf '%s\\n' \"$LATEST_SOURCE_COMMIT\"; }}\n",
            shell_quote(root.to_str().expect("latest-floor path")),
            shell_quote(expected_schema),
            shell_quote(expected_repository),
            shell_quote(expected_ref),
            shell_quote(candidate_version),
        );
        harness.push_str(
            r#"gh() {
  if [ "$1" = release ] && [ "$2" = download ]; then
    shift 2
    local destination=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --dir ]; then
        destination="$2"
        shift 2
      else
        shift
      fi
    done
    cp "$LATEST_MANIFEST" "$destination/release-manifest.json"
    cp "$LATEST_IDENTITY" "$destination/identity.json"
    return 0
  fi
  if [ "$1" = attestation ] && [ "$2" = verify ]; then
    printf '%s\n' "$*" >> "$GH_LOG"
    [ "$ATTESTATION_FAIL" != true ] || return 1
    local valid_repo=false valid_workflow=false valid_ref=false valid_digest=false
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --repo) [ "$2" = "$GITHUB_REPOSITORY" ] && valid_repo=true; shift 2 ;;
        --signer-workflow) [ "$2" = "$GITHUB_REPOSITORY/.github/workflows/stable.yml" ] && valid_workflow=true; shift 2 ;;
        --source-ref) [ "$2" = "$EXPECTED_SOURCE_REF" ] && valid_ref=true; shift 2 ;;
        --source-digest) [ "$2" = "$LATEST_SOURCE_COMMIT" ] && valid_digest=true; shift 2 ;;
        *) shift ;;
      esac
    done
    [ "$valid_repo" = true ] && [ "$valid_workflow" = true ] && [ "$valid_ref" = true ] && [ "$valid_digest" = true ]
    return
  fi
  echo "unexpected gh invocation: $*" >&2
  return 97
}
"#,
        );
        harness.push_str(verify_latest);
        harness.push_str(
            "\nverify_latest_floor\nprintf 'rolling-tag-404-recreate-branch\\n' >> \"$GH_LOG\"\n",
        );
        let manifest = consumer_fixture_path("stable", "release-manifest.json");
        let source_commit = "0123456789abcdef0123456789abcdef01234567";
        let output = Command::new("bash")
            .args(["-c", &harness])
            .env("LATEST_MANIFEST", &manifest)
            .env(
                "LATEST_IDENTITY",
                consumer_fixture_path("stable", "identity.json"),
            )
            .env("LATEST_SOURCE_COMMIT", source_commit)
            .env("GH_LOG", &log)
            .env("ATTESTATION_FAIL", fail_attestation.to_string())
            .env("GH_TOKEN", "fixture-token")
            .output()
            .expect("run generated latest-floor validation");
        let log_contents = std::fs::read_to_string(&log).unwrap_or_default();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let _ = std::fs::remove_dir_all(root);
        (output.status.success(), stderr, log_contents)
    }

    fn jq_fixture_validation(spec: &PackageReleaseSpec, lane: &str) -> (bool, bool, bool) {
        use std::process::Command;

        let manifest = consumer_fixture_path(lane, "release-manifest.json");
        let identity = consumer_fixture_path(lane, "identity.json");
        let manifest_query = format!(
            "{} and .schema == $schema and .source_repository == $repository and .source_ref == $source_ref and .source_commit == $commit",
            manifest_shape_validation(spec)
        );
        let manifest_result = Command::new("jq")
            .args([
                "-e",
                "--arg",
                "schema",
                "example.consumer-manifest-v1",
                "--arg",
                "repository",
                "example/project",
                "--arg",
                "source_ref",
                "refs/heads/main",
                "--arg",
                "commit",
                "0123456789abcdef0123456789abcdef01234567",
                "--slurpfile",
                "package_manifest",
            ])
            .arg(&manifest)
            .arg(&manifest_query)
            .arg(&manifest)
            .output()
            .expect("run fixture manifest jq validation");
        let identity_result = Command::new("jq")
            .args([
                "-e",
                "--arg",
                "repository",
                "example/project",
                "--arg",
                "source_ref",
                "refs/heads/main",
                "--arg",
                "commit",
                "0123456789abcdef0123456789abcdef01234567",
                "--slurpfile",
                "package_manifest",
            ])
            .arg(&manifest)
            .arg(
                r#"keys == ["manifest","source_digest","source_ref","source_repository"] and
   .source_repository == $repository and .source_ref == $source_ref and
   .source_digest == $commit and .manifest == $package_manifest[0]"#,
            )
            .arg(&identity)
            .output()
            .expect("run fixture identity jq validation");
        let known_shape_result = Command::new("jq")
            .args([
                "-e",
                known_manifest_shape_validation(),
                manifest.to_str().expect("fixture manifest path"),
            ])
            .output()
            .expect("run known manifest shape jq validation");
        (
            manifest_result.status.success(),
            identity_result.status.success(),
            known_shape_result.status.success(),
        )
    }

    fn version_script_accepts(
        version_format: VersionFormat,
        version: &str,
        channel: &str,
        source_commit: &str,
    ) -> bool {
        use std::process::Command;

        let script = format!(
            "version={}\nsource_commit={}\n{}",
            shell_quote(version),
            shell_quote(source_commit),
            version_validation_script(version_format, "version", "source_commit", "fixture")
        );
        Command::new("bash")
            .args(["-c", &script])
            .env("VELNOR_PACKAGE_CHANNEL", channel)
            .status()
            .expect("run generated version validation")
            .success()
    }

    fn manifest_shell_version_accepts(
        manifest: &serde_json::Value,
        version_format: VersionFormat,
        channel: &str,
    ) -> bool {
        use std::process::Command;

        let root = std::env::temp_dir().join(format!(
            "velnor-package-version-json-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create manifest version fixture");
        let manifest_path = root.join("release-manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(manifest).expect("serialize manifest version fixture"),
        )
        .expect("write manifest version fixture");
        let script = format!(
            "manifest={}\n{}\nsource_commit=\"$(jq -er '.source_commit | strings' \"$manifest\")\"\nversion=\"$(jq -er '.version | strings' \"$manifest\")\"\n{}",
            shell_quote(manifest_path.to_str().expect("manifest fixture path")),
            manifest_version_line_guard(),
            version_validation_script(version_format, "version", "source_commit", "manifest")
        );
        let output = Command::new("bash")
            .args(["-c", &script])
            .env("VELNOR_PACKAGE_CHANNEL", channel)
            .output()
            .expect("run generated manifest-to-shell version validation");
        let _ = std::fs::remove_dir_all(root);
        output.status.success()
    }

    #[test]
    fn schema2_package_release_declaration_parses_the_complete_contract() {
        let config = crate::s2::config::parse(
            std::path::Path::new("/tmp/velnor-package-release-euler.toml"),
            br#"
schema = 2

[generator]
repository = "example/project"

[workflow]
providers = ["github-hosted"]
automatic_providers = ["github-hosted"]
default_dispatch_providers = ["github-hosted"]
default_branch = "main"

[workflow.selectors.github-hosted]
runs_on = ["ubuntu-24.04"]

[[declare]]
primitive = "package-release"
file = "preview.yml"

[declare.args]
build_tasks = ["release-preview-package"]
package_dir = "dist/package"
manifest_schema = "example.consumer-manifest-v1"
source_repository = "example/project"
source_ref = "refs/heads/main"
payloads = ["a.tar.gz", "b.tar.gz", "c.tar.gz", "d.tar.gz", "e.tar.gz", "f.tar.gz"]
supporting_assets = ["SHA256SUMS", "a.tar.gz.bundle", "capsule-manifest.json"]
channel = "preview"
version_format = "channel"
manifest_format = "supporting-assets-v1"
release_tag = "preview"
github_release_type = "prerelease"
publish_environment = "github-preview"
consumer_repository = "example/tap"
consumer_branch = "main"
updater = "./scripts/package-update.sh"
updater_token_secret = "TAP_TOKEN"
update_commit_message = "chore: update verified preview"
"#,
        )
        .expect("schema 2 package declaration must parse");
        config
            .validate(&[], &[], &BTreeSet::new())
            .expect("schema 2 package declaration must validate");
        let row = config.declare().first().expect("package declaration");
        let spec = parse_spec(&Args(row.args())).expect("complete package contract");
        assert_eq!(spec.package_dir, "dist/package");
        assert_eq!(spec.payloads.len(), 6);
        assert_eq!(spec.github_release_type, "prerelease");
        assert_eq!(spec.publish_environment, "github-preview");
        assert_eq!(spec.release_title_prefix, "Preview");
        assert_eq!(spec.consumer_branch, "main");
        assert_eq!(spec.concurrency_group, "package-release-preview");
    }

    #[test]
    fn channel_version_validation_is_configured_by_channel() {
        assert!(valid_channel_version("0.1.2-preview.3+0123456", "preview"));
        assert!(!valid_channel_version("0.1.2-preview.3+0123456", "stable"));
        assert!(!valid_channel_version(
            "0.1.2-preview.3+01234567",
            "preview"
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn consumer_fixtures_match_manifest_identity_and_version_contracts() {
        let stable_spec = parse_spec(&Args(&stable_args())).expect("stable fixture config");
        let mut preview_values = args();
        preview_values.insert(
            "supporting_assets".to_owned(),
            toml::Value::Array(vec![toml::Value::String("provenance.json".to_owned())]),
        );
        let preview_spec = parse_spec(&Args(&preview_values)).expect("preview fixture config");

        for (lane, spec, format, channel, version) in [
            (
                "stable",
                &stable_spec,
                VersionFormat::Semver,
                "stable",
                "1.2.3",
            ),
            (
                "preview",
                &preview_spec,
                VersionFormat::Channel,
                "preview",
                "1.2.3-preview.7+0123456",
            ),
        ] {
            let manifest_path = consumer_fixture_path(lane, "release-manifest.json");
            let identity_path = consumer_fixture_path(lane, "identity.json");
            let manifest: serde_json::Value = serde_json::from_slice(
                &std::fs::read(&manifest_path).expect("read consumer manifest fixture"),
            )
            .expect("parse consumer manifest fixture");
            let identity: serde_json::Value = serde_json::from_slice(
                &std::fs::read(&identity_path).expect("read consumer identity fixture"),
            )
            .expect("parse consumer identity fixture");
            assert_eq!(
                manifest
                    .as_object()
                    .expect("manifest object")
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                match spec.manifest_format {
                    ManifestFormat::CoreV1 => vec![
                        "assets",
                        "schema",
                        "source_commit",
                        "source_ref",
                        "source_repository",
                        "version",
                    ],
                    ManifestFormat::SupportingAssetsV1 => vec![
                        "assets",
                        "schema",
                        "source_commit",
                        "source_ref",
                        "source_repository",
                        "supporting_assets",
                        "version",
                    ],
                }
            );
            assert_eq!(
                identity
                    .as_object()
                    .expect("identity object")
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                [
                    "manifest",
                    "source_digest",
                    "source_ref",
                    "source_repository"
                ]
            );
            assert_eq!(identity["manifest"], manifest);
            assert_eq!(identity["source_digest"], manifest["source_commit"]);
            let asset_bytes = std::fs::read(consumer_fixture_path(lane, "package.tar.gz"))
                .expect("read consumer package fixture");
            let mut asset_digest = String::with_capacity(64);
            for byte in Sha256::digest(asset_bytes) {
                let _ = write!(asset_digest, "{byte:02x}");
            }
            assert_eq!(manifest["assets"][0]["sha256"], asset_digest);
            if let Some(supporting_assets) = manifest["supporting_assets"].as_array() {
                assert_eq!(supporting_assets.len(), spec.supporting_assets.len());
                for supporting_asset in supporting_assets {
                    let name = supporting_asset["name"]
                        .as_str()
                        .expect("supporting asset name");
                    let asset_bytes = std::fs::read(consumer_fixture_path(lane, name))
                        .expect("read consumer supporting asset fixture");
                    let mut asset_digest = String::with_capacity(64);
                    for byte in Sha256::digest(asset_bytes) {
                        let _ = write!(asset_digest, "{byte:02x}");
                    }
                    assert_eq!(supporting_asset["sha256"], asset_digest);
                }
            } else {
                assert!(spec.supporting_assets.is_empty());
            }
            assert_eq!(manifest["version"], version);
            assert!(manifest_shell_version_accepts(&manifest, format, channel));
            let mut trailing_newline = manifest.clone();
            trailing_newline["version"] = serde_json::Value::String(format!("{version}\n"));
            assert!(
                !manifest_shell_version_accepts(&trailing_newline, format, channel),
                "raw JSON version with trailing LF must not be normalized by command substitution"
            );
            let mut nul_byte = manifest.clone();
            nul_byte["version"] = serde_json::Value::String(format!("{version}\0"));
            assert!(
                !manifest_shell_version_accepts(&nul_byte, format, channel),
                "raw JSON version with NUL must not be normalized by command substitution"
            );
            assert_eq!(
                manifest_key_validation(spec),
                match spec.manifest_format {
                    ManifestFormat::CoreV1 => {
                        r#"keys == ["assets","schema","source_commit","source_ref","source_repository","version"]"#
                    }
                    ManifestFormat::SupportingAssetsV1 => {
                        r#"keys == ["assets","schema","source_commit","source_ref","source_repository","supporting_assets","version"]"#
                    }
                }
            );
            assert_eq!(jq_fixture_validation(spec, lane), (true, true, true));
            assert!(version_script_accepts(
                format,
                version,
                channel,
                manifest["source_commit"]
                    .as_str()
                    .expect("fixture source commit")
            ));
        }

        assert!(!version_script_accepts(
            VersionFormat::Semver,
            "01.2.3",
            "stable",
            "0123456789abcdef0123456789abcdef01234567"
        ));
        assert!(!version_script_accepts(
            VersionFormat::Channel,
            "1.2.3-preview.07+0123456",
            "preview",
            "0123456789abcdef0123456789abcdef01234567"
        ));
        assert!(!version_script_accepts(
            VersionFormat::Channel,
            "1.2.3-preview.7+0123457",
            "preview",
            "0123456789abcdef0123456789abcdef01234567"
        ));
    }

    #[test]
    fn latest_floor_requires_expected_identity_and_verified_attestation() {
        let (valid, stderr, log) = run_latest_floor_case(
            "example.consumer-manifest-v1",
            "example/project",
            "refs/heads/main",
            false,
            "1.2.4",
        );
        assert!(valid, "valid current Latest failed: {stderr}");
        assert_eq!(log.matches("attestation verify").count(), 2, "{log}");
        assert!(log.contains("--source-digest 0123456789abcdef0123456789abcdef01234567"));
        assert!(log.contains("--signer-workflow example/project/.github/workflows/stable.yml"));
        assert!(log.contains("rolling-tag-404-recreate-branch"));

        for (schema, repository, source_ref) in [
            ("different.schema", "example/project", "refs/heads/main"),
            (
                "example.consumer-manifest-v1",
                "other/project",
                "refs/heads/main",
            ),
            (
                "example.consumer-manifest-v1",
                "example/project",
                "refs/heads/release",
            ),
        ] {
            let (valid, stderr, log) =
                run_latest_floor_case(schema, repository, source_ref, false, "1.2.4");
            assert!(
                !valid,
                "mismatched Latest identity passed: {schema} {repository} {source_ref}"
            );
            assert!(
                stderr.contains("current GitHub Latest manifest does not match"),
                "{stderr}"
            );
            assert!(
                !log.contains("attestation verify"),
                "attestation ran on invalid identity: {log}"
            );
        }

        let (valid, stderr, log) = run_latest_floor_case(
            "example.consumer-manifest-v1",
            "example/project",
            "refs/heads/main",
            true,
            "1.2.4",
        );
        assert!(!valid, "failed attestation allowed Latest version floor");
        assert_eq!(log.matches("attestation verify").count(), 1, "{log}");
        assert!(
            !log.contains("rolling-tag-404-recreate-branch"),
            "validation continued after attestation failure"
        );
        assert!(
            stderr.is_empty(),
            "attestation failure should fail closed: {stderr}"
        );

        let (valid, stderr, log) = run_latest_floor_case(
            "example.consumer-manifest-v1",
            "example/project",
            "refs/heads/main",
            false,
            "1.2.2",
        );
        assert!(!valid, "stale release candidate passed the Latest floor");
        assert!(
            stderr.contains("candidate stable version is older than current GitHub Latest"),
            "{stderr}"
        );
        assert_eq!(log.matches("attestation verify").count(), 2, "{log}");
        assert!(
            !log.contains("rolling-tag-404-recreate-branch"),
            "stale candidate reached the rolling-tag 404 recreation branch: {log}"
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn lane_defaults_and_latest_policy_follow_target_configuration() {
        let preview_spec = parse_spec(&Args(&args())).expect("preview fixture config");
        let preview = render_workflow(&render_config(), &preview_spec, "preview.yml");
        assert_eq!(preview_spec.release_title_prefix, "Preview");
        assert_eq!(preview_spec.concurrency_group, "package-release-preview");
        assert!(preview.contains("RELEASE_LATEST: \"false\""));
        assert!(preview
            .contains("-F draft=false -F \"prerelease=$RELEASE_PRERELEASE\" -F make_latest=false"));
        assert!(preview.contains("release_flags=(--latest=false)"));
        assert!(
            !preview.contains("Promote immutable release to GitHub Latest after rolling refresh")
        );
        assert!(!preview.contains("-F make_latest=true"));
        assert!(preview.contains("RELEASE_TITLE_PREFIX: Preview"));
        assert!(preview.contains("concurrency:\n  group: package-release-preview"));
        assert_eq!(preview.matches("\nconcurrency:\n").count(), 1);
        let preview_yaml: serde_yaml::Value =
            serde_yaml::from_str(&preview).expect("preview workflow yaml");
        let jobs = preview_yaml
            .get("jobs")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("workflow jobs mapping");
        let publish = jobs
            .get("publish")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("publish job mapping");
        let publish_concurrency = publish
            .get("concurrency")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("global publish concurrency lock");
        assert_eq!(
            publish_concurrency
                .get("group")
                .and_then(serde_yaml::Value::as_str),
            Some(GLOBAL_PUBLISH_CONCURRENCY_GROUP)
        );
        assert_eq!(
            publish_concurrency
                .get("cancel-in-progress")
                .and_then(serde_yaml::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            publish_concurrency
                .get("queue")
                .and_then(serde_yaml::Value::as_str),
            Some("max")
        );
        assert_ne!(
            preview_spec.concurrency_group,
            GLOBAL_PUBLISH_CONCURRENCY_GROUP
        );
        assert_eq!(
            publish
                .get("defaults")
                .and_then(serde_yaml::Value::as_mapping)
                .and_then(|defaults| defaults.get("run"))
                .and_then(serde_yaml::Value::as_mapping)
                .and_then(|run| run.get("shell"))
                .and_then(serde_yaml::Value::as_str),
            Some("bash")
        );

        let mut beta_values = args();
        beta_values.insert("channel".to_owned(), toml::Value::String("beta".to_owned()));
        let beta_shared_tag = parse_spec(&Args(&beta_values)).expect("beta sharing preview tag");
        assert_eq!(beta_shared_tag.release_title_prefix, "Beta");
        assert_eq!(beta_shared_tag.concurrency_group, "package-release-preview");
        beta_values.insert(
            "release_tag".to_owned(),
            toml::Value::String("beta".to_owned()),
        );
        let beta_spec = parse_spec(&Args(&beta_values)).expect("beta fixture config");
        let beta = render_workflow(&render_config(), &beta_spec, "beta.yml");
        assert_eq!(beta_spec.release_title_prefix, "Beta");
        assert_eq!(beta_spec.concurrency_group, "package-release-beta");
        assert!(beta.contains("RELEASE_TITLE_PREFIX: Beta"));
        assert!(beta.contains("concurrency:\n  group: package-release-beta"));
        assert!(beta.contains("RELEASE_LATEST: \"false\""));

        let stable_spec = parse_spec(&Args(&stable_args())).expect("stable fixture config");
        let stable = render_workflow(&render_config(), &stable_spec, "stable.yml");
        assert_eq!(stable_spec.release_title_prefix, "Stable");
        assert_eq!(stable_spec.concurrency_group, "package-release-stable");
        assert!(stable.contains("RELEASE_TITLE_PREFIX: Stable"));
        assert!(stable.contains("concurrency:\n  group: package-release-stable"));
        assert!(stable.contains("RELEASE_LATEST: \"true\""));
        assert!(stable.contains("release_flags=(--latest=false)"));
        assert!(stable
            .contains("-F draft=false -F \"prerelease=$RELEASE_PRERELEASE\" -F make_latest=false"));
        let refresh = stable
            .find("Refresh rolling package release")
            .expect("rolling release refresh step");
        let promote = stable
            .find("Promote immutable release to GitHub Latest after rolling refresh")
            .expect("deferred stable latest promotion step");
        let promote_flag = stable
            .find("-F make_latest=true")
            .expect("explicit stable latest promotion");
        let stale_guard = stable
            .find("candidate version is not newer than the live rolling version")
            .expect("rolling monotonicity preflight");
        let latest_floor = stable
            .find("candidate stable version is older than current GitHub Latest")
            .expect("repository-wide stable version floor");
        let first_mutation = stable.find("mutated=1").expect("mutation boundary");
        assert!(
            stale_guard < first_mutation
                && latest_floor < first_mutation
                && refresh < promote
                && promote < promote_flag
        );
    }

    #[test]
    fn release_lanes_reject_unsupported_channel_type_and_version_pairs() {
        let mut invalid = stable_args();
        invalid.insert(
            "version_format".to_owned(),
            toml::Value::String("channel".to_owned()),
        );
        assert!(parse_spec(&Args(&invalid))
            .expect_err("stable releases require semver")
            .to_string()
            .contains("release lanes require version_format"));

        let mut invalid = args();
        invalid.insert(
            "version_format".to_owned(),
            toml::Value::String("semver".to_owned()),
        );
        assert!(parse_spec(&Args(&invalid))
            .expect_err("prereleases require a channel version")
            .to_string()
            .contains("prerelease lanes require version_format"));

        let mut invalid = stable_args();
        invalid.insert(
            "channel".to_owned(),
            toml::Value::String("preview".to_owned()),
        );
        assert!(parse_spec(&Args(&invalid))
            .expect_err("preview is a prerelease lane")
            .to_string()
            .contains("preview channel requires github_release_type"));

        let mut missing_preview_sidecars = args();
        missing_preview_sidecars.remove("supporting_assets");
        assert!(parse_spec(&Args(&missing_preview_sidecars))
            .expect_err("prereleases require consumer provenance sidecars")
            .to_string()
            .contains("requires supporting_assets"));

        let mut preview_core_manifest = args();
        preview_core_manifest.remove("supporting_assets");
        preview_core_manifest.insert(
            "manifest_format".to_owned(),
            toml::Value::String("core-v1".to_owned()),
        );
        assert!(parse_spec(&Args(&preview_core_manifest))
            .expect_err("prereleases must use the sidecar manifest schema")
            .to_string()
            .contains("prerelease lanes require manifest_format"));

        let mut unsafe_group_override = args();
        unsafe_group_override.insert(
            "concurrency_group".to_owned(),
            toml::Value::String("unlocked-preview-tag".to_owned()),
        );
        assert!(parse_spec(&Args(&unsafe_group_override))
            .expect_err("concurrency must be derived from mutable release_tag")
            .to_string()
            .contains("concurrency_group is derived from release_tag"));

        let mut reserved_tag = args();
        reserved_tag.insert(
            "release_tag".to_owned(),
            toml::Value::String("global-publish".to_owned()),
        );
        assert!(parse_spec(&Args(&reserved_tag))
            .expect_err("release tags cannot collide with the global publication lock")
            .to_string()
            .contains("reserved global publish concurrency group"));

        let mut uppercase_reserved_tag = args();
        uppercase_reserved_tag.insert(
            "release_tag".to_owned(),
            toml::Value::String("GLOBAL-PUBLISH".to_owned()),
        );
        assert!(parse_spec(&Args(&uppercase_reserved_tag))
            .expect_err("GitHub treats uppercase and lowercase groups as equal")
            .to_string()
            .contains("reserved global publish concurrency group"));
    }

    #[test]
    fn release_runner_and_generated_shell_are_constrained_to_supported_hosts() {
        for labels in [
            vec!["macos-15"],
            vec!["windows-2025"],
            vec!["ubuntu-24.04", "self-hosted"],
            vec!["ubuntu-slim"],
            vec!["ubuntu-custom"],
        ] {
            let mut config = render_config();
            config
                .selectors
                .get_mut(&ProviderId::GithubHosted)
                .expect("hosted selector")
                .runs_on = labels.into_iter().map(str::to_owned).collect();
            assert!(validate_release_runner(&config).is_err());
        }

        for label in [
            "ubuntu-latest",
            "ubuntu-22.04",
            "ubuntu-24.04",
            "ubuntu-26.04",
            "ubuntu-22.04-arm",
            "ubuntu-24.04-arm",
            "ubuntu-26.04-arm",
        ] {
            let mut config = render_config();
            config
                .selectors
                .get_mut(&ProviderId::GithubHosted)
                .expect("hosted selector")
                .runs_on = vec![label.to_owned()];
            assert!(validate_release_runner(&config).is_ok(), "{label}");
        }

        let spec = parse_spec(&Args(&args())).expect("preview fixture config");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        assert_eq!(
            workflow
                .matches("defaults:\n      run:\n        shell: bash")
                .count(),
            2
        );
        assert!(workflow
            .contains("    defaults:\n      run:\n        shell: bash\n    timeout-minutes: 30"));
    }

    #[test]
    fn configured_hidden_asset_and_inventory_name_survive_exact_verification() {
        let mut values = stable_args();
        values.insert(
            "payloads".to_owned(),
            toml::Value::Array(vec![toml::Value::String(".asset-names".to_owned())]),
        );
        let spec = parse_spec(&Args(&values)).expect("hidden bare payload is valid");
        assert!(release_asset_names(&spec).contains(&".asset-names".to_owned()));
        let workflow = render_workflow(&render_config(), &spec, "stable.yml");
        assert!(workflow.contains("include-hidden-files: true"));
        let upload_step = workflow
            .find("Upload verified package handoff")
            .expect("artifact upload step");
        let upload_paths = workflow[upload_step..]
            .split("          include-hidden-files: true")
            .next()
            .expect("upload path block");
        assert!(upload_paths.contains(
            "path: |\n            ${{ github.workspace }}/dist/release-manifest.json\n            ${{ github.workspace }}/dist/identity.json\n            ${{ github.workspace }}/dist/.asset-names"
        ));
        assert!(!upload_paths.contains("dist/unconfigured"));
        let immutable = render_immutable_publish_script(&spec);
        let authenticated_tag_query =
            "git -C source -c \"http.extraheader=AUTHORIZATION: bearer $GH_TOKEN\" ls-remote origin";
        assert_eq!(
            immutable.matches(authenticated_tag_query).count(),
            2,
            "both immutable tag lookup paths must authenticate private source access"
        );
        assert!(immutable.contains("downloaded_assets=\"$transaction_dir/downloaded-assets\""));
        assert!(immutable.contains("find \"$target_dir\" -maxdepth 1 -type f -printf '%f\\n'"));
        assert!(!immutable.contains("$target_dir/.asset-names"));
    }

    #[test]
    fn rolling_asset_contract_migration_fails_before_any_release_mutation() {
        let old_spec = parse_spec(&Args(&stable_args())).expect("old stable contract");
        let mut new_values = stable_args();
        new_values.insert(
            "supporting_assets".to_owned(),
            toml::Value::Array(vec![toml::Value::String("provenance.json".to_owned())]),
        );
        new_values.insert(
            "manifest_format".to_owned(),
            toml::Value::String("supporting-assets-v1".to_owned()),
        );
        let new_spec = parse_spec(&Args(&new_values)).expect("new stable contract");
        let old_names = release_asset_names(&old_spec);
        let new_names = release_asset_names(&new_spec);
        assert_ne!(old_names, new_names);
        assert_eq!(
            manifest_key_validation(&old_spec),
            r#"keys == ["assets","schema","source_commit","source_ref","source_repository","version"]"#
        );
        assert_eq!(
            manifest_key_validation(&new_spec),
            r#"keys == ["assets","schema","source_commit","source_ref","source_repository","supporting_assets","version"]"#
        );

        let workflow = render_workflow(&render_config(), &new_spec, "stable.yml");
        let manifest_migration = workflow
            .find("existing rolling release manifest shape differs from the configured package contract")
            .expect("manifest-shape migration error");
        let asset_migration = workflow
            .find("existing rolling release asset set differs from the configured package contract")
            .expect("asset-set migration error");
        let mutation = workflow
            .find("mutated=1")
            .expect("publication mutation marker");
        assert!(manifest_migration < mutation);
        assert!(asset_migration < mutation);
        assert!(workflow.contains("explicit manifest migration is required before publication"));
        assert!(workflow.contains("explicit asset migration is required before publication"));
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_manifest_migration_before_immutable_publication() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let mut values = stable_args();
        values.insert(
            "payloads".to_owned(),
            toml::Value::Array(vec![toml::Value::String("package.tar.gz".to_owned())]),
        );
        values.insert(
            "supporting_assets".to_owned(),
            toml::Value::Array(vec![toml::Value::String("provenance.json".to_owned())]),
        );
        values.insert(
            "manifest_format".to_owned(),
            toml::Value::String("supporting-assets-v1".to_owned()),
        );
        let spec = parse_spec(&Args(&values)).expect("new stable package contract");
        let script = rolling_refresh_script_for_test(&spec, "stable.yml");
        let root = std::env::temp_dir().join(format!(
            "velnor-package-publish-preflight-{}",
            crate::unique_suffix()
        ));
        let candidate = root.join("package");
        let fake_bin = root.join("bin");
        let old_package = consumer_fixture_path("stable", "release-manifest.json");
        let old_package_dir = old_package.parent().expect("fixture package directory");
        let log = root.join("gh.log");
        std::fs::create_dir_all(&candidate).expect("create candidate package directory");
        std::fs::create_dir_all(&fake_bin).expect("create mock binary directory");
        for name in ["package.tar.gz", "provenance.json"] {
            std::fs::write(candidate.join(name), b"candidate asset")
                .expect("write candidate package asset");
        }
        let candidate_manifest = serde_json::json!({
            "assets": [{"name": "package.tar.gz", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}],
            "schema": "example.consumer-manifest-v1",
            "source_commit": "0123456789abcdef0123456789abcdef01234567",
            "source_ref": "refs/heads/main",
            "source_repository": "example/project",
            "supporting_assets": [{"name": "provenance.json", "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}],
            "version": "1.2.4"
        });
        std::fs::write(
            candidate.join("release-manifest.json"),
            serde_json::to_vec(&candidate_manifest).expect("serialize candidate manifest"),
        )
        .expect("write candidate manifest");
        std::fs::write(candidate.join("identity.json"), b"candidate identity")
            .expect("write candidate identity");

        let git_mock = fake_bin.join("git");
        std::fs::write(
            &git_mock,
            r#"#!/usr/bin/env bash
last="${@: -1}"
case "$last" in
  refs/tags/stable^{}|refs/tags/stable)
    printf '%s\t%s\n' 0123456789abcdef0123456789abcdef01234567 "$last"
    ;;
  *) echo "unexpected git query: $*" >&2; exit 97 ;;
esac
"#,
        )
        .expect("write authenticated tag query mock");
        std::fs::set_permissions(&git_mock, std::fs::Permissions::from_mode(0o755))
            .expect("make git mock executable");
        let find_mock = fake_bin.join("find");
        std::fs::write(
            &find_mock,
            r#"#!/usr/bin/env bash
directory="$1"
for path in "$directory"/* "$directory"/.[!.]* "$directory"/..?*; do
  if [ -f "$path" ]; then printf '%s\n' "${path##*/}"; fi
done
"#,
        )
        .expect("write GNU find behavior mock");
        std::fs::set_permissions(&find_mock, std::fs::Permissions::from_mode(0o755))
            .expect("make find mock executable");

        let mut harness = String::from(
            r#"gh() {
  printf 'gh %s\n' "$*" >> "$GH_LOG"
  if [ "$1" = api ] && [ "$2" = --repo ] && [ "$4" = -i ]; then
    case "$5" in
      */releases/latest)
        printf 'HTTP/2 404 Not Found\r\n\r\n'
        return 0
        ;;
      */releases/tags/stable)
        printf 'HTTP/2 200 OK\r\n\r\n'
        printf '%s\n' '{"id":42,"tag_name":"stable","name":"Stable 1.2.3","body":"old body","draft":false,"prerelease":false,"assets":[{"id":1,"name":"release-manifest.json","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},{"id":2,"name":"identity.json","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},{"id":3,"name":"package.tar.gz","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}'
        return 0
        ;;
    esac
  fi
  if [ "$1" = release ] && [ "$2" = download ]; then
    local destination=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --dir ]; then destination="$2"; shift 2; else shift; fi
    done
    cp "$OLD_PACKAGE_DIR"/* "$destination/"
    return 0
  fi
  if [[ "$*" == *"--method"* ]] || { [ "$1" = release ] && [ "$2" = upload ]; }; then
    printf 'unexpected public mutation: %s\n' "$*" >&2
    return 98
  fi
  echo "unexpected gh invocation: $*" >&2
  return 97
}
"#,
        );
        harness.push_str(&script);
        let path = std::env::var_os("PATH").expect("PATH is present");
        let joined_path = std::env::join_paths(
            std::iter::once(fake_bin.clone()).chain(std::env::split_paths(&path)),
        )
        .expect("join mock PATH");
        let output = Command::new("bash")
            .args(["-c", &harness])
            .env("PATH", joined_path)
            .env("GITHUB_WORKSPACE", &root)
            .env("GITHUB_REPOSITORY", "example/project")
            .env("PACKAGE_DIR", "package")
            .env(
                "EXPECTED_SOURCE_COMMIT",
                "0123456789abcdef0123456789abcdef01234567",
            )
            .env("EXPECTED_SOURCE_REPOSITORY", "example/project")
            .env("EXPECTED_SOURCE_REF", "refs/heads/main")
            .env("EXPECTED_MANIFEST_SCHEMA", "example.consumer-manifest-v1")
            .env("VELNOR_PACKAGE_CHANNEL", "stable")
            .env("RELEASE_TAG", "stable")
            .env("RELEASE_PRERELEASE", "false")
            .env("RELEASE_LATEST", "true")
            .env("RELEASE_TITLE_PREFIX", "Stable")
            .env("ROLLING_PREFLIGHT_ONLY", "true")
            .env("GH_TOKEN", "fixture-token")
            .env("OLD_PACKAGE_DIR", old_package_dir)
            .env("GH_LOG", &log)
            .output()
            .expect("run generated preflight with migration fault injection");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let gh_log = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(output.status.code(), Some(1), "{stderr}\n{gh_log}");
        assert!(
            stderr.contains("explicit manifest migration is required before publication"),
            "{stderr}"
        );
        assert!(gh_log.contains("releases/tags/stable"), "{gh_log}");
        assert!(!gh_log.contains("--method"), "{gh_log}");
        assert!(!gh_log.contains("release upload"), "{gh_log}");
    }

    #[test]
    fn release_type_and_publish_environment_come_from_target_config() {
        let mut values = args();
        values.insert(
            "github_release_type".to_owned(),
            toml::Value::String("release".to_owned()),
        );
        values.insert(
            "channel".to_owned(),
            toml::Value::String("stable".to_owned()),
        );
        values.insert(
            "version_format".to_owned(),
            toml::Value::String("semver".to_owned()),
        );
        values.insert(
            "release_tag".to_owned(),
            toml::Value::String("stable".to_owned()),
        );
        values.insert(
            "publish_environment".to_owned(),
            toml::Value::String("package-production".to_owned()),
        );
        let spec = parse_spec(&Args(&values)).expect("valid target publication policy");
        let workflow = render_workflow(&render_config(), &spec, "package-release.yml");
        assert!(workflow.contains("environment: package-production"));
        assert!(workflow.contains("RELEASE_PRERELEASE: \"false\""));
        assert!(workflow.contains("RELEASE_LATEST: \"true\""));
        assert!(workflow.contains("RELEASE_TITLE_PREFIX: Stable"));
        assert!(workflow.contains("concurrency:\n  group: package-release-stable"));
        assert!(workflow
            .contains("-F draft=false -F \"prerelease=$RELEASE_PRERELEASE\" -F make_latest=false"));
        assert!(
            workflow.contains("Promote immutable release to GitHub Latest after rolling refresh")
        );
        assert!(workflow.contains(r#"-F "prerelease=$RELEASE_PRERELEASE""#));
        assert!(workflow.contains(r#"--argjson prerelease "$RELEASE_PRERELEASE""#));
        assert!(!workflow.contains("environment: github-preview"));
        assert!(!workflow.contains("-F prerelease=true"));
        assert!(!workflow.contains("--draft --prerelease"));

        let mut invalid_type = values.clone();
        invalid_type.insert(
            "github_release_type".to_owned(),
            toml::Value::String("rolling".to_owned()),
        );
        assert!(parse_spec(&Args(&invalid_type)).is_err());

        let mut invalid_environment = values;
        invalid_environment.insert(
            "publish_environment".to_owned(),
            toml::Value::String("${{ github.event.inputs.environment }}".to_owned()),
        );
        assert!(parse_spec(&Args(&invalid_environment)).is_err());
    }

    #[test]
    fn workflow_file_configures_output_and_attestation_identity() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow_file = validate_workflow_file(Some("release.yml")).expect("safe workflow");
        let workflow = render_workflow(&render_config(), &spec, &workflow_file);
        assert!(workflow
            .contains("--signer-workflow \"$GITHUB_REPOSITORY/.github/workflows/release.yml\""));
        assert!(!workflow.contains("preview.yml"));
        assert!(!workflow.to_ascii_lowercase().contains("formula"));
        assert!(!workflow.to_ascii_lowercase().contains("homebrew"));
        assert_eq!(
            Path::new(".github/workflows").join(&workflow_file),
            Path::new(".github/workflows/release.yml")
        );
        assert!(validate_workflow_file(Some("../release.yml")).is_err());
        assert!(validate_workflow_file(Some("release.txt")).is_err());
        assert!(validate_workflow_file(Some("release.YML")).is_err());
    }

    #[test]
    fn existing_rolling_release_validation_is_live_and_fail_closed() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        assert!(!workflow.contains("LEGACY_ROLLING"));
        assert!(workflow.contains("validate_existing_rolling_release"));
        assert!(workflow.contains("test -s \"$manifest\""));
        assert!(workflow.contains("test -s \"$identity\""));
        assert!(workflow
            .contains("existing rolling release assets are not exactly covered by its manifest"));
        assert!(workflow.contains("existing rolling GitHub asset digest mismatch"));
        assert!(workflow.contains(
            "merge-base --is-ancestor \"$old_source_commit\" \"$EXPECTED_SOURCE_COMMIT\""
        ));
        assert!(workflow.contains("existing rolling manifest source_commit does not match its tag"));
        assert!(
            workflow.contains("existing rolling manifest version does not bind its source commit")
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_rolling_identity_failure_survives_negated_function_call() {
        use std::process::Command;

        let spec = parse_spec(&Args(&stable_args())).expect("stable fixture config");
        let script = rolling_refresh_script_for_test(&spec, "stable.yml");
        let validation = script_region(
            &script,
            "validate_existing_rolling_release() {",
            "\nreject_stale_rolling_draft() {",
        );
        let root = std::env::temp_dir().join(format!(
            "velnor-package-rolling-identity-{}",
            crate::unique_suffix()
        ));
        let transaction = root.join("transaction");
        let source_dir = root.join("live-package");
        std::fs::create_dir_all(&transaction).expect("create transaction directory");
        std::fs::create_dir_all(&source_dir).expect("create live package directory");
        for file in ["release-manifest.json", "package.tar.gz"] {
            std::fs::copy(consumer_fixture_path("stable", file), source_dir.join(file))
                .expect("copy valid stable package fixture");
        }
        let mut identity = serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(consumer_fixture_path("stable", "identity.json"))
                .expect("read stable identity fixture"),
        )
        .expect("parse stable identity fixture");
        identity["source_repository"] = serde_json::json!("untrusted/other");
        std::fs::write(
            source_dir.join("identity.json"),
            serde_json::to_vec(&identity).expect("serialize invalid identity fixture"),
        )
        .expect("write mismatched identity envelope");

        let assets = ["release-manifest.json", "identity.json", "package.tar.gz"].map(|name| {
            use std::fmt::Write as _;

            let contents = std::fs::read(source_dir.join(name)).expect("read package asset");
            let mut digest = String::with_capacity(64);
            for byte in Sha256::digest(contents) {
                write!(&mut digest, "{byte:02x}").expect("write SHA-256 digest");
            }
            serde_json::json!({"name": name, "digest": format!("sha256:{digest}")})
        });
        let body = serde_json::json!({
            "assets": assets,
            "draft": false,
            "id": 42,
            "name": "Stable 1.2.3",
            "prerelease": false,
            "tag_name": "stable"
        });
        let body = serde_json::to_string(&body).expect("serialize existing release body");
        let harness = format!(
            "set -Eeuo pipefail\ntransaction_dir={}\nrolling_tag=stable\nRELEASE_PRERELEASE=false\nRELEASE_TITLE_PREFIX=Stable\nEXPECTED_MANIFEST_SCHEMA=example.consumer-manifest-v1\nEXPECTED_SOURCE_REPOSITORY=example/project\nEXPECTED_SOURCE_REF=refs/heads/main\nEXPECTED_SOURCE_COMMIT=fedcba9876543210fedcba9876543210fedcba98\nVELNOR_PACKAGE_CHANNEL=stable\nold_tag_sha=0123456789abcdef0123456789abcdef01234567\nold_assets=\"$transaction_dir/old-assets\"\ngit() {{ return 0; }}\n{}\nbody={}\nif ! validate_existing_rolling_release \"$body\" {}; then\n  result=rejected\nelse\n  result=accepted\nfi\ntest \"$result\" = rejected\n",
            shell_quote(transaction.to_str().expect("transaction path")),
            validation,
            shell_quote(&body),
            shell_quote(source_dir.to_str().expect("source directory path")),
        );
        let output = Command::new("bash")
            .args(["-c", &harness])
            .output()
            .expect("run generated rolling identity validation");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "generated validation accepted an invalid identity: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("identity envelope does not bind"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn rendered_workflow_recovers_or_discards_interrupted_rolling_draft() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        assert!(workflow.contains(
            "(.draft | type == \"boolean\") and .tag_name == $tag and (.id | type == \"number\")"
        ));
        assert!(workflow.contains(
            r#"if [ "$old_draft" = true ] && [ "$old_prerelease" != "$RELEASE_PRERELEASE" ]; then"#
        ));
        assert!(workflow.contains("elif ! validate_existing_rolling_release"));
        assert!(workflow.contains("reject_stale_rolling_draft"));
        assert!(workflow.contains("existing rolling draft is incomplete or incompatible"));
        assert!(!workflow.contains("discard_stale_rolling_draft"));
        assert!(!workflow.contains("stale_tag_sha=\"$(remote_tag_sha \"$rolling_tag\")\""));
        assert!(workflow.contains("git/refs/tags/$rolling_tag\""));
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
    fn package_release_requires_a_payload() {
        let mut values = args();
        values.insert("payloads".to_owned(), toml::Value::Array(Vec::new()));
        let error = parse_spec(&Args(&values)).expect_err("one payload must fail");
        assert!(error.to_string().contains("at least one payload"));
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
            "keys == [\"assets\",\"schema\",\"source_commit\",\"source_ref\",\"source_repository\",\"supporting_assets\",\"version\"]"
        ));
        assert!(script.contains(".manifest == $package_manifest[0]"));
        assert!(script.contains("cmp -s \"$expected_files\" \"$actual_files\""));
        assert!(script.contains("sha256sum \"$dir/$name\""));
        assert!(script.contains("git -C \"$source_checkout\" rev-parse HEAD"));
        assert!(script.contains("source checkout repository does not match"));
        assert!(script.contains(".supporting_assets | type == \"array\""));
        assert!(script.contains("supporting asset checksum mismatch"));
        assert!(script.contains("contains a non-file entry"));
        assert!(script.contains("sha256sum --check --strict SHA256SUMS"));
        assert!(script.contains("SHA256SUMS does not name exactly the declared payloads"));
    }

    #[test]
    fn verification_rejects_loose_declared_checksum_sidecars() {
        let mut values = args();
        values.insert(
            "supporting_assets".to_owned(),
            toml::Value::Array(
                [
                    "SHA256SUMS",
                    "a.tar.gz.sha256",
                    "a.tar.gz.bundle",
                    "capsule-manifest.json",
                ]
                .into_iter()
                .map(|name| toml::Value::String(name.to_owned()))
                .collect(),
            ),
        );
        let spec = parse_spec(&Args(&values)).expect("checksum sidecar fixture");
        let script = verification_script(&spec);
        assert!(script.contains("verify_sha256_sidecar \"$dir/a.tar.gz.sha256\" \"$dir/a.tar.gz\""));
        assert!(script.contains("checksum sidecar is not one strict digest line"));
    }

    #[cfg(unix)]
    #[test]
    fn generated_verification_with_checksum_sidecar_is_bash_syntax_valid() {
        use std::process::Command;

        let mut values = args();
        values.insert(
            "supporting_assets".to_owned(),
            toml::Value::Array(
                [
                    "SHA256SUMS",
                    "a.tar.gz.sha256",
                    "a.tar.gz.bundle",
                    "capsule-manifest.json",
                ]
                .into_iter()
                .map(|name| toml::Value::String(name.to_owned()))
                .collect(),
            ),
        );
        let spec = parse_spec(&Args(&values)).expect("checksum sidecar fixture");
        let root = std::env::temp_dir().join(format!(
            "velnor-package-verification-bash-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create shell fixture");
        let path = root.join("verify.sh");
        std::fs::write(&path, verification_script(&spec)).expect("write shell fixture");
        let output = Command::new("bash")
            .args(["-n", path.to_str().expect("shell fixture path")])
            .output()
            .expect("run bash syntax check");
        let _ = std::fs::remove_dir_all(root);
        assert!(
            output.status.success(),
            "verification script is not valid bash: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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
    #[allow(clippy::too_many_lines)]
    fn rendered_workflow_rechecks_published_dir_and_attests_declared_assets() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
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
        assert!(workflow
            .contains("\"$PACKAGE_DIR/a.tar.gz\" \\\n            \"$PACKAGE_DIR/b.tar.gz\""));
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
        assert!(
            workflow.contains("run: mise --yes install --locked --include-task-tools"),
            "{workflow}"
        );
        assert!(
            workflow
                .contains("contents: write\n      pull-requests: write\n      attestations: read"),
            "{workflow}"
        );
        assert!(workflow.contains("tag=\"$RELEASE_TAG-$EXPECTED_SOURCE_COMMIT\""));
        assert!(workflow.contains("--repo \"$GITHUB_REPOSITORY\""));
        assert!(workflow
            .contains("--signer-workflow \"$GITHUB_REPOSITORY/.github/workflows/preview.yml\""));
        assert!(workflow.contains("--source-ref \"$EXPECTED_SOURCE_REF\""));
        assert!(workflow.contains("--source-digest \"$EXPECTED_SOURCE_COMMIT\""));
        assert!(workflow.contains("PACKAGE_DIR: published-package"));
        assert!(workflow.contains("Refresh rolling package release"));
        assert!(
            !workflow.contains("Promote immutable release to GitHub Latest after rolling refresh")
        );
        assert!(workflow.contains(
            "gh release upload \"$rolling_tag\" --repo \"$GITHUB_REPOSITORY\" --clobber"
        ));
        assert!(workflow.contains("staged_tag=\"$RELEASE_TAG-$EXPECTED_SOURCE_COMMIT\""));
        assert!(workflow.contains(
            "merge-base --is-ancestor \"$old_source_commit\" \"$EXPECTED_SOURCE_COMMIT\""
        ));
        assert!(workflow.contains("candidate version is not newer than the live rolling version"));
        assert!(workflow
            .contains("immutable staging tag does not resolve to the verified source commit"));
        assert!(workflow.contains("release_flags+=(--prerelease)"));
        assert!(workflow.contains("immutable release is already public but incomplete"));
        assert!(workflow.contains("verify_asset_bytes \"$tag\" \"$expected_assets\""));
        assert!(workflow.contains(r#"-F draft=true -F "prerelease=$RELEASE_PRERELEASE""#));
        assert!(workflow.contains("staged rolling release asset set is not exact"));
        assert!(workflow.contains(r#"-F draft=false -F "prerelease=$RELEASE_PRERELEASE""#));
        let rolling_stage = workflow
            .find("releases/$rolling_release_id\" -F draft=true")
            .expect("rolling draft staging");
        let rolling_upload = workflow
            .find("gh release upload \"$rolling_tag\"")
            .expect("rolling staged upload");
        let rolling_check = workflow
            .find("staged rolling release asset set is not exact")
            .expect("rolling asset check");
        let rolling_publish = rolling_check
            + workflow[rolling_check..]
                .find("-f \"name=$RELEASE_TITLE_PREFIX $candidate_version\"")
                .expect("rolling draft publish");
        assert!(rolling_stage < rolling_upload);
        assert!(rolling_upload < rolling_check);
        assert!(rolling_check < rolling_publish);
        let immutable_upload = workflow
            .find("gh release upload \"$tag\" --repo \"$GITHUB_REPOSITORY\" \"$PACKAGE_DIR/$asset_name\"")
            .expect("immutable resumable upload");
        assert!(!workflow
            .contains("gh release upload \"$tag\" --repo \"$GITHUB_REPOSITORY\" --clobber"));
        let immutable_check = workflow
            .find("immutable release asset set is not exact after publication")
            .expect("immutable asset check");
        assert!(immutable_upload < immutable_check);
        serde_yaml::from_str::<serde_yaml::Value>(&workflow).expect("rendered workflow is YAML");
    }

    #[test]
    fn rendered_workflow_has_no_trailing_whitespace() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        let offenders = workflow
            .lines()
            .enumerate()
            .filter_map(|(line, content)| (content.trim_end() != content).then_some(line + 1))
            .collect::<Vec<_>>();
        assert!(
            offenders.is_empty(),
            "trailing whitespace at lines {offenders:?}"
        );
    }

    #[test]
    fn rendered_workflow_limits_consumer_updates_to_verified_outputs() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        assert!(workflow.contains("gh pr create --repo \"$CONSUMER_REPOSITORY\""));
        assert!(workflow.contains("automation/package-release-$RELEASE_TAG"));
        assert!(workflow.contains("automation/package-release-$RELEASE_ASSET_TAG"));
        assert!(workflow.contains("gh pr close \"$stale_pr_url\""));
        assert!(workflow.contains("git ls-files --others --exclude-standard"));
        assert!(workflow.contains("git status --porcelain --untracked-files=all"));
        assert!(workflow.contains("consumer updater produced untracked files; staging them"));
        assert!(workflow.contains("bash -c \"$UPDATER\""));
        let workflow_lower = workflow.to_ascii_lowercase();
        assert!(!workflow_lower.contains("formula"));
        assert!(!workflow_lower.contains("homebrew"));
        assert!(!workflow.contains("--force-with-lease=refs/heads/$automation_branch"));
        let untracked_check = workflow
            .find("git ls-files --others --exclude-standard")
            .expect("untracked output check");
        let diff_check = workflow
            .find("git diff --check")
            .expect("consumer diff check");
        assert!(untracked_check < diff_check);
        let source_checkout = workflow
            .find("Checkout verified source for publication")
            .expect("source checkout");
        let rolling_refresh = workflow
            .find("Refresh rolling package release")
            .expect("rolling refresh");
        assert!(source_checkout < rolling_refresh);
        assert!(workflow.contains("fetch-depth: 0\n          path: source"));
        assert!(workflow.contains(
            "git -C source -c \"http.extraheader=AUTHORIZATION: bearer $GH_TOKEN\" ls-remote origin"
        ));
        let branch_rewrite_guard = workflow
            .find("immutable consumer branch already exists and would need rewriting")
            .expect("immutable branch rewrite guard");
        let consumer_commit = workflow
            .find("git commit -s -m \"$UPDATE_COMMIT_MESSAGE\"")
            .expect("consumer commit");
        assert!(branch_rewrite_guard < consumer_commit);
        serde_yaml::from_str::<serde_yaml::Value>(&workflow).expect("rendered workflow is YAML");
    }

    #[test]
    fn rendered_workflow_has_rollback_and_post_upload_proof() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        assert!(workflow.contains("repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag"));
        assert!(workflow.contains("previous state restored"));
        assert!(workflow.contains("validate_existing_rolling_release"));
        assert!(workflow
            .contains("existing rolling release assets are not exactly covered by its manifest"));
        assert!(workflow.contains("if verify_restored_assets \"$rollback_dir\" \"$old_assets\""));
        assert!(workflow.contains("remote_tag_ref_sha()"));
        assert!(workflow.contains("sha=$old_tag_ref_sha"));
        assert!(workflow.contains("remote_tag_ref_sha \"$rolling_tag\""));
        assert!(workflow.contains("rollback restored bytes differ"));
        assert!(workflow.contains("rollback GitHub digest differs"));
        assert!(workflow.contains("current GitHub Latest manifest does not match the configured schema and source identity"));
        assert!(workflow.contains(
            "current GitHub Latest identity envelope does not bind its manifest and source"
        ));
        assert!(workflow.contains("gh attestation verify \"$latest_attested_asset\""));
        assert!(workflow.contains("--source-digest \"$latest_source_commit\""));
        assert!(workflow.contains(
            "if ! gh release upload \"$rolling_tag\" --repo \"$GITHUB_REPOSITORY\" --clobber"
        ));
        assert!(
            workflow.contains("gh release download \"$rolling_tag\" --repo \"$GITHUB_REPOSITORY\"")
        );
        assert!(workflow
            .contains("gh attestation verify \"$transaction_dir/rolling-published/$payload\""));
        let refresh_start = workflow
            .find("Refresh rolling package release")
            .expect("rolling refresh step");
        let refresh_script = &workflow[refresh_start..];
        let rolling_attestations = refresh_script
            .find("for payload in \\\n")
            .map(|index| &refresh_script[index..])
            .expect("post-upload rolling attestation loop");
        for name in [
            "a.tar.gz",
            "SHA256SUMS",
            "a.tar.gz.bundle",
            "capsule-manifest.json",
            "release-manifest.json",
            "identity.json",
        ] {
            assert!(rolling_attestations.contains(name), "missing {name}");
        }
        assert!(workflow.contains("VELNOR_PACKAGE_RELEASE_TAG=\"$RELEASE_TAG\""));
        let build_attestations = workflow
            .find("Verify build attestations")
            .expect("build attestation check");
        let preflight = workflow
            .find("Preflight rolling package release")
            .expect("read-only rolling preflight");
        let immutable_publish = workflow
            .find("Publish immutable source-bound release")
            .expect("immutable release publication");
        assert!(build_attestations < preflight && preflight < immutable_publish);
        assert!(workflow.contains("ROLLING_PREFLIGHT_ONLY: \"true\""));
        assert!(workflow.contains("candidate stable version is older than current GitHub Latest"));
        assert!(!workflow.contains("gh release delete"));
        assert!(!workflow.contains("HEAD:$CONSUMER_BRANCH"));
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::too_many_lines)]
    fn rollback_retries_independent_ref_and_safe_metadata_after_uncertain_asset_restore() {
        use std::process::Command;

        let spec = parse_spec(&Args(&stable_args())).expect("stable fixture config");
        let script = rolling_refresh_script_for_test(&spec, "stable.yml");
        let rollback = script_region(&script, "rollback() {", "\non_exit() {");
        let on_exit = script_region(&script, "on_exit() {", "\ntrap 'on_exit");
        let root = std::env::temp_dir().join(format!(
            "velnor-package-rollback-fault-{}",
            crate::unique_suffix()
        ));
        let rollback_dir = root.join("old-package");
        std::fs::create_dir_all(&rollback_dir).expect("create rollback package");
        std::fs::write(rollback_dir.join("package.tar.gz"), b"old package")
            .expect("write old package asset");
        let old_assets = root.join("old-assets");
        std::fs::write(&old_assets, "package.tar.gz\n").expect("write old asset inventory");
        let log = root.join("rollback.log");
        let transaction = root.join("transaction");
        std::fs::create_dir_all(&transaction).expect("create rollback transaction");
        let old_body_file = transaction.join("old-release-body");
        std::fs::write(&old_body_file, b"Old release body\n")
            .expect("write exact old release body including trailing LF");
        let mock_body_file = root.join("mock-release-body");
        std::fs::write(&mock_body_file, b"Candidate body").expect("write candidate release body");

        let mut harness = format!(
            "set -Eeuo pipefail\ntransaction_dir={}\nrollback_dir={}\nold_assets={}\nold_body_file={}\nlatest_response={}\nrolling_response={}\nrolling_tag=preview\nrolling_release_id=42\nGITHUB_REPOSITORY=example/project\nold_tag_sha=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\nold_tag_ref_sha=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nold_name='Old Name'\nold_draft=false\nold_prerelease=false\nold_make_latest=true\nlatest_tag=stable-previous\nhad_release=1\nmutated=1\nMOCK_NAME='New Name'\nMOCK_DRAFT=false\nMOCK_PRERELEASE=false\nMOCK_REF_SHA=cccccccccccccccccccccccccccccccccccccccc\nremote_tag_sha() {{ printf '%s\\n' bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb; }}\nremote_tag_ref_sha() {{ printf 'remote-ref=%s\\n' \"$MOCK_REF_SHA\" >> \"$MOCK_LOG\"; printf '%s\\n' \"$MOCK_REF_SHA\"; }}\nverify_restored_assets() {{ printf 'verify-assets\\n' >> \"$MOCK_LOG\"; return 1; }}\n",
            shell_quote(transaction.to_str().expect("transaction path")),
            shell_quote(rollback_dir.to_str().expect("rollback path")),
            shell_quote(old_assets.to_str().expect("asset inventory path")),
            shell_quote(old_body_file.to_str().expect("old release body path")),
            shell_quote(root.join("latest-response").to_str().expect("latest response path")),
            shell_quote(root.join("rolling-response").to_str().expect("rolling response path")),
        );
        harness.push_str(
            r#"gh() {
  printf 'gh %s\n' "$*" >> "$MOCK_LOG"
  if [ "$1" = injected-failure ]; then
    return 23
  fi
  if [ "$1" = api ] && [ "$2" = --method ] && [ "$3" = PATCH ]; then
    case "$*" in
      *"git/refs/tags/preview"*)
        for argument in "$@"; do
          case "$argument" in sha=*) MOCK_REF_SHA="${argument#sha=}" ;; esac
        done
        return 0
        ;;
      *"releases/42"*)
        if [[ "$*" == *"--input"* ]]; then
          local input_file=""
          while [ "$#" -gt 0 ]; do
            if [ "$1" = --input ]; then input_file="$2"; shift 2; else shift; fi
          done
          MOCK_NAME="$(jq -er '.name' "$input_file")"
          jq -j '.body' "$input_file" > "$MOCK_BODY_FILE"
          MOCK_DRAFT="$(jq -er '.draft | tostring' "$input_file")"
          MOCK_PRERELEASE="$(jq -er '.prerelease | tostring' "$input_file")"
          printf 'restore-body-base64=%s\n' "$(jq -r '.body | @base64' "$input_file")" >> "$MOCK_LOG"
          printf 'restore-metadata=%s\n' "$(jq -c '{name,draft,prerelease,make_latest}' "$input_file")" >> "$MOCK_LOG"
          return 0
        fi
        case "$*" in
          *"draft=true"*)
            MOCK_DRAFT=true
            return 1
            ;;
        esac
        ;;
    esac
  fi
  if [ "$1" = api ] && [ "$2" = --paginate ]; then
    printf '7\tpackage.tar.gz\n'
    return 0
  fi
  if [ "$1" = api ] && [ "$2" = --repo ] && [ "$4" = repos/example/project/releases/42 ]; then
    jq -cn --arg name "$MOCK_NAME" --rawfile body "$MOCK_BODY_FILE" \
      --argjson draft "$MOCK_DRAFT" --argjson prerelease "$MOCK_PRERELEASE" \
      '{id:42,tag_name:"preview",name:$name,body:$body,draft:$draft,prerelease:$prerelease,assets:[{id:7,name:"package.tar.gz",digest:"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}'
    return 0
  fi
  if [ "$1" = release ] && [ "$2" = upload ]; then
    return 0
  fi
  echo "unexpected gh invocation: $*" >&2
  return 97
}
"#,
        );
        harness.push_str(rollback);
        harness.push('\n');
        harness.push_str(on_exit);
        harness.push_str("\ntrap 'on_exit \"$?\"' EXIT\nfailure=\"$(gh injected-failure)\"\n");
        let output = Command::new("bash")
            .args(["-c", &harness])
            .env("MOCK_LOG", &log)
            .env("MOCK_BODY_FILE", &mock_body_file)
            .output()
            .expect("run generated rollback fault injection");
        let log_contents = std::fs::read_to_string(&log).expect("read rollback action log");
        let restored_body = std::fs::read(&mock_body_file).expect("read restored release body");
        assert_eq!(restored_body, b"Old release body\n");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("draft transition response was uncertain"),
            "{stderr}"
        );
        assert!(stderr.contains("rollback was incomplete"), "{stderr}");
        assert!(log_contents.contains("verify-assets"), "{log_contents}");
        assert!(
            log_contents.contains("gh release upload preview --repo example/project --clobber"),
            "{log_contents}"
        );
        assert!(
            log_contents.contains("sha=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "{log_contents}"
        );
        assert_eq!(
            log_contents.matches("git/refs/tags/preview").count(),
            1,
            "rollback ran more than once after a command-substitution failure: {log_contents}"
        );
        assert!(
            !log_contents.contains("sha=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "peeled commit used for ref restore: {log_contents}"
        );
        assert!(
            log_contents.contains("remote-ref=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "{log_contents}"
        );
        assert!(
            log_contents.contains("restore-body-base64=T2xkIHJlbGVhc2UgYm9keQo="),
            "{log_contents}"
        );
        assert!(
            log_contents.contains(r#""name":"Old Name""#),
            "{log_contents}"
        );
        assert!(
            log_contents.contains(r#""make_latest":false"#),
            "{log_contents}"
        );
        assert!(log_contents.contains(r#""draft":true"#), "{log_contents}");
        assert!(
            log_contents.contains(r#""prerelease":false"#),
            "{log_contents}"
        );
        assert!(log_contents.contains("-F draft=true"), "{log_contents}");
    }

    #[cfg(unix)]
    #[test]
    fn generated_publish_scripts_are_bash_syntax_valid() {
        use std::process::Command;

        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        let document = serde_yaml::from_str::<serde_yaml::Value>(&workflow)
            .expect("rendered workflow is YAML");
        let publish = document
            .get("jobs")
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|jobs| jobs.get("publish"))
            .and_then(serde_yaml::Value::as_mapping)
            .expect("publish job");
        let steps = publish
            .get("steps")
            .and_then(serde_yaml::Value::as_sequence)
            .expect("publish steps");
        let root = std::env::temp_dir().join(format!(
            "velnor-package-publish-bash-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create shell fixture");
        for (index, step) in steps.iter().enumerate() {
            let Some(script) = step.get("run").and_then(serde_yaml::Value::as_str) else {
                continue;
            };
            let path = root.join(format!("step-{index}.sh"));
            std::fs::write(&path, script).expect("write shell fixture");
            let output = Command::new("bash")
                .args(["-n", path.to_str().expect("shell fixture path")])
                .output()
                .expect("run bash syntax check");
            assert!(
                output.status.success(),
                "publish step {index} is not valid bash: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn package_release_rejects_shell_ambiguous_package_directories() {
        for package_dir in ["dist:foo", "dist foo", "dist/../other", "dist\\foo"] {
            let mut values = args();
            values.insert(
                "package_dir".to_owned(),
                toml::Value::String(package_dir.to_owned()),
            );
            let error = parse_spec(&Args(&values)).expect_err("unsafe package directory must fail");
            assert!(error.to_string().contains("portable relative directory"));
        }
    }
}
