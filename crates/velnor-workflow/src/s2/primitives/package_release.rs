//! Typed source-bound package-release publication and consumer handoff.
//
// The producer task remains repository-owned: it builds the package bytes and
// writes the declared verified directory. Velnor owns the boundary around
// that directory: exact manifest/identity binding, payload checksums,
// attestation verification, immutable publication, optional rolling refresh,
// and explicit consumer updater inputs. No consumer repository or product
// name is embedded here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use super::{Args, Primitive, RenderCtx, Rendered, PACKAGE_RELEASE};
use crate::s2::provider::ProviderId;
use crate::s2::{
    github_expression, selector_runs_on_yaml, shell_quote, workflow_runtime_setup,
    workflow_runtime_setup_at_checkout_path, ActionPin, GeneratorError, ProjectConfig,
    GENERATED_HEADER,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackageReleaseSpec {
    build_tasks: Vec<String>,
    verify_tasks: Vec<String>,
    pre_publish_tasks: Vec<String>,
    publication_lock_branch: String,
    package_dir: String,
    manifest_schema: String,
    source_repository: String,
    source_ref: String,
    payloads: Vec<String>,
    supporting_assets: Vec<String>,
    channel: String,
    release_tag: String,
    consumer_tag_mode: ConsumerTagMode,
    refresh_rolling_release: bool,
    github_release_type: String,
    publish_environment: String,
    release_title_prefix: String,
    consumer_repository: String,
    consumer_branch: String,
    updater: String,
    updater_token_secret: String,
    update_commit_message: String,
    concurrency_group: String,
    release_inputs: ReleaseInputRules,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConsumerTagMode {
    Immutable,
    Legacy,
}

/// Repository-owned positive and negative path declarations for source-head
/// preview admission. Unknown paths intentionally remain production-capable:
/// an incomplete path inventory may publish an unnecessary preview, but it
/// must never silently suppress a required one.
///
/// These globs classify paths, not Rust semantics. Inline Rust tests live in
/// production `src/` files, so edits there conservatively admit a preview even
/// when the edit only changes an inline test. A path-only classifier cannot
/// claim semantic test-only precision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ReleaseInputRules {
    production_inputs: BTreeMap<String, Vec<String>>,
    production_dependencies: BTreeMap<String, Vec<String>>,
    non_production_inputs: BTreeMap<String, Vec<String>>,
}

/// A path matched by a named rule. Paths are recorded so an operator can
/// explain and replay the exact decision without consulting current main.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct AdmissionMatch {
    id: String,
    path: String,
}

/// Full path-level result of one push admission decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AdmissionPath {
    path: String,
    previous: Option<String>,
    status: String,
}

/// Source-head release admission evidence. The result has no timestamp or
/// mutable-main lookup, so replay of the same explicit source tuple and rules
/// is byte-stable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ReleaseAdmission {
    schema: String,
    repository: String,
    /// Ref named by the triggering event, retained so a replay is tied to it.
    source_ref: String,
    /// Repository-configured source ref that is allowed to publish previews.
    configured_source_ref: String,
    before_sha: String,
    head_sha: String,
    head_tree: String,
    changed_paths: Vec<AdmissionPath>,
    matched_rules: Vec<AdmissionMatch>,
    matched_dependencies: Vec<AdmissionMatch>,
    matched_non_production: Vec<AdmissionMatch>,
    disposition: String,
    reason: String,
    rules_digest: String,
}

struct AdmissionEvent<'a> {
    repository: &'a str,
    event_ref: &'a str,
    configured_source_ref: &'a str,
    event_name: &'a str,
    before_sha: &'a str,
    head_sha: &'a str,
    head_tree: &'a str,
}

pub(crate) struct PackageRelease;

impl Primitive for PackageRelease {
    fn id(&self) -> &'static str {
        PACKAGE_RELEASE
    }

    fn schema(&self) -> &'static [&'static str] {
        &[
            "build_tasks",
            "pre_publish_tasks",
            "publication_lock_branch",
            "verify_tasks",
            "channel",
            "concurrency_group",
            "consumer_branch",
            "consumer_tag_mode",
            "consumer_repository",
            "github_release_type",
            "manifest_schema",
            "package_dir",
            "payloads",
            "publish_environment",
            "release_tag",
            "release_title_prefix",
            "refresh_rolling_release",
            "production_inputs",
            "production_dependencies",
            "non_production_inputs",
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
        validate_mise_tasks(ctx.root, "verify_tasks", &spec.verify_tasks)?;
        validate_mise_tasks(ctx.root, "pre_publish_tasks", &spec.pre_publish_tasks)?;
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

fn shell_quote_asset_name(value: &str) -> String {
    format!("\"{value}\"")
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

fn validate_mise_tasks(root: &Path, key: &str, tasks: &[String]) -> Result<(), GeneratorError> {
    if tasks.is_empty() {
        return Ok(());
    }
    let declared = crate::s2::parse_mise_task_names(root)?;
    for task in tasks {
        if !declared.iter().any(|candidate| candidate == task) {
            return Err(GeneratorError::usage(format!(
                "package-release {key} entry {task} is not declared by mise.toml"
            )));
        }
    }
    Ok(())
}

fn valid_rule_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte))
}

fn compile_release_groups(
    key: &str,
    groups: &BTreeMap<String, Vec<String>>,
    required: bool,
) -> Result<Vec<(String, GlobSet)>, GeneratorError> {
    if required && groups.is_empty() {
        return Err(GeneratorError::usage(format!(
            "package-release {key} must declare at least one named path group"
        )));
    }
    let mut compiled = Vec::with_capacity(groups.len());
    for (id, patterns) in groups {
        if !valid_rule_id(id) {
            return Err(GeneratorError::usage(format!(
                "package-release {key} group id {id:?} must use lowercase letters, digits, `-`, or `_`"
            )));
        }
        if patterns.is_empty() {
            return Err(GeneratorError::usage(format!(
                "package-release {key} group {id} must contain at least one path glob"
            )));
        }
        let mut seen = BTreeSet::new();
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            if pattern.is_empty()
                || pattern.starts_with('/')
                || pattern.contains(['\\', '\0', '\n', '\r'])
                || pattern.split('/').any(|part| matches!(part, "." | ".."))
                || !seen.insert(pattern)
            {
                return Err(GeneratorError::usage(format!(
                    "package-release {key} group {id} has an unsafe or duplicate repository-relative glob: {pattern:?}"
                )));
            }
            let glob = GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map_err(|error| {
                    GeneratorError::usage(format!(
                        "package-release {key} group {id} has invalid glob {pattern:?}: {error}"
                    ))
                })?;
            builder.add(glob);
        }
        let set = builder.build().map_err(|error| {
            GeneratorError::usage(format!(
                "package-release {key} group {id} has invalid glob set: {error}"
            ))
        })?;
        compiled.push((id.clone(), set));
    }
    Ok(compiled)
}

fn parse_release_input_rules(args: &Args<'_>) -> Result<ReleaseInputRules, GeneratorError> {
    let production_inputs = args.string_tables("production_inputs")?;
    let production_inputs_declared = production_inputs.is_some();
    let production_dependencies = args
        .string_tables("production_dependencies")?
        .unwrap_or_default();
    let non_production_inputs = args
        .string_tables("non_production_inputs")?
        .unwrap_or_default();

    let rules = ReleaseInputRules {
        production_inputs: production_inputs.unwrap_or_default(),
        production_dependencies,
        non_production_inputs,
    };
    // Declarations predating release admission omit this table. Keep them
    // valid; their unmatched changes are admitted conservatively below. An
    // explicitly present table remains a strict contract, including when it
    // is empty.
    compile_release_groups(
        "production_inputs",
        &rules.production_inputs,
        production_inputs_declared,
    )?;
    compile_release_groups(
        "production_dependencies",
        &rules.production_dependencies,
        false,
    )?;
    compile_release_groups("non_production_inputs", &rules.non_production_inputs, false)?;

    for patterns in rules
        .production_inputs
        .values()
        .chain(rules.production_dependencies.values())
    {
        for pattern in patterns {
            if rules
                .non_production_inputs
                .values()
                .flatten()
                .any(|skip| skip == pattern)
            {
                return Err(GeneratorError::usage(format!(
                    "package-release path glob {pattern:?} is declared as both production and non-production input"
                )));
            }
        }
    }
    Ok(rules)
}

fn git_output(root: &Path, args: &[&str], operation: &str) -> Result<Vec<u8>, GeneratorError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| {
            GeneratorError::usage(format!(
                "run git {operation} for release admission: {error}"
            ))
        })?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "git {operation} failed for release admission: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn full_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn empty_tree_sha(root: &Path) -> Result<String, GeneratorError> {
    let mut child = Command::new("git")
        .args(["hash-object", "-t", "tree", "--stdin"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            GeneratorError::usage(format!(
                "start empty-tree identity computation for release admission: {error}"
            ))
        })?;
    child
        .stdin
        .take()
        .ok_or_else(|| GeneratorError::usage("git empty-tree stdin unavailable"))?
        .write_all(&[])
        .map_err(|error| {
            GeneratorError::usage(format!(
                "write empty-tree input for release admission: {error}"
            ))
        })?;
    let output = child.wait_with_output().map_err(|error| {
        GeneratorError::usage(format!(
            "read empty-tree identity for release admission: {error}"
        ))
    })?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "git empty-tree identity computation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let tree = String::from_utf8(output.stdout)
        .map_err(|_| GeneratorError::usage("git returned a non-UTF-8 empty-tree identity"))?
        .trim()
        .to_owned();
    if !full_sha(&tree) {
        return Err(GeneratorError::usage(
            "git returned an invalid empty-tree identity",
        ));
    }
    Ok(tree)
}

fn compiled_groups(
    key: &str,
    groups: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<(String, GlobSet)>, GeneratorError> {
    compile_release_groups(key, groups, false)
}

fn match_groups(
    groups: &[(String, GlobSet)],
    paths: &[String],
) -> (Vec<AdmissionMatch>, BTreeSet<String>) {
    let mut matches = BTreeSet::new();
    let mut matched_paths = BTreeSet::new();
    for path in paths {
        for (id, globs) in groups {
            if globs.is_match(path) {
                matches.insert(AdmissionMatch {
                    id: id.clone(),
                    path: path.clone(),
                });
                matched_paths.insert(path.clone());
            }
        }
    }
    (matches.into_iter().collect(), matched_paths)
}

fn admission_path(change: &crate::s2::reuse::ChangedPath) -> AdmissionPath {
    let status = match change.status {
        crate::s2::reuse::ChangeKind::Added => "added",
        crate::s2::reuse::ChangeKind::Modified => "modified",
        crate::s2::reuse::ChangeKind::Deleted => "deleted",
        crate::s2::reuse::ChangeKind::Renamed => "renamed",
    };
    AdmissionPath {
        path: change.path.clone(),
        previous: change.previous.clone(),
        status: status.to_owned(),
    }
}

fn evaluate_release_admission(
    event: &AdmissionEvent<'_>,
    rules: &ReleaseInputRules,
    changes: &[crate::s2::reuse::ChangedPath],
) -> Result<ReleaseAdmission, GeneratorError> {
    let production = compiled_groups("production_inputs", &rules.production_inputs)?;
    let dependencies = compiled_groups("production_dependencies", &rules.production_dependencies)?;
    let non_production = compiled_groups("non_production_inputs", &rules.non_production_inputs)?;
    let changed_paths = changes.iter().map(admission_path).collect::<Vec<_>>();
    let effective_paths = crate::s2::reuse::effective_paths(changes);
    let (matched_rules, production_paths) = match_groups(&production, &effective_paths);
    let (matched_dependencies, dependency_paths) = match_groups(&dependencies, &effective_paths);
    let (matched_non_production, non_production_paths) =
        match_groups(&non_production, &effective_paths);
    let mut all_known = production_paths.clone();
    all_known.extend(dependency_paths.iter().cloned());
    all_known.extend(non_production_paths.iter().cloned());
    let unknown_paths = effective_paths
        .iter()
        .filter(|path| !all_known.contains(*path))
        .cloned()
        .collect::<Vec<_>>();

    let (disposition, reason) = if event.event_name != "push" {
        (
            "skip".to_owned(),
            format!("event {:?} is not a qualifying push", event.event_name),
        )
    } else if event.event_ref != event.configured_source_ref {
        (
            "skip".to_owned(),
            format!(
                "push ref {:?} does not match configured source ref {:?}",
                event.event_ref, event.configured_source_ref
            ),
        )
    } else if !matched_rules.is_empty() || !matched_dependencies.is_empty() {
        let first = matched_rules
            .first()
            .map(|entry| format!("production rule {} matched {}", entry.id, entry.path))
            .or_else(|| {
                matched_dependencies.first().map(|entry| {
                    format!("production dependency {} matched {}", entry.id, entry.path)
                })
            })
            .unwrap_or_else(|| "declared production input matched".to_owned());
        ("admit".to_owned(), first)
    } else if !unknown_paths.is_empty() {
        (
            "admit".to_owned(),
            format!(
                "{} changed path(s) have no non-production declaration; admitting conservatively",
                unknown_paths.len()
            ),
        )
    } else if changed_paths.is_empty() {
        (
            "skip".to_owned(),
            "push changed no paths between its recorded before and head SHAs".to_owned(),
        )
    } else {
        (
            "skip".to_owned(),
            format!(
                "all {} changed path(s) match declared non-production inputs",
                changed_paths.len()
            ),
        )
    };
    let rules_digest = hex_sha256(&serde_json::to_vec(rules).map_err(|error| {
        GeneratorError::usage(format!("serialize release input rules: {error}"))
    })?);
    Ok(ReleaseAdmission {
        schema: "homebrew-source-release-admission/v1".to_owned(),
        repository: event.repository.to_owned(),
        source_ref: event.event_ref.to_owned(),
        configured_source_ref: event.configured_source_ref.to_owned(),
        before_sha: event.before_sha.to_owned(),
        head_sha: event.head_sha.to_owned(),
        head_tree: event.head_tree.to_owned(),
        changed_paths,
        matched_rules,
        matched_dependencies,
        matched_non_production,
        disposition,
        reason,
        rules_digest,
    })
}

const RELEASE_DIGEST_HEX: &[u8; 16] = b"0123456789abcdef";

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(RELEASE_DIGEST_HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(RELEASE_DIGEST_HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn package_release_spec(root: &Path) -> Result<PackageReleaseSpec, GeneratorError> {
    let config_path = root.join(crate::s2::config::GENERATION_CONFIG_PATH);
    let config = crate::s2::config::load(&config_path)?;
    let rows = config
        .declare()
        .iter()
        .filter(|row| row.primitive() == super::PACKAGE_RELEASE)
        .collect::<Vec<_>>();
    if rows.len() != 1 {
        return Err(GeneratorError::usage(format!(
            "release-admission requires exactly one package-release declaration; found {}",
            rows.len()
        )));
    }
    parse_spec(&Args(rows[0].args()))
}

fn checked_source_head_tree(root: &Path, head_sha: &str) -> Result<String, GeneratorError> {
    let head_commit = String::from_utf8(git_output(
        root,
        &["rev-parse", "HEAD^{commit}"],
        "rev-parse",
    )?)
    .map_err(|_| GeneratorError::usage("git returned a non-UTF-8 checkout HEAD"))?
    .trim()
    .to_owned();
    if head_commit != head_sha {
        return Err(GeneratorError::usage(format!(
            "release-admission checkout HEAD {head_commit} does not match event head {head_sha}"
        )));
    }
    let head_tree = String::from_utf8(git_output(
        root,
        &["rev-parse", "HEAD^{tree}"],
        "rev-parse tree",
    )?)
    .map_err(|_| GeneratorError::usage("git returned a non-UTF-8 checkout tree"))?
    .trim()
    .to_owned();
    if !full_sha(&head_tree) {
        return Err(GeneratorError::usage(
            "git returned an invalid source head tree identity",
        ));
    }
    Ok(head_tree)
}

fn release_changed_paths(
    root: &Path,
    before_sha: &str,
    head_sha: &str,
    qualifying_push: bool,
) -> Result<Vec<crate::s2::reuse::ChangedPath>, GeneratorError> {
    if !qualifying_push {
        return Ok(Vec::new());
    }
    let base = if before_sha.bytes().all(|byte| byte == b'0') {
        empty_tree_sha(root)?
    } else {
        if git_output(
            root,
            &["cat-file", "-e", &format!("{before_sha}^{{commit}}")],
            "cat-file before commit",
        )
        .is_err()
        {
            return Err(GeneratorError::usage(format!(
                "release-admission before commit {before_sha} is unavailable; fetch or restore that exact history and replay the recorded event, never substitute current main"
            )));
        }
        let ancestry = Command::new("git")
            .args(["merge-base", "--is-ancestor", before_sha, head_sha])
            .current_dir(root)
            .output()
            .map_err(|error| {
                GeneratorError::usage(format!("run git merge-base for release admission: {error}"))
            })?;
        if !ancestry.status.success() {
            return Err(GeneratorError::usage(format!(
                "release-admission before commit {before_sha} is not an ancestor of event head {head_sha}; force-pushed or unrelated history cannot be substituted"
            )));
        }
        before_sha.to_owned()
    };
    let diff = git_output(
        root,
        &[
            "diff",
            "--name-status",
            "--find-renames=50%",
            "-z",
            &base,
            head_sha,
        ],
        "diff",
    )?;
    crate::s2::reuse::parse_name_status_nul(&diff).ok_or_else(|| {
        GeneratorError::usage(
            "release-admission could not parse the complete NUL-delimited Git diff; refusing a skip",
        )
    })
}

fn verify_replay_receipt(path: &Path, result: &ReleaseAdmission) -> Result<(), GeneratorError> {
    let recorded = std::fs::read(path).map_err(|error| {
        GeneratorError::io("read release admission replay evidence", path, &error)
    })?;
    let expected: ReleaseAdmission = serde_json::from_slice(&recorded).map_err(|error| {
        GeneratorError::usage(format!("parse release admission replay evidence: {error}"))
    })?;
    if expected != *result {
        return Err(GeneratorError::usage(
            "release-admission replay evidence does not match the exact repository/ref/before/head/tree, rules, complete diff, or disposition",
        ));
    }
    Ok(())
}

/// Run the source-specific admission command against explicit event inputs.
/// The source checkout must already be pinned at `head`; the command never
/// reads a branch tip or queries GitHub's path-filter API.
pub(crate) fn release_admission_command(
    root: &Path,
    repository: &str,
    event_ref: &str,
    event: &str,
    before_sha: &str,
    head_sha: &str,
    replay_from: Option<&Path>,
) -> Result<(), GeneratorError> {
    let spec = package_release_spec(root)?;
    if repository != spec.source_repository {
        return Err(GeneratorError::usage(format!(
            "release-admission repository {repository:?} does not match configured source {}",
            spec.source_repository
        )));
    }
    if !full_sha(head_sha) || !full_sha(before_sha) {
        return Err(GeneratorError::usage(
            "release-admission before and head must be full 40-character lowercase commit SHAs",
        ));
    }
    let head_tree = checked_source_head_tree(root, head_sha)?;
    let qualifying_push = event == "push" && event_ref == spec.source_ref;
    let changes = release_changed_paths(root, before_sha, head_sha, qualifying_push)?;
    let event = AdmissionEvent {
        repository: &spec.source_repository,
        event_ref,
        configured_source_ref: &spec.source_ref,
        event_name: event,
        before_sha,
        head_sha,
        head_tree: &head_tree,
    };
    let result = evaluate_release_admission(&event, &spec.release_inputs, &changes)?;
    if let Some(path) = replay_from {
        verify_replay_receipt(path, &result)?;
    }
    let json = serde_json::to_string(&result)
        .map_err(|error| GeneratorError::usage(format!("serialize release admission: {error}")))?;
    println!("{json}");
    Ok(())
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

    let verify_tasks_value = args.strings("verify_tasks")?;
    let verify_tasks = verify_tasks_value.clone().unwrap_or_default();
    if verify_tasks_value.is_some() && verify_tasks.is_empty() {
        return Err(GeneratorError::usage(
            "package-release verify_tasks must contain at least one mise task when declared",
        ));
    }
    for task in &verify_tasks {
        if !crate::s2::config::valid_check_profile_task(task) {
            return Err(GeneratorError::usage(format!(
                "package-release verify_tasks entry {task} is not a plain mise task"
            )));
        }
    }

    let pre_publish_tasks_value = args.strings("pre_publish_tasks")?;
    let pre_publish_tasks = pre_publish_tasks_value.clone().unwrap_or_default();
    if pre_publish_tasks_value.is_some() && pre_publish_tasks.is_empty() {
        return Err(GeneratorError::usage(
            "package-release pre_publish_tasks must contain at least one mise task when declared",
        ));
    }
    for task in &pre_publish_tasks {
        if !crate::s2::config::valid_check_profile_task(task) {
            return Err(GeneratorError::usage(format!(
                "package-release pre_publish_tasks entry {task} is not a plain mise task"
            )));
        }
        if verify_tasks.iter().any(|verify_task| verify_task == task) {
            return Err(GeneratorError::usage(format!(
                "package-release pre_publish_tasks entry {task} overlaps verify_tasks"
            )));
        }
    }
    for (index, task) in pre_publish_tasks.iter().enumerate() {
        if pre_publish_tasks[..index]
            .iter()
            .any(|previous| previous == task)
        {
            return Err(GeneratorError::usage(format!(
                "package-release pre_publish_tasks contains duplicate entry {task}"
            )));
        }
    }

    let publication_lock_branch = required_string(args, "publication_lock_branch")?;
    if !crate::s2::runtime::valid_branch(&publication_lock_branch) {
        return Err(GeneratorError::usage(
            "package-release publication_lock_branch must be a valid branch name",
        ));
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
    let consumer_tag_mode = match args
        .string("consumer_tag_mode")?
        .as_deref()
        .unwrap_or("legacy")
    {
        "immutable" => ConsumerTagMode::Immutable,
        "legacy" => ConsumerTagMode::Legacy,
        _ => {
            return Err(GeneratorError::usage(
                "package-release consumer_tag_mode must be `immutable` or `legacy`",
            ));
        }
    };
    let refresh_rolling_release = match args.0.get("refresh_rolling_release") {
        None => true,
        Some(toml::Value::Boolean(value)) => *value,
        Some(_) => {
            return Err(GeneratorError::usage(
                "package-release refresh_rolling_release must be a boolean",
            ));
        }
    };
    if consumer_tag_mode == ConsumerTagMode::Legacy && !refresh_rolling_release {
        return Err(GeneratorError::usage(
            "package-release legacy consumer_tag_mode requires refresh_rolling_release = true",
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
    let release_inputs = parse_release_input_rules(args)?;

    Ok(PackageReleaseSpec {
        build_tasks,
        verify_tasks,
        pre_publish_tasks,
        publication_lock_branch,
        package_dir,
        manifest_schema,
        source_repository,
        source_ref,
        payloads,
        supporting_assets,
        channel,
        release_tag,
        consumer_tag_mode,
        refresh_rolling_release,
        github_release_type,
        publish_environment,
        release_title_prefix,
        consumer_repository,
        consumer_branch,
        updater,
        updater_token_secret,
        update_commit_message,
        concurrency_group,
        release_inputs,
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

fn verification_task_script(tasks: &[String]) -> String {
    let mut script = String::from(
        r#"set -euo pipefail
cd "$VELNOR_SOURCE_CHECKOUT_DIR"
"#,
    );
    for task in tasks {
        let _ = writeln!(script, "mise run {}", shell_quote(task));
    }
    script
}

fn render_verification_task_step(
    name: &str,
    tasks: &[String],
    verified_package_dir: Option<&str>,
) -> String {
    if tasks.is_empty() {
        return String::new();
    }
    let environment = verified_package_dir.map_or_else(String::new, |directory| {
        format!("        env:\n          VELNOR_VERIFIED_PACKAGE_DIR: {directory}\n")
    });
    format!(
        "      - name: {name}\n{environment}        run: |\n{}",
        indent_script(&verification_task_script(tasks), 10)
    )
}

#[allow(clippy::too_many_lines)]
fn package_build_script(tasks: &[String]) -> String {
    let mut script = String::from(
        r#"set -Eeuo pipefail
workspace="$GITHUB_WORKSPACE"
workspace_real="$(cd -- "$workspace" && pwd -P)"
expected_handoff="$workspace/$PACKAGE_DIR"
expected_handoff_real="$workspace_real/$PACKAGE_DIR"
if [[ "$VELNOR_VERIFIED_PACKAGE_DIR" != "$expected_handoff" ]]; then
  echo "::error::verified package path differs from the declared workspace handoff" >&2
  exit 1
fi
IFS='/' read -r -a handoff_segments <<< "$PACKAGE_DIR"
last_segment=$((${#handoff_segments[@]} - 1))
handoff_parent="$workspace_real"
for index in "${!handoff_segments[@]}"; do
  candidate="$handoff_parent/${handoff_segments[$index]}"
  if [[ -L "$candidate" ]]; then
    echo "::error::package handoff path contains a symlink" >&2
    exit 1
  fi
  if [[ -e "$candidate" ]]; then
    if [[ ! -d "$candidate" ]]; then
      echo "::error::package handoff path contains a non-directory entry" >&2
      exit 1
    fi
    if (( index == last_segment )); then
      echo "::error::package handoff already exists; refusing stale output" >&2
      exit 1
    fi
    resolved_parent="$(cd -- "$candidate" && pwd -P)"
    case "$resolved_parent/" in
      "$workspace_real/"*) ;;
      *) echo "::error::package handoff parent escapes the workspace" >&2; exit 1 ;;
    esac
  else
    mkdir -- "$candidate"
  fi
  handoff_parent="$candidate"
done
if ! git -C "$VELNOR_SOURCE_CHECKOUT_DIR" check-ignore -q -- "$PACKAGE_DIR"; then
  echo "::error::declared package handoff must be ignored by the source checkout" >&2
  exit 1
fi

actual_commit="$(git -C "$VELNOR_SOURCE_CHECKOUT_DIR" rev-parse HEAD^{commit})"
if [[ "$actual_commit" != "$EXPECTED_SOURCE_COMMIT" ]]; then
  echo "::error::source checkout commit differs from the admitted event commit" >&2
  exit 1
fi
actual_tree="$(git -C "$VELNOR_SOURCE_CHECKOUT_DIR" rev-parse HEAD^{tree})"
if [[ "$actual_tree" != "$EXPECTED_SOURCE_TREE" ]]; then
  echo "::error::source checkout tree differs from the admitted event tree" >&2
  exit 1
fi
source_status="$(git -C "$VELNOR_SOURCE_CHECKOUT_DIR" status --porcelain=v1 --untracked-files=all -- . ":(exclude)$PACKAGE_DIR")"
if [[ -n "$source_status" ]]; then
  echo "::error::source checkout is dirty before package production" >&2
  printf '%s\n' "$source_status" >&2
  exit 1
fi

runner_temp="$RUNNER_TEMP"
runner_temp_real="$(cd -- "$runner_temp" && pwd -P)"
case "$runner_temp_real/" in
  "$workspace_real/"*) echo "::error::package scratch must be outside the source checkout" >&2; exit 1 ;;
esac
[[ "${GITHUB_RUN_ID:-}" =~ ^[0-9]+$ && "${GITHUB_RUN_ATTEMPT:-}" =~ ^[1-9][0-9]*$ ]] || {
  echo "::error::package scratch requires numeric run and attempt identities" >&2
  exit 1
}
expected_scratch="$runner_temp/velnor-package-scratch-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"
if [[ "$VELNOR_PACKAGE_SCRATCH_DIR" != "$expected_scratch" ]]; then
  echo "::error::package scratch path differs from the run-owned runner-temp path" >&2
  exit 1
fi
if [[ -e "$VELNOR_PACKAGE_SCRATCH_DIR" || -L "$VELNOR_PACKAGE_SCRATCH_DIR" ]]; then
  echo "::error::package scratch path already exists; refusing to remove a pre-existing path" >&2
  exit 1
fi
mkdir -m 700 -- "$VELNOR_PACKAGE_SCRATCH_DIR"
scratch_owned=1
CARGO_TARGET_DIR="$VELNOR_PACKAGE_SCRATCH_DIR/target"
cleanup_package_scratch() {
  local status=$?
  trap - EXIT
  if (( scratch_owned )); then
    if ! rm -rf -- "$VELNOR_PACKAGE_SCRATCH_DIR"; then
      status=1
    fi
  fi
  exit "$status"
}
trap cleanup_package_scratch EXIT

write_source_inventory() {
  local output_file="$1"
  local path
  {
    git -C "$VELNOR_SOURCE_CHECKOUT_DIR" ls-files --others --exclude-standard -z -- .
    git -C "$VELNOR_SOURCE_CHECKOUT_DIR" ls-files --others --ignored --exclude-standard -z -- .
  } | LC_ALL=C sort -zu | while IFS= read -r -d '' path; do
    if [[ "$path" == "$PACKAGE_DIR" || "$path" == "$PACKAGE_DIR/"* ]]; then
      continue
    fi
    printf '%s\0' "$path"
  done > "$output_file"
}
print_source_inventory() {
  local path
  while IFS= read -r -d '' path; do
    printf '  %q\n' "$path" >&2
  done < "$1"
}
source_inventory_before="$VELNOR_PACKAGE_SCRATCH_DIR/source-inventory-before"
source_inventory_after="$VELNOR_PACKAGE_SCRATCH_DIR/source-inventory-after"
write_source_inventory "$source_inventory_before"
if [[ -s "$source_inventory_before" ]]; then
  echo "::error::source checkout has stale ignored or untracked inputs before package production" >&2
  print_source_inventory "$source_inventory_before"
  exit 1
fi
export PACKAGE_DIR VELNOR_VERIFIED_PACKAGE_DIR VELNOR_SOURCE_CHECKOUT_DIR CARGO_TARGET_DIR
export VELNOR_SOURCE_COMMIT VELNOR_SOURCE_REF VELNOR_PACKAGE_SCRATCH_DIR
"#,
    );
    for task in tasks {
        let _ = writeln!(script, "mise run {}", shell_quote(task));
    }
    script.push_str(
        r#"
handoff_parent="$workspace_real"
for segment in "${handoff_segments[@]}"; do
  candidate="$handoff_parent/$segment"
  if [[ -L "$candidate" || ! -d "$candidate" ]]; then
    echo "::error::package producer returned a missing or symlinked handoff component" >&2
    exit 1
  fi
  handoff_parent="$candidate"
done
resolved_handoff="$(cd -- "$expected_handoff" && pwd -P)"
if [[ "$resolved_handoff" != "$expected_handoff_real" ]]; then
  echo "::error::package producer changed the resolved handoff path" >&2
  exit 1
fi
write_source_inventory "$source_inventory_after"
if ! cmp -s "$source_inventory_before" "$source_inventory_after"; then
  echo "::error::package producer changed ignored or untracked source inputs outside the declared handoff" >&2
  echo "before:" >&2
  print_source_inventory "$source_inventory_before"
  echo "after:" >&2
  print_source_inventory "$source_inventory_after"
  exit 1
fi
source_status="$(git -C "$VELNOR_SOURCE_CHECKOUT_DIR" status --porcelain=v1 --untracked-files=all -- . ":(exclude)$PACKAGE_DIR")"
if [[ -n "$source_status" ]]; then
  echo "::error::package producer changed tracked or untracked source files outside the declared handoff" >&2
  printf '%s\n' "$source_status" >&2
  exit 1
fi
"#,
    );
    script
}

/// The verified-directory boundary shared by the producer and publisher jobs.
/// It intentionally checks the downloaded directory again: an artifact or
/// release may never be trusted merely because its producer job passed.
#[allow(clippy::too_many_lines)]
fn verification_script(spec: &PackageReleaseSpec) -> String {
    let mut script = String::from(
        r#"set -euo pipefail
dir="$VELNOR_VERIFIED_PACKAGE_DIR"
if [[ ! -d "$dir" || -L "$dir" ]]; then
  echo "::error::verified package directory is missing or a symlink" >&2
  exit 1
fi
workspace="$GITHUB_WORKSPACE"
expected_root="${VELNOR_PACKAGE_HANDOFF_ROOT:-$workspace}"
expected_relative="${VELNOR_PACKAGE_HANDOFF_RELATIVE:-$PACKAGE_DIR}"
handoff_source_commit="${VELNOR_PACKAGE_HANDOFF_SOURCE_COMMIT:-$EXPECTED_SOURCE_COMMIT}"
if [[ "$expected_root" != /* || -z "$expected_relative" || "$expected_relative" == /* || "$expected_relative" == */ || "$expected_relative" == *//* ]]; then
  echo "::error::verified package handoff root must be absolute and relative path must be normalized" >&2
  exit 1
fi
if [[ ! "$handoff_source_commit" =~ ^[0-9a-f]{40}$ || "$handoff_source_commit" != "$EXPECTED_SOURCE_COMMIT" ]]; then
  echo "::error::verified package handoff path is not bound to the expected source commit" >&2
  exit 1
fi
expected_dir="$expected_root/$expected_relative"
if [[ "$dir" != "$expected_dir" ]]; then
  echo "::error::verified package path differs from the declared handoff" >&2
  exit 1
fi
expected_root_real="$(cd -- "$expected_root" && pwd -P)"
IFS='/' read -r -a verified_segments <<< "$expected_relative"
verified_parent="$expected_root_real"
for segment in "${verified_segments[@]}"; do
  if [[ -z "$segment" || "$segment" == . || "$segment" == .. ]]; then
    echo "::error::verified package relative path contains an invalid component" >&2
    exit 1
  fi
  candidate="$verified_parent/$segment"
  if [[ -L "$candidate" ]]; then
    echo "::error::verified package path contains a symlink component" >&2
    exit 1
  fi
  if [[ ! -d "$candidate" ]]; then
    echo "::error::verified package path contains a missing or non-directory component" >&2
    exit 1
  fi
  verified_parent="$candidate"
done
resolved_dir="$(cd -- "$dir" && pwd -P)"
expected_dir_real="$expected_root_real/$expected_relative"
if [[ "$resolved_dir" != "$expected_dir_real" ]]; then
  echo "::error::verified package path does not resolve to the declared handoff" >&2
  exit 1
fi
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
source_inventory_raw="$(mktemp)"
source_inventory_filtered="$(mktemp)"
trap 'rm -f -- "$expected_files" "$actual_files" "$expected_names" "$actual_names" "$checksum_names" "$expected_supporting_names" "$actual_supporting_names" "$source_inventory_raw" "$source_inventory_filtered"' EXIT
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
source_checkout_real="$(cd -- "$source_checkout" && pwd -P)"
source_handoff_relative=""
case "$expected_dir_real/" in
  "$source_checkout_real/"*) source_handoff_relative="${expected_dir_real#"$source_checkout_real"/}" ;;
esac
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
source_status_pathspecs=(.)
if [[ -n "$source_handoff_relative" ]]; then
  source_status_pathspecs+=(":(exclude)$source_handoff_relative")
fi
source_status="$(git -C "$source_checkout" status --porcelain=v1 --untracked-files=all -- "${source_status_pathspecs[@]}")"
if [[ -n "$source_status" ]]; then
  echo "::error::source checkout has changes outside the declared package handoff" >&2
  printf '%s\n' "$source_status" >&2
  exit 1
fi
{
  git -C "$source_checkout" ls-files --others --exclude-standard -z -- .
  git -C "$source_checkout" ls-files --others --ignored --exclude-standard -z -- .
} | LC_ALL=C sort -zu > "$source_inventory_raw"
while IFS= read -r -d '' path; do
  if [[ -n "$source_handoff_relative" ]]; then
    if [[ "$path" == "$source_handoff_relative" || "$path" == "$source_handoff_relative/"* ]]; then
      continue
    fi
  fi
  printf '%s\0' "$path"
done < "$source_inventory_raw" > "$source_inventory_filtered"
if [[ -s "$source_inventory_filtered" ]]; then
  echo "::error::source checkout has ignored or untracked files outside the declared package handoff" >&2
  while IFS= read -r -d '' path; do
    printf '  %q\n' "$path" >&2
  done < "$source_inventory_filtered"
  exit 1
fi
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
        let _ = write!(script, "  {}{}", shell_quote_asset_name(name), suffix);
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
        let _ = write!(script, "  {}{}", shell_quote_asset_name(name), suffix);
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
        let _ = write!(script, "  {}{}", shell_quote_asset_name(name), suffix);
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
    if (length($1) != 64 || $1 !~ /^[0-9a-f]+$/ || name == "" || index(name, "\\") || name ~ /[[:space:]]/) {
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

fn render_admission_job(
    spec: &PackageReleaseSpec,
    runner: &str,
    checkout: &str,
    upload: &str,
    runtime_setup: &str,
) -> String {
    let repository_expr = github_expression("github.repository");
    let ref_expr = github_expression("github.ref");
    let event_expr = github_expression("github.event_name");
    let before_expr = github_expression("github.event.before || github.sha");
    let head_expr = github_expression("github.sha");
    let runner_temp_expr = github_expression("runner.temp");
    let expected_repository = crate::s2::yaml_scalar(&spec.source_repository);
    let expected_ref = crate::s2::yaml_scalar(&spec.source_ref);
    let mut output = String::new();
    let _ = writeln!(
        output,
        "  admission:\n    name: Admit production release inputs\n    if: github.event_name == 'push' || github.event_name == 'workflow_dispatch'\n    runs-on: {runner}\n    timeout-minutes: 10\n    permissions:\n      contents: read\n    outputs:\n      disposition: {}\n      head_sha: {}\n      head_tree: {}\n    steps:\n      - name: Checkout exact event head and full history\n        uses: {checkout}\n        with:\n          ref: {head_expr}\n          fetch-depth: 0\n          persist-credentials: false\n",
        github_expression("steps.admit.outputs.disposition"),
        github_expression("steps.admit.outputs.head_sha"),
        github_expression("steps.admit.outputs.head_tree"),
    );
    output.push_str(runtime_setup);
    let _ = writeln!(
        output,
        "      - name: Classify complete source push diff\n        id: admit\n        env:\n          EVENT_REPOSITORY: {repository_expr}\n          EVENT_REF: {ref_expr}\n          EVENT_NAME: {event_expr}\n          BEFORE_SHA: {before_expr}\n          HEAD_SHA: {head_expr}\n          EXPECTED_SOURCE_REPOSITORY: {expected_repository}\n          EXPECTED_SOURCE_REF: {expected_ref}\n        run: |\n          set -euo pipefail\n          result_file=\"$RUNNER_TEMP/package-release-admission.json\"\n          velnor-workflow release-admission \\\n            --repository \"$EVENT_REPOSITORY\" \\\n            --ref \"$EVENT_REF\" \\\n            --event \"$EVENT_NAME\" \\\n            --before \"$BEFORE_SHA\" \\\n            --head \"$HEAD_SHA\" > \"$result_file\"\n          [[ \"$(jq -er '.repository' \"$result_file\")\" == \"$EXPECTED_SOURCE_REPOSITORY\" ]]\n          [[ \"$(jq -er '.source_ref' \"$result_file\")\" == \"$EVENT_REF\" ]]
          [[ \"$(jq -er '.configured_source_ref' \"$result_file\")\" == \"$EXPECTED_SOURCE_REF\" ]]\n          disposition=\"$(jq -er '.disposition' \"$result_file\")\"\n          case \"$disposition\" in admit|skip) ;; *) echo \"::error::invalid release admission disposition: $disposition\" >&2; exit 1;; esac\n          printf 'disposition=%s\\n' \"$disposition\" >> \"$GITHUB_OUTPUT\"\n          printf 'head_sha=%s\\n' \"$(jq -er '.head_sha' \"$result_file\")\" >> \"$GITHUB_OUTPUT\"\n          printf 'head_tree=%s\\n' \"$(jq -er '.head_tree' \"$result_file\")\" >> \"$GITHUB_OUTPUT\"\n          jq -cr '\"admission=\" + .disposition + \" reason=\" + .reason + \" changed_paths=\" + (.changed_paths|length|tostring)' \"$result_file\"\n      - name: Retain source admission evidence\n        uses: {upload}\n        with:\n          name: source-release-admission-{head_expr}\n          path: {runner_temp_expr}/package-release-admission.json\n          if-no-files-found: error\n          retention-days: 30\n"
    );
    output
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
    let publish_runtime_setup = if provider == ProviderId::GithubHosted {
        workflow_runtime_setup_at_checkout_path(
            ProviderId::GithubHosted,
            &config.repository,
            &config.workflow_revision,
            "source",
        )
    } else {
        String::new()
    };
    let checkout = ActionPin::Checkout.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let download = ActionPin::DownloadArtifact.reference();
    let mise = ActionPin::Mise.reference();
    let attest = ActionPin::Attest.reference();
    let source_commit_expr = github_expression("needs.admission.outputs.head_sha");
    let source_tree_expr = github_expression("needs.admission.outputs.head_tree");
    let build_runtime_setup = format!(
        "{runtime_setup}      - name: Verify admitted source tree\n        run: |\n          set -euo pipefail\n          actual_commit=\"$(git rev-parse HEAD^{{commit}})\"\n          if [[ \"$actual_commit\" != \"$EXPECTED_SOURCE_COMMIT\" ]]; then echo \"::error::checked out source commit differs from admitted event commit\" >&2; exit 1; fi\n          actual_tree=\"$(git rev-parse HEAD^{{tree}})\"\n          if [[ \"$actual_tree\" != \"$EXPECTED_SOURCE_TREE\" ]]; then echo \"::error::checked out source tree differs from admitted event tree\" >&2; exit 1; fi\n          source_status=\"$(git status --porcelain=v1 --untracked-files=all -- . \":(exclude)$PACKAGE_DIR\")\"\n          if [[ -n \"$source_status\" ]]; then echo \"::error::checked out source is dirty before package production\" >&2; printf '%s\\n' \"$source_status\" >&2; exit 1; fi\n"
    );
    let publish_source_commit_expr = github_expression("needs.build.outputs.source_commit");
    let workspace_expr = github_expression("github.workspace");
    let build_verify = indent_script(&verification_script(spec), 10);
    let publish_verify = indent_script(&verification_script(spec), 10);
    let build_verify_tasks = render_verification_task_step(
        "Run repository package verification tasks",
        &spec.verify_tasks,
        None,
    );
    let updater_token_expr = github_expression(&format!("secrets.{}", spec.updater_token_secret));
    let github_token_expr = github_expression("github.token");
    let build_if = github_expression(&format!(
        "needs.admission.outputs.disposition == 'admit' && github.ref == '{}'",
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
    let package_scratch_expr = format!(
        "{}/velnor-package-scratch-{}-{}",
        github_expression("runner.temp"),
        github_expression("github.run_id"),
        github_expression("github.run_attempt")
    );
    let run_name = format!(
        "Package release · {} · {}",
        github_expression("github.event_name"),
        github_expression("github.ref_name")
    );
    let build_script = indent_script(&package_build_script(&spec.build_tasks), 10);
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
    output.push_str("jobs:\n");
    output.push_str(&render_admission_job(
        spec,
        &runner,
        checkout,
        upload,
        &runtime_setup,
    ));
    output.push('\n');
    let _ = writeln!(
        output,
        "  build:\n    name: Verify package release\n    needs: admission\n    if: {build_if}\n    runs-on: {runner}\n    timeout-minutes: 90\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    outputs:\n      version: {}\n      source_commit: {}\n    env:\n      PACKAGE_DIR: {package_dir_yaml}\n      VELNOR_VERIFIED_PACKAGE_DIR: {workspace_expr}/{package_dir}\n      VELNOR_SOURCE_CHECKOUT_DIR: {workspace_expr}\n      VELNOR_PACKAGE_CHANNEL: {channel_yaml}\n      EXPECTED_SOURCE_REPOSITORY: {source_repository_yaml}\n      EXPECTED_SOURCE_REF: {source_ref_yaml}\n      EXPECTED_MANIFEST_SCHEMA: {schema_yaml}\n      EXPECTED_SOURCE_COMMIT: {source_commit_expr}\n      EXPECTED_SOURCE_TREE: {source_tree_expr}\n",
        github_expression("steps.verify.outputs.version"),
        github_expression("steps.verify.outputs.source_commit"),
    );
    let _ = writeln!(
        output,
        "    steps:\n      - name: Checkout source\n        uses: {checkout}\n        with:\n          ref: {source_commit_expr}\n          fetch-depth: 0\n          persist-credentials: false\n{build_runtime_setup}      - name: Set up Mise\n        uses: {mise}\n        with:\n          install: false\n      - name: Install locked build tools\n        run: mise --yes install --locked --include-task-tools\n      - name: Enforce workflow policy\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\n      - name: Build verified package directory\n        env:\n          VELNOR_SOURCE_COMMIT: {source_commit_expr}\n          VELNOR_SOURCE_REF: {source_shell}\n          VELNOR_PACKAGE_SCRATCH_DIR: {package_scratch_expr}\n        run: |\n{build_script}      - name: Verify manifest, identity, checksums, and exact file set\n        id: verify\n        run: |\n{build_verify}{build_verify_tasks}      - name: Attest declared package assets\n        uses: {attest}\n        with:\n          subject-path: |\n{attestation_subjects}      - name: Upload verified package handoff\n        uses: {upload}\n        with:\n          name: package-release\n          path: |\n{artifact_upload_paths}          include-hidden-files: true\n          if-no-files-found: error\n          retention-days: 2\n",
    );
    output.push('\n');
    output.push_str(&render_publish_job(
        spec,
        &runner,
        checkout,
        download,
        &publish_verify,
        &publish_runtime_setup,
        mise,
        &spec.verify_tasks,
        &spec.pre_publish_tasks,
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

fn render_consumer_identity_check_script() -> &'static str {
    r#"set -euo pipefail
[[ "$VELNOR_PACKAGE_SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] || { echo "::error::verified package source commit is invalid" >&2; exit 1; }
[ "$VELNOR_PACKAGE_SOURCE_COMMIT" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::verified package source commit differs from the admitted source" >&2; exit 1; }
expected_asset_tag="$RELEASE_TAG-$VELNOR_PACKAGE_SOURCE_COMMIT"
[ "$VELNOR_PACKAGE_ASSET_TAG" = "$expected_asset_tag" ] || { echo "::error::immutable package asset tag does not bind to its source commit" >&2; exit 1; }
manifest="$VELNOR_VERIFIED_PACKAGE_DIR/release-manifest.json"
identity="$VELNOR_VERIFIED_PACKAGE_DIR/identity.json"
test -s "$manifest"
test -s "$identity"
jq -e \
  --arg repository "$VELNOR_PACKAGE_SOURCE_REPOSITORY" \
  --arg source_ref "$VELNOR_PACKAGE_SOURCE_REF" \
  --arg commit "$VELNOR_PACKAGE_SOURCE_COMMIT" \
  --arg version "$VELNOR_PACKAGE_VERSION" \
  '.source_repository == $repository and .source_ref == $source_ref and
   .source_commit == $commit and .version == $version' "$manifest" >/dev/null
jq -e \
  --arg repository "$VELNOR_PACKAGE_SOURCE_REPOSITORY" \
  --arg source_ref "$VELNOR_PACKAGE_SOURCE_REF" \
  --arg commit "$VELNOR_PACKAGE_SOURCE_COMMIT" \
  --slurpfile package_manifest "$manifest" \
  'keys == ["manifest","source_digest","source_ref","source_repository"] and
   .source_repository == $repository and .source_ref == $source_ref and
   .source_digest == $commit and .manifest == $package_manifest[0]' "$identity" >/dev/null
"#
}

#[allow(clippy::too_many_lines)]
fn render_publication_lock_script(include_retention: bool) -> String {
    let mut script = String::from(
        r#"
# This permanent branch is the repository-wide lock namespace for generated
# publication writers. The Contents API create-without-sha operation is
# create-only: an existing lock is never taken over, including after a stale
# or abandoned run. Only the owner that still has the exact blob SHA may
# delete the lock file; the branch itself is intentionally retained.
publication_lock_branch="${VELNOR_PUBLICATION_LOCK_BRANCH:?missing VELNOR_PUBLICATION_LOCK_BRANCH}"
publication_lock_path=".package-release-publication-lock.json"
publication_lock_sha="${VELNOR_PUBLICATION_LOCK_SHA:-}"
publication_lock_acquired=0
publication_lock_released="${VELNOR_PUBLICATION_LOCK_RELEASED:-0}"
if [ -n "$publication_lock_sha" ]; then
  publication_lock_acquired=1
fi
publication_lock_token="${GITHUB_REPOSITORY}:${GITHUB_WORKFLOW:-unknown}:${GITHUB_RUN_ID:-unknown}:${GITHUB_RUN_ATTEMPT:-unknown}"

"#,
    );

    if include_retention {
        script.push_str(
            r#"
publication_lock_retain="${VELNOR_PUBLICATION_LOCK_RETAIN:-0}"

mark_publication_lock_retain() {
  publication_lock_retain=1
  if [ -n "${GITHUB_ENV:-}" ]; then
    printf 'VELNOR_PUBLICATION_LOCK_RETAIN=1\n' >> "$GITHUB_ENV"
  fi
}

clear_publication_lock_retain() {
  publication_lock_retain=0
  if [ -n "${GITHUB_ENV:-}" ]; then
    printf 'VELNOR_PUBLICATION_LOCK_RETAIN=0\n' >> "$GITHUB_ENV"
  fi
}

"#,
        );
    }

    script.push_str(
        r#"
mark_publication_lock_released() {
  publication_lock_released=1
  publication_lock_acquired=0
  publication_lock_sha=""
  if [ -n "${GITHUB_ENV:-}" ]; then
    printf 'VELNOR_PUBLICATION_LOCK_RELEASED=1\nVELNOR_PUBLICATION_LOCK_RETAIN=0\n' >> "$GITHUB_ENV"
  fi
}

publication_lock_http_status() {
  awk '/^HTTP\/[0-9.]+ [0-9]+/ {print $2; exit}' "$1"
}

ensure_publication_lock_branch() {
  local response="$transaction_dir/publication-lock-branch"
  local response_http
  if gh api --repo "$GITHUB_REPOSITORY" -i \
    "repos/$GITHUB_REPOSITORY/git/ref/heads/$publication_lock_branch" > "$response" 2>/dev/null; then
    response_http="$(publication_lock_http_status "$response")"
    if [ "$response_http" = 200 ]; then
      return 0
    fi
    return 1
  fi
  response_http="$(publication_lock_http_status "$response")"
  if [ "$response_http" != 404 ]; then
    return 1
  fi
  if ! gh api --method POST --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/git/refs" \
    -f "ref=refs/heads/$publication_lock_branch" -f "sha=$EXPECTED_SOURCE_COMMIT" >/dev/null; then
    return 1
  fi
  if ! gh api --repo "$GITHUB_REPOSITORY" -i \
    "repos/$GITHUB_REPOSITORY/git/ref/heads/$publication_lock_branch" > "$response" 2>/dev/null; then
    return 1
  fi
  response_http="$(publication_lock_http_status "$response")"
  if [ "$response_http" = 200 ]; then
    return 0
  fi
  return 1
}

acquire_publication_lock() {
  local payload="$transaction_dir/publication-lock.json"
  local encoded lock_response
  if ! ensure_publication_lock_branch; then
    return 1
  fi
  if ! jq -cn \
    --arg token "$publication_lock_token" \
    --arg repository "$GITHUB_REPOSITORY" \
    --arg run "${GITHUB_RUN_ID:-unknown}" \
    --arg attempt "${GITHUB_RUN_ATTEMPT:-unknown}" \
    '{schema: 1, token: $token, repository: $repository, run: $run, attempt: $attempt}' > "$payload"; then
    return 1
  fi
  if ! encoded="$(base64 < "$payload" | tr -d '\n')"; then
    return 1
  fi
  if ! lock_response="$(gh api --method PUT --repo "$GITHUB_REPOSITORY" \
    "repos/$GITHUB_REPOSITORY/contents/$publication_lock_path" \
    -f "message=Acquire Velnor publication lock $publication_lock_token" \
    -f "content=$encoded" -f "branch=$publication_lock_branch")"; then
    echo "::error::Velnor publication lock is held; refusing release validation and mutation" >&2
    return 1
  fi
  if ! publication_lock_sha="$(jq -er '.content.sha | strings | select(test("^[0-9a-f]{40}$"))' <<<"$lock_response")"; then
    echo "::error::Velnor publication lock response did not contain an exact lock SHA; manual recovery is required" >&2
    return 1
  fi
  publication_lock_acquired=1
}

assert_publication_lock() {
  local lock_response current_sha
  if [ "$publication_lock_acquired" != 1 ] || [ -z "$publication_lock_sha" ]; then
    return 1
  fi
  if ! lock_response="$(gh api --repo "$GITHUB_REPOSITORY" \
    "repos/$GITHUB_REPOSITORY/contents/$publication_lock_path?ref=$publication_lock_branch")"; then
    return 1
  fi
  if ! jq -e --arg path "$publication_lock_path" \
    '.type == "file" and .path == $path' <<<"$lock_response" >/dev/null; then
    return 1
  fi
  if ! current_sha="$(jq -er '.sha | strings | select(test("^[0-9a-f]{40}$"))' <<<"$lock_response")"; then
    return 1
  fi
  if [ "$current_sha" != "$publication_lock_sha" ]; then
    return 1
  fi
  return 0
}

release_publication_lock() {
  if [ "$publication_lock_acquired" = 0 ] || [ "$publication_lock_released" = 1 ]; then
    return 0
  fi
  if ! assert_publication_lock; then
    echo "::error::Velnor publication lock ownership changed; retaining lock for manual recovery" >&2
    return 1
  fi
  if ! gh api --method DELETE --repo "$GITHUB_REPOSITORY" \
    "repos/$GITHUB_REPOSITORY/contents/$publication_lock_path" \
    -f "message=Release Velnor publication lock $publication_lock_token" \
    -f "sha=$publication_lock_sha" -f "branch=$publication_lock_branch" >/dev/null; then
    echo "::error::Velnor publication lock release failed; retaining lock for manual recovery" >&2
    return 1
  fi
  mark_publication_lock_released
}
"#,
    );
    script
}

fn render_publication_lock_acquire_script() -> String {
    let mut script = String::from(
        r#"set -Eeuo pipefail
transaction_dir="$(mktemp -d)"
publication_lock_handoff=0
"#,
    );
    script.push_str(&render_publication_lock_script(false));
    script.push_str(
        r#"
cleanup_publication_lock_acquisition() {
  local status="$1"
  trap - EXIT
  if [ "$publication_lock_acquired" = 1 ] && [ "$publication_lock_handoff" = 0 ]; then
    if ! release_publication_lock; then
      status=1
    fi
  fi
  if ! rm -rf -- "$transaction_dir"; then
    status=1
  fi
  exit "$status"
}
trap 'cleanup_publication_lock_acquisition "$?"' EXIT

if [ "$publication_lock_acquired" = 0 ]; then
  if ! acquire_publication_lock; then
    echo "::error::Velnor publication lock could not be acquired; refusing publication" >&2
    exit 1
  fi
fi
if ! assert_publication_lock; then
  echo "::error::Velnor publication lock could not be verified; refusing publication" >&2
  exit 1
fi
if [ -z "${GITHUB_ENV:-}" ]; then
  echo "::error::GITHUB_ENV is unavailable; refusing publication without a lock handoff" >&2
  exit 1
fi
printf 'VELNOR_PUBLICATION_LOCK_SHA=%s\n' "$publication_lock_sha" >> "$GITHUB_ENV"
publication_lock_handoff=1
"#,
    );
    script
}

fn render_publication_lock_finalizer_script() -> String {
    let mut script = String::from(
        r#"set -Eeuo pipefail
transaction_dir="$(mktemp -d)"
"#,
    );
    script.push_str(&render_publication_lock_script(true));
    script.push_str(
        r#"
cleanup_publication_lock_finalizer() {
  local status="$1"
  trap - EXIT
  if ! rm -rf -- "$transaction_dir"; then
    status=1
  fi
  exit "$status"
}
trap 'cleanup_publication_lock_finalizer "$?"' EXIT

if [ "$publication_lock_acquired" = 0 ] || [ "$publication_lock_released" = 1 ]; then
  exit 0
fi
if [ "$publication_lock_retain" = 1 ]; then
  echo "::warning::Velnor publication lock retained for manual recovery after an uncertain remote mutation" >&2
  exit 0
fi
if ! release_publication_lock; then
  echo "::error::Velnor publication lock finalizer could not release its owned lock" >&2
  exit 1
fi
"#,
    );
    script
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
[[ "$EXPECTED_SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] || {
  echo "::error::rolling handoff requires a lowercase source commit identity" >&2
  exit 1
}
rolling_handoff_relative="$EXPECTED_SOURCE_COMMIT/rolling-published"
rolling_handoff_root="$transaction_dir/$EXPECTED_SOURCE_COMMIT"
rolling_published_dir="$rolling_handoff_root/rolling-published"
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
owner_draft=""
owner_tag_sha=""
owner_name=""
owner_body=""
owner_source_commit=""
owner_assets="$transaction_dir/owner-assets"
candidate_version="$(jq -er '.version | strings' "$published_dir/release-manifest.json")"
had_release=0
mutated=0
preexisting_rolling_tag=0
rolling_body_ready=0
"#,
    );
    script.push_str(&render_publication_lock_script(true));
    script.push_str(
        r#"
remote_tag_sha() {
  local tag_name="$1"
  local refs sha
  if ! refs="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name^{}")"; then
    return 1
  fi
  if ! sha="$(awk 'NR == 1 {print $1}' <<<"$refs")"; then
    return 1
  fi
  if [ -z "$sha" ]; then
    if ! refs="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name")"; then
      return 1
    fi
    if ! sha="$(awk 'NR == 1 {print $1}' <<<"$refs")"; then
      return 1
    fi
  fi
  if [ -n "$sha" ] && ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    return 1
  fi
  printf '%s\n' "$sha"
}

read_rolling_asset_set() {
  local output="$1"
  local asset_set
  if ! asset_set="$(gh api --paginate --repo "$GITHUB_REPOSITORY" \
    "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id/assets" \
    --jq '.[] | if (type == "object" and (.id | type == "number" and floor == . and . > 0) and (.name | type == "string" and length > 0 and test("^[-A-Za-z0-9._+~]+$")) and (.digest | type == "string" and test("^sha256:[0-9a-f]{64}$"))) then [.id, .name, .digest] | @tsv else error("invalid rolling release asset") end')"; then
    return 1
  fi
  if [ -n "$asset_set" ]; then
    if ! LC_ALL=C sort <<<"$asset_set" > "$output"; then
      return 1
    fi
  else
    if ! : > "$output"; then
      return 1
    fi
  fi
  if ! awk -F '\t' '
    NF == 3 && ++ids[$1] == 1 && ++names[$2] == 1 { next }
    { exit 1 }
  ' "$output"; then
    return 1
  fi
}

assert_asset_names() {
  local asset_set="$1"
  local expected_names="$2"
  local actual_names="$transaction_dir/asset-names"
  if ! cut -f2 "$asset_set" | LC_ALL=C sort > "$actual_names"; then
    return 1
  fi
  cmp -s "$expected_names" "$actual_names"
}

assert_release_absent() {
  local response="$transaction_dir/release-absence"
  local response_http
  if gh api --repo "$GITHUB_REPOSITORY" -i \
    "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" > "$response" 2>/dev/null; then
    echo "::error::rolling release still exists after DELETE" >&2
    return 1
  fi
  if ! response_http="$(awk '/^HTTP\/[0-9.]+ [0-9]+/ {print $2; exit}' "$response")"; then
    return 1
  fi
  if [ "$response_http" != 404 ]; then
    echo "::error::rolling release DELETE was not verified as HTTP 404" >&2
    return 1
  fi
}

resolve_existing_rolling_release() {
  local pages="$transaction_dir/rolling-releases"
  local matches="$transaction_dir/rolling-release-matches"
  if ! gh api --paginate --repo "$GITHUB_REPOSITORY" \
    "repos/$GITHUB_REPOSITORY/releases?per_page=100" > "$pages"; then
    return 1
  fi
  if ! jq -sr --arg tag "$rolling_tag" '
    [map(.[])[] | select((.tag_name | type == "string") and .tag_name == $tag)]
    | if length > 1 then error("multiple releases use the rolling tag")
      elif length == 1 then .[0].id
      else empty
      end
  ' "$pages" > "$matches"; then
    return 1
  fi
  if [ ! -s "$matches" ]; then
    return 2
  fi
  rolling_release_id="$(< "$matches")"
  if ! gh api --repo "$GITHUB_REPOSITORY" \
    "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" > "$rolling_body"; then
    return 1
  fi
  rolling_body_ready=1
}

validate_existing_rolling_release() {
  local body="$1"
  local source_dir="$2"
  local expected_draft="${3:-false}"
  local manifest="$source_dir/release-manifest.json"
  local identity="$source_dir/identity.json"
  local manifest_assets="$transaction_dir/live-manifest-assets"
  local downloaded_assets="$transaction_dir/live-downloaded-assets"
  local body_assets="$transaction_dir/body-assets"
  local manifest_names="$transaction_dir/manifest-names"
  local body_names="$transaction_dir/body-names"
  local manifest_digests="$transaction_dir/manifest-digests"
  local api_digests="$transaction_dir/api-digests"
  local name expected_digest actual_digest api_digest

  if ! jq -e --arg tag "$rolling_tag" --argjson prerelease "$RELEASE_PRERELEASE" --argjson draft "$expected_draft" '
    (.draft == $draft) and .prerelease == $prerelease and .tag_name == $tag and
    (.id | type == "number") and
    (.assets | type == "array" and length > 0) and
    ((.assets | map(.name)) as $names |
      ($names | unique | length) == ($names | length)) and
    (.assets | all(.[];
        type == "object" and
        (.id | type == "number" and floor == . and . > 0) and
        (.name | type == "string" and length > 0 and test("^[-A-Za-z0-9._+~]+$")) and
        (.digest | type == "string" and test("^sha256:[0-9a-f]{64}$"))))
  ' <<<"$body" >/dev/null; then
    return 1
  fi
  if ! test -s "$manifest" || ! test -s "$identity"; then
    return 1
  fi

  if ! jq -e '
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
  ' "$manifest" >/dev/null; then
    return 1
  fi

  if ! old_source_commit="$(jq -er '.source_commit | strings' "$manifest")"; then
    return 1
  fi
  if ! old_version="$(jq -er '.version | strings' "$manifest")"; then
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
  if ! jq -e \
    --arg schema "$EXPECTED_MANIFEST_SCHEMA" \
    --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
    --arg source_ref "$EXPECTED_SOURCE_REF" \
    --arg commit "$old_source_commit" \
    --slurpfile rolling_manifest "$manifest" \
    '.schema == $schema and .source_repository == $repository and
     .source_ref == $source_ref and .source_commit == $commit' "$manifest" >/dev/null; then
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
    return 1
  fi
  if ! jq -e --arg name "$RELEASE_TITLE_PREFIX $old_version" '.name == $name' <<<"$body" >/dev/null; then
    return 1
  fi

  if ! jq -r '(.assets[] | .name), (.supporting_assets[] | .name)' "$manifest" > "$manifest_names"; then
    return 1
  fi
  if ! {
    printf '%s\n' "release-manifest.json" "identity.json"
    cat "$manifest_names"
  } | LC_ALL=C sort > "$manifest_assets"; then
    return 1
  fi
  if ! jq -r '.assets[].name' <<<"$body" > "$body_names"; then
    return 1
  fi
  if ! LC_ALL=C sort "$body_names" > "$old_assets"; then
    return 1
  fi
  if ! find "$source_dir" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort > "$downloaded_assets"; then
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

  if ! jq -r '(.assets[] | [.name, .sha256] | @tsv), (.supporting_assets[] | [.name, .sha256] | @tsv)' "$manifest" > "$manifest_digests"; then
    return 1
  fi
  while IFS=$'\t' read -r name expected_digest; do
    if ! test -s "$source_dir/$name"; then
      return 1
    fi
    if ! actual_digest="$(sha256sum -- "$source_dir/$name" | awk '{print $1}')"; then
      return 1
    fi
    [ "$actual_digest" = "$expected_digest" ] || {
      echo "::error::existing rolling manifest digest mismatch: $name" >&2
      return 1
    }
  done < "$manifest_digests"
  if ! jq -r '.assets[] | [.name, .digest] | @tsv' <<<"$body" > "$api_digests"; then
    return 1
  fi
  while IFS=$'\t' read -r name api_digest; do
    if ! actual_digest="$(sha256sum -- "$source_dir/$name" | awk '{print $1}')"; then
      return 1
    fi
    [ "$api_digest" = "sha256:$actual_digest" ] || {
      echo "::error::existing rolling GitHub asset digest mismatch: $name" >&2
      return 1
    }
  done < "$api_digests"

  if ! read_rolling_asset_set "$body_assets"; then
    return 1
  fi
  if ! jq -r '.assets[] | [.id, .name, .digest] | @tsv' <<<"$body" | LC_ALL=C sort > "$body_assets.expected"; then
    return 1
  fi
  if ! cmp -s "$body_assets.expected" "$body_assets"; then
    echo "::error::existing rolling release asset set changed during validation" >&2
    return 1
  fi
  if ! cp -- "$body_assets" "$owner_assets"; then
    return 1
  fi

  if ! git -C source cat-file -e "${old_source_commit}^{commit}"; then
    echo "::error::existing rolling source commit is not present in the checked-out history" >&2
    return 1
  fi
  if ! git -C source merge-base --is-ancestor "$old_source_commit" "$EXPECTED_SOURCE_COMMIT"; then
    echo "::error::candidate source commit is not a descendant of the live rolling source" >&2
    return 1
  fi
}

assert_rolling_ownership() {
  local expected_draft="$1"
  local expected_tag_sha="$2"
  local expected_name="$3"
  local expected_body="$4"
  local expected_source_commit="$5"
  local current_body current_tag_sha current_assets
  if ! assert_publication_lock; then
    echo "::error::Velnor publication lock ownership changed; refusing release mutation" >&2
    return 1
  fi
  if [ -z "$rolling_release_id" ] || [ -z "$expected_tag_sha" ] || [ -z "$expected_source_commit" ] || ! test -f "$owner_assets"; then
    return 1
  fi
  if ! current_body="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id")"; then
    return 1
  fi
  jq -e \
    --arg id "$rolling_release_id" \
    --arg tag "$rolling_tag" \
    --arg name "$expected_name" \
    --arg body "$expected_body" \
    --argjson prerelease "$RELEASE_PRERELEASE" \
    --argjson draft "$expected_draft" \
    '(.id | tostring) == $id and .draft == $draft and .prerelease == $prerelease and
     .tag_name == $tag and .name == $name and (.body // "") == $body' <<<"$current_body" >/dev/null || return 1
  if ! current_tag_sha="$(remote_tag_sha "$rolling_tag")"; then
    return 1
  fi
  if [ "$current_tag_sha" != "$expected_tag_sha" ] || [ "$current_tag_sha" != "$expected_source_commit" ]; then
    return 1
  fi
  current_assets="$transaction_dir/current-assets"
  if ! read_rolling_asset_set "$current_assets"; then
    return 1
  fi
  cmp -s "$owner_assets" "$current_assets"
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

replace_owned_asset_after_delete() {
  local asset_id="$1"
  local asset_name="$2"
  local expected="$transaction_dir/expected-after-delete"
  local current="$transaction_dir/current-assets"
  if ! awk -F '\t' -v id="$asset_id" -v name="$asset_name" '
    $1 == id && $2 == name { removed++; next }
    { print }
    END { if (removed != 1) exit 1 }
  ' "$owner_assets" | LC_ALL=C sort > "$expected"; then
    return 1
  fi
  if ! read_rolling_asset_set "$current"; then
    return 1
  fi
  if ! cmp -s "$expected" "$current"; then
    echo "::error::rollback asset set drifted after DELETE" >&2
    return 1
  fi
  if ! mv -- "$current" "$owner_assets"; then
    return 1
  fi
}

replace_owned_asset_after_upload() {
  local asset_name="$1"
  local source_file="$2"
  local expected="$transaction_dir/expected-before-upload"
  local current="$transaction_dir/current-assets"
  local source_digest uploaded_digest uploaded_count
  if ! source_digest="$(sha256sum -- "$source_file" | awk '{print $1}')"; then
    return 1
  fi
  if ! awk -F '\t' -v name="$asset_name" '$2 != name { print }' "$owner_assets" | LC_ALL=C sort > "$expected"; then
    return 1
  fi
  if ! read_rolling_asset_set "$current"; then
    return 1
  fi
  if ! awk -F '\t' -v name="$asset_name" '$2 != name { print }' "$current" | LC_ALL=C sort | cmp -s - "$expected"; then
    echo "::error::rollback asset set drifted after upload" >&2
    return 1
  fi
  if ! uploaded_count="$(awk -F '\t' -v name="$asset_name" '$2 == name { count++; digest = $3 } END { if (count != 1) exit 1; print digest }' "$current")"; then
    return 1
  fi
  uploaded_digest="$uploaded_count"
  if [ "$uploaded_digest" != "sha256:$source_digest" ]; then
    echo "::error::rollback uploaded asset digest differs: $asset_name" >&2
    return 1
  fi
  if ! mv -- "$current" "$owner_assets"; then
    return 1
  fi
}

assert_asset_set_matches_files() {
  local asset_set="$1"
  local source_dir="$2"
  local expected_names="$3"
  local asset_id asset_name asset_digest expected_digest
  if ! assert_asset_names "$asset_set" "$expected_names"; then
    return 1
  fi
  while IFS=$'\t' read -r asset_id asset_name asset_digest; do
    if ! expected_digest="$(sha256sum -- "$source_dir/$asset_name" | awk '{print $1}')"; then
      return 1
    fi
    if [ "$asset_digest" != "sha256:$expected_digest" ]; then
      echo "::error::owned asset digest differs: $asset_name" >&2
      return 1
    fi
  done < "$asset_set"
}

adopt_owned_asset_set_from_files() {
  local source_dir="$1"
  local expected_names="$2"
  local refreshed="$transaction_dir/refreshed-assets"
  if ! read_rolling_asset_set "$refreshed"; then
    return 1
  fi
  if ! assert_asset_set_matches_files "$refreshed" "$source_dir" "$expected_names"; then
    return 1
  fi
  if ! mv -- "$refreshed" "$owner_assets"; then
    return 1
  fi
}

asset_was_in_old_set() (
  local asset_name="$1"
  local grep_status
  set +e
  grep -Fqx -- "$asset_name" "$old_assets"
  grep_status="$?"
  case "$grep_status" in
    0) exit 0 ;;
    1) exit 1 ;;
    *) exit 2 ;;
  esac
)

rollback() {
  local status="$1"
  local current_tag_sha
  trap - ERR
  if [ "$mutated" = 1 ]; then
    set +e
    rollback_status=0
    if [ -z "$rolling_release_id" ] || [ -z "$owner_draft" ] || [ -z "$owner_tag_sha" ]; then
      rollback_status=1
    elif ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit"; then
      echo "::error::rolling preview ownership changed; refusing rollback mutation" >&2
      rollback_status=1
    elif [ "$had_release" = 1 ]; then
      # Keep the rollback release hidden while restoring its complete old set.
      if ! gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" -F draft=true >/dev/null; then
        rollback_status=1
      else
        owner_draft=true
        if ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit"; then
          echo "::error::rolling preview ownership changed after hiding release; refusing rollback mutation" >&2
          rollback_status=1
        fi
        if [ "$rollback_status" -eq 0 ]; then
          while IFS=$'\t' read -r asset_id asset_name asset_digest; do
            if [ -z "$asset_id" ] || [ -z "$asset_name" ] || ! [[ "$asset_digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
              rollback_status=1
              break
            fi
            asset_status=0
            asset_was_in_old_set "$asset_name"
            asset_status="$?"
            case "$asset_status" in
              0) continue ;;
              1) ;;
              *)
                rollback_status=1
                break
                ;;
            esac
            if ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit" \
              || ! gh api --method DELETE --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/assets/$asset_id" >/dev/null \
              || ! replace_owned_asset_after_delete "$asset_id" "$asset_name"; then
              rollback_status=1
              break
            fi
          done < "$owner_assets"
        fi
        if [ "$rollback_status" -eq 0 ]; then
          while IFS= read -r asset_name; do
            if ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit" \
              || ! gh release upload "$rolling_tag" --repo "$GITHUB_REPOSITORY" --clobber "$rollback_dir/$asset_name" \
              || ! replace_owned_asset_after_upload "$asset_name" "$rollback_dir/$asset_name"; then
              rollback_status=1
              break
            fi
          done < "$old_assets"
        fi
        if [ "$rollback_status" -eq 0 ] && ! verify_restored_assets "$rollback_dir" "$old_assets"; then
          rollback_status=1
        fi
        if [ "$rollback_status" -eq 0 ] && ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit"; then
          echo "::error::rolling preview ownership changed before tag rollback; refusing force-move" >&2
          rollback_status=1
        fi
        if [ "$rollback_status" -eq 0 ]; then
          if ! current_tag_sha="$(remote_tag_sha "$rolling_tag")"; then
            rollback_status=1
          elif [ "$current_tag_sha" = "$old_tag_sha" ]; then
            :
          elif ! assert_publication_lock; then
            rollback_status=1
          elif [ "$current_tag_sha" = "$owner_tag_sha" ] && gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag" -f "sha=$old_tag_sha" -F force=true >/dev/null; then
            owner_tag_sha="$old_tag_sha"
          else
            echo "::error::rolling preview tag changed before rollback; refusing force-move" >&2
            rollback_status=1
          fi
        fi
        if [ "$rollback_status" -eq 0 ] && ! assert_rolling_ownership "$owner_draft" "$old_tag_sha" "$owner_name" "$owner_body" "$old_source_commit"; then
          echo "::error::rolling preview ownership changed before metadata rollback; refusing mutation" >&2
          rollback_status=1
        fi
        if [ "$rollback_status" -eq 0 ] && ! gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" \
          -f "name=$old_name" -f "body=$old_body" -F "draft=$old_draft" -F "prerelease=$old_prerelease" -F make_latest=false >/dev/null; then
          rollback_status=1
        fi
        if [ "$rollback_status" -eq 0 ]; then
          owner_draft="$old_draft"
          owner_tag_sha="$old_tag_sha"
          owner_name="$old_name"
          owner_body="$old_body"
          owner_source_commit="$old_source_commit"
          if ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit"; then
            rollback_status=1
          fi
        fi
      fi
    else
      if ! assert_publication_lock; then
        rollback_status=1
      elif ! gh api --method DELETE --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" >/dev/null; then
        rollback_status=1
      elif ! assert_release_absent; then
        rollback_status=1
      elif ! current_tag_sha="$(remote_tag_sha "$rolling_tag")"; then
        rollback_status=1
      elif [ "$preexisting_rolling_tag" = 1 ]; then
        if [ "$current_tag_sha" != "$owner_tag_sha" ] || [ "$current_tag_sha" != "$owner_source_commit" ]; then
          echo "::error::pre-existing rolling tag changed before rollback; refusing mutation" >&2
        else
          echo "::error::pre-existing rolling tag retained without a release; retaining publication lock for manual recovery" >&2
        fi
        rollback_status=1
      elif [ -z "$current_tag_sha" ]; then
        :
      else
        # The release-create API does not prove that this run created the tag.
        # A matching SHA is not ownership: another writer may have created the
        # tag after the preflight snapshot. Never delete such a tag.
        echo "::error::rolling preview tag retained without proven ownership; retaining publication lock for manual recovery" >&2
        rollback_status=1
      fi
    fi
    if [ "$rollback_status" -ne 0 ]; then
      echo "::error::rolling preview publication failed and rollback was incomplete" >&2
      publication_lock_retain=1
      status=1
    else
      echo "::warning::rolling preview publication failed; previous release restored" >&2
      clear_publication_lock_retain
    fi
  fi
  exit "$status"
}
cleanup_publication() {
  local status="$1"
  trap - EXIT
  if [ "$publication_lock_acquired" = 1 ] && [ "$publication_lock_retain" = 0 ]; then
    if ! release_publication_lock; then
      status=1
    fi
  fi
  if ! rm -rf -- "$transaction_dir"; then
    status=1
  fi
  exit "$status"
}
trap 'rollback "$?"' ERR
trap 'cleanup_publication "$?"' EXIT

if [ "$publication_lock_acquired" = 0 ]; then
  if ! acquire_publication_lock; then
    echo "::error::Velnor publication lock could not be acquired; refusing release validation and mutation" >&2
    exit 1
  fi
fi
if ! assert_publication_lock; then
  echo "::error::Velnor publication lock could not be verified; refusing release validation and mutation" >&2
  exit 1
fi
clear_publication_lock_retain

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
if ! jq -e --arg tag "$staged_tag" --argjson prerelease "$RELEASE_PRERELEASE" '(.draft | not) and .prerelease == $prerelease and .tag_name == $tag' <<<"$staged_body" >/dev/null; then
  echo "::error::verified immutable staging release identity is invalid" >&2
  exit 1
fi
if ! jq -r '.assets[].name' <<<"$staged_body" | LC_ALL=C sort > "$staged_assets"; then
  echo "::error::verified immutable staging release assets are invalid" >&2
  exit 1
fi
cmp -s "$expected_assets" "$staged_assets" || { echo "::error::immutable staging release asset set is not exact" >&2; exit 1; }
if ! staged_tag_sha="$(remote_tag_sha "$staged_tag")"; then
  echo "::error::immutable staging tag lookup failed" >&2
  exit 1
fi
[ "$staged_tag_sha" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::immutable staging tag does not resolve to the verified source commit" >&2; exit 1; }

if gh api --repo "$GITHUB_REPOSITORY" -i "repos/$GITHUB_REPOSITORY/releases/tags/$rolling_tag" > "$rolling_response" 2>/dev/null; then
  :
fi
rolling_http="$(awk 'NR == 1 {print $2; exit}' "$rolling_response")"
while :; do
case "$rolling_http" in
  200)
    if [ "$rolling_body_ready" != 1 ]; then
      awk 'body {print; next} /^\r?$/ {body = 1}' "$rolling_response" > "$rolling_body"
    fi
    had_release=1
    if ! jq -e --arg tag "$rolling_tag" --argjson prerelease "$RELEASE_PRERELEASE" \
      '(.draft | type == "boolean") and .prerelease == $prerelease and .tag_name == $tag and (.id | type == "number")' "$rolling_body" >/dev/null; then
      echo "::error::existing rolling release failed immutable validation; refusing mutation" >&2
      exit 1
    fi
    rolling_release_id="$(jq -er '.id' "$rolling_body")"
    old_name="$(jq -er '.name | strings' "$rolling_body")"
    old_body="$(jq -r '.body // ""' "$rolling_body")"
    old_draft="$(jq -er '.draft | tostring' "$rolling_body")"
    old_prerelease="$(jq -er '.prerelease | tostring' "$rolling_body")"
    if ! old_tag_sha="$(remote_tag_sha "$rolling_tag")"; then
      echo "::error::existing rolling tag lookup failed; refusing mutation" >&2
      exit 1
    fi
    if ! [[ "$old_tag_sha" =~ ^[0-9a-f]{40}$ ]]; then
      echo "::error::existing public rolling release failed immutable validation; refusing mutation" >&2
      exit 1
    elif ! mkdir -p "$rollback_dir" || ! gh release download "$rolling_tag" --repo "$GITHUB_REPOSITORY" --dir "$rollback_dir" --clobber; then
      echo "::error::existing rolling release failed immutable validation; refusing mutation" >&2
      exit 1
    fi
    if ! validate_existing_rolling_release "$rolling_body" "$rollback_dir" "$old_draft"; then
      echo "::error::existing rolling release failed immutable validation; refusing mutation" >&2
      exit 1
    fi
    # A validated current-contract draft is already a recoverable transaction:
    # keep its release, tag, metadata, and asset ledger so rollback can restore
    # the exact bytes. GitHub cannot undelete a release or tag.
    owner_draft="$old_draft"
    owner_tag_sha="$old_tag_sha"
    owner_name="$old_name"
    owner_body="$old_body"
    owner_source_commit="$old_source_commit"
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
    break
"#,
    );
    script.push_str(
        r#"
    ;;
  404)
    if resolve_existing_rolling_release; then
      rolling_http=200
      continue
    else
      rolling_lookup_status="$?"
    fi
    if [ "$rolling_lookup_status" -ne 2 ]; then
      echo "::error::rolling release listing failed; refusing mutation" >&2
      exit 1
    fi
    if ! rolling_tag_sha="$(remote_tag_sha "$rolling_tag")"; then
      echo "::error::rolling tag lookup failed; refusing to overwrite it" >&2
      exit 1
    fi
    if [ -n "$rolling_tag_sha" ]; then
      [ "$rolling_tag_sha" = "$EXPECTED_SOURCE_COMMIT" ] || {
        echo "::error::rolling tag exists without a release; refusing to overwrite it unless it resolves to the verified source commit" >&2
        exit 1
      }
      preexisting_rolling_tag=1
    fi
    break
    ;;
  *)
    echo "::error::rolling release preflight failed with HTTP $rolling_http" >&2
    exit 1
    ;;
esac
done

if [ "$had_release" = 0 ]; then
  mutated=1
  mark_publication_lock_retain
  create_json="$(gh api --method POST --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases" \
    -f "tag_name=$rolling_tag" -f "target_commitish=$EXPECTED_SOURCE_COMMIT" \
    -f "name=$RELEASE_TITLE_PREFIX $candidate_version" \
    -f "body=Verified package release from $EXPECTED_SOURCE_COMMIT" \
    -F draft=true -F "prerelease=$RELEASE_PRERELEASE" -F make_latest=false)"
  rolling_release_id="$(jq -er '.id' <<<"$create_json")"
  owner_draft=true
  owner_tag_sha="$EXPECTED_SOURCE_COMMIT"
  owner_name="$RELEASE_TITLE_PREFIX $candidate_version"
  owner_body="Verified package release from $EXPECTED_SOURCE_COMMIT"
  owner_source_commit="$EXPECTED_SOURCE_COMMIT"
  created_assets="$transaction_dir/created-assets"
  if ! read_rolling_asset_set "$created_assets"; then
    echo "::error::new rolling preview asset ownership could not be established; refusing mutation" >&2
    false
  fi
  if [ -s "$created_assets" ]; then
    echo "::error::new rolling preview unexpectedly contains assets; refusing mutation" >&2
    false
  fi
  if ! cp -- "$created_assets" "$owner_assets"; then
    echo "::error::new rolling preview asset ownership could not be recorded; refusing mutation" >&2
    false
  fi
  if ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit"; then
    echo "::error::new rolling preview ownership could not be established; refusing mutation" >&2
    false
  fi
else
  mutated=1
  mark_publication_lock_retain
  if ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit"; then
    echo "::error::rolling preview ownership changed before publication; refusing mutation" >&2
    false
  fi
  gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" -F draft=true >/dev/null
  owner_draft=true
  if ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit"; then
    echo "::error::rolling preview ownership changed after publication was hidden; refusing mutation" >&2
    false
  fi
fi

# The immutable source-bound release is the candidate staging record.  Copy its
# already-verified files only while the rolling release remains a draft; draft
# publication is the visibility boundary, so readers never see mixed assets.
if ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit"; then
  echo "::error::rolling preview ownership changed before asset publication; refusing mutation" >&2
  false
fi
gh release upload "$rolling_tag" --repo "$GITHUB_REPOSITORY" --clobber \
"#,
    );
    script.push_str(published_assets);
    script.push_str(
        r#"

rolling_stage_json="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id")"
if ! adopt_owned_asset_set_from_files "$published_dir" "$expected_assets"; then
  echo "::error::rolling staged asset set is not exactly the verified package" >&2
  false
fi
if ! jq -e --arg tag "$rolling_tag" --argjson prerelease "$RELEASE_PRERELEASE" '.draft == true and .prerelease == $prerelease and .tag_name == $tag' <<<"$rolling_stage_json" >/dev/null; then
  echo "::error::staged rolling release identity is invalid" >&2
  false
fi
if ! jq -r '.assets[].name' <<<"$rolling_stage_json" | LC_ALL=C sort > "$rolling_staged_assets"; then
  echo "::error::staged rolling release assets are invalid" >&2
  false
fi
if ! cmp -s "$expected_assets" "$rolling_staged_assets"; then
  echo "::error::staged rolling release asset set is not exact" >&2
  false
fi

if [ "$had_release" = 1 ]; then
  if ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit"; then
    echo "::error::rolling preview ownership changed before tag publication; refusing force-move" >&2
    false
  fi
  gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag" -f "sha=$EXPECTED_SOURCE_COMMIT" -F force=true >/dev/null
  owner_tag_sha="$EXPECTED_SOURCE_COMMIT"
  owner_source_commit="$EXPECTED_SOURCE_COMMIT"
  if ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit"; then
    echo "::error::rolling preview ownership changed after tag publication; refusing mutation" >&2
    false
  fi
fi
if ! assert_rolling_ownership "$owner_draft" "$owner_tag_sha" "$owner_name" "$owner_body" "$owner_source_commit"; then
  echo "::error::rolling preview ownership changed before release publication; refusing mutation" >&2
  false
fi
gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" \
  -f "name=$RELEASE_TITLE_PREFIX $candidate_version" \
  -f "body=Verified package release from $EXPECTED_SOURCE_COMMIT" \
  -F draft=false -F "prerelease=$RELEASE_PRERELEASE" -F make_latest=false >/dev/null
owner_draft=false
owner_name="$RELEASE_TITLE_PREFIX $candidate_version"
owner_body="Verified package release from $EXPECTED_SOURCE_COMMIT"
owner_source_commit="$EXPECTED_SOURCE_COMMIT"
if ! assert_rolling_ownership "$owner_draft" "$EXPECTED_SOURCE_COMMIT" "$owner_name" "$owner_body" "$owner_source_commit"; then
  echo "::error::rolling preview ownership changed after release publication; refusing mutation" >&2
  false
fi

if ! new_tag_sha="$(remote_tag_sha "$rolling_tag")"; then
  echo "::error::rolling tag lookup failed after publication" >&2
  false
fi
if [ "$new_tag_sha" != "$EXPECTED_SOURCE_COMMIT" ]; then
  echo "::error::rolling tag does not resolve to the verified source commit" >&2
  false
fi
rolling_post_json="$(gh api --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/tags/$rolling_tag")"
if ! jq -e --arg tag "$rolling_tag" --argjson prerelease "$RELEASE_PRERELEASE" '(.draft | not) and .prerelease == $prerelease and .tag_name == $tag' <<<"$rolling_post_json" >/dev/null; then
  echo "::error::rolling release identity is not exact after publication" >&2
  false
fi
if ! jq -r '.assets[].name' <<<"$rolling_post_json" | LC_ALL=C sort > "$rolling_published_assets"; then
  echo "::error::rolling release assets are invalid after publication" >&2
  false
fi
if ! cmp -s "$expected_assets" "$rolling_published_assets"; then
  echo "::error::rolling release asset set is not exact after publication" >&2
  false
fi
mkdir -- "$rolling_handoff_root"
mkdir -- "$rolling_published_dir"
gh release download "$rolling_tag" --repo "$GITHUB_REPOSITORY" --dir "$rolling_published_dir" --clobber
export VELNOR_VERIFIED_PACKAGE_DIR="$rolling_published_dir"
export VELNOR_PACKAGE_HANDOFF_ROOT="$transaction_dir"
export VELNOR_PACKAGE_HANDOFF_RELATIVE="$rolling_handoff_relative"
export VELNOR_PACKAGE_HANDOFF_SOURCE_COMMIT="$EXPECTED_SOURCE_COMMIT"
"#,
    );
    // Execute the verifier as a foreground Bash child. Its temporary-file
    // EXIT trap stays in the child, while the parent keeps its rollback and
    // finalizer traps active. The explicit `if` status boundary preserves
    // fail-fast behavior and the child's exact exit status on Bash 3.2.
    script.push_str("\nif bash -euo pipefail -c ");
    script.push_str(&shell_quote(verification.script));
    script.push_str(
        "; then\n  :\nelse\n  verification_status=\"$?\"\n  rollback \"$verification_status\"\nfi\n",
    );
    script.push_str("\nfor payload in \\\n");
    script.push_str(payload_names);
    script.push_str("do\n  gh attestation verify \"$rolling_published_dir/$payload\" ");
    script.push_str(verification.attestation_flags);
    script.push_str(
        "\ndone\nif ! assert_rolling_ownership \"$owner_draft\" \"$EXPECTED_SOURCE_COMMIT\" \"$owner_name\" \"$owner_body\" \"$owner_source_commit\"; then\n  echo \"::error::rolling preview ownership changed before lock release; refusing to release the publication lock\" >&2\n  false\nfi\n# Every rolling byte, attestation, and final ownership check is verified. The\n# lock is safe to release; failures before this point retain it for recovery.\nclear_publication_lock_retain\n# cleanup_publication releases the exact lock only after rollback or successful completion.\n",
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
  local refs sha
  if ! refs="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name^{}")"; then
    return 1
  fi
  if ! sha="$(awk 'NR == 1 {print $1}' <<<"$refs")"; then
    return 1
  fi
  if [ -z "$sha" ]; then
    if ! refs="$(git -C source -c "http.extraheader=AUTHORIZATION: bearer $GH_TOKEN" ls-remote origin "refs/tags/$tag_name")"; then
      return 1
    fi
    if ! sha="$(awk 'NR == 1 {print $1}' <<<"$refs")"; then
      return 1
    fi
  fi
  if [ -n "$sha" ] && ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
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

"#
}

// Keep the immutable publication transaction in one ordered script so its
// preflight, upload, and post-publication checks cannot be reordered by
// splitting the shell state across unrelated renderers.
#[allow(clippy::too_many_lines)]
fn render_immutable_publish_script(spec: &PackageReleaseSpec) -> String {
    let mut script = String::from(immutable_publish_script_prelude());
    script.push_str(&render_publication_lock_script(true));
    script.push_str(
        r#"
immutable_publication_mutated=0
immutable_publication_handoff=0
cleanup_immutable_publication() {
  local status="$1"
  trap - EXIT
  if [ "$publication_lock_acquired" = 1 ] && [ "$immutable_publication_mutated" = 0 ] && [ "$immutable_publication_handoff" = 0 ] && [ "$publication_lock_retain" = 0 ]; then
    if ! release_publication_lock; then
      status=1
    fi
  elif [ "$publication_lock_acquired" = 1 ] && [ "$immutable_publication_handoff" = 0 ]; then
    echo "::error::immutable publication failed after mutation; retaining publication lock for manual recovery" >&2
    status=1
  fi
  if ! rm -rf -- "$transaction_dir"; then
    status=1
  fi
  exit "$status"
}
trap 'cleanup_immutable_publication "$?"' EXIT

if [ "$publication_lock_acquired" = 0 ]; then
  if ! acquire_publication_lock; then
    echo "::error::Velnor publication lock could not be acquired; refusing immutable release validation and mutation" >&2
    exit 1
  fi
fi
if ! assert_publication_lock; then
  echo "::error::Velnor publication lock could not be verified; refusing immutable release validation and mutation" >&2
  exit 1
fi
if [ -z "${GITHUB_ENV:-}" ]; then
  echo "::error::GITHUB_ENV is unavailable; refusing to publish without the held lock handoff" >&2
  exit 1
fi
printf 'VELNOR_PUBLICATION_LOCK_SHA=%s\n' "$publication_lock_sha" >> "$GITHUB_ENV"

{
"#,
    );
    for name in release_asset_names(spec) {
        let _ = writeln!(script, "  printf '%s\\n' {}", shell_quote(&name));
    }
    script.push_str(
        r#"} | LC_ALL=C sort > "$expected_assets"

if ! tag_sha="$(remote_tag_sha "$tag")"; then
  echo "::error::immutable release tag lookup failed" >&2
  exit 1
fi
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
    if ! assert_publication_lock; then
      echo "::error::Velnor publication lock ownership changed before immutable release creation" >&2
      exit 1
    fi
    immutable_publication_mutated=1
    mark_publication_lock_retain
    if [ -n "$tag_sha" ]; then
      gh release create "$tag" --repo "$GITHUB_REPOSITORY" --verify-tag --draft "${release_flags[@]}" --title "$title" --notes "Verified immutable package release $version from $EXPECTED_SOURCE_COMMIT"
    else
      gh release create "$tag" --repo "$GITHUB_REPOSITORY" --target "$EXPECTED_SOURCE_COMMIT" --draft "${release_flags[@]}" --title "$title" --notes "Verified immutable package release $version from $EXPECTED_SOURCE_COMMIT"
    fi
    if ! tag_sha="$(remote_tag_sha "$tag")"; then
      echo "::error::new immutable release tag lookup failed" >&2
      exit 1
    fi
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
      if ! assert_publication_lock; then
        echo "::error::Velnor publication lock ownership changed before immutable asset upload" >&2
        exit 1
      fi
      immutable_publication_mutated=1
      mark_publication_lock_retain
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
  if ! assert_publication_lock; then
    echo "::error::Velnor publication lock ownership changed before immutable release publication" >&2
    exit 1
  fi
  immutable_publication_mutated=1
  mark_publication_lock_retain
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
if ! final_tag_sha="$(remote_tag_sha "$tag")"; then
  echo "::error::immutable release tag lookup failed after publication" >&2
  exit 1
fi
[ "$final_tag_sha" = "$EXPECTED_SOURCE_COMMIT" ] || { echo "::error::immutable tag does not resolve to the verified source commit" >&2; exit 1; }
printf 'immutable_tag=%s\n' "$tag" >> "$GITHUB_OUTPUT"
immutable_publication_handoff=1
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
    runtime_setup: &str,
    mise: &str,
    verify_tasks: &[String],
    pre_publish_tasks: &[String],
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
        let _ = writeln!(
            payload_names,
            "  {}{}",
            shell_quote_asset_name(name),
            suffix
        );
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
    let rolling_refresh = spec.refresh_rolling_release.then(|| {
        indent_script(
            &render_rolling_refresh_script(
                &published_assets,
                &expected_asset_names,
                &payload_names,
                &verification,
            ),
            10,
        )
    });
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
    output.push_str("\n      rolling_refresh_outcome: ");
    let rolling_refresh_outcome = if spec.refresh_rolling_release {
        github_expression("steps.rolling-refresh.outcome")
    } else {
        github_expression("'not-requested'")
    };
    output.push_str(&rolling_refresh_outcome);
    output.push_str("\n    env:\n      PACKAGE_DIR: package\n      VELNOR_VERIFIED_PACKAGE_DIR: ");
    output.push_str(workspace_expr);
    output.push_str("/package\n      VELNOR_PACKAGE_CHANNEL: ");
    output.push_str(channel_yaml);
    output.push_str("\n      VELNOR_PUBLICATION_LOCK_BRANCH: ");
    output.push_str(&crate::s2::yaml_scalar(&spec.publication_lock_branch));
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
    output.push_str("\n      UPDATE_COMMIT_MESSAGE: ");
    output.push_str(message_yaml);
    // This is the required repository-wide writer lock for every generated
    // publication job; all generated publication writers must share its group.
    // Keep cancellation disabled so a writer can finish rollback and release
    // the remote publication lease before the next run starts.
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

    output.push_str(runtime_setup);
    output.push_str("      - name: Set up Mise\n        uses: ");
    output.push_str(mise);
    output.push_str(
        "\n        with:\n          install: false\n      - name: Install locked package verification tools\n        working-directory: source\n        run: mise --yes install --locked --include-task-tools\n",
    );

    output.push_str(
        "      - name: Require empty package handoff destination\n        run: |\n          set -euo pipefail\n          destination=\"$GITHUB_WORKSPACE/package\"\n          if [[ -e \"$destination\" || -L \"$destination\" ]]; then echo \"::error::package handoff destination already exists\" >&2; exit 1; fi\n",
    );
    output.push_str("      - name: Download verified package handoff\n        uses: ");
    output.push_str(download);
    output.push_str("\n        with:\n          name: package-release\n          path: package\n          merge-multiple: true\n");
    output.push_str(
        "      - name: Re-verify downloaded handoff\n        id: verify\n        run: |\n",
    );
    output.push_str(publish_verify);
    output.push_str(&render_verification_task_step(
        "Run handoff package verification tasks",
        verify_tasks,
        None,
    ));

    output.push_str("      - name: Verify build attestations\n        env:\n          GH_TOKEN: ");
    output.push_str(github_token_expr);
    output.push_str("\n        run: |\n          set -euo pipefail\n          for payload in \\\n");
    output.push_str(publish_attestation_targets);
    output.push_str("          do\n            gh attestation verify \"$payload\" ");
    output.push_str(attestation_flags);
    output.push_str("\n          done\n");

    output.push_str(
        "      - name: Acquire package publication lock\n        env:\n          GH_TOKEN: ",
    );
    output.push_str(github_token_expr);
    output.push_str("\n        run: |\n");
    output.push_str(&indent_script(
        &render_publication_lock_acquire_script(),
        10,
    ));

    if !pre_publish_tasks.is_empty() {
        output.push_str(
            "      - name: Run pre-publish migration tasks\n        env:\n          GH_TOKEN: ",
        );
        output.push_str(github_token_expr);
        output.push_str("\n          VELNOR_SOURCE_CHECKOUT_DIR: ");
        output.push_str(workspace_expr);
        output.push_str(
            "/source\n        working-directory: source\n        run: |\n          set -euo pipefail\n",
        );
        for task in pre_publish_tasks {
            let _ = writeln!(output, "          mise run {}", shell_quote(task));
        }
    }

    output.push_str("      - name: Publish immutable source-bound release\n        id: publish\n        env:\n          GH_TOKEN: ");
    output.push_str(github_token_expr);
    output.push_str("\n        run: |\n");
    output.push_str(&indent_script(&render_immutable_publish_script(spec), 10));

    let immutable_tag_output = github_expression("steps.publish.outputs.immutable_tag");
    output.push_str("      - name: Download and re-verify published release\n        env:\n          GH_TOKEN: ");
    output.push_str(github_token_expr);
    output.push_str("\n          PACKAGE_DIR: published-package\n          RELEASE_ASSET_TAG: ");
    output.push_str(&immutable_tag_output);
    output.push_str(
        "\n        run: |\n          set -euo pipefail\n          published_dir=\"$GITHUB_WORKSPACE/published-package\"\n          if [[ -e \"$published_dir\" || -L \"$published_dir\" ]]; then echo \"::error::published package handoff destination already exists\" >&2; exit 1; fi\n          mkdir -- \"$published_dir\"\n          gh release download \"$RELEASE_ASSET_TAG\" --repo \"$GITHUB_REPOSITORY\" --dir \"$published_dir\"\n          export VELNOR_VERIFIED_PACKAGE_DIR=\"$published_dir\"\n",
    );
    output.push_str(publish_verify);
    output.push_str(&render_verification_task_step(
        "Run published package verification tasks",
        verify_tasks,
        Some(&format!("{workspace_expr}/published-package")),
    ));

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

    if !spec.refresh_rolling_release {
        output.push_str(
            "      - name: Release immutable-only publication lock after verification\n        if: success()\n        run: |\n          printf 'VELNOR_PUBLICATION_LOCK_RETAIN=0\\n' >> \"$GITHUB_ENV\"\n",
        );
    }

    if let Some(rolling_refresh) = rolling_refresh.as_deref() {
        output.push_str(
            "      - name: Refresh rolling preview release\n        id: rolling-refresh\n",
        );
        if spec.consumer_tag_mode == ConsumerTagMode::Immutable {
            output.push_str("        continue-on-error: true\n");
        }
        output.push_str("        env:\n          GH_TOKEN: ");
        output.push_str(github_token_expr);
        output.push_str("\n        run: |\n");
        output.push_str(rolling_refresh);
    }
    output.push_str(
        "      - name: Finalize package publication lock\n        if: ${{ always() }}\n        env:\n          GH_TOKEN: ",
    );
    output.push_str(github_token_expr);
    output.push_str("\n        run: |\n");
    output.push_str(&indent_script(
        &render_publication_lock_finalizer_script(),
        10,
    ));

    output.push_str(
        "      - name: Verify immutable consumer package identity\n        env:\n          VELNOR_PACKAGE_ASSET_TAG: ",
    );
    output.push_str(&immutable_tag_output);
    output.push_str("\n          VELNOR_PACKAGE_VERSION: ");
    output.push_str(&github_expression("steps.verify.outputs.version"));
    output.push_str("\n          VELNOR_PACKAGE_SOURCE_COMMIT: ");
    output.push_str(&github_expression("steps.verify.outputs.source_commit"));
    output.push_str("\n          VELNOR_PACKAGE_SOURCE_REPOSITORY: ");
    output.push_str(source_repository_yaml);
    output.push_str("\n          VELNOR_PACKAGE_SOURCE_REF: ");
    output.push_str(source_ref_yaml);
    output.push_str("\n          VELNOR_PACKAGE_CHANNEL: ");
    output.push_str(channel_yaml);
    output.push_str("\n          VELNOR_VERIFIED_PACKAGE_DIR: ");
    output.push_str(workspace_expr);
    output.push_str("/published-package\n        run: |\n");
    output.push_str(&indent_script(render_consumer_identity_check_script(), 10));

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
    output.push_str("\n          UPDATER_TOKEN: ");
    output.push_str(updater_token_expr);
    output.push_str("\n          VELNOR_PACKAGE_ASSET_TAG: ");
    output.push_str(&immutable_tag_output);
    output.push_str("\n          VELNOR_PACKAGE_VERSION: ");
    output.push_str(&github_expression("steps.verify.outputs.version"));
    output.push_str("\n          VELNOR_PACKAGE_SOURCE_COMMIT: ");
    output.push_str(&github_expression("steps.verify.outputs.source_commit"));
    output.push_str("\n          VELNOR_PACKAGE_SOURCE_REPOSITORY: ");
    output.push_str(source_repository_yaml);
    output.push_str("\n          VELNOR_PACKAGE_SOURCE_REF: ");
    output.push_str(source_ref_yaml);
    output.push_str("\n          VELNOR_PACKAGE_CHANNEL: ");
    output.push_str(channel_yaml);
    output.push_str("\n          VELNOR_VERIFIED_PACKAGE_DIR: ");
    output.push_str(workspace_expr);
    output.push_str("/published-package");
    if spec.consumer_tag_mode == ConsumerTagMode::Legacy {
        output.push_str("\n          VELNOR_PACKAGE_RELEASE_TAG: ");
        output.push_str(tag_yaml);
    }
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
          if ! remote_branch_refs="$(git -c "http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN" ls-remote origin "refs/heads/$automation_branch")"; then
            echo "::error::consumer automation branch lookup failed; refusing branch mutation" >&2
            exit 1
          fi
          if ! remote_branch_sha="$(awk 'NF >= 2 {print $1; exit}' <<<"$remote_branch_refs")"; then
            echo "::error::consumer automation branch response could not be parsed; refusing branch mutation" >&2
            exit 1
          fi
          if [ -n "$remote_branch_sha" ] && ! [[ "$remote_branch_sha" =~ ^[0-9a-f]{40}$ ]]; then
            echo "::error::consumer automation branch response contained an invalid object ID" >&2
            exit 1
          fi
          if [ -n "$remote_branch_sha" ]; then
            git -c "http.extraheader=AUTHORIZATION: bearer $UPDATER_TOKEN" fetch origin "refs/heads/$automation_branch:refs/remotes/origin/$automation_branch"
            git switch --detach "origin/$automation_branch"
          else
            git switch --create "$automation_branch" "origin/$CONSUMER_BRANCH"
          fi
"#,
    );
    if spec.consumer_tag_mode == ConsumerTagMode::Immutable {
        output.push_str("          unset RELEASE_TAG\n");
    }
    output.push_str(
        r#"          bash -c "$UPDATER"
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
verify_tasks = ["verify-preview-package"]
package_dir = "dist"
manifest_schema = "example.consumer-manifest-v1"
source_repository = "example/project"
source_ref = "refs/heads/main"
payloads = ["a.tar.gz", "b.tar.gz", "c.tar.gz", "d.tar.gz", "e.tar.gz", "f.tar.gz"]
supporting_assets = ["SHA256SUMS", "a.tar.gz.bundle", "capsule-manifest.json"]
channel = "preview"
release_tag = "preview"
publication_lock_branch = "package-release-lock"
github_release_type = "prerelease"
publish_environment = "github-preview"
consumer_repository = "example/tap"
consumer_branch = "main"
release_title_prefix = "Preview"
updater = "./scripts/package-update.sh"
updater_token_secret = "TAP_TOKEN"
update_commit_message = "chore: update verified preview"
concurrency_group = "package-release-preview"

[production_inputs]
application = ["crates/app/src/**", "Cargo.lock"]

[production_dependencies]
engine = ["crates/engine/**"]

[non_production_inputs]
documentation = ["README.md", "docs/**"]
contract_fixture = ["tests/contract-fixtures/**"]
"#,
        )
        .expect("fixture args")
    }

    #[test]
    fn legacy_release_declarations_without_input_tables_parse_render_and_admit_unknown_changes() {
        let mut values = args();
        for key in [
            "production_inputs",
            "production_dependencies",
            "non_production_inputs",
        ] {
            values.remove(key);
        }

        let spec = parse_spec(&Args(&values)).expect("legacy declaration remains valid");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        assert!(workflow.contains("name: Admit production release inputs"));

        let changes = [crate::s2::reuse::ChangedPath {
            path: "crates/velnorctl/src/new_module.rs".to_owned(),
            previous: None,
            status: crate::s2::reuse::ChangeKind::Added,
        }];
        let admission = evaluate_release_admission(
            &AdmissionEvent {
                repository: "example/project",
                event_ref: "refs/heads/main",
                configured_source_ref: "refs/heads/main",
                event_name: "push",
                before_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                head_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                head_tree: "cccccccccccccccccccccccccccccccccccccccc",
            },
            &spec.release_inputs,
            &changes,
        )
        .expect("legacy unknown path classification");
        assert_eq!(admission.disposition, "admit");
        assert!(admission.reason.contains("admitting conservatively"));
    }

    #[test]
    fn explicitly_empty_production_inputs_table_remains_invalid() {
        let mut values = args();
        values.insert(
            "production_inputs".to_owned(),
            toml::Value::Table(toml::map::Map::new()),
        );

        let error = parse_release_input_rules(&Args(&values))
            .expect_err("explicit empty production inputs must fail closed");
        assert!(error
            .to_string()
            .contains("must declare at least one named path group"));
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
default_branch = "main"

[workflow.selectors.github-hosted]
runs_on = ["ubuntu-24.04"]

[[declare]]
primitive = "package-release"
file = "preview.yml"

[declare.args]
build_tasks = ["release-preview-package"]
verify_tasks = ["verify-preview-package"]
pre_publish_tasks = ["migrate-preview-legacy"]
package_dir = "dist/package"
manifest_schema = "example.consumer-manifest-v1"
source_repository = "example/project"
source_ref = "refs/heads/main"
payloads = ["a.tar.gz", "b.tar.gz", "c.tar.gz", "d.tar.gz", "e.tar.gz", "f.tar.gz"]
supporting_assets = ["SHA256SUMS", "a.tar.gz.bundle", "capsule-manifest.json"]
channel = "preview"
release_tag = "preview"
publication_lock_branch = "package-release-lock"
github_release_type = "prerelease"
publish_environment = "github-preview"
release_title_prefix = "Preview"
consumer_repository = "example/tap"
consumer_branch = "main"
updater = "./scripts/package-update.sh"
updater_token_secret = "TAP_TOKEN"
update_commit_message = "chore: update verified preview"
concurrency_group = "package-release-preview"

[declare.args.production_inputs]
application = ["crates/app/src/**", "Cargo.lock"]

[declare.args.production_dependencies]
engine = ["crates/engine/**"]

[declare.args.non_production_inputs]
documentation = ["README.md", "docs/**"]
contract_fixture = ["tests/contract-fixtures/**"]
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
        assert_eq!(spec.verify_tasks, ["verify-preview-package"]);
        assert_eq!(spec.pre_publish_tasks, ["migrate-preview-legacy"]);
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
        assert!(workflow.contains("(.draft == $draft) and .prerelease == $prerelease"));
        assert!(workflow.contains("(.draft | type == \"boolean\") and .prerelease == $prerelease"));
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
        assert!(workflow.contains(
            "existing public rolling release failed immutable validation; refusing mutation"
        ));
        assert!(workflow.contains("assert_rolling_ownership"));
    }

    #[test]
    fn rendered_workflow_refuses_invalid_rolling_release_before_mutation() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        assert!(!workflow.contains("discard_current_typed_rolling_draft"));
        assert!(!workflow.contains("typed rolling draft ownership changed; refusing cleanup"));
        assert!(workflow.contains(
            "existing public rolling release failed immutable validation; refusing mutation"
        ));
        assert!(!workflow.contains("discard_stale_rolling_draft"));
        assert!(!workflow.contains("LEGACY_ROLLING"));
        let refusal = workflow
            .find("existing public rolling release failed immutable validation; refusing mutation")
            .expect("invalid rolling release refusal");
        let first_mutation = workflow
            .find("if [ \"$had_release\" = 0 ]; then")
            .expect("rolling publication mutation boundary");
        assert!(refusal < first_mutation);
    }

    #[test]
    fn rolling_tag_without_release_accepts_only_the_expected_source_commit() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        assert!(workflow.contains("preexisting_rolling_tag=0"));
        let expected_target = workflow
            .find("[ \"$rolling_tag_sha\" = \"$EXPECTED_SOURCE_COMMIT\" ]")
            .expect("expected-source tag guard");
        let preexisting_marker = workflow
            .find("preexisting_rolling_tag=1")
            .expect("pre-existing tag marker");
        let refusal = workflow
            .find("rolling tag exists without a release; refusing to overwrite it unless it resolves to the verified source commit")
            .expect("unexpected tag refusal");
        let first_mutation = workflow
            .find("if [ \"$had_release\" = 0 ]; then")
            .expect("rolling publication mutation boundary");
        assert!(expected_target < preexisting_marker);
        assert!(expected_target < refusal);
        assert!(refusal < first_mutation);
    }

    #[cfg(unix)]
    #[test]
    fn rolling_tag_without_release_preflight_requires_expected_commit() {
        use std::process::Command;

        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script("", "", "", &verification);
        let preflight_start = rolling_script
            .find("while :; do\ncase \"$rolling_http\" in")
            .expect("rolling preflight");
        let preflight_end = rolling_script[preflight_start..]
            .find("\n\nif [ \"$had_release\" = 0 ]; then")
            .map(|offset| preflight_start + offset)
            .expect("rolling mutation boundary");
        let preflight = &rolling_script[preflight_start..preflight_end];
        let run_preflight = |tag_sha: &str| {
            let script = format!(
                r#"set -Eeuo pipefail
rolling_http=404
rolling_tag=preview
rolling_tag_sha=
preexisting_rolling_tag=0
resolve_existing_rolling_release() {{ return 2; }}
remote_tag_sha() {{ printf '%s\n' "$TEST_TAG_SHA"; }}
{preflight}
printf 'marker=%s\n' "$preexisting_rolling_tag"
"#
            );
            Command::new("bash")
                .arg("-c")
                .arg(script)
                .env(
                    "EXPECTED_SOURCE_COMMIT",
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                )
                .env("TEST_TAG_SHA", tag_sha)
                .output()
                .expect("run rolling preflight")
        };

        let accepted = run_preflight("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert!(
            accepted.status.success(),
            "expected source tag was rejected:\n{}{}",
            String::from_utf8_lossy(&accepted.stdout),
            String::from_utf8_lossy(&accepted.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&accepted.stdout).trim(), "marker=1");

        let rejected = run_preflight("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(!rejected.status.success(), "mismatched tag was accepted");
        assert!(String::from_utf8_lossy(&rejected.stderr)
            .contains("unless it resolves to the verified source commit"));
    }

    #[cfg(unix)]
    #[test]
    fn draft_rolling_release_lookup_uses_listing_before_orphan_creation() {
        use std::process::Command;

        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        let lookup_start = workflow
            .find("resolve_existing_rolling_release() {")
            .expect("draft release lookup helper");
        let validation_start = workflow[lookup_start..]
            .find("validate_existing_rolling_release")
            .expect("existing release validator");
        let lookup_end = lookup_start + validation_start - 2;
        let lookup = &workflow[lookup_start..lookup_end];
        let root = std::env::temp_dir().join(format!(
            "velnor-package-draft-lookup-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create draft lookup fixture");
        let script = format!(
            r#"set -Eeuo pipefail
GITHUB_REPOSITORY=example/project
rolling_tag=preview
transaction_dir="$TEST_TMPDIR"
rolling_body="$transaction_dir/rolling.json"
rolling_release_id=""
rolling_body_ready=0
gh() {{
  case "$*" in
    *"releases?per_page=100"*)
      printf '%s\n' '[{{"id":123,"tag_name":"preview","draft":true}}]'
      ;;
    *"releases/123"*)
      printf '%s\n' '{{"id":123,"tag_name":"preview","draft":true}}'
      ;;
    *) return 1 ;;
  esac
}}
{lookup}
resolve_existing_rolling_release
test "$rolling_release_id" = 123
test "$rolling_body_ready" = 1
jq -e '.id == 123 and .tag_name == "preview" and .draft == true' "$rolling_body" >/dev/null
"#
        );
        let output = Command::new("bash")
            .arg("-c")
            .arg(script)
            .env("TEST_TMPDIR", &root)
            .output()
            .expect("run draft release lookup fixture");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "draft release lookup did not resolve the existing release:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    struct RollingPreflightCase<'a> {
        listing: &'a str,
        listing_status: &'a str,
        detail: &'a str,
        validate_status: &'a str,
    }

    fn rolling_preflight_script(lookup: &str, preflight: &str) -> String {
        format!(
            r#"set -Eeuo pipefail
GITHUB_REPOSITORY=example/project
rolling_tag=preview
rolling_response="$TEST_TMPDIR/rolling-response"
rolling_body="$TEST_TMPDIR/rolling.json"
transaction_dir="$TEST_TMPDIR"
rollback_dir="$TEST_TMPDIR/rollback"
rolling_release_id=""
rolling_http=404
rolling_body_ready=0
rolling_tag_sha=""
had_release=0
mutated=0
RELEASE_PRERELEASE=true
EXPECTED_SOURCE_COMMIT=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
candidate_version=1.0.0
old_version=1.0.0
old_source_commit=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
validate_existing_rolling_release() {{ return "$TEST_VALIDATE_STATUS"; }}
remote_tag_sha() {{ printf '%s\n' "$TEST_TAG_SHA"; }}
gh() {{
  case "$*" in
    *"releases?per_page=100"*)
      [ "$TEST_LISTING_STATUS" = 0 ] || return "$TEST_LISTING_STATUS"
      printf '%s\n' "$TEST_LISTING"
      ;;
    *"releases/123"*)
      [ "$TEST_DETAIL_STATUS" = 0 ] || return "$TEST_DETAIL_STATUS"
      printf '%s\n' "$TEST_DETAIL"
      ;;
    *"release download"*)
      return 0
      ;;
    *"--method POST"*|*"--method PATCH"*|*"--method DELETE"*)
      : > "$TEST_MUTATION"
      return 0
      ;;
    *) return 1 ;;
  esac
}}
{lookup}
{preflight}
test "$rolling_release_id" = 123
test "$had_release" = 1
test "$rolling_body_ready" = 1
jq -e '.id == 123 and .tag_name == "preview" and .draft == true and .prerelease == true' "$rolling_body" >/dev/null
test ! -e "$TEST_MUTATION"
"#,
        )
    }

    fn run_rolling_preflight_case(
        root: &std::path::Path,
        lookup: &str,
        preflight: &str,
        case: &RollingPreflightCase<'_>,
    ) -> std::process::Output {
        let script = rolling_preflight_script(lookup, preflight);
        std::process::Command::new("bash")
            .arg("-c")
            .arg(script)
            .env("TEST_TMPDIR", root)
            .env("TEST_LISTING", case.listing)
            .env("TEST_LISTING_STATUS", case.listing_status)
            .env("TEST_DETAIL", case.detail)
            .env("TEST_DETAIL_STATUS", "0")
            .env("TEST_VALIDATE_STATUS", case.validate_status)
            .env("TEST_TAG_SHA", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .env("TEST_MUTATION", root.join("mutation"))
            .output()
            .expect("run rolling preflight fixture")
    }

    fn output_text(output: &std::process::Output) -> String {
        format!(
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    fn assert_no_mutation(root: &std::path::Path) {
        assert!(
            !root.join("mutation").exists(),
            "preflight mutated publication state"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rolling_preflight_preserves_listed_draft_and_fails_before_mutation() {
        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script("", "", "", &verification);
        let lookup_start = rolling_script
            .find("resolve_existing_rolling_release() {")
            .expect("draft release lookup helper");
        let validation_start = rolling_script[lookup_start..]
            .find("validate_existing_rolling_release")
            .expect("existing release validator");
        let lookup_end = lookup_start + validation_start - 2;
        let lookup = &rolling_script[lookup_start..lookup_end];
        let preflight_start = rolling_script
            .find("while :; do\ncase \"$rolling_http\" in")
            .expect("rolling preflight");
        let preflight_end = rolling_script[preflight_start..]
            .find("\n\nif [ \"$had_release\" = 0 ]; then")
            .map(|offset| preflight_start + offset)
            .expect("rolling mutation boundary");
        let preflight = &rolling_script[preflight_start..preflight_end];
        let root = std::env::temp_dir().join(format!(
            "velnor-package-rolling-preflight-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create preflight fixture");
        let valid = run_rolling_preflight_case(
            &root,
            lookup,
            preflight,
            &RollingPreflightCase {
                listing: r#"[{"id":123,"tag_name":"preview"}]"#,
                listing_status: "0",
                detail: r#"{"id":123,"tag_name":"preview","draft":true,"prerelease":true,"name":"old","body":"old"}"#,
                validate_status: "0",
            },
        );
        assert!(
            valid.status.success(),
            "valid listed draft failed: {}",
            output_text(&valid)
        );

        let invalid = run_rolling_preflight_case(
            &root,
            lookup,
            preflight,
            &RollingPreflightCase {
                listing: r#"[{"id":123,"tag_name":"preview"}]"#,
                listing_status: "0",
                detail: r#"{"id":123,"tag_name":"preview","draft":true,"prerelease":true,"name":"old","body":"old"}"#,
                validate_status: "1",
            },
        );
        assert!(
            !invalid.status.success(),
            "invalid draft validation was accepted"
        );
        assert_no_mutation(&root);

        let multiple = run_rolling_preflight_case(
            &root,
            lookup,
            preflight,
            &RollingPreflightCase {
                listing: r#"[{"id":123,"tag_name":"preview"},{"id":124,"tag_name":"preview"}]"#,
                listing_status: "0",
                detail: "",
                validate_status: "0",
            },
        );
        assert!(!multiple.status.success(), "ambiguous listing was accepted");
        assert_no_mutation(&root);

        let failed_listing = run_rolling_preflight_case(
            &root,
            lookup,
            preflight,
            &RollingPreflightCase {
                listing: "",
                listing_status: "1",
                detail: "",
                validate_status: "0",
            },
        );
        assert!(
            !failed_listing.status.success(),
            "listing failure was accepted"
        );
        assert_no_mutation(&root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn rollback_retains_a_preexisting_rolling_tag_and_publication_lock() {
        use std::process::Command;

        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script("", "", "", &verification);
        let rollback_start = rolling_script
            .find("rollback() {")
            .expect("rollback helper");
        let rollback_end = rolling_script
            .find("\ncleanup_publication() {")
            .expect("cleanup helper");
        let rollback = rolling_script[rollback_start..rollback_end]
            .replace("\n  exit \"$status\"\n}", "\n  return \"$status\"\n}");
        let root = std::env::temp_dir().join(format!(
            "velnor-package-preexisting-tag-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create rollback fixture");
        let script = format!(
            r#"set -Eeuo pipefail
GITHUB_REPOSITORY=example/project
rolling_tag=preview
rolling_release_id=123
owner_draft=true
owner_tag_sha=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
owner_name=old-name
owner_body=old-body
owner_source_commit=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
preexisting_rolling_tag=1
had_release=0
mutated=1
publication_lock_retain=0
assert_rolling_ownership() {{ return 0; }}
assert_publication_lock() {{ return 0; }}
assert_release_absent() {{ return 0; }}
remote_tag_sha() {{ printf '%s\n' "$owner_tag_sha"; }}
gh() {{
  if [[ "$*" == *"releases/123"* ]] && [[ "$*" == *"--method DELETE"* ]]; then
    : > "$TEST_TMPDIR/release-deleted"
    return 0
  fi
  if [[ "$*" == *"git/refs/tags/preview"* ]] && [[ "$*" == *"--method DELETE"* ]]; then
    : > "$TEST_TMPDIR/tag-deleted"
    return 0
  fi
  return 0
}}
{rollback}
set +e
rollback 1
rollback_status=$?
set -e
test "$rollback_status" -eq 1
test -e "$TEST_TMPDIR/release-deleted"
test ! -e "$TEST_TMPDIR/tag-deleted"
test "$publication_lock_retain" -eq 1
"#
        );
        let output = Command::new("bash")
            .arg("-c")
            .arg(script)
            .env("TEST_TMPDIR", &root)
            .output()
            .expect("run pre-existing tag rollback fixture");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "pre-existing tag rollback was unsafe:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn rollback_does_not_delete_tag_created_after_preflight_at_expected_commit() {
        use std::process::Command;

        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script("", "", "", &verification);
        let rollback_start = rolling_script
            .find("rollback() {")
            .expect("rollback helper");
        let rollback_end = rolling_script
            .find("\ncleanup_publication() {")
            .expect("cleanup helper");
        let rollback = rolling_script[rollback_start..rollback_end]
            .replace("\n  exit \"$status\"\n}", "\n  return \"$status\"\n}");
        let root = std::env::temp_dir().join(format!(
            "velnor-package-concurrent-tag-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create rollback fixture");
        let script = format!(
            r#"set -Eeuo pipefail
GITHUB_REPOSITORY=example/project
rolling_tag=preview
rolling_release_id=123
owner_draft=true
owner_tag_sha=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
owner_name=candidate-name
owner_body=candidate-body
owner_source_commit=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
# The preflight saw no tag. An external writer creates the matching tag before rollback.
preexisting_rolling_tag=0
had_release=0
mutated=1
publication_lock_retain=0
assert_rolling_ownership() {{ return 0; }}
assert_publication_lock() {{ return 0; }}
assert_release_absent() {{ return 0; }}
clear_publication_lock_retain() {{ publication_lock_retain=0; }}
: > "$TEST_TMPDIR/external-tag-created-after-preflight"
remote_tag_sha() {{
  if [ -e "$TEST_TMPDIR/tag-deleted" ]; then
    printf '\n'
  else
    test -e "$TEST_TMPDIR/external-tag-created-after-preflight"
    printf '%s\n' "$owner_tag_sha"
  fi
}}
gh() {{
  if [[ "$*" == *"releases/123"* ]] && [[ "$*" == *"--method DELETE"* ]]; then
    : > "$TEST_TMPDIR/release-deleted"
    return 0
  fi
  if [[ "$*" == *"git/refs/tags/preview"* ]] && [[ "$*" == *"--method DELETE"* ]]; then
    : > "$TEST_TMPDIR/tag-deleted"
    return 0
  fi
  return 0
}}
{rollback}
set +e
rollback 1
rollback_status=$?
set -e
test "$rollback_status" -eq 1
test -e "$TEST_TMPDIR/release-deleted"
if [ -e "$TEST_TMPDIR/tag-deleted" ]; then
  echo "rollback deleted the external writer tag" >&2
  exit 1
fi
test "$publication_lock_retain" -eq 1
"#
        );
        let output = Command::new("bash")
            .arg("-c")
            .arg(script)
            .env("TEST_TMPDIR", &root)
            .output()
            .expect("run concurrent tag rollback fixture");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "concurrent tag rollback was unsafe:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn package_release_schema_has_no_legacy_compatibility_fields() {
        assert!(PackageRelease
            .schema()
            .iter()
            .all(|field| !field.starts_with("legacy_")));
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
            rust_pin: None,
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
            reviewers: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
            mise_lock_backends: BTreeMap::new(),
            mise_install_deps: crate::s2::config::MiseInstallDeps::default(),
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
    fn package_release_rejects_empty_verification_tasks() {
        let mut values = args();
        values.insert("verify_tasks".to_owned(), toml::Value::Array(Vec::new()));
        let error = parse_spec(&Args(&values)).expect_err("empty verification tasks must fail");
        assert!(error.to_string().contains("verify_tasks"));
    }

    #[test]
    fn package_release_rejects_non_task_verification_names() {
        let mut values = args();
        values.insert(
            "verify_tasks".to_owned(),
            toml::Value::Array(vec![toml::Value::String("mise run verify".to_owned())]),
        );
        let error = parse_spec(&Args(&values)).expect_err("command arrays must fail");
        assert!(error.to_string().contains("plain mise task"));
    }

    #[test]
    fn package_release_rejects_empty_or_command_pre_publish_tasks() {
        let mut empty = args();
        empty.insert(
            "pre_publish_tasks".to_owned(),
            toml::Value::Array(Vec::new()),
        );
        let error = parse_spec(&Args(&empty)).expect_err("empty pre-publish tasks must fail");
        assert!(error.to_string().contains("pre_publish_tasks"));

        let mut command = args();
        command.insert(
            "pre_publish_tasks".to_owned(),
            toml::Value::Array(vec![toml::Value::String(
                "mise run migrate-preview-legacy".to_owned(),
            )]),
        );
        let error = parse_spec(&Args(&command)).expect_err("command arrays must fail");
        assert!(error.to_string().contains("plain mise task"));
    }

    #[test]
    fn package_release_rejects_duplicate_or_overlapping_pre_publish_tasks() {
        let mut duplicate = args();
        duplicate.insert(
            "pre_publish_tasks".to_owned(),
            toml::Value::Array(vec![
                toml::Value::String("migrate-preview-legacy".to_owned()),
                toml::Value::String("migrate-preview-legacy".to_owned()),
            ]),
        );
        let error = parse_spec(&Args(&duplicate)).expect_err("duplicates must fail");
        assert!(error.to_string().contains("duplicate entry"));

        let mut overlap = args();
        overlap.insert(
            "pre_publish_tasks".to_owned(),
            toml::Value::Array(vec![toml::Value::String(
                "verify-preview-package".to_owned(),
            )]),
        );
        let error = parse_spec(&Args(&overlap)).expect_err("verification overlap must fail");
        assert!(error.to_string().contains("overlaps verify_tasks"));
    }

    #[test]
    fn package_release_verification_tasks_must_exist_in_mise() {
        let root = std::env::temp_dir().join(format!(
            "velnor-package-release-mise-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create mise fixture");
        std::fs::write(
            root.join("mise.toml"),
            "[tasks]\nverify-preview-package = \"echo verify\"\nmigrate-preview-legacy = \"echo migrate\"\n\n[tasks.header-task]\nrun = \"echo header\"\n",
        )
        .expect("write mise fixture");
        let tasks = vec![
            "verify-preview-package".to_owned(),
            "header-task".to_owned(),
        ];
        validate_mise_tasks(&root, "verify_tasks", &tasks).expect("declared task validates");
        validate_mise_tasks(
            &root,
            "pre_publish_tasks",
            &["migrate-preview-legacy".to_owned()],
        )
        .expect("declared pre-publish task validates");
        let missing = vec!["missing-task".to_owned()];
        let error = validate_mise_tasks(&root, "verify_tasks", &missing)
            .expect_err("missing task must fail closed");
        assert!(error.to_string().contains("missing-task"));
        let missing_pre_publish = vec!["missing-migration".to_owned()];
        let error = validate_mise_tasks(&root, "pre_publish_tasks", &missing_pre_publish)
            .expect_err("missing pre-publish task must fail closed");
        assert!(error.to_string().contains("missing-migration"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn publication_lock_renderer_scopes_retention_helpers_to_mutating_fragments() {
        let acquire = render_publication_lock_acquire_script();
        assert!(acquire.contains("mark_publication_lock_released()"));
        assert!(!acquire.contains("publication_lock_retain="));
        assert!(!acquire.contains("mark_publication_lock_retain()"));
        assert!(!acquire.contains("clear_publication_lock_retain()"));

        let finalizer = render_publication_lock_finalizer_script();
        assert!(finalizer.contains("publication_lock_retain="));
        assert!(finalizer.contains("mark_publication_lock_retain()"));
        assert!(finalizer.contains("clear_publication_lock_retain()"));
    }

    #[test]
    fn immutable_publication_scopes_asset_group_after_all_helper_definitions() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let script = render_immutable_publish_script(&spec);
        let group_start = script
            .find("\n{\n  printf '%s\\n' 'release-manifest.json'\n")
            .expect("expected asset group");
        let group_end = script
            .find("\n} | LC_ALL=C sort > \"$expected_assets\"")
            .expect("expected asset group terminator");
        let verify_helper = script
            .find("verify_asset_bytes()")
            .expect("verify asset helper");
        let lock_helper = script
            .find("publication_lock_branch=")
            .expect("publication lock initialization");
        let handoff = script
            .find("printf 'VELNOR_PUBLICATION_LOCK_SHA=")
            .expect("publication lock handoff");
        assert!(verify_helper < group_start);
        assert!(lock_helper < group_start);
        assert!(handoff < group_start);

        let asset_group = &script[group_start..group_end];
        assert_eq!(
            asset_group.matches("printf '%s\\n'").count(),
            release_asset_names(&spec).len()
        );
        assert!(!asset_group.contains("() {"));
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
    #[allow(clippy::too_many_lines)]
    fn publication_lock_finalizer_releases_pre_publish_failure_for_next_run() {
        use std::process::Command;

        let finalizer = render_publication_lock_finalizer_script();
        let acquire = render_publication_lock_acquire_script();
        let lock_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let root = std::env::temp_dir().join(format!(
            "velnor-publication-lock-finalizer-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create finalizer fixture");
        std::fs::write(root.join("lock-state"), b"held").expect("seed held lock");
        let finalizer_script = format!(
            r#"set -Eeuo pipefail
gh() {{
  printf '%s\n' "$*" >> "$TEST_TMPDIR/gh.log"
  if [[ "$*" == *"contents/.package-release-publication-lock.json?ref=package-release-lock"* ]]; then
    if [ ! -e "$TEST_TMPDIR/lock-state" ]; then return 1; fi
    printf '{{"type":"file","path":".package-release-publication-lock.json","sha":"{lock_sha}"}}\n'
    return 0
  fi
  if [[ "$*" == *"--method DELETE"* ]]; then
    rm -f -- "$TEST_TMPDIR/lock-state"
    : > "$TEST_TMPDIR/released"
    return 0
  fi
  return 1
}}
{finalizer}
"#,
        );
        let env_file = root.join("github-env");
        let output = Command::new("bash")
            .arg("-c")
            .arg(&finalizer_script)
            .env("TEST_TMPDIR", &root)
            .env("GITHUB_REPOSITORY", "example/project")
            .env("GITHUB_WORKFLOW", "preview")
            .env("GITHUB_RUN_ID", "42")
            .env("GITHUB_RUN_ATTEMPT", "3")
            .env("VELNOR_PUBLICATION_LOCK_BRANCH", "package-release-lock")
            .env("VELNOR_PUBLICATION_LOCK_SHA", lock_sha)
            .env("GITHUB_ENV", &env_file)
            .env("GH_TOKEN", "test-token")
            .output()
            .expect("run publication lock finalizer");
        assert!(
            output.status.success(),
            "finalizer failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(root.join("released").exists());
        assert!(!root.join("lock-state").exists());
        assert!(std::fs::read_to_string(&env_file)
            .expect("read finalizer environment")
            .contains("VELNOR_PUBLICATION_LOCK_RELEASED=1"));

        let next_env = root.join("next-github-env");
        let acquire_script = format!(
            r#"set -Eeuo pipefail
gh() {{
  printf '%s\n' "$*" >> "$TEST_TMPDIR/next-gh.log"
  if [[ "$*" == *"git/ref/heads/package-release-lock"* ]]; then
    printf 'HTTP/2 200 OK\r\n\r\n{{}}\n'
    return 0
  fi
  if [[ "$*" == *"--method PUT"* ]]; then
    if [ -e "$TEST_TMPDIR/lock-state" ]; then return 1; fi
    : > "$TEST_TMPDIR/lock-state"
    printf '{{"content":{{"sha":"{lock_sha}"}}}}\n'
    return 0
  fi
  if [[ "$*" == *"contents/.package-release-publication-lock.json?ref=package-release-lock"* ]]; then
    printf '{{"type":"file","path":".package-release-publication-lock.json","sha":"{lock_sha}"}}\n'
    return 0
  fi
  return 1
}}
{acquire}
"#,
        );
        let output = Command::new("bash")
            .arg("-c")
            .arg(&acquire_script)
            .env("TEST_TMPDIR", &root)
            .env("GITHUB_REPOSITORY", "example/project")
            .env("GITHUB_WORKFLOW", "preview")
            .env("GITHUB_RUN_ID", "43")
            .env("GITHUB_RUN_ATTEMPT", "1")
            .env("VELNOR_PUBLICATION_LOCK_BRANCH", "package-release-lock")
            .env(
                "EXPECTED_SOURCE_COMMIT",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .env("GITHUB_ENV", &next_env)
            .env("GH_TOKEN", "test-token")
            .output()
            .expect("run next publication lock acquisition");
        assert!(
            output.status.success(),
            "next acquisition failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(root.join("lock-state").exists());
        assert!(std::fs::read_to_string(next_env)
            .expect("read next acquisition environment")
            .contains("VELNOR_PUBLICATION_LOCK_SHA="));
        let _ = std::fs::remove_dir_all(root);
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

    #[cfg(unix)]
    #[test]
    fn rolling_verifier_accepts_source_bound_transaction_handoff_outside_workspace() {
        use std::process::Command;

        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let verifier = verification_script(&spec);
        let path_check_end = verifier
            .find("manifest=\"$dir/release-manifest.json\"")
            .expect("verified handoff path preflight");
        let path_check = &verifier[..path_check_end];
        let verification = PublishVerification {
            script: path_check,
            attestation_flags: "",
        };
        let rolling = render_rolling_refresh_script("", "", "", &verification);
        let handoff_assignments = rolling
            .find("rolling_handoff_relative=\"$EXPECTED_SOURCE_COMMIT/rolling-published\"")
            .expect("source-bound rolling path assignment");
        let assignments_end = rolling[handoff_assignments..]
            .find("\nrolling_response=")
            .map(|offset| handoff_assignments + offset)
            .expect("rolling transaction assignments");
        let assignments = &rolling[handoff_assignments..assignments_end];
        let fragment_start = rolling
            .find("\nmkdir -- \"$rolling_handoff_root\"")
            .expect("owned rolling handoff creation");
        let fragment_end = rolling[fragment_start..]
            .find("\n\nfor payload")
            .map(|offset| fragment_start + offset)
            .expect("rolling verifier call end");
        let fragment = &rolling[fragment_start..fragment_end];

        let root = std::env::temp_dir().join(format!(
            "velnor-rolling-transaction-handoff-{}",
            crate::unique_suffix()
        ));
        let workspace = root.join("workspace");
        let transaction_dir = root.join("runner-temp").join("transaction");
        std::fs::create_dir_all(&workspace).expect("create rolling workspace");
        std::fs::create_dir_all(&transaction_dir).expect("create owned transaction root");
        let commit = "a".repeat(40);
        let harness = format!(
            r#"set -Eeuo pipefail
GITHUB_WORKSPACE="$TEST_TMPDIR/workspace"
transaction_dir="$TEST_TMPDIR/runner-temp/transaction"
EXPECTED_SOURCE_COMMIT={commit}
rolling_tag=preview
GITHUB_REPOSITORY=example/project
PACKAGE_DIR=package
export GITHUB_WORKSPACE EXPECTED_SOURCE_COMMIT PACKAGE_DIR
gh() {{ return 0; }}
rollback() {{ exit "$1"; }}
{assignments}
{fragment}
test -d "$rolling_published_dir"
"#
        );
        let output = Command::new("bash")
            .arg("-c")
            .arg(harness)
            .env("TEST_TMPDIR", &root)
            .output()
            .expect("run source-bound rolling verifier handoff");
        assert!(
            output.status.success(),
            "rolling handoff verifier rejected its generated transaction path:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(transaction_dir
            .join(&commit)
            .join("rolling-published")
            .is_dir());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn generated_checksum_name_verifier_runs_with_host_awk() {
        use std::process::Command;

        let spec = parse_spec(&Args(&args())).expect("valid checksum fixture");
        let script = verification_script(&spec);
        let checker_start = script
            .find("if ! awk '\n  NF == 2 {\n    name = $2")
            .expect("generated SHA256SUMS awk checker");
        let checker_end = checker_start
            + script[checker_start..]
                .find("\nif ! (cd \"$dir\" && sha256sum --check --strict SHA256SUMS)")
                .expect("checksum byte verification boundary");
        let checker = &script[checker_start..checker_end];
        let shell = format!(
            r#"set -euo pipefail
dir="$TEST_TMPDIR/package"
checksum_names="$TEST_TMPDIR/checksum-names"
expected_names="$TEST_TMPDIR/expected-names"
{checker}
"#
        );
        let root = std::env::temp_dir().join(format!(
            "velnor-package-host-awk-{}",
            crate::unique_suffix()
        ));
        let package_dir = root.join("package");
        std::fs::create_dir_all(&package_dir).expect("create host awk fixture");
        std::fs::write(root.join("expected-names"), "a.tar.gz\n")
            .expect("write expected checksum names");

        let run_checker = |line: &str| {
            std::fs::write(package_dir.join("SHA256SUMS"), line).expect("write SHA256SUMS fixture");
            Command::new("bash")
                .arg("-c")
                .arg(&shell)
                .env("TEST_TMPDIR", &root)
                .output()
                .expect("run generated checksum checker with host awk")
        };
        let digest = "a".repeat(64);
        let valid = run_checker(&format!("{digest}  a.tar.gz\n"));
        assert!(
            valid.status.success(),
            "generated SHA256SUMS check rejected a valid entry under host awk:\n{}{}",
            String::from_utf8_lossy(&valid.stdout),
            String::from_utf8_lossy(&valid.stderr)
        );

        let unsafe_name = run_checker(&format!("{digest}  a\\b.tar.gz\n"));
        assert!(
            !unsafe_name.status.success(),
            "generated SHA256SUMS check accepted a path containing a backslash"
        );
        let _ = std::fs::remove_dir_all(root);
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
    fn rendered_workflow_runs_repository_verification_tasks_at_all_boundaries() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        assert_eq!(
            workflow
                .matches("mise run 'verify-preview-package'")
                .count(),
            3
        );
        let producer = workflow
            .find("Build verified package directory")
            .expect("producer step");
        let producer_verify = workflow
            .find("Run repository package verification tasks")
            .expect("producer verification task step");
        let handoff = workflow
            .find("Re-verify downloaded handoff")
            .expect("handoff step");
        let handoff_verify = workflow
            .find("Run handoff package verification tasks")
            .expect("handoff verification task step");
        let immutable = workflow
            .find("Download and re-verify published release")
            .expect("immutable download step");
        let immutable_verify = workflow
            .find("Run published package verification tasks")
            .expect("immutable verification task step");
        let rolling = workflow
            .find("Refresh rolling preview release")
            .expect("rolling update step");
        assert!(producer < producer_verify);
        assert!(producer_verify < handoff);
        assert!(handoff < handoff_verify);
        assert!(handoff_verify < immutable);
        assert!(immutable < immutable_verify);
        assert!(immutable_verify < rolling);
        assert!(workflow
            .contains("VELNOR_VERIFIED_PACKAGE_DIR: ${{ github.workspace }}/published-package"));
        assert!(!workflow.contains("verify-preview-package --"));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn rendered_workflow_rechecks_published_dir_and_attests_declared_assets() {
        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        assert!(
            workflow.contains("          published_dir=\"$GITHUB_WORKSPACE/published-package\""),
            "{workflow}"
        );
        assert!(
            workflow.contains(
                "          if [[ -e \"$published_dir\" || -L \"$published_dir\" ]]; then"
            ),
            "{workflow}"
        );
        assert!(
            workflow.contains("          mkdir -- \"$published_dir\""),
            "{workflow}"
        );
        assert!(
            workflow.contains("          export VELNOR_VERIFIED_PACKAGE_DIR=\"$published_dir\""),
            "{workflow}"
        );
        assert!(!workflow.contains("rm -rf published-package"), "{workflow}");
        assert!(
            !workflow.contains("mkdir -p published-package"),
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
    #[allow(clippy::too_many_lines)]
    fn pre_publish_hook_is_locked_and_isolated_from_verification_boundaries() {
        let mut values = args();
        values.insert(
            "pre_publish_tasks".to_owned(),
            toml::Value::Array(vec![toml::Value::String(
                "migrate-preview-legacy".to_owned(),
            )]),
        );
        let spec = parse_spec(&Args(&values)).expect("valid pre-publish task");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        let document = serde_yaml::from_str::<serde_yaml::Value>(&workflow)
            .expect("rendered workflow is YAML");
        let publish = document
            .get("jobs")
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|jobs| jobs.get("publish"))
            .and_then(serde_yaml::Value::as_mapping)
            .expect("publish job");
        let permissions = publish
            .get("permissions")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("publish permissions");
        assert_eq!(
            permissions
                .get("contents")
                .and_then(serde_yaml::Value::as_str),
            Some("write")
        );
        assert_eq!(
            permissions
                .get("pull-requests")
                .and_then(serde_yaml::Value::as_str),
            Some("write")
        );
        assert_eq!(
            permissions
                .get("attestations")
                .and_then(serde_yaml::Value::as_str),
            Some("read")
        );
        let steps = publish
            .get("steps")
            .and_then(serde_yaml::Value::as_sequence)
            .expect("publish steps");
        let step = |name: &str| {
            steps
                .iter()
                .find(|step| step.get("name").and_then(serde_yaml::Value::as_str) == Some(name))
                .expect("missing publish step")
        };
        let handoff = step("Re-verify downloaded handoff");
        let attest = step("Verify build attestations");
        let lock = step("Acquire package publication lock");
        let migration = step("Run pre-publish migration tasks");
        let immutable = step("Publish immutable source-bound release");
        let published = step("Download and re-verify published release");
        let consumer_checkout = step("Checkout consumer repository");
        let consumer_update = step("Run updater and create or update consumer PR");
        let index = |target: &serde_yaml::Value| {
            steps
                .iter()
                .position(|candidate| candidate == target)
                .expect("publish step index")
        };
        assert!(index(handoff) < index(attest));
        assert!(index(attest) < index(lock));
        assert!(index(lock) < index(migration));
        assert!(index(migration) < index(immutable));
        assert!(index(immutable) < index(published));

        let migration_env = migration
            .get("env")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("migration environment");
        assert_eq!(
            migration_env
                .get("GH_TOKEN")
                .and_then(serde_yaml::Value::as_str),
            Some("${{ github.token }}")
        );
        assert_eq!(
            migration_env
                .get("VELNOR_SOURCE_CHECKOUT_DIR")
                .and_then(serde_yaml::Value::as_str),
            Some("${{ github.workspace }}/source")
        );
        assert_eq!(
            migration
                .get("working-directory")
                .and_then(serde_yaml::Value::as_str),
            Some("source")
        );
        let migration_run = migration
            .get("run")
            .and_then(serde_yaml::Value::as_str)
            .expect("migration script");
        assert!(migration_run.contains("set -euo pipefail"));
        assert!(migration_run.contains("mise run 'migrate-preview-legacy'"));
        assert!(!migration_run.contains("TAP_TOKEN"));
        assert!(!migration_env.contains_key("UPDATER_TOKEN"));
        let source_publish_env = immutable
            .get("env")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("immutable environment");
        assert_eq!(
            source_publish_env
                .get("GH_TOKEN")
                .and_then(serde_yaml::Value::as_str),
            Some("${{ github.token }}")
        );
        let consumer_checkout_with = consumer_checkout
            .get("with")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("consumer checkout inputs");
        assert_eq!(
            consumer_checkout_with
                .get("token")
                .and_then(serde_yaml::Value::as_str),
            Some("${{ secrets.TAP_TOKEN }}")
        );
        let consumer_update_env = consumer_update
            .get("env")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("consumer update environment");
        assert_eq!(
            consumer_update_env
                .get("GH_TOKEN")
                .and_then(serde_yaml::Value::as_str),
            Some("${{ secrets.TAP_TOKEN }}")
        );
        assert_eq!(
            consumer_update_env
                .get("UPDATER_TOKEN")
                .and_then(serde_yaml::Value::as_str),
            Some("${{ secrets.TAP_TOKEN }}")
        );
        let publish_job_env = publish
            .get("env")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("publish job environment");
        assert!(!publish_job_env.contains_key("UPDATER_TOKEN"));
        assert_eq!(workflow.matches("${{ secrets.TAP_TOKEN }}").count(), 3);
        assert_eq!(
            workflow
                .matches("mise run 'migrate-preview-legacy'")
                .count(),
            1
        );
        let build_steps = document
            .get("jobs")
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|jobs| jobs.get("build"))
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|build| build.get("steps"))
            .and_then(serde_yaml::Value::as_sequence)
            .expect("build steps");
        for name in [
            "Build verified package directory",
            "Run repository package verification tasks",
        ] {
            let script = build_steps
                .iter()
                .find(|candidate| {
                    candidate.get("name").and_then(serde_yaml::Value::as_str) == Some(name)
                })
                .and_then(|candidate| candidate.get("run"))
                .and_then(serde_yaml::Value::as_str)
                .expect("missing build step");
            assert!(!script.contains("migrate-preview-legacy"), "{name}");
        }
        for name in [
            "Run handoff package verification tasks",
            "Run published package verification tasks",
        ] {
            let script = step(name)
                .get("run")
                .and_then(serde_yaml::Value::as_str)
                .unwrap_or("");
            assert!(!script.contains("migrate-preview-legacy"), "{name}");
        }
        let lock_run = lock
            .get("run")
            .and_then(serde_yaml::Value::as_str)
            .expect("lock script");
        assert!(lock_run.contains("acquire_publication_lock"));
        assert!(lock_run.contains("VELNOR_PUBLICATION_LOCK_SHA"));
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
            .contains("VELNOR_PACKAGE_ASSET_TAG: ${{ steps.publish.outputs.immutable_tag }}"));
        assert!(workflow.contains("VELNOR_PACKAGE_RELEASE_TAG: preview"));
        assert!(workflow.contains("VELNOR_PACKAGE_VERSION: ${{ steps.verify.outputs.version }}"));
        assert!(workflow
            .contains("VELNOR_PACKAGE_SOURCE_COMMIT: ${{ steps.verify.outputs.source_commit }}"));
        assert!(workflow.contains("VELNOR_PACKAGE_SOURCE_REPOSITORY: \"example/project\""));
        assert!(workflow.contains("VELNOR_PACKAGE_SOURCE_REF: \"refs/heads/main\""));
        assert!(workflow
            .contains("VELNOR_VERIFIED_PACKAGE_DIR: ${{ github.workspace }}/published-package"));
        assert!(!workflow.contains("git switch --force-create \"$automation_branch\""));
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
        assert!(workflow.contains("immutable package asset tag does not bind to its source commit"));
        assert!(!workflow.contains("unset RELEASE_TAG"));
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
    fn immutable_consumer_uses_verified_source_tag_without_rolling_alias() {
        let mut configured = args();
        configured.insert(
            "consumer_tag_mode".to_owned(),
            toml::Value::String("immutable".to_owned()),
        );
        configured.insert(
            "refresh_rolling_release".to_owned(),
            toml::Value::Boolean(false),
        );
        let spec = parse_spec(&Args(&configured)).expect("valid immutable-only fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        let document = serde_yaml::from_str::<serde_yaml::Value>(&workflow)
            .expect("rendered workflow is YAML");
        let publish = &document["jobs"]["publish"];
        assert!(publish["outputs"]["rolling_refresh_outcome"]
            .as_str()
            .is_some_and(|value| value.contains("not-requested")));
        assert!(!workflow.contains("Refresh rolling preview release"));
        assert!(workflow.contains("Verify immutable consumer package identity"));
        let steps = publish["steps"].as_sequence().expect("publish steps");
        let identity_index = steps
            .iter()
            .position(|step| {
                step["name"].as_str() == Some("Verify immutable consumer package identity")
            })
            .expect("identity check step");
        let checkout_index = steps
            .iter()
            .position(|step| step["name"].as_str() == Some("Checkout consumer repository"))
            .expect("consumer checkout step");
        assert!(identity_index < checkout_index);
        let updater = steps
            .iter()
            .find(|step| {
                step["name"].as_str() == Some("Run updater and create or update consumer PR")
            })
            .expect("consumer updater step");
        let env = updater["env"].as_mapping().expect("updater environment");
        let env_value = |key: &str| env.get(key).and_then(serde_yaml::Value::as_str);
        assert_eq!(
            env_value("VELNOR_PACKAGE_ASSET_TAG"),
            Some("${{ steps.publish.outputs.immutable_tag }}")
        );
        assert_eq!(
            env_value("VELNOR_PACKAGE_SOURCE_COMMIT"),
            Some("${{ steps.verify.outputs.source_commit }}")
        );
        assert_eq!(
            env_value("VELNOR_PACKAGE_VERSION"),
            Some("${{ steps.verify.outputs.version }}")
        );
        assert_eq!(
            env_value("VELNOR_VERIFIED_PACKAGE_DIR"),
            Some("${{ github.workspace }}/published-package")
        );
        assert!(env_value("VELNOR_PACKAGE_RELEASE_TAG").is_none());
        let updater_script = updater["run"].as_str().expect("consumer updater script");
        let unset_legacy_tag = updater_script
            .find("unset RELEASE_TAG")
            .expect("immutable updater hides the legacy rolling tag");
        let updater_execution = updater_script
            .find("bash -c \"$UPDATER\"")
            .expect("consumer updater execution");
        assert!(unset_legacy_tag < updater_execution);
    }

    #[test]
    fn immutable_consumer_refresh_failure_does_not_skip_tap_update() {
        let mut configured = args();
        configured.insert(
            "consumer_tag_mode".to_owned(),
            toml::Value::String("immutable".to_owned()),
        );
        configured.insert(
            "refresh_rolling_release".to_owned(),
            toml::Value::Boolean(true),
        );
        let spec = parse_spec(&Args(&configured)).expect("valid immutable refresh fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        let document = serde_yaml::from_str::<serde_yaml::Value>(&workflow)
            .expect("rendered workflow is YAML");
        let publish = &document["jobs"]["publish"];
        let steps = publish["steps"].as_sequence().expect("publish steps");
        let refresh = steps
            .iter()
            .find(|step| step["name"].as_str() == Some("Refresh rolling preview release"))
            .expect("rolling refresh step");
        assert_eq!(refresh["continue-on-error"].as_bool(), Some(true));
        let refresh_index = steps
            .iter()
            .position(|step| step["name"].as_str() == Some("Refresh rolling preview release"))
            .expect("rolling refresh step index");
        let updater_index = steps
            .iter()
            .position(|step| {
                step["name"].as_str() == Some("Run updater and create or update consumer PR")
            })
            .expect("consumer updater step index");
        assert!(refresh_index < updater_index);
        assert!(publish["outputs"]["rolling_refresh_outcome"]
            .as_str()
            .is_some_and(|value| value.contains("steps.rolling-refresh.outcome")));
    }

    #[test]
    fn immutable_only_lock_releases_after_published_assets_are_verified() {
        let mut configured = args();
        configured.insert(
            "consumer_tag_mode".to_owned(),
            toml::Value::String("immutable".to_owned()),
        );
        configured.insert(
            "refresh_rolling_release".to_owned(),
            toml::Value::Boolean(false),
        );
        let spec = parse_spec(&Args(&configured)).expect("valid immutable-only fixture");
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
        let verified_release = workflow
            .find("Verify published release attestations")
            .expect("published release attestation step");
        let release_lock = workflow
            .find("Release immutable-only publication lock after verification")
            .expect("immutable-only lock release step");
        let finalizer = workflow
            .find("Finalize package publication lock")
            .expect("publication lock finalizer");
        let consumer_checkout = workflow
            .find("Checkout consumer repository")
            .expect("consumer checkout");
        assert!(verified_release < release_lock);
        assert!(release_lock < finalizer);
        assert!(finalizer < consumer_checkout);
        assert!(workflow.contains("VELNOR_PUBLICATION_LOCK_RETAIN=0"));
    }

    #[test]
    fn rolling_refresh_preflight_releases_inherited_retention_before_mutation() {
        let verification = PublishVerification {
            script: "set -euo pipefail",
            attestation_flags: "--repo example/project",
        };
        let script = render_rolling_refresh_script("", "", "", &verification);
        let lock_verified = script
            .find(
                "publication lock could not be verified; refusing release validation and mutation",
            )
            .expect("lock ownership check");
        let retention_cleared = script
            .find("\nclear_publication_lock_retain\n")
            .expect("preflight retention release");
        let first_mutation = script
            .find("mutated=1\n  mark_publication_lock_retain")
            .expect("retention restored before rolling mutation");
        assert!(lock_verified < retention_cleared);
        assert!(retention_cleared < first_mutation);
    }

    #[test]
    fn legacy_consumer_requires_rolling_refresh() {
        let mut configured = args();
        configured.insert(
            "consumer_tag_mode".to_owned(),
            toml::Value::String("legacy".to_owned()),
        );
        configured.insert(
            "refresh_rolling_release".to_owned(),
            toml::Value::Boolean(false),
        );
        assert!(parse_spec(&Args(&configured)).is_err());
    }

    #[test]
    fn consumer_tag_mode_and_rolling_refresh_types_fail_closed() {
        let mut invalid_mode = args();
        invalid_mode.insert(
            "consumer_tag_mode".to_owned(),
            toml::Value::String("latest".to_owned()),
        );
        assert!(parse_spec(&Args(&invalid_mode)).is_err());

        let mut invalid_refresh = args();
        invalid_refresh.insert(
            "refresh_rolling_release".to_owned(),
            toml::Value::String("false".to_owned()),
        );
        assert!(parse_spec(&Args(&invalid_refresh)).is_err());
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
            "cached whitespace check did not identify trailing whitespace: {diagnostics}"
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
        assert!(workflow.contains(
            "if [ \"$rollback_status\" -eq 0 ] && ! verify_restored_assets \"$rollback_dir\" \"$old_assets\""
        ));
        assert!(workflow.contains("rolling preview ownership changed; refusing rollback mutation"));
        assert!(workflow.contains("refusing force-move"));
        assert!(workflow.contains("rollback restored bytes differ"));
        assert!(workflow.contains("rollback GitHub digest differs"));
        assert!(workflow.contains(
            "gh release upload \"$rolling_tag\" --repo \"$GITHUB_REPOSITORY\" --clobber"
        ));
        assert!(
            workflow.contains("gh release download \"$rolling_tag\" --repo \"$GITHUB_REPOSITORY\"")
        );
        assert!(workflow.contains("gh attestation verify \"$rolling_published_dir/$payload\""));
        assert!(workflow
            .contains("rolling_handoff_relative=\"$EXPECTED_SOURCE_COMMIT/rolling-published\""));
        assert!(workflow.contains("VELNOR_PACKAGE_RELEASE_TAG: preview"));
        assert!(!workflow.contains("gh release delete"));
        assert!(!workflow.contains("HEAD:$CONSUMER_BRANCH"));
    }

    #[test]
    fn generated_publish_job_serializes_and_fences_every_publication_writer() {
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
        let concurrency = publish
            .get("concurrency")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("publish concurrency");
        assert_eq!(
            concurrency.get("group").and_then(serde_yaml::Value::as_str),
            Some("package-release-preview")
        );
        assert_eq!(
            concurrency
                .get("cancel-in-progress")
                .and_then(serde_yaml::Value::as_bool),
            Some(false)
        );

        let steps = publish
            .get("steps")
            .and_then(serde_yaml::Value::as_sequence)
            .expect("publish steps");
        let step_script = |name: &str| {
            steps
                .iter()
                .find(|step| step.get("name").and_then(serde_yaml::Value::as_str) == Some(name))
                .and_then(|step| step.get("run"))
                .and_then(serde_yaml::Value::as_str)
                .expect("missing script")
        };
        let immutable = step_script("Publish immutable source-bound release");
        let rolling = step_script("Refresh rolling preview release");
        let acquire = immutable
            .find("if ! acquire_publication_lock; then")
            .expect("immutable lock acquisition");
        let first_immutable_mutation = immutable
            .find("gh release create")
            .or_else(|| immutable.find("gh release upload"))
            .or_else(|| immutable.find("gh api --method PATCH"))
            .expect("immutable release mutation");
        assert!(acquire < first_immutable_mutation);
        assert!(immutable.contains(
            "publication_lock_branch=\"${VELNOR_PUBLICATION_LOCK_BRANCH:?missing VELNOR_PUBLICATION_LOCK_BRANCH}\""
        ));
        assert!(
            immutable.contains("publication_lock_path=\".package-release-publication-lock.json\"")
        );
        assert!(immutable.contains("VELNOR_PUBLICATION_LOCK_SHA"));
        assert!(immutable.contains("assert_publication_lock"));
        assert!(immutable
            .contains("-f \"sha=$publication_lock_sha\" -f \"branch=$publication_lock_branch\""));
        assert!(rolling.contains("cleanup_publication \"$?\""));
        assert!(rolling.contains("release_publication_lock"));
        assert!(rolling.contains("assert_publication_lock"));
        assert!(rolling.contains("trap - ERR"));
        assert!(rolling.contains("trap - EXIT"));
        assert!(rolling.contains(
            "clear_publication_lock_retain\n# cleanup_publication releases the exact lock"
        ));
    }

    #[cfg(unix)]
    fn test_bash_path(root: &std::path::Path, shell: &str) -> std::ffi::OsString {
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("create shell fixture");
        std::os::unix::fs::symlink(shell, bin.join("bash")).expect("link tested bash");
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(paths).expect("join shell fixture PATH")
    }

    #[cfg(unix)]
    #[test]
    fn rolling_verification_failure_preserves_publication_traps() {
        use std::process::Command;

        let verification = PublishVerification {
            script: r#"set -euo pipefail
trap 'rm -f -- "$TEST_TMPDIR/verifier-temp"' EXIT
: > "$TEST_TMPDIR/verifier-temp"
: > "$TEST_TMPDIR/verifier-before-failure"
false
: > "$TEST_TMPDIR/verifier-after-failure"
"#,
            attestation_flags: "",
        };
        let rolling = render_rolling_refresh_script("", "", "", &verification);
        let start = rolling
            .find("\nif bash -euo pipefail -c ")
            .expect("verification child");
        let end = rolling
            .find("\n\nfor payload")
            .expect("verification child end");
        let fragment = &rolling[start..end];

        for shell in ["/bin/bash", "/opt/homebrew/bin/bash"] {
            if !std::path::Path::new(shell).is_file() {
                continue;
            }
            let root = std::env::temp_dir().join(format!(
                "velnor-publication-verifier-traps-{}-{}",
                shell.rsplit('/').next().expect("shell basename"),
                crate::unique_suffix()
            ));
            std::fs::create_dir_all(&root).expect("create verifier trap fixture");
            let path = test_bash_path(&root, shell);
            let script = format!(
                r#"set -Eeuo pipefail
rollback() {{ : > "$TEST_TMPDIR/rollback"; exit "$1"; }}
cleanup_publication() {{ : > "$TEST_TMPDIR/cleanup"; }}
trap 'rollback "$?"' ERR
trap 'cleanup_publication "$?"' EXIT
{fragment}
"#
            );
            let output = Command::new(shell)
                .arg("-c")
                .arg(script)
                .env("TEST_TMPDIR", &root)
                .env("PATH", path)
                .output()
                .expect("run verifier trap fixture");
            assert!(!output.status.success());
            assert!(root.join("rollback").exists());
            assert!(root.join("cleanup").exists());
            assert!(!root.join("verifier-temp").exists());
            assert!(root.join("verifier-before-failure").exists());
            assert!(!root.join("verifier-after-failure").exists());
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[cfg(unix)]
    fn run_rolling_verifier_status_case(name: &str, body: &str, succeeds: bool, shell: &str) {
        use std::process::Command;

        let verification_script = format!(
            r#"set -euo pipefail
trap 'rm -f -- "$TEST_TMPDIR/verifier-temp"' EXIT
: > "$TEST_TMPDIR/verifier-temp"
: > "$TEST_TMPDIR/verifier-before"
{body}
: > "$TEST_TMPDIR/verifier-after"
"#
        );
        let verification = PublishVerification {
            script: &verification_script,
            attestation_flags: "",
        };
        let rolling = render_rolling_refresh_script("", "", "", &verification);
        let start = rolling
            .find("\nif bash -euo pipefail -c ")
            .expect("verification child");
        let end = rolling
            .find("\n\nfor payload")
            .expect("verification child end");
        let fragment = &rolling[start..end];
        let root = std::env::temp_dir().join(format!(
            "velnor-publication-verifier-status-{name}-{}-{}",
            shell.rsplit('/').next().expect("shell basename"),
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create verifier status fixture");
        let path = test_bash_path(&root, shell);
        let script = format!(
            r#"set -Eeuo pipefail
rollback() {{
  count=0
  if [ -f "$TEST_TMPDIR/rollback-count" ]; then count="$(cat "$TEST_TMPDIR/rollback-count")"; fi
  count=$((count + 1))
  printf '%s\n' "$count" > "$TEST_TMPDIR/rollback-count"
  exit "$1"
}}
cleanup_publication() {{
  count=0
  if [ -f "$TEST_TMPDIR/cleanup-count" ]; then count="$(cat "$TEST_TMPDIR/cleanup-count")"; fi
  count=$((count + 1))
  printf '%s\n' "$count" > "$TEST_TMPDIR/cleanup-count"
}}
trap 'rollback "$?"' ERR
trap 'cleanup_publication "$?"' EXIT
{fragment}
"#
        );
        let output = Command::new(shell)
            .arg("-c")
            .arg(script)
            .env("TEST_TMPDIR", &root)
            .env("PATH", path)
            .output()
            .expect("run verifier status fixture");
        assert_eq!(output.status.success(), succeeds, "{name}: {output:?}");
        assert_eq!(
            std::fs::read_to_string(root.join("cleanup-count"))
                .ok()
                .as_deref(),
            Some("1\n"),
            "{name}: cleanup runs exactly once"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("rollback-count"))
                .ok()
                .as_deref(),
            if succeeds { None } else { Some("1\n") },
            "{name}: rollback runs exactly once only on failure"
        );
        assert!(
            root.join("verifier-before").exists(),
            "{name}: verifier starts"
        );
        assert_eq!(
            root.join("verifier-after").exists(),
            succeeds,
            "{name}: verifier preserves fail-fast status"
        );
        assert!(
            !root.join("verifier-temp").exists(),
            "{name}: verifier EXIT trap runs"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn rolling_verifier_status_boundary_preserves_failfast_and_single_rollback() {
        let cases = [
            ("false", "false", false),
            ("explicit-exit", "exit 7", false),
            (
                "nested-pipeline",
                "printf '%s\\n' value | grep -F missing",
                false,
            ),
            ("success", ":", true),
        ];
        for (name, body, succeeds) in cases {
            for shell in ["/bin/bash", "/opt/homebrew/bin/bash"] {
                if std::path::Path::new(shell).is_file() {
                    run_rolling_verifier_status_case(name, body, succeeds, shell);
                }
            }
        }
    }

    #[cfg(unix)]
    fn validate_owned_process_group(
        child_pid: rustix::process::Pid,
        process_group: rustix::process::Pid,
        parent_group: rustix::process::Pid,
    ) -> Result<rustix::process::Pid, String> {
        if child_pid.is_init() {
            return Err("fixture child unexpectedly has init PID".to_owned());
        }
        if process_group != child_pid {
            return Err(format!(
                "fixture child group {process_group} does not equal child PID {child_pid}"
            ));
        }
        if process_group == parent_group {
            return Err(format!(
                "fixture child group {process_group} is the test process group"
            ));
        }
        Ok(process_group)
    }

    #[cfg(unix)]
    fn owned_fixture_group(child: &std::process::Child) -> Result<rustix::process::Pid, String> {
        let child_pid = rustix::process::Pid::from_child(child);
        let process_group = rustix::process::getpgid(Some(child_pid))
            .map_err(|error| format!("read fixture process group: {error}"))?;
        let parent_group = rustix::process::getpgid(None)
            .map_err(|error| format!("read test process group: {error}"))?;
        validate_owned_process_group(child_pid, process_group, parent_group)
    }

    #[cfg(unix)]
    fn owned_fixture_process(
        child: &std::process::Child,
        raw_pid: i32,
    ) -> Result<rustix::process::Pid, String> {
        let process = validated_fixture_process(raw_pid)?;
        let fixture_group = owned_fixture_group(child)?;
        let process_group = rustix::process::getpgid(Some(process))
            .map_err(|error| format!("read verifier process group: {error}"))?;
        if process_group != fixture_group {
            return Err(format!(
                "verifier group {process_group} does not equal fixture group {fixture_group}"
            ));
        }
        Ok(process)
    }

    #[cfg(unix)]
    fn validated_fixture_process(raw_pid: i32) -> Result<rustix::process::Pid, String> {
        if raw_pid <= 1 {
            return Err(format!("invalid verifier PID {raw_pid}"));
        }
        let process = rustix::process::Pid::from_raw(raw_pid)
            .ok_or_else(|| format!("invalid verifier PID {raw_pid}"))?;
        if process.is_init() {
            return Err("verifier unexpectedly has init PID".to_owned());
        }
        Ok(process)
    }

    #[cfg(unix)]
    fn signal_fixture(
        child: &std::process::Child,
        verifier_pid: i32,
        signal_group: bool,
        signal: rustix::process::Signal,
    ) -> Result<(), String> {
        if signal_group {
            let process_group = owned_fixture_group(child)?;
            rustix::process::kill_process_group(process_group, signal)
                .map_err(|error| format!("signal fixture process group {process_group}: {error}"))
        } else {
            let process = owned_fixture_process(child, verifier_pid)?;
            rustix::process::kill_process(process, signal)
                .map_err(|error| format!("signal verifier process {process}: {error}"))
        }
    }

    #[cfg(unix)]
    fn terminate_fixture_group(child: &mut std::process::Child) {
        let process_group = owned_fixture_group(child).ok();
        if let Some(process_group) = process_group {
            let _ =
                rustix::process::kill_process_group(process_group, rustix::process::Signal::TERM);
        } else {
            let _ = child.kill();
        }
        for _ in 0..200 {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        if let Some(process_group) = process_group {
            let _ =
                rustix::process::kill_process_group(process_group, rustix::process::Signal::KILL);
        } else {
            let _ = child.kill();
        }
        let _ = child.wait();
    }

    #[cfg(unix)]
    fn wait_for_fixture_pid(
        root: &std::path::Path,
        child: &mut std::process::Child,
    ) -> Result<i32, String> {
        for _ in 0..200 {
            if let Some(pid) = std::fs::read_to_string(root.join("verifier-pid"))
                .ok()
                .and_then(|pid| pid.trim().parse::<i32>().ok())
            {
                return Ok(pid);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        terminate_fixture_group(child);
        Err(format!("verifier did not start: {}", child.id()))
    }

    #[cfg(unix)]
    fn wait_for_fixture_exit(
        child: &mut std::process::Child,
    ) -> Result<std::process::ExitStatus, String> {
        for _ in 0..200 {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(error) => {
                    terminate_fixture_group(child);
                    return Err(format!("fixture wait failed: {error}"));
                }
            }
        }
        terminate_fixture_group(child);
        Err("fixture did not exit after cancellation".to_owned())
    }

    #[cfg(unix)]
    fn spawn_rolling_cancellation_fixture(
        shell: &str,
        fragment: &str,
        lock_script: &str,
        cleanup: &str,
        signal_group: bool,
    ) -> (std::path::PathBuf, std::process::Child, i32) {
        use std::fs;
        use std::os::unix::fs::symlink;
        use std::os::unix::process::CommandExt;
        use std::process::Command;

        let version = shell.rsplit('/').next().expect("shell basename");
        let root = std::env::temp_dir().join(format!(
            "velnor-publication-verifier-cancel-{version}-{}-{signal_group}",
            crate::unique_suffix()
        ));
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("create cancellation fixture");
        symlink(shell, bin.join("bash")).expect("link tested bash into PATH");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(&path));
        let path = std::env::join_paths(paths).expect("join test PATH");
        let script = format!(
            r#"set -Eeuo pipefail
transaction_dir="$TEST_TMPDIR/transaction"
mkdir -p "$transaction_dir"
{lock_script}
release_publication_lock() {{
  : > "$TEST_TMPDIR/lock-released"
  return 0
}}
mark_publication_lock_retain
increment() {{
  local file="$1"
  local count=0
  if [ -f "$file" ]; then count="$(cat "$file")"; fi
  count=$((count + 1))
  printf '%s\n' "$count" > "$file"
}}
rollback() {{
  increment "$TEST_TMPDIR/rollback-count"
  exit "$1"
}}
{cleanup}
trap 'rollback "$?"' ERR
trap 'cleanup_publication "$?"' EXIT
{fragment}
: > "$TEST_TMPDIR/after"
"#
        );
        let mut child = Command::new(shell)
            .arg("-c")
            .arg(&script)
            .env("TEST_TMPDIR", &root)
            .env("GITHUB_ENV", root.join("github-env"))
            .env("GITHUB_REPOSITORY", "example/project")
            .env("VELNOR_PUBLICATION_LOCK_BRANCH", "package-release-lock")
            .env(
                "VELNOR_PUBLICATION_LOCK_SHA",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .env("PATH", &path)
            .process_group(0)
            .spawn()
            .expect("start cancellation fixture");
        let verifier_pid = wait_for_fixture_pid(&root, &mut child).expect("verifier startup");
        (root, child, verifier_pid)
    }

    #[cfg(unix)]
    fn run_rolling_cancellation_case(
        shell: &str,
        fragment: &str,
        lock_script: &str,
        cleanup: &str,
        signal_group: bool,
    ) {
        use std::fs;
        let (root, mut child, verifier_pid) =
            spawn_rolling_cancellation_fixture(shell, fragment, lock_script, cleanup, signal_group);
        let signal_result = signal_fixture(
            &child,
            verifier_pid,
            signal_group,
            rustix::process::Signal::TERM,
        );
        if let Err(error) = &signal_result {
            terminate_fixture_group(&mut child);
            assert!(
                signal_result.is_ok(),
                "signal failed for {shell} group={signal_group}: {error}"
            );
            return;
        }
        let status = wait_for_fixture_exit(&mut child).expect("fixture cancellation exit");
        assert!(
            !status.success(),
            "{shell} group={signal_group}: {status:?}"
        );
        assert!(
            root.join("github-env").is_file()
                && fs::read_to_string(root.join("github-env"))
                    .unwrap_or_default()
                    .contains("VELNOR_PUBLICATION_LOCK_RETAIN=1"),
            "generated retain handoff missing for {shell} group={signal_group}"
        );
        assert!(
            !root.join("lock-released").exists(),
            "cancellation never releases lock for {shell} group={signal_group}"
        );
        assert!(
            !root.join("transaction").exists(),
            "generated cleanup removes transaction for {shell} group={signal_group}"
        );
        assert!(
            !root.join("after").exists(),
            "cancellation stops publication for {shell} group={signal_group}"
        );
        if signal_group {
            assert!(
                !root.join("rollback-count").exists(),
                "group TERM reaches finalizer without a second rollback for {shell}"
            );
        } else {
            assert_eq!(
                fs::read_to_string(root.join("rollback-count"))
                    .ok()
                    .as_deref(),
                Some("1\n"),
                "child cancellation rolls back exactly once for {shell}"
            );
            assert!(
                root.join("verifier-term").exists(),
                "child receives TERM for {shell}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn process_group_validation_rejects_broadcast_and_shared_groups() {
        let child_pid = rustix::process::Pid::from_raw(4_321).expect("positive fixture PID");
        let private_group = rustix::process::Pid::from_raw(4_321).expect("positive group PID");
        let other_group = rustix::process::Pid::from_raw(9_876).expect("positive parent PID");
        assert_eq!(
            validate_owned_process_group(child_pid, private_group, other_group),
            Ok(private_group)
        );
        assert!(validate_owned_process_group(child_pid, other_group, other_group).is_err());
        assert!(validate_owned_process_group(child_pid, private_group, private_group).is_err());
        assert!(validate_owned_process_group(
            rustix::process::Pid::INIT,
            rustix::process::Pid::INIT,
            other_group
        )
        .is_err());
        assert!(validated_fixture_process(-1).is_err());
        assert!(validated_fixture_process(0).is_err());
        assert!(validated_fixture_process(1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rolling_verifier_cancellation_retains_lock_without_release() {
        let shells = ["/bin/bash", "/opt/homebrew/bin/bash"];
        for shell in shells {
            if !std::path::Path::new(shell).is_file() {
                continue;
            }
            let verification = PublishVerification {
                script: r#"set -euo pipefail
trap ': > "$TEST_TMPDIR/verifier-term"; exit 143' TERM
printf '%s\n' "$$" > "$TEST_TMPDIR/verifier-pid"
while :; do :; done
"#,
                attestation_flags: "",
            };
            let rolling = render_rolling_refresh_script("", "", "", &verification);
            let start = rolling
                .find("\nif bash -euo pipefail -c ")
                .expect("verification child");
            let end = rolling
                .find("\n\nfor payload")
                .expect("verification child end");
            let fragment = &rolling[start..end];
            let lock_script = render_publication_lock_script(true);
            let cleanup_start = rolling
                .find("\ncleanup_publication() {")
                .expect("publication cleanup");
            let cleanup_end = rolling
                .find("\ntrap 'rollback")
                .expect("publication cleanup end");
            let cleanup = &rolling[cleanup_start + 1..cleanup_end];
            run_rolling_cancellation_case(shell, fragment, &lock_script, cleanup, false);
            run_rolling_cancellation_case(shell, fragment, &lock_script, cleanup, true);
        }
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::too_many_lines, clippy::uninlined_format_args)]
    fn publication_lock_lifecycle_expands_owner_and_fails_closed_when_held() {
        use std::process::Command;

        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script("", "", "", &verification);
        assert!(!rolling_script.contains(r"\${GITHUB_REPOSITORY}"));
        assert!(!rolling_script.contains(r"\${GITHUB_RUN_ID:-unknown}"));
        let lock_start = rolling_script
            .find("publication_lock_branch=\"")
            .expect("lock initialization");
        let lock_end = rolling_script
            .find("\nremote_tag_sha() {")
            .expect("lock helper boundary");
        let lock = &rolling_script[lock_start..lock_end];
        assert!(rolling_script.contains("\nif bash -euo pipefail -c "));
        assert!(rolling_script.contains(
            "clear_publication_lock_retain\n# cleanup_publication releases the exact lock"
        ));
        let cleanup_start = rolling_script
            .find("cleanup_publication() {")
            .expect("cleanup helper");
        let cleanup_end = rolling_script
            .find("\ntrap 'rollback")
            .expect("rollback trap boundary");
        let cleanup = &rolling_script[cleanup_start..cleanup_end];
        let lock_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        for mode in ["success", "retained-success", "held"] {
            let root = std::env::temp_dir().join(format!(
                "velnor-publication-lock-{mode}-{}",
                crate::unique_suffix()
            ));
            std::fs::create_dir_all(&root).expect("create lock fixture");
            let script = format!(
                r#"set -Eeuo pipefail
GITHUB_REPOSITORY=example/project
GITHUB_WORKFLOW=preview
GITHUB_RUN_ID=42
GITHUB_RUN_ATTEMPT=3
VELNOR_PUBLICATION_LOCK_BRANCH=velnor-publication-lock
EXPECTED_SOURCE_COMMIT=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
transaction_dir="$TEST_TMPDIR/transaction"
mkdir -p "$transaction_dir"
gh() {{
  printf '%s\n' "$*" >> "$TEST_TMPDIR/gh.log"
  if [[ "$*" == *"git/ref/heads/velnor-publication-lock"* ]]; then
    printf 'HTTP/2 200 OK\r\n\r\n{{}}\n'
    return 0
  fi
  if [[ "$*" == *"--method PUT"* ]]; then
    if [ "$TEST_MODE" = held ]; then
      return 1
    fi
    printf '{{"content":{{"sha":"{lock_sha}"}}}}\n'
    return 0
  fi
  if [[ "$*" == *"contents/.package-release-publication-lock.json?ref=velnor-publication-lock"* ]]; then
    printf '{{"type":"file","path":".package-release-publication-lock.json","sha":"{lock_sha}"}}\n'
    return 0
  fi
  if [[ "$*" == *"--method DELETE"* ]]; then
    : > "$TEST_TMPDIR/released"
    return 0
  fi
  return 1
}}
{lock}
{cleanup}
if [ "$TEST_MODE" = held ]; then
  if acquire_publication_lock; then
    exit 1
  fi
  test "$publication_lock_acquired" -eq 0
  test ! -e "$TEST_TMPDIR/released"
  exit 0
fi
acquire_publication_lock
assert_publication_lock
if [ "$TEST_MODE" = retained-success ]; then
  mark_publication_lock_retain
  clear_publication_lock_retain
fi
cleanup_publication 0
"#,
                lock = lock,
                cleanup = cleanup,
                lock_sha = lock_sha,
            );
            let output = Command::new("bash")
                .arg("-c")
                .arg(script)
                .env("TEST_MODE", mode)
                .env("TEST_TMPDIR", &root)
                .output()
                .expect("run publication lock fixture");
            assert!(
                output.status.success(),
                "publication lock mode {mode} failed:\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let log = std::fs::read_to_string(root.join("gh.log")).expect("lock API log");
            if mode == "success" || mode == "retained-success" {
                assert!(
                    log.contains("Acquire Velnor publication lock example/project:preview:42:3")
                );
                assert!(log.contains("--method DELETE"));
                assert!(log.contains("sha=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
                assert!(log.contains("branch=velnor-publication-lock"));
                assert!(root.join("released").exists());
            } else {
                assert!(log.contains("--method PUT"));
                assert!(!log.contains("--method DELETE"));
                assert!(!root.join("released").exists());
            }
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::too_many_lines, clippy::uninlined_format_args)]
    fn immutable_lock_handoff_retains_failed_mutation_and_exports_sha() {
        use std::process::Command;

        let spec = parse_spec(&Args(&args())).expect("valid fixture");
        let immutable_script = render_immutable_publish_script(&spec);
        let lock_start = immutable_script
            .find("publication_lock_branch=\"")
            .expect("lock initialization");
        let lock_end = immutable_script
            .find("\nimmutable_publication_mutated=0")
            .expect("immutable lock lifecycle boundary");
        let lock = &immutable_script[lock_start..lock_end];
        let cleanup_start = immutable_script
            .find("cleanup_immutable_publication() {")
            .expect("immutable cleanup helper");
        let cleanup_end = immutable_script
            .find("\ntrap 'cleanup_immutable_publication")
            .expect("immutable cleanup trap");
        let cleanup = &immutable_script[cleanup_start..cleanup_end];
        let handoff = immutable_script
            .lines()
            .find(|line| line.starts_with("printf 'VELNOR_PUBLICATION_LOCK_SHA="))
            .expect("lock handoff export");
        let lock_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        for (mode, expected_status) in [
            ("failed-mutation", 1),
            ("prepublish-retain", 1),
            ("handoff", 0),
        ] {
            let root = std::env::temp_dir().join(format!(
                "velnor-immutable-lock-{mode}-{}",
                crate::unique_suffix()
            ));
            std::fs::create_dir_all(&root).expect("create immutable lock fixture");
            let script = format!(
                r#"set -Eeuo pipefail
GITHUB_REPOSITORY=example/project
GITHUB_WORKFLOW=preview
GITHUB_RUN_ID=42
GITHUB_RUN_ATTEMPT=3
VELNOR_PUBLICATION_LOCK_BRANCH=velnor-publication-lock
EXPECTED_SOURCE_COMMIT=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
transaction_dir="$TEST_TMPDIR/transaction"
GITHUB_ENV="$TEST_TMPDIR/github-env"
mkdir -p "$transaction_dir"
gh() {{
  printf '%s\n' "$*" >> "$TEST_TMPDIR/gh.log"
  if [[ "$*" == *"contents/.package-release-publication-lock.json?ref=velnor-publication-lock"* ]]; then
    printf '{{"type":"file","path":".package-release-publication-lock.json","sha":"{lock_sha}"}}\n'
    return 0
  fi
  if [[ "$*" == *"--method DELETE"* ]]; then
    : > "$TEST_TMPDIR/released"
    return 0
  fi
  return 1
}}
{lock}
{cleanup}
immutable_publication_mutated=0
immutable_publication_handoff=0
publication_lock_acquired=1
publication_lock_sha={lock_sha}
if [ "$TEST_MODE" = failed-mutation ]; then
  immutable_publication_mutated=1
elif [ "$TEST_MODE" = prepublish-retain ]; then
  mark_publication_lock_retain
else
  {handoff}
  immutable_publication_handoff=1
fi
set +e
(cleanup_immutable_publication {status})
cleanup_status=$?
set -e
test "$cleanup_status" -eq {status}
test ! -e "$TEST_TMPDIR/released"
"#,
                lock = lock,
                cleanup = cleanup,
                handoff = handoff,
                lock_sha = lock_sha,
                status = expected_status,
            );
            let output = Command::new("bash")
                .arg("-c")
                .arg(script)
                .env("TEST_MODE", mode)
                .env("TEST_TMPDIR", &root)
                .output()
                .expect("run immutable lock fixture");
            assert!(
                output.status.success(),
                "immutable lock mode {mode} failed:\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if mode == "handoff" {
                let exported =
                    std::fs::read_to_string(root.join("github-env")).expect("read lock handoff");
                assert_eq!(
                    exported,
                    format!("VELNOR_PUBLICATION_LOCK_SHA={lock_sha}\n")
                );
            }
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::too_many_lines)]
    fn post_mutation_failures_invoke_rollback_and_restore_previous_release() {
        use std::process::Command;

        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script(
            "\"$published_dir/release-manifest.json\"",
            "  printf '%s\\n' release-manifest.json",
            "  'payload.tar.gz'",
            &verification,
        );
        let mutation_start = rolling_script
            .find("if [ \"$had_release\" = 0 ]; then\n  mutated=1")
            .expect("rolling mutation boundary");
        let post_mutation = &rolling_script[mutation_start..];
        let rollback_start = rolling_script
            .find("rollback() {")
            .expect("rollback helper");
        let rollback_end = rolling_script
            .find("\ntrap 'rollback")
            .expect("rollback trap");
        let rollback = &rolling_script[rollback_start..rollback_end];
        let upload_start = post_mutation
            .find("gh release upload \"$rolling_tag\"")
            .expect("upload mutation");
        let upload_end = post_mutation[upload_start..]
            .find("\n\nrolling_stage_json")
            .map(|offset| upload_start + offset)
            .expect("upload mutation end");
        let upload = &post_mutation[upload_start..upload_end];
        let tag_marker = r#"gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/git/refs/tags/$rolling_tag""#;
        let tag_start = post_mutation.find(tag_marker).expect("tag mutation");
        let tag_end = post_mutation[tag_start..]
            .find("\n  owner_tag_sha")
            .map(|offset| tag_start + offset)
            .expect("tag mutation end");
        let tag = &post_mutation[tag_start..tag_end];
        let release_marker = r#"gh api --method PATCH --repo "$GITHUB_REPOSITORY" "repos/$GITHUB_REPOSITORY/releases/$rolling_release_id" \
  -f "name=$RELEASE_TITLE_PREFIX $candidate_version""#;
        let release_start = post_mutation
            .find(release_marker)
            .expect("release mutation");
        let release_end = post_mutation[release_start..]
            .find("\nowner_draft=false")
            .map(|offset| release_start + offset)
            .expect("release mutation end");
        let release = &post_mutation[release_start..release_end];

        for (mode, mutation) in [("upload", upload), ("tag", tag), ("release", release)] {
            let root = std::env::temp_dir().join(format!(
                "velnor-package-rollback-failure-{mode}-{}",
                crate::unique_suffix()
            ));
            std::fs::create_dir_all(&root).expect("create shell fixture");
            let mut harness = String::from("set -Eeuo pipefail\n");
            harness.push_str(rollback);
            harness.push_str(
                r#"
GITHUB_REPOSITORY=example/project
GH_TOKEN=test-token
RELEASE_PRERELEASE=true
RELEASE_TITLE_PREFIX=Preview
candidate_version=candidate
rolling_tag=preview
rolling_release_id=123
EXPECTED_SOURCE_COMMIT=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
old_tag_sha=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
old_source_commit=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
old_name=old-name
old_body=old-body
old_draft=false
old_prerelease=true
owner_draft=true
owner_tag_sha=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
owner_name=old-name
owner_body=old-body
owner_source_commit=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
had_release=1
mutated=1
transaction_dir="$TEST_TMPDIR/transaction"
owner_assets="$transaction_dir/owner-assets"
old_assets="$TEST_TMPDIR/old-assets"
rollback_dir="$TEST_TMPDIR/rollback"
mkdir -p "$transaction_dir" "$rollback_dir"
: > "$owner_assets"
: > "$old_assets"
assert_rolling_ownership() { return 0; }
assert_publication_lock() { return 0; }
verify_restored_assets() { : > "$TEST_TMPDIR/assets-restored"; return 0; }
remote_tag_sha() {
  if [ -e "$TEST_TMPDIR/tag-published" ]; then
    printf '%s\n' "$EXPECTED_SOURCE_COMMIT"
  else
    printf '%s\n' "$old_tag_sha"
  fi
}
gh() {
  if [[ "$*" == *"git/refs/tags/preview"* ]] && [[ "$*" == *"--method PATCH"* ]]; then
    if [ "$TEST_MODE" = tag ] && [ ! -e "$TEST_TMPDIR/tag-failure-used" ]; then
      : > "$TEST_TMPDIR/tag-failure-used"
      return 1
    fi
    if [ -e "$TEST_TMPDIR/tag-published" ]; then
      rm -f -- "$TEST_TMPDIR/tag-published"
      : > "$TEST_TMPDIR/tag-restored"
    else
      : > "$TEST_TMPDIR/tag-published"
    fi
    return 0
  fi
  if [[ "$*" == *"releases/123"* ]] && [[ "$*" == *"--method PATCH"* ]]; then
    if [ "$TEST_MODE" = release ] && [[ "$*" == *"-F draft=false"* ]] && [ ! -e "$TEST_TMPDIR/release-failure-used" ]; then
      : > "$TEST_TMPDIR/release-failure-used"
      return 1
    fi
    if [[ "$*" == *"name=old-name"* ]]; then
      : > "$TEST_TMPDIR/release-restored"
    fi
    return 0
  fi
  if [[ "$*" == "release upload"* ]]; then
    if [ "$TEST_MODE" = upload ] && [ ! -e "$TEST_TMPDIR/upload-failure-used" ]; then
      : > "$TEST_TMPDIR/upload-failure-used"
      return 1
    fi
    return 0
  fi
  return 0
}
"#,
            );
            harness.push_str("TEST_MODE=");
            harness.push_str(mode);
            harness.push_str(
                r#"
published_dir="$TEST_TMPDIR"
set +e
(
  trap 'rollback "$?"' ERR
"#,
            );
            if mode == "release" {
                harness.push_str("  ");
                harness.push_str(tag);
                harness.push_str(
                    r#"
  owner_tag_sha="$EXPECTED_SOURCE_COMMIT"
  owner_source_commit="$EXPECTED_SOURCE_COMMIT"
"#,
                );
            }
            harness.push_str("  ");
            harness.push_str(mutation);
            harness.push_str(
                r#"
)
rollback_status=$?
set -e
test "$rollback_status" -eq 1
test -e "$TEST_TMPDIR/release-restored"
test -e "$TEST_TMPDIR/assets-restored"
if [ "$TEST_MODE" = release ]; then
  test -e "$TEST_TMPDIR/tag-restored"
else
  test ! -e "$TEST_TMPDIR/tag-restored"
fi
"#,
            );
            let output = Command::new("bash")
                .arg("-c")
                .arg(harness)
                .env("TEST_TMPDIR", &root)
                .output()
                .expect("run rollback failure harness");
            let _ = std::fs::remove_dir_all(&root);
            assert!(
                output.status.success(),
                "{mode} failure did not restore the previous release:\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn malformed_existing_release_validation_fails_inside_negated_call() {
        use std::process::Command;

        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script("", "", "", &verification);
        let helper_start = rolling_script
            .find("validate_existing_rolling_release() {")
            .expect("validation helper");
        let helper_end = rolling_script
            .find("\nassert_rolling_ownership() {")
            .expect("ownership helper");
        let helper = &rolling_script[helper_start..helper_end];
        let source_commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let digest = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let root = std::env::temp_dir().join(format!(
            "velnor-package-malformed-release-{}",
            crate::unique_suffix()
        ));
        let source_dir = root.join("rollback");
        std::fs::create_dir_all(&source_dir).expect("create malformed release fixture");
        let mut script = String::from("set -Eeuo pipefail\n");
        script.push_str(helper);
        write!(
            &mut script,
            r#"
transaction_dir="$TEST_TMPDIR/transaction"
mkdir -p "$transaction_dir"
rolling_tag=preview
RELEASE_PRERELEASE=true
RELEASE_TITLE_PREFIX=Preview
EXPECTED_MANIFEST_SCHEMA=example.consumer-manifest-v1
EXPECTED_SOURCE_REPOSITORY=example/project
EXPECTED_SOURCE_REF=refs/heads/main
VELNOR_PACKAGE_CHANNEL=preview
EXPECTED_SOURCE_COMMIT={source_commit}
old_tag_sha={source_commit}
owner_assets="$TEST_TMPDIR/owner-assets"
: > "$owner_assets"
source_dir="$TEST_TMPDIR/rollback"
digest={digest}
printf '%s\n' payload > "$source_dir/a.tar.gz"
printf '{{"assets":[{{"name":"a.tar.gz","sha256":"%s"}}],"schema":"example.consumer-manifest-v1","source_commit":"{source_commit}","source_ref":"refs/heads/main","source_repository":"example/project","supporting_assets":[],"version":"1.0.0-preview.1+aaaaaaa"}}\n' "$digest" > "$source_dir/release-manifest.json"
printf '{{\n' > "$source_dir/identity.json"
sha256sum() {{ printf '%s  %s\n' "$digest" "$2"; }}
git() {{ return 0; }}
read_rolling_asset_set() {{ : > "$1"; }}
body="$(printf '{{"draft":false,"prerelease":true,"tag_name":"preview","id":123,"name":"Preview 1.0.0-preview.1+aaaaaaa","assets":[{{"id":1,"name":"release-manifest.json","digest":"sha256:%s"}},{{"id":2,"name":"identity.json","digest":"sha256:%s"}},{{"id":3,"name":"a.tar.gz","digest":"sha256:%s"}}]}}' "$digest" "$digest" "$digest")"
if false; then
  :
elif ! validate_existing_rolling_release "$body" "$source_dir" false; then
  validation_failed=1
else
  validation_failed=0
fi
test "$validation_failed" -eq 1
"#,
        )
        .expect("render malformed release validation fixture");
        let output = Command::new("bash")
            .arg("-c")
            .arg(script)
            .env("TEST_TMPDIR", &root)
            .output()
            .expect("run malformed release validation");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "malformed release validation was not fail-closed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn typed_draft_reuses_validated_owner_without_destructive_cleanup() {
        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script("", "", "", &verification);
        assert!(!rolling_script.contains("discard_current_typed_rolling_draft"));
        assert!(!rolling_script.contains("discarding incomplete current-contract rolling draft"));
        assert!(rolling_script.contains("GitHub cannot undelete a release or tag"));
        assert!(rolling_script.contains(
            "owner_draft=\"$old_draft\"\n    owner_tag_sha=\"$old_tag_sha\"\n    owner_name=\"$old_name\""
        ));
        let draft_reuse = rolling_script
            .find("owner_draft=\"$old_draft\"")
            .expect("validated draft owner initialization");
        let existing_mutation = rolling_script
            .find("\nelse\n  mutated=1\n")
            .expect("existing release mutation boundary");
        assert!(draft_reuse < existing_mutation);
    }

    #[cfg(unix)]
    #[test]
    fn rollback_refuses_release_or_tag_sha_drift_before_destructive_calls() {
        use std::process::Command;

        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script("", "", "", &verification);
        let helper_start = rolling_script
            .find("read_rolling_asset_set() {")
            .expect("asset ownership helper");
        let helper_end = rolling_script
            .find("\ntrap 'rollback")
            .expect("rollback trap");
        let helpers = &rolling_script[helper_start..helper_end];
        let source_commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let root = std::env::temp_dir().join(format!(
            "velnor-package-rollback-race-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create shell fixture");

        for mode in ["state", "tag"] {
            let mut script = String::from("set -Eeuo pipefail\n");
            script.push_str(helpers);
            write!(
                &mut script,
                r#"
GITHUB_REPOSITORY=example/project
RELEASE_PRERELEASE=true
rolling_tag=preview
rolling_release_id=123
owner_draft=false
owner_tag_sha={source_commit}
owner_name='Preview 1.0.0-preview.1+aaaaaaa'
owner_body='old-body'
owner_source_commit={source_commit}
had_release=1
mutated=1
old_tag_sha={source_commit}
old_source_commit={source_commit}
old_name="$owner_name"
old_body="$owner_body"
old_draft=false
old_prerelease=true
old_assets="$TEST_TMPDIR/old-assets"
rollback_dir="$TEST_TMPDIR/rollback"
transaction_dir="$TEST_TMPDIR/transaction"
owner_assets="$TEST_TMPDIR/owner-assets"
: > "$old_assets"
: > "$owner_assets"
mkdir -p "$transaction_dir"
remote_tag_sha() {{
  if [ "$TEST_MODE" = tag ]; then
    printf '%s\n' bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
  else
    printf '%s\n' {source_commit}
  fi
}}
gh() {{
  if [[ "$*" == *"releases/123/assets"* ]]; then
    return 0
  fi
  if [[ "$*" == *"releases/123"* ]] && [[ "$*" != *"--method"* ]]; then
    if [ "$TEST_MODE" = state ]; then
      printf '%s\n' '{{"id":123,"draft":false,"prerelease":true,"tag_name":"preview","name":"Foreign","body":"old-body"}}'
    else
      printf '%s\n' '{{"id":123,"draft":false,"prerelease":true,"tag_name":"preview","name":"Preview 1.0.0-preview.1+aaaaaaa","body":"old-body"}}'
    fi
    return 0
  fi
  : > "$TEST_TMPDIR/mutation"
}}
TEST_MODE={mode}
set +e
(rollback 1)
rollback_status=$?
set -e
test "$rollback_status" -eq 1
test ! -e "$TEST_TMPDIR/mutation"
"#
            )
            .expect("render rollback ownership regression shell");
            let output = Command::new("bash")
                .arg("-c")
                .arg(script)
                .env("TEST_TMPDIR", &root)
                .output()
                .expect("run rollback ownership regression");
            assert!(
                output.status.success(),
                "rollback drift mode {mode} was not fail-closed:\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let _ = std::fs::remove_file(root.join("mutation"));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn rollback_refuses_complete_asset_set_drift_before_mutation() {
        use std::process::Command;

        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script("", "", "", &verification);
        let helper_start = rolling_script
            .find("read_rolling_asset_set() {")
            .expect("asset ownership helper");
        let helper_end = rolling_script
            .find("\ntrap 'rollback")
            .expect("rollback trap");
        let helpers = &rolling_script[helper_start..helper_end];
        let source_commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let root = std::env::temp_dir().join(format!(
            "velnor-package-rollback-asset-race-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create shell fixture");
        let mut script = String::from("set -Eeuo pipefail\n");
        script.push_str(helpers);
        write!(
            &mut script,
            r#"
GITHUB_REPOSITORY=example/project
RELEASE_PRERELEASE=true
rolling_tag=preview
rolling_release_id=123
owner_draft=false
owner_tag_sha={source_commit}
owner_name='Preview 1.0.0-preview.1+aaaaaaa'
owner_body='old-body'
owner_source_commit={source_commit}
owner_assets="$TEST_TMPDIR/owner-assets"
old_assets="$TEST_TMPDIR/old-assets"
rollback_dir="$TEST_TMPDIR/rollback"
transaction_dir="$TEST_TMPDIR/transaction"
mkdir -p "$transaction_dir"
: > "$old_assets"
printf '%s\n' '10	candidate.tar.gz	sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' > "$owner_assets"
had_release=1
mutated=1
remote_tag_sha() {{ printf '%s\n' {source_commit}; }}
gh() {{
  if [[ "$*" == *"releases/123/assets"* ]]; then
    printf '%s\n' '11	foreign.tar.gz	sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'
    return 0
  fi
  if [[ "$*" == *"releases/123"* ]] && [[ "$*" != *"--method"* ]]; then
    printf '%s\n' '{{"id":123,"draft":false,"prerelease":true,"tag_name":"preview","name":"Preview 1.0.0-preview.1+aaaaaaa","body":"old-body"}}'
    return 0
  fi
  : > "$TEST_TMPDIR/mutation"
}}
set +e
(rollback 1)
rollback_status=$?
set -e
test "$rollback_status" -eq 1
test ! -e "$TEST_TMPDIR/mutation"
"#,
        )
        .expect("render asset drift regression shell");
        let output = Command::new("bash")
            .arg("-c")
            .arg(script)
            .env("TEST_TMPDIR", &root)
            .output()
            .expect("run asset drift regression");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "rollback asset drift was not fail-closed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn rollback_deletes_candidate_only_asset_before_restoring_previous_set() {
        use std::process::Command;

        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script("", "", "", &verification);
        let helper_start = rolling_script
            .find("asset_was_in_old_set() (")
            .expect("asset membership helper");
        let rollback_end = rolling_script
            .find("\ntrap 'rollback")
            .expect("rollback trap");
        let helpers = &rolling_script[helper_start..rollback_end];
        let root = std::env::temp_dir().join(format!(
            "velnor-package-rollback-candidate-only-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create shell fixture");
        let mut script = String::from("set -Eeuo pipefail\n");
        script.push_str(helpers);
        script.push_str(
            r#"
GITHUB_REPOSITORY=example/project
RELEASE_PRERELEASE=true
rolling_tag=preview
rolling_release_id=123
owner_draft=false
owner_tag_sha=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
owner_name='Preview 1.0.0-preview.1+aaaaaaa'
owner_body='old-body'
owner_source_commit=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
owner_assets="$TEST_TMPDIR/owner-assets"
old_assets="$TEST_TMPDIR/old-assets"
rollback_dir="$TEST_TMPDIR/rollback"
transaction_dir="$TEST_TMPDIR/transaction"
old_tag_sha=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
old_source_commit=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
old_name="$owner_name"
old_body="$owner_body"
old_draft=false
old_prerelease=true
had_release=1
mutated=1
mkdir -p "$transaction_dir"
: > "$old_assets"
printf '%s\n' '7	candidate.tar.gz	sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' > "$owner_assets"
assert_rolling_ownership() { return 0; }
replace_owned_asset_after_delete() {
  printf '%s\t%s\n' "$1" "$2" > "$TEST_TMPDIR/observed-asset"
  test "$1" = 7
  test "$2" = candidate.tar.gz
  : > "$owner_assets"
}
verify_restored_assets() { return 0; }
remote_tag_sha() { printf '%s\n' aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; }
gh() {
  if [[ "$*" == *"releases/assets/7"* ]]; then
    printf '%s\n' deleted >> "$TEST_TMPDIR/deleted"
  fi
  return 0
}
old_assets="$TEST_TMPDIR/missing-old-assets"
set +e
asset_was_in_old_set candidate.tar.gz
asset_status="$?"
set -e
test "$asset_status" -eq 2
old_assets="$TEST_TMPDIR/old-assets"
set +e
(rollback 1)
rollback_status=$?
set -e
test "$rollback_status" -eq 1
test -s "$TEST_TMPDIR/deleted"
test "$(wc -l < "$TEST_TMPDIR/deleted")" -eq 1
printf '%s\t%s\n' 7 candidate.tar.gz > "$TEST_TMPDIR/expected-asset"
cmp -s "$TEST_TMPDIR/expected-asset" "$TEST_TMPDIR/observed-asset"
test ! -s "$owner_assets"
"#,
        );
        let output = Command::new("bash")
            .arg("-c")
            .arg(script)
            .env("TEST_TMPDIR", &root)
            .output()
            .expect("run candidate-only rollback regression");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success(),
            "candidate-only asset was not deleted during rollback:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_tag_lookup_does_not_mask_ls_remote_failure() {
        use std::process::Command;

        let verification = PublishVerification {
            script: "",
            attestation_flags: "",
        };
        let rolling_script = render_rolling_refresh_script("", "", "", &verification);
        let helper_start = rolling_script
            .find("remote_tag_sha() {")
            .expect("tag helper");
        let helper_end = rolling_script
            .find("\nread_rolling_asset_set() {")
            .expect("asset helper");
        let helper = &rolling_script[helper_start..helper_end];
        let root = std::env::temp_dir().join(format!(
            "velnor-package-tag-lookup-{}",
            crate::unique_suffix()
        ));
        std::fs::create_dir_all(&root).expect("create shell fixture");
        let script = format!(
            r"set -Eeuo pipefail
{helper}
git() {{ return 42; }}
if remote_tag_sha preview; then
  exit 1
fi
",
        );
        let output = Command::new("bash")
            .arg("-c")
            .arg(script)
            .current_dir(&root)
            .output()
            .expect("run tag lookup regression");
        let _ = std::fs::remove_dir_all(root);
        assert!(
            output.status.success(),
            "ls-remote failure was masked:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
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

    #[test]
    fn configured_hidden_asset_and_inventory_name_survive_exact_verification() {
        let mut values = args();
        values.insert(
            "payloads".to_owned(),
            toml::Value::Array(vec![toml::Value::String(".asset-names".to_owned())]),
        );
        let spec = parse_spec(&Args(&values)).expect("hidden bare payload is valid");
        assert!(release_asset_names(&spec).contains(&".asset-names".to_owned()));
        let workflow = render_workflow(&render_config(), &spec, "preview.yml");
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
        assert!(immutable.contains("downloaded_assets=\"$transaction_dir/downloaded-assets\""));
        assert!(immutable.contains("find \"$target_dir\" -maxdepth 1 -type f -printf '%f\\n'"));
        assert!(!immutable.contains("$target_dir/.asset-names"));
    }
}
