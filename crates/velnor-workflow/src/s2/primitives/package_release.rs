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
        ]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let spec = parse_spec(args)?;
        if !ctx.config.providers.contains(&ProviderId::GithubHosted) {
            return Err(GeneratorError::usage(
                "package-release requires the github-hosted provider for GitHub release and attestation APIs",
            ));
        }
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
    if !crate::s2::runtime::valid_package(&channel) {
        return Err(GeneratorError::usage(
            "package-release channel must be a portable release-channel token",
        ));
    }
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
expected_files="$(mktemp)"
actual_files="$(mktemp)"
expected_names="$(mktemp)"
actual_names="$(mktemp)"
checksum_names="$(mktemp)"
expected_supporting_names="$(mktemp)"
actual_supporting_names="$(mktemp)"
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
version="$(jq -er '.version | strings' "$manifest")"
[[ "$source_commit" =~ ^[0-9a-f]{40}$ ]] || { echo "::error::manifest source_commit is not 40 lowercase hex" >&2; exit 1; }
[ "$source_commit" = "$actual_source_commit" ] || { echo "::error::manifest source_commit is not the checked-out commit" >&2; exit 1; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+-([A-Za-z0-9_-]+)\.[0-9]+\+[0-9a-f]{7}$ ]] || { echo "::error::manifest version is not a source-bound channel version" >&2; exit 1; }
[ "${BASH_REMATCH[1]}" = "$VELNOR_PACKAGE_CHANNEL" ] || { echo "::error::manifest version channel does not match the configured channel" >&2; exit 1; }
short_commit="$(printf '%s' "$source_commit" | cut -c1-7)"
version_suffix="$(printf '%s' "$version" | awk -F+ '{print $2}')"
[ "$version_suffix" = "$short_commit" ] || { echo "::error::version does not bind its source commit" >&2; exit 1; }
jq -e \
  --arg schema "$EXPECTED_MANIFEST_SCHEMA" \
  --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
  --arg source_ref "$EXPECTED_SOURCE_REF" \
  --arg commit "$source_commit" \
  --slurpfile package_manifest "$manifest" \
  'keys == ["assets","schema","source_commit","source_ref","source_repository","supporting_assets","version"] and
   .schema == $schema and .source_repository == $repository and
   .source_ref == $source_ref and .source_commit == $commit and
   (.assets | type == "array" and
    all(.[]; type == "object" and (keys == ["name","sha256"]) and
      (.name | strings | length > 0) and
      (.sha256 | strings | test("^[0-9a-f]{64}$")))) and
   (.supporting_assets | type == "array" and
    all(.[]; type == "object" and (keys == ["name","sha256"]) and
      (.name | strings | length > 0) and
      (.sha256 | strings | test("^[0-9a-f]{64}$"))))' "$manifest" >/dev/null
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
{
"#,
    );
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
    let package_path_yaml = crate::s2::yaml_scalar(&format!("{workspace_expr}/{package_dir}"));
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
        "jobs:\n  build:\n    name: Verify package release\n    if: {build_if}\n    runs-on: {runner}\n    timeout-minutes: 90\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    outputs:\n      version: {}\n      source_commit: {}\n    env:\n      PACKAGE_DIR: {package_dir_yaml}\n      VELNOR_VERIFIED_PACKAGE_DIR: {workspace_expr}/{package_dir}\n      VELNOR_SOURCE_CHECKOUT_DIR: {workspace_expr}\n      VELNOR_PACKAGE_CHANNEL: {channel_yaml}\n      EXPECTED_SOURCE_REPOSITORY: {source_repository_yaml}\n      EXPECTED_SOURCE_REF: {source_ref_yaml}\n      EXPECTED_MANIFEST_SCHEMA: {schema_yaml}\n      EXPECTED_SOURCE_COMMIT: {source_commit_expr}\n",
        github_expression("steps.verify.outputs.version"),
        github_expression("steps.verify.outputs.source_commit"),
    );
    let _ = writeln!(
        output,
        "    steps:\n      - name: Checkout source\n        uses: {checkout}\n        with:\n          ref: {source_commit_expr}\n          fetch-depth: 0\n          persist-credentials: false\n{runtime_setup}      - name: Set up Mise\n        uses: {mise}\n        with:\n          install: false\n      - name: Install locked build tools\n        run: mise --yes install --locked --include-task-tools\n      - name: Enforce workflow policy\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\n      - name: Build verified package directory\n        env:\n          VELNOR_SOURCE_COMMIT: {source_commit_expr}\n          VELNOR_SOURCE_REF: {source_shell}\n        run: |\n          set -euo pipefail\n          mkdir -p \"$VELNOR_VERIFIED_PACKAGE_DIR\"\n{tasks}      - name: Verify manifest, identity, checksums, and exact file set\n        id: verify\n        run: |\n{build_verify}      - name: Attest declared package assets\n        uses: {attest}\n        with:\n          subject-path: |\n{attestation_subjects}      - name: Upload verified package handoff\n        uses: {upload}\n        with:\n          name: package-release\n          path: {package_path_yaml}\n          if-no-files-found: error\n          retention-days: 2\n",
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
        &concurrency_yaml,
    ));
    output
}

struct PublishVerification<'a> {
    script: &'a str,
    attestation_flags: &'a str,
}

#[allow(clippy::too_many_lines)]
fn render_rolling_refresh_script(
    published_assets: &str,
    expected_asset_names: &str,
    payload_names: &str,
    verification: &PublishVerification<'_>,
) -> String {
    let mut script = String::from(
        r#"set -Eeuo pipefail
case "$RELEASE_PRERELEASE" in
  true|false) ;;
  *) echo "::error::invalid configured GitHub release type" >&2; exit 1 ;;
esac
rolling_tag="$RELEASE_TAG"
staged_tag="$RELEASE_TAG-$EXPECTED_SOURCE_COMMIT"
published_dir="$GITHUB_WORKSPACE/published-package"
transaction_dir="$(mktemp -d)"
rolling_response="$transaction_dir/rolling-response"
rolling_body="$transaction_dir/rolling.json"
expected_assets="$transaction_dir/expected-assets"
old_assets="$transaction_dir/old-assets"
staged_assets="$transaction_dir/staged-assets"
rolling_staged_assets="$transaction_dir/rolling-staged-assets"
rolling_published_assets="$transaction_dir/rolling-published-assets"
rollback_dir="$transaction_dir/old-package"
rolling_release_id=""
old_tag_sha=""
old_name=""
old_body=""
old_draft=""
old_prerelease=""
old_source_commit=""
old_version=""
candidate_version="$(jq -er '.version | strings' "$published_dir/release-manifest.json")"
had_release=0
mutated=0

remote_tag_sha() {
  local tag_name="$1"
  local sha
  sha="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name^{}" | awk 'NR == 1 {print $1}')"
  if [ -z "$sha" ]; then
    sha="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name" | awk 'NR == 1 {print $1}')"
  fi
  printf '%s\n' "$sha"
}

validate_existing_rolling_release() {
  local body="$1"
  local source_dir="$2"
  local manifest="$source_dir/release-manifest.json"
  local identity="$source_dir/identity.json"
  local manifest_assets="$transaction_dir/live-manifest-assets"
  local downloaded_assets="$transaction_dir/live-downloaded-assets"
  local name expected_digest actual_digest api_digest

  jq -e --arg tag "$rolling_tag" --argjson prerelease "$RELEASE_PRERELEASE" '
    (.draft | type == "boolean") and .prerelease == $prerelease and .tag_name == $tag and
    (.id | type == "number") and
    (.assets | type == "array" and length > 0) and
    ((.assets | map(.name)) as $names |
      ($names | unique | length) == ($names | length)) and
    (.assets | all(.[];
        type == "object" and
        (.name | type == "string") and
        (.digest | type == "string" and test("^sha256:[0-9a-f]{64}$"))))
  ' <<<"$body" >/dev/null
  test -s "$manifest"
  test -s "$identity"

  jq -e '
    keys == ["assets","schema","source_commit","source_ref","source_repository","supporting_assets","version"] and
    (.assets | type == "array" and
      all(.[];
        type == "object" and
        (.name | type == "string") and
        (.name | length > 0 and . != "." and . != ".." and test("^[-A-Za-z0-9._+~]+$")) and
        (.sha256 | type == "string" and test("^[0-9a-f]{64}$")))) and
    (.supporting_assets | type == "array" and
      all(.[];
        type == "object" and
        (.name | type == "string") and
        (.name | length > 0 and . != "." and . != ".." and test("^[-A-Za-z0-9._+~]+$")) and
        (.sha256 | type == "string" and test("^[0-9a-f]{64}$")))) and
    ((.assets + .supporting_assets | map(.name)) as $names |
      ($names | unique | length) == ($names | length))
  ' "$manifest" >/dev/null

  old_source_commit="$(jq -er '.source_commit | strings' "$manifest")"
  old_version="$(jq -er '.version | strings' "$manifest")"
  [[ "$old_source_commit" =~ ^[0-9a-f]{40}$ ]] || {
    echo "::error::existing rolling manifest source_commit is not 40 lowercase hex" >&2
    return 1
  }
  [ "$old_source_commit" = "$old_tag_sha" ] || {
    echo "::error::existing rolling manifest source_commit does not match its tag" >&2
    return 1
  }
  [[ "$old_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+-([A-Za-z0-9_-]+)\.[0-9]+\+[0-9a-f]{7}$ ]] || {
    echo "::error::existing rolling manifest version is not a source-bound channel version" >&2
    return 1
  }
  [ "${BASH_REMATCH[1]}" = "$VELNOR_PACKAGE_CHANNEL" ] || {
    echo "::error::existing rolling manifest channel does not match the configured channel" >&2
    return 1
  }
  [ "${old_version##*+}" = "${old_source_commit:0:7}" ] || {
    echo "::error::existing rolling manifest version does not bind to its source" >&2
    return 1
  }
  jq -e \
    --arg schema "$EXPECTED_MANIFEST_SCHEMA" \
    --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
    --arg source_ref "$EXPECTED_SOURCE_REF" \
    --arg commit "$old_source_commit" \
    --slurpfile rolling_manifest "$manifest" \
    '.schema == $schema and .source_repository == $repository and
     .source_ref == $source_ref and .source_commit == $commit' "$manifest" >/dev/null
  jq -e \
    --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
    --arg source_ref "$EXPECTED_SOURCE_REF" \
    --arg commit "$old_source_commit" \
    --slurpfile rolling_manifest "$manifest" \
    'keys == ["manifest","source_digest","source_ref","source_repository"] and
     .source_repository == $repository and .source_ref == $source_ref and
     .source_digest == $commit and .manifest == $rolling_manifest[0]' "$identity" >/dev/null
  jq -e --arg name "$RELEASE_TITLE_PREFIX $old_version" '.name == $name' <<<"$body" >/dev/null

  {
    printf '%s\n' "release-manifest.json" "identity.json"
    jq -r '(.assets[] | .name), (.supporting_assets[] | .name)' "$manifest"
  } | LC_ALL=C sort > "$manifest_assets"
  jq -r '.assets[].name' <<<"$body" | LC_ALL=C sort > "$old_assets"
  find "$source_dir" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort > "$downloaded_assets"
  cmp -s "$manifest_assets" "$old_assets" || {
    echo "::error::existing rolling release assets are not exactly covered by its manifest" >&2
    return 1
  }
  cmp -s "$old_assets" "$downloaded_assets" || {
    echo "::error::existing rolling release download differs from its live asset set" >&2
    return 1
  }

  while IFS=$'\t' read -r name expected_digest; do
    test -s "$source_dir/$name"
    actual_digest="$(sha256sum -- "$source_dir/$name" | awk '{print $1}')"
    [ "$actual_digest" = "$expected_digest" ] || {
      echo "::error::existing rolling manifest digest mismatch: $name" >&2
      return 1
    }
  done < <(jq -r '(.assets[] | [.name, .sha256] | @tsv), (.supporting_assets[] | [.name, .sha256] | @tsv)' "$manifest")
  while IFS=$'\t' read -r name api_digest; do
    actual_digest="$(sha256sum -- "$source_dir/$name" | awk '{print $1}')"
    [ "$api_digest" = "sha256:$actual_digest" ] || {
      echo "::error::existing rolling GitHub asset digest mismatch: $name" >&2
      return 1
    }
  done < <(jq -r '.assets[] | [.name, .digest] | @tsv' <<<"$body")

  if ! git -C source cat-file -e "${old_source_commit}^{commit}"; then
    echo "::error::existing rolling source commit is not present in the checked-out history" >&2
    return 1
  fi
  if ! git -C source merge-base --is-ancestor "$old_source_commit" "$EXPECTED_SOURCE_COMMIT"; then
    echo "::error::candidate source commit is not a descendant of the live rolling source" >&2
    return 1
  fi
}

discard_stale_rolling_draft() {
  local stale_tag_sha
  echo "::warning::discarding incomplete rolling draft and retrying publication" >&2
  gh api --method DELETE --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" >/dev/null
  stale_tag_sha="$(remote_tag_sha "$rolling_tag")"
  if [ -n "$stale_tag_sha" ]; then
    gh api --method DELETE --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag" >/dev/null
  fi
  had_release=0
  rolling_release_id=""
  old_tag_sha=""
  old_name=""
  old_body=""
  old_draft=""
  old_prerelease=""
  old_source_commit=""
  old_version=""
  : > "$old_assets"
  rm -rf -- "$rollback_dir"
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
  trap - ERR
  if [ "$mutated" = 1 ]; then
    set +e
    rollback_status=0
    if [ "$had_release" = 1 ]; then
      # Keep the rollback release hidden while restoring its complete old set.
      if gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" -F draft=true >/dev/null; then
        while IFS=$'\t' read -r asset_id asset_name; do
          if ! grep -Fqx -- "$asset_name" "$old_assets"; then
            gh api --method DELETE --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/assets/$asset_id" >/dev/null || rollback_status=1
          fi
        done < <(gh api --paginate --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id/assets" --jq '.[] | [.id, .name] | @tsv')
        while IFS= read -r asset_name; do
          if ! gh release upload "$rolling_tag" --repo "$GITHUB_REPOSITORY" --clobber "$rollback_dir/$asset_name"; then
            rollback_status=1
          fi
        done < "$old_assets"
        restore_dir="$rollback_dir"
        restore_assets="$old_assets"
        if ! verify_restored_assets "$restore_dir" "$restore_assets"; then
          rollback_status=1
        fi
        rollback_ready=1
        if [ "$rollback_status" -ne 0 ]; then
          rollback_ready=0
        fi
        if [ -n "$old_tag_sha" ] && ! gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag" -f "sha=$old_tag_sha" -F force=true >/dev/null; then
          rollback_ready=0
        fi
        if [ "$rollback_ready" = 1 ]; then
          gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" \
            -f "name=$old_name" -f "body=$old_body" -F "draft=$old_draft" -F "prerelease=$old_prerelease" -F make_latest=false >/dev/null || rollback_status=1
        else
          rollback_status=1
        fi
      else
        rollback_status=1
      fi
    else
      if [ -n "$rolling_release_id" ]; then
        gh api --method DELETE --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" >/dev/null || rollback_status=1
      fi
      if [ -n "$(remote_tag_sha "$rolling_tag")" ]; then
        gh api --method DELETE --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag" >/dev/null || rollback_status=1
      fi
    fi
    if [ "$rollback_status" -ne 0 ]; then
      echo "::error::rolling preview publication failed and rollback was incomplete" >&2
      status=1
    else
      echo "::warning::rolling preview publication failed; previous release restored" >&2
    fi
  fi
  exit "$status"
}
trap 'rollback "$?"' ERR
trap 'rm -rf -- "$transaction_dir"' EXIT

{
"#,
    );
    script.push_str(expected_asset_names);
    script.push_str(
        r#"
} | LC_ALL=C sort > "$expected_assets"

if ! staged_body="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/tags/$staged_tag")"; then
  echo "::error::verified immutable staging release is missing" >&2
  exit 1
fi
jq -e --arg tag "$staged_tag" --argjson prerelease "$RELEASE_PRERELEASE" '(.draft | not) and .prerelease == $prerelease and .tag_name == $tag' <<<"$staged_body" >/dev/null
jq -r '.assets[].name' <<<"$staged_body" | LC_ALL=C sort > "$staged_assets"
cmp -s "$expected_assets" "$staged_assets" || { echo "::error::immutable staging release asset set is not exact" >&2; exit 1; }
staged_tag_sha="$(remote_tag_sha "$staged_tag")"
[ "$staged_tag_sha" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::immutable staging tag does not resolve to the verified source commit" >&2; exit 1; }

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
    old_body="$(jq -r '.body // ""' "$rolling_body")"
    old_draft="$(jq -er '.draft | tostring' "$rolling_body")"
    old_prerelease="$(jq -er '.prerelease | tostring' "$rolling_body")"
    old_tag_sha="$(remote_tag_sha "$rolling_tag")"
    if ! [[ "$old_tag_sha" =~ ^[0-9a-f]{40}$ ]]; then
      if [ "$old_draft" = true ]; then
        discard_stale_rolling_draft
      else
        echo "::error::rolling release tag is not a commit ref" >&2
        exit 1
      fi
    elif [ "$old_draft" = true ] && [ "$old_prerelease" != "$RELEASE_PRERELEASE" ]; then
      discard_stale_rolling_draft
    elif ! mkdir -p "$rollback_dir" || ! gh release download "$rolling_tag" --repo "$GITHUB_REPOSITORY" --dir "$rollback_dir" --clobber; then
      if [ "$old_draft" = true ]; then
        discard_stale_rolling_draft
      else
        echo "::error::existing rolling release could not be downloaded" >&2
        exit 1
      fi
    elif ! validate_existing_rolling_release "$rolling_body" "$rollback_dir"; then
      if [ "$old_draft" = true ]; then
        discard_stale_rolling_draft
      else
        echo "::error::existing rolling release failed immutable validation" >&2
        exit 1
      fi
    fi
    if [ "$had_release" = 1 ]; then
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
    test -z "$(remote_tag_sha "$rolling_tag")" || { echo "::error::rolling tag exists without a release; refusing to overwrite it" >&2; exit 1; }
    ;;
  *)
    echo "::error::rolling release preflight failed with HTTP $rolling_http" >&2
    exit 1
    ;;
esac

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
"#,
    );
    script.push_str(verification.script);
    script.push_str("\nfor payload in \\\n");
    script.push_str(payload_names);
    script.push_str("do\n  gh attestation verify \"$transaction_dir/rolling-published/$payload\" ");
    script.push_str(verification.attestation_flags);
    script.push_str("\ndone\ntrap 'rm -rf -- \"$transaction_dir\"' EXIT\n");
    script
}

/// Common verification helpers and initialization for immutable publication.
fn immutable_publish_script_prelude() -> &'static str {
    r#"set -euo pipefail
case "$RELEASE_PRERELEASE" in
  true|false) ;;
  *) echo "::error::invalid configured GitHub release type" >&2; exit 1 ;;
esac
release_flags=(--latest=false)
if [ "$RELEASE_PRERELEASE" = true ]; then
  release_flags+=(--prerelease)
fi
tag="$RELEASE_TAG-$EXPECTED_SOURCE_COMMIT"
version="$(jq -er '.version | strings' "$PACKAGE_DIR/release-manifest.json")"
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
  local sha
  sha="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name^{}" | awk 'NR == 1 {print $1}')"
  if [ -z "$sha" ]; then
    sha="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name" | awk 'NR == 1 {print $1}')"
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
  downloaded_assets="$target_dir/.asset-names"
  find "$target_dir" -maxdepth 1 -type f ! -name '.asset-names' -printf '%f\n' | LC_ALL=C sort > "$downloaded_assets"
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
    script
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
    concurrency_yaml: &str,
) -> String {
    let publish_environment_yaml = crate::s2::yaml_scalar(&spec.publish_environment);
    let release_prerelease_yaml =
        crate::s2::yaml_scalar(if spec.github_release_type == "prerelease" {
            "true"
        } else {
            "false"
        });
    let mut payload_names = String::new();
    for (index, name) in spec.payloads.iter().enumerate() {
        let suffix = if index + 1 == spec.payloads.len() {
            ""
        } else {
            " \\"
        };
        let _ = writeln!(payload_names, "  {}{}", shell_quote(name), suffix);
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
            &published_assets,
            &expected_asset_names,
            &payload_names,
            &verification,
        ),
        10,
    );
    let mut output = String::new();
    let _ = writeln!(output, "  publish:");
    output.push_str("    name: Publish immutable package and update consumer\n");
    output.push_str("    needs: build\n");
    output.push_str("    if: ");
    output.push_str(&github_expression(&format!(
        "github.event_name == 'push' && github.ref == '{}'",
        spec.source_ref
    )));
    output.push('\n');
    output.push_str("    runs-on: ");
    output.push_str(runner);
    output.push_str("\n    timeout-minutes: 30\n    environment: ");
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
    output.push_str("\n    concurrency:\n      group: ");
    output.push_str(concurrency_yaml);
    output.push_str("\n      cancel-in-progress: false\n    steps:\n");

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
        "      - name: Refresh rolling preview release\n        env:\n          GH_TOKEN: ",
    );
    output.push_str(github_token_expr);
    output.push_str("\n        run: |\n");
    output.push_str(&rolling_refresh);
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
        r#"
        run: |
          set -euo pipefail
          cd consumer
          git config user.name "github-actions[bot]"
          git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
          stale_branch="automation/package-release-$RELEASE_TAG"
          automation_branch="automation/package-release-$RELEASE_ASSET_TAG"
          while IFS= read -r stale_pr_url; do
            if [ -n "$stale_pr_url" ]; then
              gh pr close "$stale_pr_url" --repo "$CONSUMER_REPOSITORY" --comment "Superseded by immutable package release $RELEASE_ASSET_TAG"
            fi
          done < <(gh pr list --repo "$CONSUMER_REPOSITORY" --head "$stale_branch" --base "$CONSUMER_BRANCH" --state open --json url --jq '.[].url')
          remote_branch_sha="$(git -c "http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN" ls-remote origin "refs/heads/$automation_branch" | awk 'NR == 1 {print $1}')"
          if [ -n "$remote_branch_sha" ]; then
            git -c "http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN" fetch origin "refs/heads/$automation_branch:refs/remotes/origin/$automation_branch"
            git switch --detach "origin/$automation_branch"
          else
            git switch --create "$automation_branch" "origin/$CONSUMER_BRANCH"
          fi
          VELNOR_PACKAGE_CHANNEL="$VELNOR_PACKAGE_CHANNEL" VELNOR_PACKAGE_RELEASE_TAG="$RELEASE_TAG" VELNOR_VERIFIED_PACKAGE_DIR="$GITHUB_WORKSPACE/published-package" bash -c "$UPDATER"
          untracked_files="$(git ls-files --others --exclude-standard)"
          if [ -n "$untracked_files" ]; then
            echo "::notice::consumer updater produced untracked files; staging them"
            printf '%s\n' "$untracked_files"
          fi
          git add -A
          git diff --cached --check
          if [ -z "$(git status --porcelain --untracked-files=all)" ]; then
            echo "consumer already references the verified release"
          else
            if [ -n "$remote_branch_sha" ]; then
              echo "::error::immutable consumer branch already exists and would need rewriting; refusing to mutate it" >&2
              exit 1
            fi
            git commit -s -m "$UPDATE_COMMIT_MESSAGE"
            git -c "http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN" push origin "HEAD:refs/heads/$automation_branch"
          fi
          pr_url="$(gh pr list --repo "$CONSUMER_REPOSITORY" --head "$automation_branch" --base "$CONSUMER_BRANCH" --state open --json url --jq '.[0].url // empty')"
          if [ -z "$pr_url" ] && ! git diff --quiet HEAD "origin/$CONSUMER_BRANCH"; then
            pr_url="$(gh pr create --repo "$CONSUMER_REPOSITORY" --head "$automation_branch" --base "$CONSUMER_BRANCH" --title "$UPDATE_COMMIT_MESSAGE ($RELEASE_ASSET_TAG)" --body "Automated verified package update. Review and merge this PR; the publisher never merges consumer changes.")"
          fi
          printf 'pr_url=%s\n' "$pr_url" >> "$GITHUB_OUTPUT"
          if [ -n "$pr_url" ]; then echo "::notice::Consumer update PR: $pr_url"; else echo "::notice::Consumer update PR: none"; fi
"#,
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
github_release_type = "prerelease"
publish_environment = "github-preview"
consumer_repository = "example/tap"
consumer_branch = "main"
release_title_prefix = "Preview"
updater = "./scripts/package-update.sh"
updater_token_secret = "TAP_TOKEN"
update_commit_message = "chore: update verified preview"
concurrency_group = "package-release-preview"
"#,
        )
        .expect("fixture args")
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
release_tag = "preview"
github_release_type = "prerelease"
publish_environment = "github-preview"
release_title_prefix = "Preview"
consumer_repository = "example/tap"
consumer_branch = "main"
updater = "./scripts/package-update.sh"
updater_token_secret = "TAP_TOKEN"
update_commit_message = "chore: update verified preview"
concurrency_group = "package-release-preview"
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
    fn release_type_and_publish_environment_come_from_target_config() {
        let mut values = args();
        values.insert(
            "github_release_type".to_owned(),
            toml::Value::String("release".to_owned()),
        );
        values.insert(
            "publish_environment".to_owned(),
            toml::Value::String("package-production".to_owned()),
        );
        let spec = parse_spec(&Args(&values)).expect("valid target publication policy");
        let workflow = render_workflow(&render_config(), &spec, "package-release.yml");
        assert!(workflow.contains("environment: package-production"));
        assert!(workflow.contains("RELEASE_PRERELEASE: \"false\""));
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
        assert!(workflow.contains("existing rolling manifest version does not bind to its source"));
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
        assert!(workflow.contains("discard_stale_rolling_draft"));
        assert!(workflow.contains("discarding incomplete rolling draft and retrying publication"));
        assert!(workflow.contains(
            "gh api --method DELETE --repo \"$GITHUB_REPOSITORY\" \"repos/$GITHUB_REPOSITORY/releases/$rolling_release_id\""
        ));
        assert!(workflow.contains("stale_tag_sha=\"$(remote_tag_sha \"$rolling_tag\")\""));
        assert!(workflow.contains("git/refs/tags/$rolling_tag\""));
        assert!(workflow.contains("had_release=0"));
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
        assert!(workflow.contains("Refresh rolling preview release"));
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
        assert!(workflow.contains("git switch --detach \"origin/$automation_branch\""));
        assert!(workflow
            .contains("VELNOR_PACKAGE_RELEASE_TAG=\"$RELEASE_TAG\" VELNOR_VERIFIED_PACKAGE_DIR"));
        assert!(!workflow.contains("git switch --force-create \"$automation_branch\""));
        assert!(!workflow.contains(
            "VELNOR_PACKAGE_RELEASE_TAG=\"$RELEASE_ASSET_TAG\" VELNOR_VERIFIED_PACKAGE_DIR"
        ));
        assert!(workflow.contains("git ls-files --others --exclude-standard"));
        assert!(workflow.contains("git status --porcelain --untracked-files=all"));
        assert!(workflow.contains("consumer updater produced untracked files; staging them"));
        let unchanged_gate = workflow
            .find("if [ -z \"$(git status --porcelain --untracked-files=all)\" ]; then")
            .expect("clean consumer tree skips commit");
        let unchanged_message = workflow
            .find("echo \"consumer already references the verified release\"")
            .expect("clean consumer tree reports no update");
        let consumer_commit = workflow
            .find("git commit -s -m \"$UPDATE_COMMIT_MESSAGE\"")
            .expect("changed consumer tree commits");
        assert!(unchanged_gate < unchanged_message);
        assert!(unchanged_message < consumer_commit);
        assert!(!workflow
            .contains("if [ -n \"$(git status --porcelain --untracked-files=all)\" ]; then"));
        assert!(workflow.contains("bash -c \"$UPDATER\""));
        let workflow_lower = workflow.to_ascii_lowercase();
        assert!(!workflow_lower.contains("formula"));
        assert!(!workflow_lower.contains("homebrew"));
        assert!(!workflow.contains("--force-with-lease=refs/heads/$automation_branch"));
        let untracked_check = workflow
            .find("git ls-files --others --exclude-standard")
            .expect("untracked output check");
        let stage_check = workflow
            .find("git add -A")
            .expect("stage all consumer output");
        let diff_check = workflow
            .find("git diff --cached --check")
            .expect("staged consumer diff check");
        assert!(untracked_check < stage_check);
        assert!(stage_check < diff_check);
        assert!(!workflow.contains("git diff --check"));
        let source_checkout = workflow
            .find("Checkout verified source for publication")
            .expect("source checkout");
        let rolling_refresh = workflow
            .find("Refresh rolling preview release")
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
    fn consumer_commit_check_rejects_trailing_whitespace_in_untracked_output() {
        use std::process::Command;

        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        let lines = workflow.lines().collect::<Vec<_>>();
        let stage_index = lines
            .iter()
            .position(|line| line.trim() == "git add -A")
            .expect("consumer staging command");
        let check_index = lines
            .iter()
            .position(|line| line.trim() == "git diff --cached --check")
            .expect("staged whitespace check");
        assert_eq!(check_index, stage_index + 1, "stage before checking output");

        let root = std::env::temp_dir().join(format!(
            "velnor-package-consumer-diff-check-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create Git fixture");
        let git = |arguments: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(arguments)
                .output()
                .expect("run Git fixture command")
        };
        let initialized = git(&["init", "--quiet"]);
        assert!(initialized.status.success(), "initialize Git fixture");
        std::fs::write(root.join("README.md"), "clean baseline\n").expect("write baseline");
        let added = git(&["add", "-A"]);
        assert!(added.status.success(), "stage baseline");
        let committed = git(&[
            "-c",
            "user.name=consumer",
            "-c",
            "user.email=consumer@example.test",
            "commit",
            "--quiet",
            "--message",
            "baseline",
        ]);
        assert!(committed.status.success(), "commit baseline");
        std::fs::write(root.join("generated.yml"), "generated value \t\n")
            .expect("write malformed untracked output");

        let validation = format!(
            "set -euo pipefail\n{}\n{}",
            lines[stage_index].trim(),
            lines[check_index].trim()
        );
        let output = Command::new("bash")
            .arg("-c")
            .arg(validation)
            .current_dir(&root)
            .output()
            .expect("run generated validation commands");
        let _ = std::fs::remove_dir_all(root);
        assert!(
            !output.status.success(),
            "cached whitespace check accepted malformed untracked output"
        );
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            diagnostics.contains("trailing whitespace"),
            "cached whitespace check did not identify trailing whitespace: {}",
            diagnostics
        );
    }

    #[test]
    fn rendered_workflow_has_rollback_and_post_upload_proof() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        assert!(workflow.contains("repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag"));
        assert!(workflow.contains("previous release restored"));
        assert!(workflow.contains("validate_existing_rolling_release"));
        assert!(workflow
            .contains("existing rolling release assets are not exactly covered by its manifest"));
        assert!(
            workflow.contains("if ! verify_restored_assets \"$restore_dir\" \"$restore_assets\"")
        );
        assert!(workflow.contains("rollback restored bytes differ"));
        assert!(workflow.contains("rollback GitHub digest differs"));
        assert!(workflow.contains(
            "if ! gh release upload \"$rolling_tag\" --repo \"$GITHUB_REPOSITORY\" --clobber"
        ));
        assert!(
            workflow.contains("gh release download \"$rolling_tag\" --repo \"$GITHUB_REPOSITORY\"")
        );
        assert!(workflow
            .contains("gh attestation verify \"$transaction_dir/rolling-published/$payload\""));
        assert!(workflow.contains("VELNOR_PACKAGE_RELEASE_TAG=\"$RELEASE_TAG\""));
        assert!(!workflow.contains("gh release delete"));
        assert!(!workflow.contains("HEAD:$CONSUMER_BRANCH"));
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
