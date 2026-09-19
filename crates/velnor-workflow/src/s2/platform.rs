//! Unit execution needs, prerequisite products, and the agreed job environment.
//!
//! Providers are deployment detail: a unit never names a runner label. It
//! declares a typed platform, a trust tier, and a capability set, and
//! generation maps each need onto the eligible providers. A need no enabled
//! provider can serve is a generation error that names the unit, the need,
//! and the remedy, never a silently skipped job.
//!
//! The same module carries the prerequisite contract: a unit produces named
//! products through named tasks, and a consumer declares which producer
//! products it needs. Generation compiles each edge into the selection graph
//! (`depends_on`, so producer changes select the consumer transitively) and
//! into prepare commands (local products rebuild on the consumer; explicit
//! artifact products build once on the producer and cross the hosted DAG),
//! with environment flowing from task inputs to job outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::Serialize;

use crate::s2::{GeneratorError, ProjectConfig, Unit, UnitKind};

/// A named build product one unit produces for others: an `XCFramework`
/// bundle, a generated header set, a packed archive. `task` is the repository
/// task that rebuilds it (run through the task runner, never a shell string),
/// and `env` carries the task's outputs — the paths and flags consumers need
/// once the product exists.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct NamedProduct {
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) env: BTreeMap<String, String>,
    /// Optional cross-provider materialization contract. Without this field
    /// the existing local prepare semantics remain in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) artifact: Option<ProductArtifact>,
}

/// A product archive that crosses hosted jobs. The path is restored exactly
/// relative to the repository root; the kind lets the consumer prove that a
/// bundle/file/directory was materialized before its checks run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ProductArtifact {
    pub(crate) path: String,
    pub(crate) kind: ProductArtifactKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ProductArtifactKind {
    File,
    Directory,
    Xcframework,
}

impl ProductArtifactKind {
    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value {
            "file" => Ok(Self::File),
            "directory" => Ok(Self::Directory),
            "xcframework" => Ok(Self::Xcframework),
            other => Err(GeneratorError::usage(format!(
                "product artifact kind `{other}` is unsupported; use `file`, `directory`, or `xcframework`"
            ))),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Xcframework => "xcframework",
        }
    }
}

/// One prerequisite edge: `producer` builds `product` for this consumer.
/// `task` overrides the product's own task for this consumer, and `env`
/// carries the task inputs the consumer's prepare step exports.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct Prerequisite {
    pub(crate) producer: String,
    pub(crate) product: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) env: BTreeMap<String, String>,
}

impl Prerequisite {
    /// The task the consumer's prepare step runs: the edge override when the
    /// consumer declares one, the product's own task otherwise.
    pub(crate) fn effective_task<'a>(&'a self, product: &'a NamedProduct) -> Option<&'a str> {
        self.task.as_deref().or(product.task.as_deref())
    }
}

/// A product or capability name: lowercase, short, shell-safe.
pub(crate) fn valid_product_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

/// A repository task name, as the task runner resolves it: no whitespace, no
/// shell metacharacters, so the prepare step can invoke it without quoting.
pub(crate) fn valid_task_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'/' | b'-')
        })
}

/// An artifact path is a repository-relative, slash-separated path. Reject
/// absolute paths, parent traversal, control bytes, and backslashes before it
/// reaches tar or a generated shell step.
pub(crate) fn valid_artifact_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.starts_with('-')
        && !value.contains('\\')
        && !value.contains("//")
        && !value.chars().any(char::is_control)
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(segment) if !segment.is_empty()))
}

/// Shell preflight shared by the producer and consumer artifact steps. The
/// consumer reruns this census over downloaded bytes before extraction.
#[expect(
    clippy::too_many_lines,
    reason = "the producer and consumer share one executable archive validator"
)]
pub(crate) fn artifact_archive_validation_script() -> &'static str {
    r#"          artifact_parents_are_safe() {
            local relative="$1" current="$GITHUB_WORKSPACE" component index last
            local -a parts
            IFS='/' read -r -a parts <<<"$relative"
            last=$((${#parts[@]} - 1))
            for ((index = 0; index < last; index++)); do
              component="${parts[index]}"
              current="$current/$component"
              if [[ -L "$current" || ( -e "$current" && ! -d "$current" ) ]]; then
                echo "::error::artifact path parent is an existing symlink or non-directory: $current" >&2
                return 1
              fi
            done
          }

          artifact_normalize_path() {
            awk -v value="$1" '
              BEGIN {
                count = split(value, parts, "/")
                output = ""
                for (part_index = 1; part_index <= count; part_index++) {
                  if (parts[part_index] == "" || parts[part_index] == ".") continue
                  if (parts[part_index] == "..") {
                    if (output == "") exit 1
                    if (index(output, "/") == 0) output = ""
                    else sub(/\/[^\/]*$/, "", output)
                    continue
                  }
                  output = output (output == "" ? "" : "/") parts[part_index]
                }
                if (output == "") exit 1
                print output
              }
            '
          }

          artifact_link_target() {
            local link_name="$1" target="$2"
            local candidate normalized
            if [[ -z "$target" || "$target" == /* || "$target" == *$'\n'* || "$target" == *$'\r'* || "$target" == *$'\t'* ]]; then
              return 1
            fi
            if [[ "$link_name" == */* ]]; then
              candidate="${link_name%/*}/$target"
            else
              candidate="$target"
            fi
            normalized="$(artifact_normalize_path "$candidate")" || return 1
            printf '%s\t%s\n' "$link_name" "$normalized"
          }

          artifact_link_target_for() {
            local link_name="$1" links="$2"
            awk -F '\t' -v wanted="$link_name" '
              $1 == wanted { print $2; found = 1; exit }
              END { exit(found ? 0 : 1) }
            ' "$links"
          }

          artifact_entry_exists() {
            local path="$1" entries="$2"
            grep -F -x -q -- "$path" "$entries" || grep -F -x -q -- "$path/" "$entries"
          }

          artifact_resolve_path() {
            local path="$1" entries="$2" links="$3"
            local visited="$links.visited" prefix target suffix
            local steps=0 found_link status=1
            : >"$visited"
            while ((steps < 128)); do
              found_link=0
              prefix="$path"
              while :; do
                if target="$(artifact_link_target_for "$prefix" "$links")"; then
                  found_link=1
                  if grep -F -x -q -- "$prefix" "$visited"; then
                    break 2
                  fi
                  printf '%s\n' "$prefix" >>"$visited"
                  suffix="${path#"$prefix"}"
                  path="$(artifact_normalize_path "$target$suffix")" || break 2
                  steps=$((steps + 1))
                  break
                fi
                if [[ "$prefix" != */* ]]; then
                  break
                fi
                prefix="${prefix%/*}"
              done
              if ((found_link)); then
                continue
              fi
              if artifact_entry_exists "$path" "$entries"; then
                status=0
              fi
              break
            done
            rm -f "$visited"
            ((status == 0))
          }

          validate_artifact_archive() {
            local archive="$1" expected_root="$2" expected_kind="$3"
            local entries="$archive.entries" symlinks="$archive.symlinks" links="$archive.links"
            local entry detail type link_prefix link_name link_target root_type parent
            rm -f "$entries" "$symlinks" "$links"
            : >"$symlinks"
            : >"$links"
            tar -tf "$archive" >"$entries"
            if [[ ! -s "$entries" ]]; then
              echo "::error::artifact archive is empty" >&2
              return 1
            fi
            if ! awk '{ raw = $0; name = raw; sub(/\/+$/, "", name); if (name == "" || raw ~ /\/\// || raw ~ /(^|\/)\.($|\/)/ || raw ~ /[\t\r]/) exit 1 }' "$entries"; then
              echo "::error::artifact archive contains a non-canonical member path" >&2
              return 1
            fi
            if ! awk '{ name = $0; sub(/\/+$/, "", name); if (seen[name]++) exit 1 }' "$entries"; then
              echo "::error::artifact archive contains duplicate member names" >&2
              return 1
            fi
            while IFS= read -r entry; do
              case "$entry" in
                "$expected_root"|"$expected_root/"|"$expected_root/"*) ;;
                *) echo "::error::artifact archive contains unexpected path $entry" >&2; return 1 ;;
              esac
              case "$entry" in
                /*|../*|*/../*|*/..|.) echo "::error::artifact archive traversal entry $entry" >&2; return 1 ;;
              esac
            done <"$entries"
            while IFS= read -r detail; do
              type="${detail:0:1}"
              case "$type" in
                d|-)
                  if [[ "$detail" == *" $expected_root" || "$detail" == *" $expected_root/" ]]; then
                    root_type="$type"
                  fi
                  ;;
                l)
                  link_target="${detail##* -> }"
                  link_prefix="${detail% -> *}"
                  link_name=""
                  while IFS= read -r entry; do
                    if [[ "$link_prefix" == *" $entry" ]]; then
                      link_name="$entry"
                      break
                    fi
                  done <"$entries"
                  if [[ -z "$link_name" ]]; then
                    echo "::error::artifact archive symlink has no member name" >&2
                    return 1
                  fi
                  printf '%s\n' "$link_name" >>"$symlinks"
                  if ! artifact_link_target "$link_name" "$link_target" >>"$links"; then
                    echo "::error::artifact archive symlink escapes its bundle: $link_name -> $link_target" >&2
                    return 1
                  fi
                  ;;
                h|b|c|p|s)
                  echo "::error::artifact archive contains unsupported hardlink or special member type: $detail" >&2
                  return 1
                  ;;
                *)
                  echo "::error::artifact archive contains unknown member type: $detail" >&2
                  return 1
                  ;;
              esac
            done < <(tar -tvf "$archive")
            case "$expected_kind:$root_type" in
              file:-|directory:d|xcframework:d) ;;
              *) echo "::error::artifact root type does not match its declared kind" >&2; return 1 ;;
            esac
            while IFS=$'\t' read -r link_name link_target; do
              if ! artifact_resolve_path "$link_target" "$entries" "$links"; then
                echo "::error::artifact archive symlink escapes its bundle or contains a cycle: $link_name -> $link_target" >&2
                return 1
              fi
            done <"$links"
            while IFS= read -r entry; do
              if grep -F -x -q -- "$entry" "$symlinks"; then
                continue
              fi
              parent="$entry"
              while [[ "$parent" == */* ]]; do
                parent="${parent%/*}"
                if grep -F -x -q -- "$parent" "$symlinks"; then
                  echo "::error::artifact archive contains a member below a symlink: $entry" >&2
                  return 1
                fi
              done
            done <"$entries"
          }
"#
}

/// An environment variable name for unit and task env.
pub(crate) fn valid_env_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// An environment value: anything without control characters, so it renders
/// into YAML and shell prefixes verbatim.
pub(crate) fn valid_env_value(value: &str) -> bool {
    value.len() <= 4096 && !value.chars().any(char::is_control)
}

/// Validate one env map.
///
/// # Errors
/// Returns a usage error naming the offending entry.
pub(crate) fn validate_env(
    env: &BTreeMap<String, String>,
    context: &str,
) -> Result<(), GeneratorError> {
    for (name, value) in env {
        if !valid_env_name(name) {
            return Err(GeneratorError::usage(format!(
                "{context} declares env `{name}`, which is not an environment variable name; use letters, digits, and underscores starting with a letter or underscore"
            )));
        }
        if !valid_env_value(value) {
            return Err(GeneratorError::usage(format!(
                "{context} declares env `{name}` with control characters; keep values to printable text"
            )));
        }
    }
    Ok(())
}

/// Whether a Cargo `[lib] crate-type` list marks an FFI crate: it builds a
/// native static library others link against.
pub(crate) fn is_ffi_crate_type(crate_types: &[String]) -> bool {
    crate_types
        .iter()
        .any(|crate_type| crate_type == "staticlib" || crate_type == "cdylib")
}

/// The task-runner invocation that rebuilds a prerequisite product, with the
/// edge's task inputs exported ahead of it.
pub(crate) fn prepare_command(task: &str, env: &BTreeMap<String, String>) -> String {
    let mut command = String::new();
    for (name, value) in env {
        command.push_str(name);
        command.push('=');
        command.push_str(&crate::s2::shell_quote(value));
        command.push(' ');
    }
    command.push_str("mise run ");
    command.push_str(task);
    command
}

/// Resolve the platform surface over `config`: validate every prerequisite
/// edge and object-transport toggle, compile edges into the selection graph
/// and prepare commands, merge product outputs into consumer env, and reject
/// any placement no enabled lane can serve.
///
/// # Errors
/// Returns a usage error for an edge that names an unknown producer or a
/// product the producer does not declare, for an object-transport toggle on a
/// unit that cannot use it, and for a unit no enabled lane can execute.
pub(crate) fn resolve(config: &mut ProjectConfig) -> Result<(), GeneratorError> {
    validate_mbx_toggles(config)?;
    materialize_prerequisites(config)?;
    Ok(())
}

fn validate_mbx_toggles(config: &ProjectConfig) -> Result<(), GeneratorError> {
    for unit in &config.units {
        if unit.kind != UnitKind::Rust && unit.mbx == Some(true) {
            return Err(GeneratorError::usage(format!(
                "unit `{}` is a {} unit, which never runs under the object transport; `mbx` applies to Rust units only",
                unit.id,
                unit.kind.label(),
            )));
        }
    }
    Ok(())
}

fn find_product<'a>(
    config: &'a ProjectConfig,
    unit_id: &str,
    name: &str,
) -> Option<&'a NamedProduct> {
    config
        .units
        .iter()
        .find(|unit| unit.id == unit_id)
        .and_then(|unit| unit.products.iter().find(|product| product.name == name))
}

fn validate_artifact_prerequisite(
    config: &ProjectConfig,
    unit: &Unit,
    producer: &Unit,
    prerequisite: &Prerequisite,
    product: &NamedProduct,
) -> Result<(), GeneratorError> {
    let Some(artifact) = product.artifact.as_ref() else {
        return Ok(());
    };
    if prerequisite.effective_task(product).is_none() {
        return Err(GeneratorError::usage(format!(
            "unit `{}` requires artifact product `{}:{}` but the producer declares no build task",
            unit.id, prerequisite.producer, prerequisite.product
        )));
    }
    if config.providers.len() != 1
        || !config
            .providers
            .contains(&crate::s2::provider::ProviderId::GithubHosted)
    {
        return Err(GeneratorError::usage(format!(
            "artifact product `{}:{}` requires the GitHub-hosted-only provider set because local providers have no artifact transport; remove local providers from [workflow] providers",
            prerequisite.producer, prerequisite.product
        )));
    }
    if !crate::s2::provider_supports_unit(crate::s2::provider::ProviderId::GithubHosted, producer)
        || !crate::s2::provider_supports_unit(crate::s2::provider::ProviderId::GithubHosted, unit)
    {
        return Err(GeneratorError::usage(format!(
            "artifact product `{}:{}` for unit `{}` requires both producer and consumer to be GitHub-hosted eligible; artifact transfer never falls back to a local provider",
            prerequisite.producer, prerequisite.product, unit.id
        )));
    }
    if artifact.kind == ProductArtifactKind::Xcframework
        && (producer.platform != crate::s2::provider::Platform::MacosArm64
            || unit.platform != crate::s2::provider::Platform::MacosArm64)
    {
        return Err(GeneratorError::usage(format!(
            "XCFramework product `{}:{}` requires Apple-bound producer `{}` and consumer `{}` platform contracts",
            prerequisite.producer, prerequisite.product, producer.id, unit.id
        )));
    }
    if prerequisite.task.is_some() {
        return Err(GeneratorError::usage(format!(
            "artifact prerequisite `{}:{}` cannot override the producer task; declare the canonical artifact-producing task on the producer product",
            prerequisite.producer, prerequisite.product
        )));
    }
    Ok(())
}

fn validate_prerequisite_edges(config: &ProjectConfig) -> Result<(), GeneratorError> {
    let mut artifact_inputs: BTreeMap<(String, String), (String, BTreeMap<String, String>)> =
        BTreeMap::new();
    for unit in &config.units {
        for prerequisite in &unit.prerequisites {
            let Some(producer) = config
                .units
                .iter()
                .find(|candidate| candidate.id == prerequisite.producer)
            else {
                return Err(GeneratorError::usage(format!(
                    "unit `{}` requires product `{}` from `{}`, a unit the repository does not declare; known units: {}",
                    unit.id,
                    prerequisite.product,
                    prerequisite.producer,
                    config
                        .units
                        .iter()
                        .map(|candidate| candidate.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            };
            let Some(product) = producer
                .products
                .iter()
                .find(|product| product.name == prerequisite.product)
            else {
                let offered = producer
                    .products
                    .iter()
                    .map(|product| product.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let offered = if offered.is_empty() {
                    "it declares no products".to_owned()
                } else {
                    format!("it declares: {offered}")
                };
                return Err(GeneratorError::usage(format!(
                    "unit `{}` requires product `{}` from `{}`, which does not produce it; {}",
                    unit.id, prerequisite.product, prerequisite.producer, offered
                )));
            };
            validate_artifact_prerequisite(config, unit, producer, prerequisite, product)?;
            if product.artifact.is_some()
                && let Some(task) = prerequisite.effective_task(product)
            {
                let key = (producer.id.clone(), product.name.clone());
                let candidate = (task.to_owned(), prerequisite.env.clone());
                if let Some(existing) = artifact_inputs.get(&key) {
                    if existing != &candidate {
                        return Err(GeneratorError::usage(format!(
                            "artifact product `{}:{}` is requested with conflicting producer task inputs; one canonical task/input map is required for every consumer",
                            producer.id, product.name
                        )));
                    }
                } else {
                    artifact_inputs.insert(key, candidate);
                }
            }
        }
    }
    validate_prerequisite_dag(config)?;
    Ok(())
}

fn validate_prerequisite_dag(config: &ProjectConfig) -> Result<(), GeneratorError> {
    let ids = config
        .units
        .iter()
        .map(|unit| unit.id.clone())
        .collect::<BTreeSet<_>>();
    let mut indegree = ids
        .iter()
        .map(|id| (id.clone(), 0usize))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing = ids
        .iter()
        .map(|id| (id.clone(), BTreeSet::new()))
        .collect::<BTreeMap<String, BTreeSet<String>>>();
    for unit in &config.units {
        for prerequisite in &unit.prerequisites {
            if outgoing
                .entry(prerequisite.producer.clone())
                .or_default()
                .insert(unit.id.clone())
            {
                *indegree.entry(unit.id.clone()).or_default() += 1;
            }
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(id.clone()))
        .collect::<BTreeSet<_>>();
    let mut visited = 0usize;
    while let Some(id) = ready.pop_first() {
        visited += 1;
        if let Some(children) = outgoing.get(&id) {
            for child in children {
                let Some(degree) = indegree.get_mut(child) else {
                    return Err(GeneratorError::usage(format!(
                        "prerequisite graph references undeclared child unit `{child}`"
                    )));
                };
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(child.clone());
                }
            }
        }
    }
    if visited != ids.len() {
        let involved = indegree
            .into_iter()
            .filter_map(|(id, degree)| (degree != 0).then_some(id))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(GeneratorError::usage(format!(
            "prerequisite graph contains a cycle; producer/consumer edges must be a DAG (involved units: {involved})"
        )));
    }
    Ok(())
}

/// Compile prerequisite edges into `depends_on` (so producer changes select
/// the consumer through the existing transitive closure), prepare commands,
/// and consumer env. Local products keep the old consumer-side prepare step;
/// artifact products run their task in the producer and are transferred by
/// the hosted workflow renderer.
fn materialize_prerequisites(config: &mut ProjectConfig) -> Result<(), GeneratorError> {
    validate_prerequisite_edges(config)?;
    let mut prepared: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut producer_prepared: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut inherited_env: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for unit in &config.units {
        for prerequisite in &unit.prerequisites {
            let Some(product) = find_product(config, &prerequisite.producer, &prerequisite.product)
            else {
                return Err(GeneratorError::usage(format!(
                    "unit `{}` requires product `{}` from `{}`, which does not produce it",
                    unit.id, prerequisite.product, prerequisite.producer
                )));
            };
            edges
                .entry(unit.id.clone())
                .or_default()
                .push(prerequisite.producer.clone());
            for (name, value) in &product.env {
                inherited_env
                    .entry(unit.id.clone())
                    .or_default()
                    .entry(name.clone())
                    .or_insert_with(|| value.clone());
            }
            if let Some(task) = prerequisite.effective_task(product) {
                if product.artifact.is_some() {
                    let commands = producer_prepared
                        .entry(prerequisite.producer.clone())
                        .or_default();
                    let command = prepare_command(task, &prerequisite.env);
                    if !commands.contains(&command) {
                        commands.push(command);
                    }
                } else {
                    prepared
                        .entry(unit.id.clone())
                        .or_default()
                        .push(prepare_command(task, &prerequisite.env));
                }
            }
        }
    }
    for unit in &mut config.units {
        if let Some(producers) = edges.remove(&unit.id) {
            for producer in producers {
                if !unit.depends_on.contains(&producer) {
                    unit.depends_on.push(producer);
                }
            }
        }
        if let Some(env) = inherited_env.remove(&unit.id) {
            for (name, value) in env {
                unit.env.entry(name).or_insert(value);
            }
        }
        if let Some(commands) = prepared.remove(&unit.id) {
            prepend_prepare_commands(unit, &commands);
        }
        if let Some(commands) = producer_prepared.remove(&unit.id) {
            prepend_prepare_commands(unit, &commands);
        }
    }
    Ok(())
}

/// Prepend prepare commands ahead of every command vector the unit runs, so
/// the product rebuilds before the unit's own checks on every provider and in
/// local runs, which read the same serialized vectors.
fn prepend_prepare_commands(unit: &mut Unit, commands: &[String]) {
    let mut pr_commands = commands.to_vec();
    pr_commands.extend(unit.pr_commands.iter().cloned());
    unit.pr_commands = pr_commands;
    let mut full_commands = commands.to_vec();
    full_commands.extend(unit.full_commands.iter().cloned());
    unit.full_commands = full_commands;
    unit.watch.sort();
    unit.watch.dedup();
}

/// The job-level env a collapsed kind workflow agrees on: every member's env
/// merged, failing closed when two members export different values for one
/// name, since the shared job can carry only one.
///
/// # Errors
/// Returns a usage error when members of one kind disagree on a value.
pub(crate) fn agreed_env(
    members: &[&Unit],
    kind: UnitKind,
) -> Result<BTreeMap<String, String>, GeneratorError> {
    let mut agreed = BTreeMap::new();
    for member in members {
        for (name, value) in &member.env {
            match agreed.get(name) {
                None => {
                    agreed.insert(name.clone(), value.clone());
                }
                Some(current) if current == value => {}
                Some(_) => {
                    return Err(GeneratorError::usage(format!(
                        "collapsed {} job cannot render one env block: members disagree on `{name}`; keep per-unit env identical within a kind or split the kind",
                        kind.label(),
                    )));
                }
            }
        }
    }
    Ok(agreed)
}

#[cfg(test)]
mod tests {
    use super::{
        agreed_env, is_ffi_crate_type, prepare_command, valid_artifact_path, valid_env_name,
        valid_env_value, valid_product_name, valid_task_name, NamedProduct, Prerequisite,
    };
    use crate::s2::provider::{Capabilities, Platform, TrustReq};
    use crate::s2::{Unit, UnitKind};

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_err<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error,
        }
    }

    fn unit(id: &str, kind: UnitKind) -> Unit {
        Unit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind,
            root: ".".to_owned(),
            pinned_lockfile: false,
            watch: Vec::new(),
            pr_commands: Vec::new(),
            full_commands: Vec::new(),
            depends_on: Vec::new(),
            cache: None,
            tool_version: None,
            mise_tools: Vec::new(),
            toolchain: None,
            services: Vec::new(),
            trust: TrustReq::UntrustedOk,
            platform: Platform::LinuxX64,
            capabilities: Capabilities::default(),
            workspace_check: false,
            products: Vec::new(),
            prerequisites: Vec::new(),
            docker_contexts: Vec::new(),
            env: std::collections::BTreeMap::new(),
            mbx: None,
            apple_native: None,
            prepared_tools: Vec::new(),
        }
    }

    #[test]
    fn names_reject_shell_metacharacters() {
        assert!(valid_product_name("xcframework"));
        assert!(valid_product_name("sys-headers_v2"));
        assert!(!valid_product_name(""));
        assert!(!valid_product_name("XCFramework"));
        assert!(!valid_product_name("xcode build"));
        assert!(!valid_product_name("x;rm"));
        assert!(valid_task_name("build-xcframework"));
        assert!(valid_task_name("ffi:headers"));
        assert!(!valid_task_name("build xcframework"));
        assert!(!valid_task_name("build;test"));
        assert!(valid_env_name("XCFRAMEWORK_PATH"));
        assert!(!valid_env_name("2FAST"));
        assert!(!valid_env_name("HAS-DASH"));
        assert!(valid_env_value("-C link-arg=-fuse-ld=mold"));
        assert!(!valid_env_value("line\nbreak"));
        assert!(valid_artifact_path("target/xcframework/App.xcframework"));
        assert!(!valid_artifact_path("../outside"));
        assert!(!valid_artifact_path("/tmp/product"));
        assert!(!valid_artifact_path("target\\product"));
        assert!(!valid_artifact_path("target//product"));
    }

    #[test]
    fn ffi_detection_follows_crate_types() {
        assert!(is_ffi_crate_type(&["staticlib".to_owned()]));
        assert!(is_ffi_crate_type(&["rlib".to_owned(), "cdylib".to_owned()]));
        assert!(!is_ffi_crate_type(&["rlib".to_owned()]));
        assert!(!is_ffi_crate_type(&[]));
    }

    #[test]
    fn prepare_exports_task_inputs() {
        let env = std::collections::BTreeMap::from([
            ("B_KEY".to_owned(), "b".to_owned()),
            ("A_KEY".to_owned(), "a b".to_owned()),
        ]);
        assert_eq!(
            prepare_command("build-xcframework", &env),
            "A_KEY='a b' B_KEY='b' mise run build-xcframework"
        );
        assert_eq!(
            prepare_command("build-xcframework", &std::collections::BTreeMap::new()),
            "mise run build-xcframework"
        );
    }

    #[test]
    fn prerequisite_prefers_its_own_task() {
        let product = NamedProduct {
            name: "xcframework".to_owned(),
            task: Some("build-xcframework".to_owned()),
            env: std::collections::BTreeMap::new(),
            artifact: None,
        };
        let plain = Prerequisite {
            producer: "rust-ffi".to_owned(),
            product: "xcframework".to_owned(),
            task: None,
            env: std::collections::BTreeMap::new(),
        };
        assert_eq!(plain.effective_task(&product), Some("build-xcframework"));
        let overridden = Prerequisite {
            task: Some("build-xcframework-device".to_owned()),
            ..plain
        };
        assert_eq!(
            overridden.effective_task(&product),
            Some("build-xcframework-device")
        );
    }

    #[test]
    fn agreed_env_fails_closed_on_conflict() {
        let mut left = unit("left", UnitKind::Swift);
        left.env.insert("KEY".to_owned(), "one".to_owned());
        let mut right = unit("right", UnitKind::Swift);
        right.env.insert("KEY".to_owned(), "two".to_owned());
        let error = must_err(
            agreed_env(&[&left, &right], UnitKind::Swift),
            "conflicting env fails closed",
        );
        assert!(
            error.to_string().contains("`KEY`"),
            "unexpected error: {error}"
        );
        right.env.insert("KEY".to_owned(), "one".to_owned());
        let agreed = must_ok(
            agreed_env(&[&left, &right], UnitKind::Swift),
            "agreeing env merges",
        );
        assert_eq!(agreed.get("KEY").map(String::as_str), Some("one"));
    }
}
