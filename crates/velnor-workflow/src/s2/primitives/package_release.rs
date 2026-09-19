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
    legacy_rolling_release_id: Option<String>,
    legacy_rolling_source_commit: Option<String>,
    legacy_rolling_version: Option<String>,
    legacy_rolling_assets: Vec<String>,
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
            "legacy_rolling_assets",
            "legacy_rolling_release_id",
            "legacy_rolling_source_commit",
            "legacy_rolling_version",
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

fn valid_preview_version(value: &str) -> bool {
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
    let Some((release, sequence)) = base.split_once("-preview.") else {
        return false;
    };
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

    let legacy_rolling_release_id = args
        .string("legacy_rolling_release_id")?
        .filter(|value| !value.is_empty());
    let legacy_rolling_source_commit = args
        .string("legacy_rolling_source_commit")?
        .filter(|value| !value.is_empty());
    let legacy_rolling_version = args
        .string("legacy_rolling_version")?
        .filter(|value| !value.is_empty());
    let legacy_rolling_assets = args.strings("legacy_rolling_assets")?.unwrap_or_default();
    let legacy_fields_present = legacy_rolling_release_id.is_some()
        || legacy_rolling_source_commit.is_some()
        || legacy_rolling_version.is_some()
        || !legacy_rolling_assets.is_empty();
    if legacy_fields_present
        && (legacy_rolling_release_id.is_none()
            || legacy_rolling_source_commit.is_none()
            || legacy_rolling_version.is_none()
            || legacy_rolling_assets.is_empty())
    {
        return Err(GeneratorError::usage(
            "package-release legacy rolling migration needs release id, source commit, version, and assets together",
        ));
    }
    if let Some(release_id) = legacy_rolling_release_id.as_deref()
        && !release_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(GeneratorError::usage(
            "package-release legacy_rolling_release_id must be decimal digits",
        ));
    }
    if let Some(source_commit) = legacy_rolling_source_commit.as_deref()
        && (source_commit.len() != 40
            || !source_commit
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
    {
        return Err(GeneratorError::usage(
            "package-release legacy_rolling_source_commit must be 40 lowercase hex characters",
        ));
    }
    if let Some(version) = legacy_rolling_version.as_deref() {
        validate_one_line("legacy_rolling_version", version)?;
        if !valid_preview_version(version) {
            return Err(GeneratorError::usage(
                "package-release legacy_rolling_version must be a preview version with a seven-hex source suffix",
            ));
        }
    }
    if let (Some(source_commit), Some(version)) = (
        legacy_rolling_source_commit.as_deref(),
        legacy_rolling_version.as_deref(),
    ) && version.rsplit_once('+').map(|(_, suffix)| suffix) != Some(&source_commit[..7])
    {
        return Err(GeneratorError::usage(
            "package-release legacy_rolling_version source suffix must match legacy_rolling_source_commit",
        ));
    }
    let mut legacy_names = BTreeSet::new();
    for asset in &legacy_rolling_assets {
        validate_asset_name("legacy_rolling_assets", asset)?;
        if !legacy_names.insert(asset.clone()) {
            return Err(GeneratorError::usage(format!(
                "package-release legacy_rolling_assets contains duplicate {asset}"
            )));
        }
        if !names.contains(asset) {
            return Err(GeneratorError::usage(format!(
                "package-release legacy asset {asset} is not part of the new package contract"
            )));
        }
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
        release_tag,
        release_title_prefix,
        consumer_repository,
        consumer_branch,
        updater,
        updater_token_secret,
        update_commit_message,
        concurrency_group,
        legacy_rolling_release_id,
        legacy_rolling_source_commit,
        legacy_rolling_version,
        legacy_rolling_assets,
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
checksum_names="$(mktemp)"
trap 'rm -f -- "$expected_files" "$actual_files" "$expected_names" "$actual_names" "$checksum_names"' EXIT
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
   (.assets | type == "array" and length == 6 and
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
  echo "::error::SHA256SUMS does not name exactly the six declared payloads" >&2
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
    for name in &spec.payloads {
        let _ = writeln!(
            attestation_subjects,
            "            {workspace_expr}/{package_dir}/{name}"
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
    let attestation_flags = "--repo \"$GITHUB_REPOSITORY\" --signer-workflow \"$GITHUB_REPOSITORY/.github/workflows/preview.yml\" --source-ref \"$EXPECTED_SOURCE_REF\" --source-digest \"$EXPECTED_SOURCE_COMMIT\"";

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
    output = output.replace(
        "          install: false\n      - name: Enforce workflow policy",
        "          install: false\n      - name: Install locked build tools\n        run: mise --yes install --locked --include-task-tools\n      - name: Enforce workflow policy",
    );
    output = output.replace(
        &format!("          path: {workspace_expr}/{package_dir}\n"),
        &format!("          path: {package_path_yaml}\n"),
    );
    output.push('\n');
    output.push_str(&render_publish_job(
        spec,
        &runner,
        checkout,
        download,
        &publish_verify,
        &publish_attestation_targets,
        &assets,
        attestation_flags,
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
    published_assets: &str,
    rollback_assets: &str,
    expected_asset_names: &str,
    payload_names: &str,
    verification: &PublishVerification<'_>,
    legacy_expected_asset_names: &str,
    legacy_rollback_assets: &str,
) -> String {
    let mut script = String::from(
        r#"set -Eeuo pipefail
rolling_tag="$RELEASE_TAG"
staged_tag="$RELEASE_TAG-$EXPECTED_SOURCE_COMMIT"
published_dir="$GITHUB_WORKSPACE/published-package"
transaction_dir="$(mktemp -d)"
rolling_response="$transaction_dir/rolling-response"
rolling_body="$transaction_dir/rolling.json"
expected_assets="$transaction_dir/expected-assets"
old_assets="$transaction_dir/old-assets"
staged_assets="$transaction_dir/staged-assets"
rollback_dir="$transaction_dir/old-package"
legacy_dir="$transaction_dir/legacy-package"
legacy_assets_api="$transaction_dir/legacy-assets-api"
legacy_expected_assets="$transaction_dir/legacy-assets"
rolling_release_id=""
old_tag_sha=""
old_name=""
old_body=""
old_draft=""
old_prerelease=""
legacy_migration="${LEGACY_ROLLING_MIGRATION:-0}"
legacy_expected_release_id="${LEGACY_ROLLING_RELEASE_ID:-}"
legacy_expected_source_commit="${LEGACY_ROLLING_SOURCE_COMMIT:-}"
legacy_expected_version="${LEGACY_ROLLING_VERSION:-}"
candidate_version="$(jq -er '.version | strings' "$published_dir/release-manifest.json")"
rollback_mode="normal"
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
        if [ "$rollback_mode" = legacy ]; then
          while IFS=$'\t' read -r asset_id asset_name; do
            if ! grep -Fqx -- "$asset_name" "$legacy_expected_assets"; then
              gh api --method DELETE --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/assets/$asset_id" >/dev/null || rollback_status=1
            fi
          done < <(gh api --paginate --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id/assets" --jq '.[] | [.id, .name] | @tsv')
          if ! gh release upload "$rolling_tag" --repo "$GITHUB_REPOSITORY" --clobber \
"#,
    );
    script.push_str(legacy_rollback_assets);
    script.push_str(
        r#"
          then
            rollback_status=1
          fi
          restore_dir="$legacy_dir"
          restore_assets="$legacy_expected_assets"
        else
          if ! gh release upload "$rolling_tag" --repo "$GITHUB_REPOSITORY" --clobber \
"#,
    );
    script.push_str(rollback_assets);
    script.push_str(
        r#"
          then
            rollback_status=1
          fi
          restore_dir="$rollback_dir"
          restore_assets="$expected_assets"
        fi
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
jq -e --arg tag "$staged_tag" '(.draft | not) and .prerelease == true and .tag_name == $tag' <<<"$staged_body" >/dev/null
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
    jq -e --arg tag "$rolling_tag" '(.draft | not) and .prerelease == true and .tag_name == $tag' "$rolling_body" >/dev/null
    rolling_release_id="$(jq -er '.id' "$rolling_body")"
    old_name="$(jq -er '.name | strings' "$rolling_body")"
    old_body="$(jq -r '.body // ""' "$rolling_body")"
    old_draft="$(jq -er '.draft | tostring' "$rolling_body")"
    old_prerelease="$(jq -er '.prerelease | tostring' "$rolling_body")"
    old_tag_sha="$(remote_tag_sha "$rolling_tag")"
    [[ "$old_tag_sha" =~ ^[0-9a-f]{40}$ ]] || { echo "::error::rolling release tag is not a commit ref" >&2; exit 1; }
    jq -r '.assets[].name' "$rolling_body" | LC_ALL=C sort > "$old_assets"
    if cmp -s "$expected_assets" "$old_assets"; then
      rollback_mode="normal"
    elif [ "$legacy_migration" = 1 ]; then
      {
"#,
    );
    if legacy_expected_asset_names.is_empty() {
        script.push_str("      :\n");
    } else {
        script.push_str(legacy_expected_asset_names);
    }
    script.push_str(
        r#"
      } | LC_ALL=C sort > "$legacy_expected_assets"
      cmp -s "$legacy_expected_assets" "$old_assets" || { echo "::error::legacy rolling release asset set changed" >&2; exit 1; }
      jq -e --arg id "$legacy_expected_release_id" --arg name "$RELEASE_TITLE_PREFIX $legacy_expected_version" \
        '(.id | tostring) == $id and .name == $name and .draft == false and .prerelease == true' "$rolling_body" >/dev/null
      [ "$rolling_release_id" = "$legacy_expected_release_id" ] || { echo "::error::legacy rolling release id changed" >&2; exit 1; }
      [ "$old_tag_sha" = "$legacy_expected_source_commit" ] || { echo "::error::legacy rolling tag changed" >&2; exit 1; }
      [[ "$legacy_expected_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+-preview\.[0-9]+\+[0-9a-f]{7}$ ]] || { echo "::error::legacy rolling version is not an exact preview version" >&2; exit 1; }
      [ "${legacy_expected_version##*+}" = "${legacy_expected_source_commit:0:7}" ] || { echo "::error::legacy rolling version does not bind to its source commit" >&2; exit 1; }
      if ! git -C source cat-file -e "${legacy_expected_source_commit}^{commit}"; then
        echo "::error::legacy rolling source commit is not present in the checked-out history" >&2
        exit 1
      fi
      if ! git -C source merge-base --is-ancestor "$legacy_expected_source_commit" "$EXPECTED_SOURCE_COMMIT"; then
        echo "::error::candidate source commit is not a descendant of the legacy rolling source" >&2
        exit 1
      fi
      rollback_mode="legacy"
      mkdir -p "$legacy_dir"
      gh release download "$rolling_tag" --repo "$GITHUB_REPOSITORY" --dir "$legacy_dir" --clobber
      while IFS= read -r name; do
        test -s "$legacy_dir/$name" || { echo "::error::legacy rolling asset is missing: $name" >&2; exit 1; }
      done < "$legacy_expected_assets"
      jq -r '.assets[] | [.name, (.digest // "")] | @tsv' "$rolling_body" > "$legacy_assets_api"
      while IFS=$'\t' read -r name digest; do
        [ "$digest" = "sha256:$(sha256sum "$legacy_dir/$name" | awk '{print $1}')" ] || { echo "::error::legacy rolling asset digest mismatch: $name" >&2; exit 1; }
      done < "$legacy_assets_api"
    else
      echo "::error::existing rolling release asset set is not the exact package contract" >&2
      exit 1
    fi
    if [ "$rollback_mode" = normal ]; then
      mkdir -p "$rollback_dir"
"#,
    );
    script.push_str(
        r#"
    gh release download "$rolling_tag" --repo "$GITHUB_REPOSITORY" --dir "$rollback_dir" --clobber
    for name in \
"#,
    );
    script.push_str(payload_names);
    script.push_str(
        r#"do
      test -s "$rollback_dir/$name"
      expected="$(jq -er --arg name "$name" '[.assets[] | select(.name == $name)] | select(length == 1) | .[0].sha256 | select(test("^[0-9a-f]{64}$"))' "$rollback_dir/release-manifest.json")"
      actual="$(sha256sum "$rollback_dir/$name" | awk '{print $1}')"
      [ "$actual" = "$expected" ] || { echo "::error::existing rolling payload checksum is invalid: $name" >&2; exit 1; }
    done
    old_source_commit="$(jq -er '.source_commit | strings' "$rollback_dir/release-manifest.json")"
    [ "$old_source_commit" = "$old_tag_sha" ] || { echo "::error::existing rolling release provenance does not match its tag" >&2; exit 1; }
    old_version="$(jq -er '.version | strings' "$rollback_dir/release-manifest.json")"
    [[ "$old_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+-preview\.[0-9]+\+[0-9a-f]{7}$ ]] || { echo "::error::existing rolling release version is not a preview version" >&2; exit 1; }
    [ "${old_version##*+}" = "${old_source_commit:0:7}" ] || { echo "::error::existing rolling release version does not bind its source" >&2; exit 1; }
    old_version_order="${old_version%%+*}"
    candidate_version_order="${candidate_version%%+*}"
    if [ "$old_source_commit" != "$EXPECTED_SOURCE_COMMIT" ]; then
      if ! git -C source cat-file -e "${old_source_commit}^{commit}"; then
        echo "::error::existing rolling source commit is not present in the checked-out history" >&2
        exit 1
      fi
      if ! git -C source merge-base --is-ancestor "$old_source_commit" "$EXPECTED_SOURCE_COMMIT"; then
        echo "::error::candidate source commit is not a descendant of the live rolling source" >&2
        exit 1
      fi
    fi
    if [ "$old_version" = "$candidate_version" ] && [ "$old_source_commit" = "$EXPECTED_SOURCE_COMMIT" ]; then
      :
    elif [ "$old_version_order" = "$candidate_version_order" ] || [ "$(printf '%s\n' "$old_version_order" "$candidate_version_order" | LC_ALL=C sort -V | tail -n 1)" != "$candidate_version_order" ]; then
      echo "::error::candidate preview version is not newer than the live rolling version" >&2
      exit 1
    fi
    jq -e --arg repository "$EXPECTED_SOURCE_REPOSITORY" --arg source_ref "$EXPECTED_SOURCE_REF" --arg commit "$old_source_commit" --slurpfile old_manifest "$rollback_dir/release-manifest.json" 'keys == ["manifest","source_digest","source_ref","source_repository"] and .source_repository == $repository and .source_ref == $source_ref and .source_digest == $commit and .manifest == $old_manifest[0]' "$rollback_dir/identity.json" >/dev/null
    else
      old_version="$legacy_expected_version"
      old_version_order="${old_version%%+*}"
      candidate_version_order="${candidate_version%%+*}"
      if [ "$old_version_order" = "$candidate_version_order" ] || [ "$(printf '%s\n' "$old_version_order" "$candidate_version_order" | LC_ALL=C sort -V | tail -n 1)" != "$candidate_version_order" ]; then
        echo "::error::candidate preview version is not newer than the legacy rolling version" >&2
        exit 1
      fi
    fi
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
    -F draft=true -F prerelease=true -F make_latest=false)"
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
jq -e --arg tag "$rolling_tag" '.draft == true and .prerelease == true and .tag_name == $tag' <<<"$rolling_stage_json" >/dev/null
jq -r '.assets[].name' <<<"$rolling_stage_json" | LC_ALL=C sort > "$old_assets"
cmp -s "$expected_assets" "$old_assets" || { echo "::error::staged rolling release asset set is not exact" >&2; exit 1; }

if [ "$had_release" = 1 ]; then
  gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag" -f "sha=$EXPECTED_SOURCE_COMMIT" -F force=true >/dev/null
fi
gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" \
  -f "name=$RELEASE_TITLE_PREFIX $candidate_version" \
  -f "body=Verified package release from $EXPECTED_SOURCE_COMMIT" \
  -F draft=false -F prerelease=true -F make_latest=false >/dev/null

new_tag_sha="$(remote_tag_sha "$rolling_tag")"
[ "$new_tag_sha" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::rolling tag does not resolve to the verified source commit" >&2; exit 1; }
rolling_post_json="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/tags/$rolling_tag")"
jq -e --arg tag "$rolling_tag" '(.draft | not) and .prerelease == true and .tag_name == $tag' <<<"$rolling_post_json" >/dev/null
jq -r '.assets[].name' <<<"$rolling_post_json" | LC_ALL=C sort > "$old_assets"
cmp -s "$expected_assets" "$old_assets" || { echo "::error::rolling release asset set is not exact after publication" >&2; exit 1; }
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

fn render_formula_mapping_script(payload_names: &str) -> String {
    let mut script = String::from(
        r#"set -euo pipefail
manifest="$GITHUB_WORKSPACE/published-package/release-manifest.json"
test -d Formula
formula_pairs="$(mktemp)"
trap 'rm -f -- "$formula_pairs"' EXIT
current_url=""
while IFS= read -r line; do
  if [[ "$line" =~ ^[[:space:]]*url[[:space:]]+\"([^\"]+)\" ]]; then
    current_url="${BASH_REMATCH[1]}"
  elif [[ "$line" =~ ^[[:space:]]*sha256[[:space:]]+\"([0-9a-f]{64})\" ]]; then
    if [ -n "$current_url" ]; then
      printf '%s\t%s\n' "$current_url" "${BASH_REMATCH[1]}"
    fi
    current_url=""
  fi
done < <(git grep -h -E '^[[:space:]]*(url|sha256)[[:space:]]+' -- Formula || true) > "$formula_pairs"
for payload in \
"#,
    );
    script.push_str(payload_names);
    script.push_str(
        r#"do
  url="https://github.com/$GITHUB_REPOSITORY/releases/download/$RELEASE_TAG/$payload"
  expected="$(jq -er --arg name "$payload" '[.assets[] | select(.name == $name)] | select(length == 1) | .[0].sha256' "$manifest")"
  expected_pair="$(printf '%s\t%s' "$url" "$expected")"
  grep -Fqx -- "$expected_pair" "$formula_pairs" || { echo "::error::formula URL/checksum pair does not match the verified rolling asset: $payload" >&2; exit 1; }
done
"#,
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
    assets: &str,
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
    let mut immutable_expected_asset_names = String::new();
    for name in release_asset_names(spec) {
        let _ = writeln!(
            immutable_expected_asset_names,
            "            printf '%s\\n' {}",
            shell_quote(&name)
        );
    }
    let mut published_assets = String::new();
    let mut rollback_assets = String::new();
    for (index, name) in release_asset_names(spec).iter().enumerate() {
        let suffix = if index + 1 == release_asset_names(spec).len() {
            ""
        } else {
            " \\"
        };
        let _ = writeln!(published_assets, "  \"$published_dir/{name}\"{suffix}");
        let _ = writeln!(rollback_assets, "      \"$rollback_dir/{name}\"{suffix}");
    }
    let mut legacy_expected_asset_names = String::new();
    let mut legacy_rollback_assets = String::new();
    for (index, name) in spec.legacy_rolling_assets.iter().enumerate() {
        let suffix = if index + 1 == spec.legacy_rolling_assets.len() {
            ""
        } else {
            " \\"
        };
        let _ = writeln!(
            legacy_expected_asset_names,
            "  printf '%s\\n' {}",
            shell_quote(name)
        );
        let _ = writeln!(
            legacy_rollback_assets,
            "      \"$legacy_dir/{name}\"{suffix}"
        );
    }
    let verification = PublishVerification {
        script: publish_verify,
        attestation_flags,
    };
    let rolling_refresh = indent_script(
        &render_rolling_refresh_script(
            &published_assets,
            &rollback_assets,
            &expected_asset_names,
            &payload_names,
            &verification,
            &legacy_expected_asset_names,
            &legacy_rollback_assets,
        ),
        10,
    );
    let formula_mapping = indent_script(&render_formula_mapping_script(&payload_names), 10);
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
    output.push_str("\n    timeout-minutes: 30\n    environment: github-preview\n");
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
    output.push_str("\n      RELEASE_TAG: ");
    output.push_str(tag_yaml);
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
    output.push_str("\n      LEGACY_ROLLING_MIGRATION: ");
    output.push_str(&crate::s2::yaml_scalar(
        if spec.legacy_rolling_release_id.is_some() {
            "1"
        } else {
            "0"
        },
    ));
    if let Some(value) = spec.legacy_rolling_release_id.as_deref() {
        output.push_str("\n      LEGACY_ROLLING_RELEASE_ID: ");
        output.push_str(&crate::s2::yaml_scalar(value));
        output.push_str("\n      LEGACY_ROLLING_SOURCE_COMMIT: ");
        output.push_str(&crate::s2::yaml_scalar(
            spec.legacy_rolling_source_commit
                .as_deref()
                .unwrap_or_default(),
        ));
        output.push_str("\n      LEGACY_ROLLING_VERSION: ");
        output.push_str(&crate::s2::yaml_scalar(
            spec.legacy_rolling_version.as_deref().unwrap_or_default(),
        ));
    }
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
    output.push_str(
        "\n        run: |\n          set -euo pipefail\n          tag=\"$RELEASE_TAG-$EXPECTED_SOURCE_COMMIT\"\n          version=\"$(jq -er '.version' \"$PACKAGE_DIR/release-manifest.json\")\"\n          title=\"$RELEASE_TITLE_PREFIX $version\"\n          tag_sha=\"$(git -C source ls-remote origin \"refs/tags/$tag^{}\" | awk 'NR == 1 {print $1}')\"\n          if [ -z \"$tag_sha\" ]; then\n            tag_sha=\"$(git -C source ls-remote origin \"refs/tags/$tag\" | awk 'NR == 1 {print $1}')\"\n          fi\n          if [ -n \"$tag_sha\" ] && [ \"$tag_sha\" != \"$EXPECTED_SOURCE_COMMIT\" ]; then\n            echo \"::error::immutable release tag resolves to an unexpected source commit\" >&2\n            exit 1\n          fi\n          release_json=\"$(mktemp)\"\n          expected_assets=\"$(mktemp)\"\n          existing_assets=\"$(mktemp)\"\n          trap 'rm -f -- \"$release_json\" \"$expected_assets\" \"$existing_assets\"' EXIT\n          {\n",
    );
    output.push_str(&immutable_expected_asset_names);
    output.push_str(
        "          } | LC_ALL=C sort > \"$expected_assets\"\n          if ! gh api --repo \"$GITHUB_REPOSITORY\" -i \"repos/$GITHUB_REPOSITORY/releases/tags/$tag\" > \"$release_json\" 2>/dev/null; then\n            :\n          fi\n          response_http=\"$(awk 'NR == 1 {print $2; exit}' \"$release_json\")\"\n          if [ \"$response_http\" = 404 ]; then\n            if [ -n \"$tag_sha\" ]; then\n              gh release create \"$tag\" --repo \"$GITHUB_REPOSITORY\" --verify-tag --prerelease --latest=false --title \"$title\" --notes \"Verified immutable package release $version from $EXPECTED_SOURCE_COMMIT\"\n            else\n              gh release create \"$tag\" --repo \"$GITHUB_REPOSITORY\" --target \"$EXPECTED_SOURCE_COMMIT\" --prerelease --latest=false --title \"$title\" --notes \"Verified immutable package release $version from $EXPECTED_SOURCE_COMMIT\"\n            fi\n          elif [ \"$response_http\" = 200 ]; then\n            live_body=\"$(awk 'body {print; next} /^\\r?$/ {body = 1}' \"$release_json\")\"\n            jq -e --arg tag \"$tag\" --arg title \"$title\" '(.draft | not) and .prerelease == true and .tag_name == $tag and .name == $title' <<<\"$live_body\" >/dev/null\n            test -n \"$tag_sha\" || { echo \"::error::existing release has no exact source tag\" >&2; exit 1; }\n            jq -r '.assets[].name' <<<\"$live_body\" | LC_ALL=C sort > \"$existing_assets\"\n            if comm -23 \"$existing_assets\" \"$expected_assets\" | grep -q .; then\n              echo \"::error::immutable release contains an undeclared asset\" >&2\n              exit 1\n            fi\n          else\n            echo \"::error::release preflight failed; refusing publication\" >&2\n            exit 1\n          fi\n          # Create first, upload second: a failed upload leaves a valid, resumable\n          # immutable release instead of an unrecoverable partial create.\n          gh release upload \"$tag\" --repo \"$GITHUB_REPOSITORY\" --clobber ",
    );
    output.push_str(assets);
    output.push_str(
        "\n          immutable_body=\"$(gh api --repo \"$GITHUB_REPOSITORY\" \"repos/$GITHUB_REPOSITORY/releases/tags/$tag\")\"\n          jq -e --arg tag \"$tag\" --arg title \"$title\" '(.draft | not) and .prerelease == true and .tag_name == $tag and .name == $title' <<<\"$immutable_body\" >/dev/null\n          jq -r '.assets[].name' <<<\"$immutable_body\" | LC_ALL=C sort > \"$existing_assets\"\n          cmp -s \"$expected_assets\" \"$existing_assets\" || { echo \"::error::immutable release asset set is not exact after publication\" >&2; exit 1; }\n          final_tag_sha=\"$(git -C source ls-remote origin \"refs/tags/$tag^{}\" | awk 'NR == 1 {print $1}')\"\n          if [ -z \"$final_tag_sha\" ]; then\n            final_tag_sha=\"$(git -C source ls-remote origin \"refs/tags/$tag\" | awk 'NR == 1 {print $1}')\"\n          fi\n          [ \"$final_tag_sha\" = \"$EXPECTED_SOURCE_COMMIT\" ] || { echo \"::error::immutable tag does not resolve to the verified source commit\" >&2; exit 1; }\n          printf 'immutable_tag=%s\\n' \"$tag\" >> \"$GITHUB_OUTPUT\"\n",
    );

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
        "\n        run: |\n          set -euo pipefail\n          cd consumer\n          git config user.name \"github-actions[bot]\"\n          git config user.email \"41898282+github-actions[bot]@users.noreply.github.com\"\n          automation_branch=\"automation/package-release-$RELEASE_TAG\"\n          remote_branch_sha=\"$(git ls-remote origin \"refs/heads/$automation_branch\" | awk 'NR == 1 {print $1}')\"\n          git switch --force-create \"$automation_branch\" \"origin/$CONSUMER_BRANCH\"\n          VELNOR_PACKAGE_CHANNEL=\"$VELNOR_PACKAGE_CHANNEL\" VELNOR_PACKAGE_RELEASE_TAG=\"$RELEASE_ASSET_TAG\" VELNOR_VERIFIED_PACKAGE_DIR=\"$GITHUB_WORKSPACE/published-package\" bash -c \"$UPDATER\"\n          git diff --check\n          if git diff --quiet; then\n            echo \"consumer already references the verified release\"\n          else\n            git add -A\n            git commit -s -m \"$UPDATE_COMMIT_MESSAGE\"\n            if [ -n \"$remote_branch_sha\" ]; then\n              git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" push --force-with-lease=refs/heads/$automation_branch:$remote_branch_sha origin \"HEAD:refs/heads/$automation_branch\"\n            else\n              git -c \"http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN\" push origin \"HEAD:refs/heads/$automation_branch\"\n            fi\n          fi\n          pr_url=\"$(gh pr list --repo \"$CONSUMER_REPOSITORY\" --head \"$automation_branch\" --base \"$CONSUMER_BRANCH\" --state open --json url --jq '.[0].url // empty')\"\n          if [ -z \"$pr_url\" ] && ! git diff --quiet HEAD \"origin/$CONSUMER_BRANCH\"; then\n            pr_url=\"$(gh pr create --repo \"$CONSUMER_REPOSITORY\" --head \"$automation_branch\" --base \"$CONSUMER_BRANCH\" --title \"$UPDATE_COMMIT_MESSAGE ($RELEASE_ASSET_TAG)\" --body \"Automated verified package update. Review and merge this PR; the publisher never merges consumer changes.\")\"\n          fi\n          printf 'pr_url=%s\\n' \"$pr_url\" >> \"$GITHUB_OUTPUT\"\n          if [ -n \"$pr_url\" ]; then echo \"::notice::Consumer update PR: $pr_url\"; else echo \"::notice::Consumer update PR: none\"; fi\n",
    );
    output = output.replace(
        "          git diff --check\n          if git diff --quiet; then",
        &format!(
            "          git diff --check\n{formula_mapping}          if git diff --quiet; then"
        ),
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

    fn legacy_args() -> BTreeMap<String, toml::Value> {
        let mut values = args();
        values.insert(
            "legacy_rolling_release_id".to_owned(),
            toml::Value::String("12345".to_owned()),
        );
        values.insert(
            "legacy_rolling_source_commit".to_owned(),
            toml::Value::String("0123456789abcdef0123456789abcdef01234567".to_owned()),
        );
        values.insert(
            "legacy_rolling_version".to_owned(),
            toml::Value::String("0.1.2-preview.3+0123456".to_owned()),
        );
        values.insert(
            "legacy_rolling_assets".to_owned(),
            toml::Value::Array(
                [
                    "a.tar.gz",
                    "b.tar.gz",
                    "c.tar.gz",
                    "d.tar.gz",
                    "e.tar.gz",
                    "f.tar.gz",
                    "SHA256SUMS",
                    "a.tar.gz.bundle",
                    "capsule-manifest.json",
                ]
                .into_iter()
                .map(|name| toml::Value::String(name.to_owned()))
                .collect(),
            ),
        );
        values
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
        assert_eq!(spec.release_title_prefix, "Preview");
        assert_eq!(spec.consumer_branch, "main");
        assert_eq!(spec.concurrency_group, "package-release-preview");
    }

    #[test]
    fn package_release_legacy_rolling_contract_is_complete_and_bound() {
        let spec = parse_spec(&Args(&legacy_args())).expect("legacy migration contract");
        assert_eq!(spec.legacy_rolling_release_id.as_deref(), Some("12345"));
        assert_eq!(spec.legacy_rolling_assets.len(), 9);

        let workflow = render_workflow(&render_config(), &spec);
        assert!(
            workflow.contains("LEGACY_ROLLING_MIGRATION: \"1\""),
            "{workflow}"
        );
        assert!(
            workflow.contains("LEGACY_ROLLING_RELEASE_ID: \"12345\""),
            "{workflow}"
        );
        assert!(
            workflow.contains("legacy rolling release asset set changed"),
            "{workflow}"
        );
        assert!(
            workflow.contains("legacy rolling tag changed"),
            "{workflow}"
        );
        assert!(workflow.contains("legacy rolling version does not bind to its source commit"));
        assert!(workflow.contains(
            "merge-base --is-ancestor \"$legacy_expected_source_commit\" \"$EXPECTED_SOURCE_COMMIT\""
        ));
        assert!(workflow.contains("legacy rolling version"), "{workflow}");
    }

    #[test]
    fn package_release_rejects_partial_legacy_rolling_contract() {
        let mut values = legacy_args();
        values.remove("legacy_rolling_version");
        let error = parse_spec(&Args(&values)).expect_err("partial legacy contract must fail");
        assert!(error.to_string().contains("needs release id"));
    }

    #[test]
    fn package_release_rejects_legacy_version_source_mismatch() {
        let mut values = legacy_args();
        values.insert(
            "legacy_rolling_version".to_owned(),
            toml::Value::String("0.1.2-preview.3+deadbee".to_owned()),
        );
        let error = parse_spec(&Args(&values)).expect_err("mismatched source suffix must fail");
        assert!(error.to_string().contains("source suffix"));
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
        assert!(script.contains("contains a non-file entry"));
        assert!(script.contains("sha256sum --check --strict SHA256SUMS"));
        assert!(script.contains("SHA256SUMS does not name exactly the six declared payloads"));
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
        assert!(workflow.contains("gh pr create --repo \"$CONSUMER_REPOSITORY\""));
        assert!(workflow.contains("automation/package-release-$RELEASE_TAG"));
        assert!(workflow.contains("--force-with-lease=refs/heads/$automation_branch"));
        assert!(workflow.contains("PACKAGE_DIR: published-package"));
        assert!(workflow.contains("Refresh rolling preview release"));
        assert!(workflow.contains(
            "gh release upload \"$rolling_tag\" --repo \"$GITHUB_REPOSITORY\" --clobber"
        ));
        assert!(workflow.contains("staged_tag=\"$RELEASE_TAG-$EXPECTED_SOURCE_COMMIT\""));
        assert!(workflow.contains(
            "merge-base --is-ancestor \"$old_source_commit\" \"$EXPECTED_SOURCE_COMMIT\""
        ));
        assert!(workflow
            .contains("candidate preview version is not newer than the live rolling version"));
        assert!(workflow
            .contains("immutable staging tag does not resolve to the verified source commit"));
        assert!(workflow.contains("-F draft=true -F prerelease=true"));
        assert!(workflow.contains("staged rolling release asset set is not exact"));
        assert!(workflow.contains("-F draft=false -F prerelease=true"));
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
            .find("gh release upload \"$tag\" --repo \"$GITHUB_REPOSITORY\" --clobber")
            .expect("immutable resumable upload");
        let immutable_check = workflow
            .find("immutable release asset set is not exact after publication")
            .expect("immutable asset check");
        assert!(immutable_upload < immutable_check);
        serde_yaml::from_str::<serde_yaml::Value>(&workflow).expect("rendered workflow is YAML");
    }

    #[test]
    fn rendered_workflow_has_rollback_and_post_upload_proof() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec);
        assert!(workflow.contains("repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag"));
        assert!(workflow.contains("previous release restored"));
        assert!(workflow.contains("rollback_mode=\"normal\""));
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
        assert!(workflow.contains(
            "https://github.com/$GITHUB_REPOSITORY/releases/download/$RELEASE_TAG/$payload"
        ));
        assert!(
            workflow.contains("git grep -h -E '^[[:space:]]*(url|sha256)[[:space:]]+' -- Formula")
        );
        assert!(workflow.contains("formula URL/checksum pair does not match"));
        assert!(workflow.contains("VELNOR_PACKAGE_RELEASE_TAG=\"$RELEASE_TAG\""));
        assert!(!workflow.contains("gh release delete"));
        assert!(!workflow.contains("HEAD:$CONSUMER_BRANCH"));
    }

    #[cfg(unix)]
    #[test]
    fn generated_publish_scripts_are_bash_syntax_valid() {
        use std::process::Command;

        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec);
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

    #[cfg(unix)]
    #[test]
    fn generated_legacy_publish_script_is_bash_syntax_valid() {
        use std::process::Command;

        let spec = parse_spec(&Args(&legacy_args())).expect("legacy fixture");
        let workflow = render_workflow(&render_config(), &spec);
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
            "velnor-package-legacy-publish-bash-{}",
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
                "legacy publish step {index} is not valid bash: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::too_many_lines)]
    fn generated_formula_mapping_accepts_only_verified_rolling_assets() {
        use std::process::Command;

        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let root = std::env::temp_dir().join(format!(
            "velnor-package-formula-mapping-{}",
            crate::unique_suffix()
        ));
        let package_dir = root.join("published-package");
        let formula_dir = root.join("Formula");
        std::fs::create_dir_all(&package_dir).expect("create package fixture");
        std::fs::create_dir_all(&formula_dir).expect("create formula fixture");

        let mut payload_names = String::new();
        let mut manifest_assets = String::new();
        let mut formula = String::new();
        for (index, payload) in spec.payloads.iter().enumerate() {
            let suffix = if index + 1 == spec.payloads.len() {
                String::new()
            } else {
                String::from(" ") + "\\"
            };
            let _ = writeln!(payload_names, "  {}{suffix}", shell_quote(payload));
            let digest = format!("{:064x}", index + 1);
            if !manifest_assets.is_empty() {
                manifest_assets.push(',');
            }
            let _ = write!(
                manifest_assets,
                "{{\"name\":\"{payload}\",\"sha256\":\"{digest}\"}}"
            );
            let _ = writeln!(
                formula,
                "  url \"https://github.com/example/project/releases/download/preview/{payload}\"\n  sha256 \"{digest}\""
            );
        }
        std::fs::write(
            package_dir.join("release-manifest.json"),
            format!("{{\"assets\":[{manifest_assets}]}}"),
        )
        .expect("write manifest fixture");
        std::fs::write(formula_dir.join("example-preview.rb"), &formula)
            .expect("write formula fixture");
        let script = root.join("formula-mapping.sh");
        std::fs::write(&script, render_formula_mapping_script(&payload_names))
            .expect("write mapping script");

        for args in [
            ["init", "--quiet"].as_slice(),
            [
                "add",
                "Formula/example-preview.rb",
                "published-package/release-manifest.json",
            ]
            .as_slice(),
            [
                "-c",
                "user.name=Velnor test",
                "-c",
                "user.email=velnor-test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ]
            .as_slice(),
        ] {
            let status = Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("run git fixture command");
            assert!(status.success(), "git fixture command failed: {args:?}");
        }

        let run = |expected_success: bool| {
            let output = Command::new("bash")
                .arg(&script)
                .current_dir(&root)
                .env("GITHUB_WORKSPACE", &root)
                .env("GITHUB_REPOSITORY", "example/project")
                .env("RELEASE_TAG", "preview")
                .output()
                .expect("run formula mapping script");
            assert_eq!(
                output.status.success(),
                expected_success,
                "formula mapping output: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(true);

        let stale = formula.replace("/releases/download/preview/", "/releases/download/old/");
        std::fs::write(formula_dir.join("example-preview.rb"), stale)
            .expect("write stale formula fixture");
        run(false);

        let first_digest = format!("{:064x}", 1);
        let second_digest = format!("{:064x}", 2);
        let placeholder = "sha256 \"formula-pair-placeholder\"";
        let swapped = formula
            .replacen(&format!("sha256 \"{first_digest}\""), placeholder, 1)
            .replacen(
                &format!("sha256 \"{second_digest}\""),
                &format!("sha256 \"{first_digest}\""),
                1,
            )
            .replace(placeholder, &format!("sha256 \"{second_digest}\""));
        std::fs::write(formula_dir.join("example-preview.rb"), swapped)
            .expect("write mismatched formula fixture");
        run(false);

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
