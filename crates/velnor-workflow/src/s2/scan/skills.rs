//! Skills/plugin repository detector.
//!
//! Plugin metadata and skill instructions are validated before generic
//! language detectors run. Template resources owned by discovered components
//! remain inert so examples do not become executable project units.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};
use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use super::{RepositoryShape, ScanContext};
use crate::s2::GeneratorError;

const PORTABLE_MANIFEST: &str = "plugin.json";
const PORTABLE_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
const ANTIGRAVITY_SCHEMA: &str = "https://antigravity.google/schemas/v1/plugin.json";
const CODEX_MANIFEST: &str = ".codex-plugin/plugin.json";
const CLAUDE_MANIFEST: &str = ".claude-plugin/plugin.json";
const CLAUDE_MARKETPLACE: &str = ".claude-plugin/marketplace.json";
const KIMI_MANIFEST: &str = "kimi.plugin.json";
const KIMI_COMPAT_MANIFEST: &str = ".kimi-plugin/plugin.json";
const DEFAULT_SKILLS_ROOT: &str = "skills";
const CODEX_MAX_SKILL_SCAN_DEPTH: usize = 6;
const KIMI_MAX_SKILL_SCAN_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum FrontmatterMode {
    LiveDefinition,
    AntigravityDefinition,
    CodexDefinition,
    ClaudeDefinition,
    ClaudeCommand,
    KimiDefinition,
    KimiFlatDefinition,
    KimiCommand,
    FlatDefinition,
    EmbeddedTemplate,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SkillProvider {
    AgentPlugins,
    Antigravity,
    Codex,
    Claude,
    ClaudeMarketplace,
    Kimi,
}

impl SkillProvider {
    const fn as_str(self) -> &'static str {
        match self {
            Self::AgentPlugins => "agent-plugins",
            Self::Antigravity => "antigravity",
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::ClaudeMarketplace => "claude-marketplace",
            Self::Kimi => "kimi",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Frontmatter {
    values: BTreeMap<String, String>,
    kimi_sub_skills_enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PluginManifest {
    provider: SkillProvider,
    skill_roots: Vec<SkillRoot>,
    command_roots: Vec<CommandRoot>,
    root_skill: Option<SkillRoot>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SkillRoot {
    path: String,
    flat_markdown: bool,
    frontmatter_mode: FrontmatterMode,
    require_skill_directory: bool,
    plugin_root: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CommandRoot {
    path: String,
    plugin_root: String,
    recursive_markdown: bool,
}

#[derive(Default)]
struct DiscoveredSkillFiles {
    skills: BTreeMap<String, BTreeSet<FrontmatterMode>>,
    commands: BTreeMap<String, BTreeSet<FrontmatterMode>>,
    resource_roots: BTreeMap<String, BTreeSet<String>>,
    component_roots: BTreeMap<String, BTreeSet<String>>,
    codex_scan_roots: BTreeMap<String, BTreeSet<String>>,
    blocked_kimi_resource_roots: BTreeMap<String, BTreeSet<String>>,
    root_only_kimi_skills: BTreeSet<String>,
}

#[derive(Default)]
struct ProviderResourceScopes {
    codex: BTreeMap<String, BTreeSet<String>>,
    codex_scan_roots: BTreeMap<String, BTreeSet<String>>,
    kimi: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Default)]
struct MarkdownValidationScopes {
    codex_plugin_roots: BTreeSet<String>,
    other_plugin_roots: BTreeSet<String>,
}

#[derive(Default)]
struct KimiDirectorySkillScan {
    files: BTreeSet<String>,
    blocked_resource_roots: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeclaredPathKind {
    CodexSkill,
    ClaudeSkill,
    ClaudeCommand,
    KimiSkill,
    KimiCommand,
}

/// Detect plugin metadata, validate discovered Skills, and return inert
/// template files to exclude from language detectors.
///
/// Provider manifests are independent inputs. A marketplace catalog, generated
/// documentation bundle, and the other providers' manifests are not required
/// to recognize one valid plugin.
#[expect(
    clippy::too_many_lines,
    reason = "manifest selection, gated discovery, and validation share one accepted scope"
)]
pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<BTreeSet<String>, GeneratorError> {
    let (manifests, manifest_limitations) = read_plugin_manifests(context)?;
    shape.limitations.extend(manifest_limitations);
    if manifests.is_empty() {
        return Ok(BTreeSet::new());
    }
    let codex_repository = manifests
        .iter()
        .any(|manifest| manifest.provider == SkillProvider::Codex)
        .then(|| super::file_walk::CodexRepository::new(context.root))
        .transpose()?;
    let mut files = context.files.to_vec();
    for manifest in manifests
        .iter()
        .filter(|manifest| manifest.provider == SkillProvider::Codex)
    {
        let skill_roots = manifest
            .skill_roots
            .iter()
            .map(|root| root.path.clone())
            .collect::<Vec<_>>();
        let Some(codex_repository) = codex_repository.as_ref() else {
            return Err(GeneratorError::usage(
                "Codex Skills manifest has no repository view",
            ));
        };
        super::file_walk::add_codex_component_files_with_repository(
            codex_repository,
            &skill_roots,
            context.exclude,
            CODEX_MAX_SKILL_SCAN_DEPTH,
            &mut files,
        )?;
    }
    let provider_skill_roots = manifests
        .iter()
        .filter(|manifest| {
            !matches!(
                manifest.provider,
                SkillProvider::Codex | SkillProvider::Kimi
            )
        })
        .flat_map(|manifest| {
            manifest
                .skill_roots
                .iter()
                .map(|root| (root.path.clone(), root.require_skill_directory))
                .chain(
                    manifest
                        .root_skill
                        .iter()
                        .map(|root| (root.path.clone(), false)),
                )
        })
        .collect::<Vec<_>>();
    super::file_walk::add_provider_skill_definition_files(
        context.root,
        &provider_skill_roots,
        context.exclude,
        &mut files,
    )?;
    let provider_command_roots = manifests
        .iter()
        .filter(|manifest| {
            manifest.provider == SkillProvider::ClaudeMarketplace
                || manifest.provider == SkillProvider::Claude
        })
        .flat_map(|manifest| manifest.command_roots.iter())
        .filter(|root| !root.recursive_markdown)
        .map(|root| root.path.clone())
        .collect::<Vec<_>>();
    super::file_walk::add_provider_command_files(
        context.root,
        &provider_command_roots,
        context.exclude,
        &mut files,
    )?;
    for manifest in manifests
        .iter()
        .filter(|manifest| manifest.provider == SkillProvider::Kimi)
    {
        let skill_roots = manifest
            .skill_roots
            .iter()
            .filter(|root| root.flat_markdown)
            .map(|root| root.path.clone())
            .collect::<Vec<_>>();
        let command_roots = manifest
            .command_roots
            .iter()
            .filter(|root| root.recursive_markdown)
            .map(|root| root.path.clone())
            .collect::<Vec<_>>();
        super::file_walk::add_kimi_component_files(
            context.root,
            &skill_roots,
            &command_roots,
            context.exclude,
            KIMI_MAX_SKILL_SCAN_DEPTH,
            &mut files,
        )?;
    }
    let discovered = {
        let mut file_set = context.file_set.clone();
        file_set.extend(files.iter().cloned());
        let discovery_context = ScanContext {
            root: context.root,
            files: &files,
            file_set: &file_set,
            exclude: context.exclude,
        };
        discover_skills(&discovery_context, &manifests)?
    };
    let provider_component_roots = discovered
        .component_roots
        .iter()
        .filter_map(|(definition, roots)| {
            let modes = discovered
                .skills
                .get(definition)
                .or_else(|| discovered.commands.get(definition))?;
            modes
                .iter()
                .any(|mode| {
                    !matches!(
                        mode,
                        FrontmatterMode::CodexDefinition
                            | FrontmatterMode::KimiDefinition
                            | FrontmatterMode::KimiFlatDefinition
                            | FrontmatterMode::KimiCommand
                    )
                })
                .then_some(roots)
        })
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    super::file_walk::add_provider_component_resource_files(
        context.root,
        &provider_component_roots,
        context.exclude,
        &mut files,
    )?;
    let (kimi_skill_resource_roots, kimi_pruned_skill_resource_roots, kimi_command_resource_roots) =
        kimi_component_resource_roots(&discovered);
    for (resource_root, blocked_roots) in &kimi_skill_resource_roots {
        let resource_roots = [resource_root.clone()];
        let skip_roots = blocked_roots.iter().cloned().collect::<Vec<_>>();
        super::file_walk::add_kimi_component_resource_files(
            context.root,
            &resource_roots,
            super::file_walk::KimiResourceKind::Skill,
            context.exclude,
            &skip_roots,
            &mut files,
        )?;
    }
    for resource_root in &kimi_pruned_skill_resource_roots {
        let resource_roots = [resource_root.clone()];
        super::file_walk::add_kimi_component_resource_files(
            context.root,
            &resource_roots,
            super::file_walk::KimiResourceKind::PrunedSkill,
            context.exclude,
            &[],
            &mut files,
        )?;
    }
    if !kimi_command_resource_roots.is_empty() {
        let resource_roots = kimi_command_resource_roots.into_iter().collect::<Vec<_>>();
        super::file_walk::add_kimi_component_resource_files(
            context.root,
            &resource_roots,
            super::file_walk::KimiResourceKind::Command,
            context.exclude,
            &[],
            &mut files,
        )?;
    }
    files.sort();
    files.dedup();
    let mut file_set = context.file_set.clone();
    file_set.extend(files.iter().cloned());
    let context = ScanContext {
        root: context.root,
        files: &files,
        file_set: &file_set,
        exclude: context.exclude,
    };
    validate_skills(&context, &discovered.skills, codex_repository.as_ref())?;
    validate_skills(&context, &discovered.commands, codex_repository.as_ref())?;
    let owned_template_roots = owned_template_roots(&discovered);
    let resource_scopes = provider_resource_scopes(&discovered);
    shape.limitations.push(
        "Markdown link validation covers parsed CommonMark links and images plus enabled tables, strikethrough, task lists, and footnotes; bare GFM autolink literals are not parsed.".to_owned(),
    );
    let frontmatter_modes_by_definition = definition_frontmatter_modes(&discovered);
    if has_unparsed_mdx(
        &context,
        &discovered.component_roots,
        &frontmatter_modes_by_definition,
        &discovered.skills,
        &discovered.blocked_kimi_resource_roots,
        &owned_template_roots,
    ) {
        shape.limitations.push(
            "MDX file content, including Markdown links and JSX URL attributes, is not parsed or link-validated.".to_owned(),
        );
    }
    let (has_unparsed_html_links, linked_template_roots) = validate_links(
        &context,
        &discovered,
        &owned_template_roots,
        &resource_scopes,
        codex_repository.as_ref(),
    )?;
    if has_unparsed_html_links {
        shape.limitations.push(
            "Raw HTML URL attributes in .md skill documents are not link-validated.".to_owned(),
        );
    }
    let template_files = template_files(&context, &owned_template_roots, &linked_template_roots);
    validate_templates(
        &context,
        &template_files,
        &resource_scopes,
        codex_repository.as_ref(),
    )?;
    let mut ignored_skill_files = template_files.clone();
    ignored_skill_files.extend(codex_canonical_template_files(
        &context,
        &template_files,
        &resource_scopes,
        codex_repository.as_ref(),
    )?);
    ignored_skill_files.extend(blocked_kimi_scan_files(&context, &discovered));
    for provider in manifests
        .iter()
        .map(|manifest| manifest.provider)
        .collect::<BTreeSet<_>>()
    {
        shape
            .detected
            .push(format!("skills-provider:{}", provider.as_str()));
    }
    shape.detected.push("skills-plugin".to_owned());
    shape
        .detected
        .push(format!("skills-count:{}", discovered.skills.len()));
    if !discovered.commands.is_empty() {
        shape
            .detected
            .push(format!("commands-count:{}", discovered.commands.len()));
    }
    shape.limitations.push(
        "Skill/plugin metadata and Markdown are validated during repository scanning; no provider-independent CI verification command is generated.".to_owned(),
    );
    Ok(ignored_skill_files)
}

fn blocked_kimi_scan_files(
    context: &ScanContext<'_>,
    discovered: &DiscoveredSkillFiles,
) -> BTreeSet<String> {
    let blocked_roots = discovered
        .blocked_kimi_resource_roots
        .values()
        .flat_map(BTreeSet::iter)
        .collect::<BTreeSet<_>>();
    if blocked_roots.is_empty() {
        return BTreeSet::new();
    }

    // A distinct provider can independently claim the same subtree. Keep that
    // provider's files available to its detector while removing Kimi content
    // hidden by the parent-skill gate from generic project scanning.
    let independently_scanned_roots = discovered
        .component_roots
        .iter()
        .filter_map(|(definition, roots)| {
            let modes = discovered
                .skills
                .get(definition)
                .or_else(|| discovered.commands.get(definition))?;
            let has_other_provider = modes.iter().any(|mode| {
                !matches!(
                    mode,
                    FrontmatterMode::KimiDefinition
                        | FrontmatterMode::KimiFlatDefinition
                        | FrontmatterMode::KimiCommand
                )
            });
            let has_independent_kimi_root = roots.iter().any(|root| {
                blocked_roots
                    .iter()
                    .any(|blocked| path_is_within_root(root, blocked))
            });
            (has_other_provider || has_independent_kimi_root).then_some(roots)
        })
        .flatten()
        .collect::<BTreeSet<_>>();

    context
        .files
        .iter()
        .filter(|file| {
            blocked_roots
                .iter()
                .any(|blocked| path_is_within_root(file, blocked))
                && !independently_scanned_roots
                    .iter()
                    .any(|root| path_is_within_root(file, root))
        })
        .cloned()
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "provider discovery and precedence stay explicit in one place"
)]
fn read_plugin_manifests(
    context: &ScanContext<'_>,
) -> Result<(Vec<PluginManifest>, Vec<String>), GeneratorError> {
    let mut manifests = Vec::new();
    let mut limitations = Vec::new();
    let mut has_portable_manifest = false;
    let mut has_antigravity_manifest = false;
    if context.file_set.contains(PORTABLE_MANIFEST) {
        let object = read_json_object(context, PORTABLE_MANIFEST)?;
        match object.get("$schema") {
            Some(Value::String(schema)) if schema == PORTABLE_SCHEMA => {
                has_portable_manifest = true;
                limitations.extend(validate_portable_manifest(&object)?);
                let mut manifest = default_skill_manifest(SkillProvider::AgentPlugins);
                if super::file_walk::repository_path_is_file(
                    context.root,
                    DEFAULT_SKILLS_ROOT,
                    context.exclude,
                )? {
                    manifest.skill_roots.clear();
                    limitations.push(
                        "Agent Plugins fixed skills root is a file; its Skills component is invalid and was skipped while other plugin components continue.".to_owned(),
                    );
                }
                manifests.push(manifest);
            }
            Some(Value::String(schema)) if schema == ANTIGRAVITY_SCHEMA => {
                validate_antigravity_manifest(&object)?;
                has_antigravity_manifest = true;
                manifests.push(default_skill_manifest(SkillProvider::Antigravity));
                limitations.push(
                    "Antigravity metadata is checked against Velnor's narrow local compatibility contract; full published-schema validation is not performed, and locally accepted fields may fail that schema.".to_owned(),
                );
            }
            None => {
                if has_root_antigravity_skill(context)? {
                    validate_antigravity_manifest(&object)?;
                    has_antigravity_manifest = true;
                    manifests.push(default_skill_manifest(SkillProvider::Antigravity));
                    limitations.push(
                        "Antigravity metadata is checked against Velnor's narrow local compatibility contract; full published-schema validation is not performed, and locally accepted fields may fail that schema.".to_owned(),
                    );
                    limitations.push(
                        "A root plugin.json without $schema is classified as Antigravity only when it fits the local compatibility contract, includes a name, and has a direct skills/<name>/SKILL.md; generic manifests without that layout are not classified.".to_owned(),
                    );
                } else {
                    limitations.push(
                        "A root plugin.json without $schema is not classified as Antigravity unless its local manifest contract and direct skills/<name>/SKILL.md layout identify a Skills plugin.".to_owned(),
                    );
                }
            }
            Some(_) => {
                return Err(GeneratorError::usage(format!(
                    "{PORTABLE_MANIFEST} has an unsupported or invalid $schema"
                )));
            }
        }
    }
    if context.file_set.contains(CODEX_MANIFEST) && !has_portable_manifest {
        let object = read_json_object(context, CODEX_MANIFEST)?;
        validate_codex_manifest(&object, CODEX_MANIFEST)?;
        let declared_roots = object
            .get("skills")
            .filter(|value| !value.is_null())
            .map(|value| {
                parse_declared_paths(
                    value,
                    CODEX_MANIFEST,
                    "skills",
                    DeclaredPathKind::CodexSkill,
                )
            })
            .transpose()?
            .unwrap_or_default();
        let skill_roots = if declared_roots.is_empty() {
            vec![SkillRoot {
                path: DEFAULT_SKILLS_ROOT.to_owned(),
                flat_markdown: false,
                frontmatter_mode: FrontmatterMode::CodexDefinition,
                require_skill_directory: false,
                plugin_root: ".".to_owned(),
            }]
        } else {
            declared_roots
                .into_iter()
                .map(|path| SkillRoot {
                    path,
                    flat_markdown: false,
                    frontmatter_mode: FrontmatterMode::CodexDefinition,
                    require_skill_directory: false,
                    plugin_root: ".".to_owned(),
                })
                .collect()
        };
        manifests.push(PluginManifest {
            provider: SkillProvider::Codex,
            skill_roots,
            command_roots: Vec::new(),
            root_skill: None,
        });
    }

    if context.file_set.contains(CLAUDE_MANIFEST) {
        let object = read_json_object(context, CLAUDE_MANIFEST)?;
        validate_named_manifest(&object, CLAUDE_MANIFEST, ManifestNameKind::Claude)?;
        record_uninspected_claude_components(&object, CLAUDE_MANIFEST, &mut limitations);
        manifests.push(claude_manifest(&object, context)?);
    }
    if context.file_set.contains(CLAUDE_MARKETPLACE) {
        let object = read_json_object(context, CLAUDE_MARKETPLACE)?;
        let marketplace_manifests =
            claude_marketplace_manifests(context, &object, &mut limitations)?;
        manifests.extend(marketplace_manifests);
    }
    if !context.file_set.contains(CLAUDE_MANIFEST)
        && !has_explicit_non_claude_provider(context)
        && !has_antigravity_manifest
        && (has_claude_default_components(context)
            || (context.file_set.contains("SKILL.md")
                && ![
                    PORTABLE_MANIFEST,
                    CODEX_MANIFEST,
                    KIMI_MANIFEST,
                    KIMI_COMPAT_MANIFEST,
                ]
                .iter()
                .any(|path| context.file_set.contains(*path))))
    {
        // Claude plugin.json is optional. Default-layout skill and command
        // components remain recognizable when no manifest is present.
        manifests.push(claude_manifest(&serde_json::Map::new(), context)?);
    }

    // The root Kimi manifest supersedes its compatibility-location manifest.
    // The root manifest takes precedence over its compatibility location.
    let kimi_path = if context.file_set.contains(KIMI_MANIFEST) {
        Some((KIMI_MANIFEST, SkillProvider::Kimi))
    } else if context.file_set.contains(KIMI_COMPAT_MANIFEST) {
        Some((KIMI_COMPAT_MANIFEST, SkillProvider::Kimi))
    } else {
        None
    };
    if let Some((path, provider)) = kimi_path {
        let object = read_json_object(context, path)?;
        validate_named_manifest(&object, path, ManifestNameKind::Kimi)?;
        let (skill_roots, root_skill) = if let Some(value) = object.get("skills") {
            let mut skill_roots = Vec::new();
            for skill_root in parse_declared_skill_roots(value, path, DeclaredPathKind::KimiSkill)?
            {
                let is_file = super::file_walk::repository_path_is_file(
                    context.root,
                    &skill_root,
                    context.exclude,
                )?;
                if context.file_set.contains(&skill_root) || is_file {
                    limitations.push(format!(
                        "Kimi plugin {path} ignores Skills path {skill_root} because the current CLI accepts directory roots only."
                    ));
                } else {
                    skill_roots.push(SkillRoot {
                        path: skill_root,
                        flat_markdown: true,
                        frontmatter_mode: FrontmatterMode::KimiDefinition,
                        require_skill_directory: false,
                        plugin_root: ".".to_owned(),
                    });
                }
            }
            (skill_roots, None)
        } else {
            (
                Vec::new(),
                context.file_set.contains("SKILL.md").then(|| SkillRoot {
                    path: "SKILL.md".to_owned(),
                    flat_markdown: false,
                    frontmatter_mode: FrontmatterMode::KimiDefinition,
                    require_skill_directory: false,
                    plugin_root: ".".to_owned(),
                }),
            )
        };
        let command_roots = object
            .get("commands")
            .map(|value| parse_kimi_command_roots(value, path, &mut limitations))
            .unwrap_or_default();
        manifests.push(PluginManifest {
            provider,
            skill_roots,
            command_roots,
            root_skill,
        });
    }
    Ok((manifests, limitations))
}

fn has_explicit_non_claude_provider(context: &ScanContext<'_>) -> bool {
    [
        PORTABLE_MANIFEST,
        CODEX_MANIFEST,
        KIMI_MANIFEST,
        KIMI_COMPAT_MANIFEST,
    ]
    .iter()
    .any(|path| context.file_set.contains(*path))
}

fn has_root_antigravity_skill(context: &ScanContext<'_>) -> Result<bool, GeneratorError> {
    if context.file_set.iter().any(|path| {
        path.strip_prefix("skills/").is_some_and(|relative| {
            relative
                .split_once('/')
                .is_some_and(|(skill_directory, file)| {
                    !skill_directory.is_empty() && file == "SKILL.md"
                })
        })
    }) {
        return Ok(true);
    }
    let mut recovered = Vec::new();
    super::file_walk::add_provider_skill_definition_files(
        context.root,
        &[(DEFAULT_SKILLS_ROOT.to_owned(), true)],
        context.exclude,
        &mut recovered,
    )?;
    Ok(recovered.iter().any(|path| {
        path.strip_prefix("skills/").is_some_and(|relative| {
            relative
                .split_once('/')
                .is_some_and(|(skill_directory, file)| {
                    !skill_directory.is_empty() && file == "SKILL.md"
                })
        })
    }))
}

fn default_skill_manifest(provider: SkillProvider) -> PluginManifest {
    let frontmatter_mode = if provider == SkillProvider::Antigravity {
        FrontmatterMode::AntigravityDefinition
    } else {
        FrontmatterMode::LiveDefinition
    };
    PluginManifest {
        provider,
        skill_roots: vec![SkillRoot {
            path: DEFAULT_SKILLS_ROOT.to_owned(),
            flat_markdown: false,
            frontmatter_mode,
            require_skill_directory: true,
            plugin_root: ".".to_owned(),
        }],
        command_roots: Vec::new(),
        root_skill: None,
    }
}

fn join_plugin_root(plugin_root: &str, component: &str) -> String {
    if component == "." {
        plugin_root.to_owned()
    } else if plugin_root == "." {
        component.to_owned()
    } else {
        format!("{plugin_root}/{component}")
    }
}

#[derive(Clone, Copy)]
enum ManifestNameKind {
    Claude,
    Kimi,
    Portable,
}

fn validate_codex_manifest(
    object: &serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), GeneratorError> {
    if let Some(name) = object.get("name")
        && !name.is_string()
    {
        return Err(GeneratorError::usage(format!(
            "{path} name must be a string when present"
        )));
    }
    if let Some(version) = object.get("version")
        && !version.is_null()
        && !version.is_string()
    {
        return Err(GeneratorError::usage(format!(
            "{path} version must be a string when present"
        )));
    }
    Ok(())
}

fn validate_named_manifest(
    object: &serde_json::Map<String, Value>,
    path: &str,
    name_kind: ManifestNameKind,
) -> Result<(), GeneratorError> {
    let raw_name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| GeneratorError::usage(format!("{path} requires string field `name`")))?;
    let name = if matches!(name_kind, ManifestNameKind::Kimi) {
        raw_name.trim()
    } else {
        raw_name
    };
    if !matches!(name_kind, ManifestNameKind::Kimi)
        && let Some(version) = object.get("version")
        && !version.is_string()
    {
        return Err(GeneratorError::usage(format!(
            "{path} version must be a string when present"
        )));
    }
    let valid = match name_kind {
        ManifestNameKind::Claude => valid_claude_name(name),
        ManifestNameKind::Kimi => valid_kimi_name(name),
        ManifestNameKind::Portable => valid_portable_name(name),
    };
    if !valid {
        return Err(GeneratorError::usage(format!(
            "{path} name does not match the provider's identifier rules"
        )));
    }
    Ok(())
}

fn valid_claude_name(name: &str) -> bool {
    valid_kebab_name(name) && name.as_bytes()[0].is_ascii_lowercase()
}

fn valid_kebab_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
        && !name.contains("--")
}

fn valid_portable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
        && !name.contains("--")
        && !name.contains("..")
}

fn valid_kimi_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
        && name.as_bytes()[0].is_ascii_alphanumeric()
}

fn validate_portable_manifest(
    object: &serde_json::Map<String, Value>,
) -> Result<Vec<String>, GeneratorError> {
    const MANIFEST: &str = PORTABLE_MANIFEST;
    let schema = string_field(object, "$schema", MANIFEST)?;
    if schema != PORTABLE_SCHEMA {
        return Err(GeneratorError::usage(format!(
            "{MANIFEST} $schema must equal {PORTABLE_SCHEMA}"
        )));
    }
    validate_named_manifest(object, MANIFEST, ManifestNameKind::Portable)?;
    let unknown = object
        .keys()
        .filter(|key| {
            !matches!(
                key.as_str(),
                "$schema"
                    | "name"
                    | "version"
                    | "description"
                    | "author"
                    | "homepage"
                    | "repository"
                    | "license"
                    | "keywords"
                    | "extensions"
            )
        })
        .map(|field| {
            format!(
                "Agent Plugins field `{field}` is outside the published schema and is unsupported; it is not used for plugin discovery."
            )
        })
        .collect::<Vec<_>>();
    for key in ["description", "homepage", "repository", "license"] {
        if let Some(value) = object.get(key)
            && !value.is_string()
        {
            return Err(GeneratorError::usage(format!(
                "{MANIFEST} {key} must be a string when present"
            )));
        }
    }
    if let Some(author) = object.get("author")
        && !author.as_object().is_some_and(|author| {
            author
                .keys()
                .all(|key| matches!(key.as_str(), "name" | "email" | "url"))
                && author.values().all(Value::is_string)
        })
    {
        return Err(GeneratorError::usage(format!(
            "{MANIFEST} author must be an object with optional string name, email, and url fields"
        )));
    }
    if let Some(keywords) = object.get("keywords")
        && !keywords
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
    {
        return Err(GeneratorError::usage(format!(
            "{MANIFEST} keywords must be an array of strings"
        )));
    }
    let mut limitations = unknown;
    if let Some(extensions) = object.get("extensions") {
        if let Some(values) = extensions.as_object() {
            if !values.values().all(Value::is_object) {
                return Err(GeneratorError::usage(format!(
                    "{MANIFEST} extensions entries must be objects"
                )));
            }
        } else {
            limitations.push(
                "Agent Plugins `extensions` is not an object and is ignored while component discovery continues, as required by the published specification.".to_owned(),
            );
        }
    }
    Ok(limitations)
}

fn validate_antigravity_manifest(
    object: &serde_json::Map<String, Value>,
) -> Result<(), GeneratorError> {
    const MANIFEST: &str = PORTABLE_MANIFEST;
    if let Some(schema) = object.get("$schema")
        && schema.as_str() != Some(ANTIGRAVITY_SCHEMA)
    {
        return Err(GeneratorError::usage(format!(
            "{MANIFEST} $schema must equal {ANTIGRAVITY_SCHEMA}"
        )));
    }
    if object.get("$schema").is_none() && !object.contains_key("name") {
        return Err(GeneratorError::usage(format!(
            "{MANIFEST} must identify Antigravity with $schema or name"
        )));
    }
    if let Some(name) = object.get("name") {
        let name = name.as_str().ok_or_else(|| {
            GeneratorError::usage(format!("{MANIFEST} name must be a string when present"))
        })?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(GeneratorError::usage(format!(
                "{MANIFEST} Antigravity name must contain only ASCII letters, digits, hyphens, and underscores"
            )));
        }
    }
    if let Some(description) = object.get("description")
        && !description.is_string()
    {
        return Err(GeneratorError::usage(format!(
            "{MANIFEST} description must be a string when present"
        )));
    }
    for key in ["homepage", "repository"] {
        if let Some(value) = object.get(key)
            && !value.is_string()
        {
            return Err(GeneratorError::usage(format!(
                "{MANIFEST} {key} must be a string when present"
            )));
        }
    }
    if let Some(keywords) = object.get("keywords")
        && !keywords
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
    {
        return Err(GeneratorError::usage(format!(
            "{MANIFEST} keywords must be an array of strings when present"
        )));
    }
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "$schema" | "name" | "description" | "homepage" | "repository" | "keywords"
        ) {
            return Err(GeneratorError::usage(format!(
                "{MANIFEST} contains unsupported Antigravity field `{key}`"
            )));
        }
    }
    Ok(())
}

fn record_uninspected_claude_components(
    object: &serde_json::Map<String, Value>,
    source: &str,
    limitations: &mut Vec<String>,
) {
    for field in [
        "agents",
        "hooks",
        "mcpServers",
        "lspServers",
        "outputStyles",
        "workflows",
        "channels",
        "themes",
        "monitors",
    ] {
        if object.contains_key(field) {
            limitations.push(format!(
                "Claude {source} field `{field}` is not inspected by Skills scanning."
            ));
        }
    }
    if let Some(experimental) = object.get("experimental").and_then(Value::as_object) {
        for field in ["themes", "monitors"] {
            if experimental.contains_key(field) {
                limitations.push(format!(
                    "Claude {source} field `experimental.{field}` is not inspected by Skills scanning."
                ));
            }
        }
    }
}

fn claude_manifest_declares_components(object: &serde_json::Map<String, Value>) -> bool {
    [
        "skills",
        "commands",
        "agents",
        "hooks",
        "mcpServers",
        "lspServers",
        "outputStyles",
        "workflows",
        "channels",
        "themes",
        "monitors",
    ]
    .iter()
    .any(|field| object.contains_key(*field))
        || object
            .get("experimental")
            .and_then(Value::as_object)
            .is_some_and(|experimental| {
                ["themes", "monitors"]
                    .iter()
                    .any(|field| experimental.contains_key(*field))
            })
}

fn claude_manifest(
    object: &serde_json::Map<String, Value>,
    context: &ScanContext<'_>,
) -> Result<PluginManifest, GeneratorError> {
    claude_manifest_at(
        object,
        context,
        ".",
        true,
        true,
        CLAUDE_MANIFEST,
        SkillProvider::Claude,
    )
}

fn claude_manifest_at(
    object: &serde_json::Map<String, Value>,
    context: &ScanContext<'_>,
    plugin_root: &str,
    include_default_skills: bool,
    include_default_commands: bool,
    manifest_path: &str,
    provider: SkillProvider,
) -> Result<PluginManifest, GeneratorError> {
    let declared_skill_paths = object
        .get("skills")
        .map(|value| {
            parse_declared_paths(
                value,
                manifest_path,
                "skills",
                DeclaredPathKind::ClaudeSkill,
            )
        })
        .transpose()?
        .unwrap_or_default();
    let has_declared_skills = object.contains_key("skills");
    let default_skills_root = join_plugin_root(plugin_root, DEFAULT_SKILLS_ROOT);
    let has_declared_skills_for_fallback = claude_marketplace_skill_fallbacks(
        object,
        context,
        plugin_root,
        provider,
        &declared_skill_paths,
    );
    let mut skill_roots = if include_default_skills {
        vec![SkillRoot {
            path: default_skills_root.clone(),
            flat_markdown: false,
            frontmatter_mode: FrontmatterMode::ClaudeDefinition,
            require_skill_directory: true,
            plugin_root: plugin_root.to_owned(),
        }]
    } else {
        Vec::new()
    };
    if has_declared_skills {
        skill_roots.extend(declared_skill_paths.into_iter().map(|path| SkillRoot {
            path: join_plugin_root(plugin_root, &path),
            flat_markdown: false,
            frontmatter_mode: FrontmatterMode::ClaudeDefinition,
            require_skill_directory: false,
            plugin_root: plugin_root.to_owned(),
        }));
    }
    let command_roots = if let Some(value) = object.get("commands") {
        parse_declared_paths(
            value,
            manifest_path,
            "commands",
            DeclaredPathKind::ClaudeCommand,
        )?
        .into_iter()
        .map(|path| CommandRoot {
            path: join_plugin_root(plugin_root, &path),
            plugin_root: plugin_root.to_owned(),
            recursive_markdown: false,
        })
        .collect()
    } else if include_default_commands {
        vec![CommandRoot {
            path: join_plugin_root(plugin_root, "commands"),
            plugin_root: plugin_root.to_owned(),
            recursive_markdown: false,
        }]
    } else {
        Vec::new()
    };
    let has_default_skills = context
        .files
        .iter()
        .any(|file| file.starts_with(&format!("{default_skills_root}/")))
        || has_default_skills_directory(context, &default_skills_root)?;
    let root_skill = (!has_declared_skills_for_fallback
        && !has_default_skills
        && context
            .file_set
            .contains(&join_plugin_root(plugin_root, "SKILL.md")))
    .then(|| SkillRoot {
        path: plugin_root.to_owned(),
        flat_markdown: false,
        frontmatter_mode: FrontmatterMode::ClaudeDefinition,
        require_skill_directory: false,
        plugin_root: plugin_root.to_owned(),
    });
    Ok(PluginManifest {
        provider,
        skill_roots,
        command_roots,
        root_skill,
    })
}

fn claude_marketplace_skill_fallbacks(
    object: &serde_json::Map<String, Value>,
    context: &ScanContext<'_>,
    plugin_root: &str,
    provider: SkillProvider,
    declared_skill_paths: &[String],
) -> bool {
    let has_declared_skills = object.contains_key("skills");
    if provider != SkillProvider::ClaudeMarketplace || plugin_root != "." || !has_declared_skills {
        return has_declared_skills;
    }

    declared_skill_paths.iter().any(|path| {
        let path = join_plugin_root(plugin_root, path);
        context
            .files
            .iter()
            .any(|file| file == &path || file.starts_with(&format!("{path}/")))
    })
}

fn has_default_skills_directory(
    context: &ScanContext<'_>,
    relative: &str,
) -> Result<bool, GeneratorError> {
    let prefix = format!("{relative}/");
    if context.files.iter().any(|file| file.starts_with(&prefix)) {
        return Ok(true);
    }
    // Git scans use the index as their complete input surface. An untracked
    // empty directory in the checkout must not change root-skill fallback.
    if super::file_walk::is_git_work_tree(context.root) {
        return Ok(false);
    }
    let path = repository_path(context, relative, "default Claude skills root")?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(GeneratorError::io(
            "inspect default Claude skills root",
            &path,
            &error,
        )),
    }
}

fn claude_marketplace_entry_includes_default_skills(
    context: &ScanContext<'_>,
    plugin: &serde_json::Map<String, Value>,
    source_root: &str,
) -> Result<bool, GeneratorError> {
    if source_root != "." {
        return Ok(true);
    }
    let Some(value) = plugin.get("skills") else {
        return Ok(true);
    };
    let declared_paths = parse_declared_paths(
        value,
        CLAUDE_MARKETPLACE,
        "skills",
        DeclaredPathKind::ClaudeSkill,
    )?;
    let declares_full_marketplace_scan = declared_paths
        .iter()
        .any(|path| path == "." || path == DEFAULT_SKILLS_ROOT);
    let declared_paths_exist = declared_paths.iter().any(|path| {
        path == "."
            || context
                .files
                .iter()
                .any(|file| file == path || file.starts_with(&format!("{path}/")))
    });
    Ok(!declared_paths_exist || declares_full_marketplace_scan)
}

fn claude_marketplace_plugin_root(
    marketplace: &serde_json::Map<String, Value>,
) -> Result<Option<String>, GeneratorError> {
    let Some(metadata) = marketplace.get("metadata") else {
        return Ok(None);
    };
    let metadata = metadata.as_object().ok_or_else(|| {
        GeneratorError::usage(format!("{CLAUDE_MARKETPLACE} metadata must be an object"))
    })?;
    let Some(plugin_root) = metadata.get("pluginRoot") else {
        return Ok(None);
    };
    let plugin_root = plugin_root.as_str().ok_or_else(|| {
        GeneratorError::usage(format!(
            "{CLAUDE_MARKETPLACE} metadata.pluginRoot must be a relative path string"
        ))
    })?;
    normalize_skill_root(
        plugin_root,
        CLAUDE_MARKETPLACE,
        DeclaredPathKind::ClaudeSkill,
    )
    .map(Some)
}

fn claude_marketplace_source_root(
    source: &str,
    plugin_root: Option<&str>,
    name: &str,
) -> Result<Option<String>, GeneratorError> {
    if source == "." {
        return Err(GeneratorError::usage(format!(
            "{CLAUDE_MARKETPLACE} plugin {name} source `.` is invalid; use `./` for the marketplace root"
        )));
    }
    if source.starts_with("./") {
        return normalize_skill_root(source, CLAUDE_MARKETPLACE, DeclaredPathKind::ClaudeSkill)
            .map(Some);
    }
    if source.contains('/') {
        return Err(GeneratorError::usage(format!(
            "{CLAUDE_MARKETPLACE} plugin {name} source containing `/` must start with `./`"
        )));
    }
    let is_local_name = !matches!(source, "" | "." | "..")
        && !source.contains('/')
        && !source.contains('\\')
        && !source.chars().any(char::is_control)
        && !is_windows_drive_path(source);
    if !is_local_name {
        return Err(GeneratorError::usage(format!(
            "{CLAUDE_MARKETPLACE} plugin {name} source must be `./` path or a bare directory name"
        )));
    }
    let Some(plugin_root) = plugin_root else {
        return Err(GeneratorError::usage(format!(
            "{CLAUDE_MARKETPLACE} plugin {name} bare source `{source}` requires metadata.pluginRoot"
        )));
    };
    Ok(Some(join_plugin_root(plugin_root, source)))
}

fn merge_marketplace_component_paths(
    manifest: &mut serde_json::Map<String, Value>,
    entry: &serde_json::Map<String, Value>,
    field: &str,
    plugin_name: &str,
) -> Result<(), GeneratorError> {
    let Some(entry_paths) = entry.get(field) else {
        return Ok(());
    };
    let mut paths = marketplace_component_path_values(manifest.get(field), field, plugin_name)?;
    paths.extend(marketplace_component_path_values(
        Some(entry_paths),
        field,
        plugin_name,
    )?);
    manifest.insert(field.to_owned(), Value::Array(paths));
    Ok(())
}

fn marketplace_component_path_values(
    value: Option<&Value>,
    field: &str,
    plugin_name: &str,
) -> Result<Vec<Value>, GeneratorError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    match value {
        Value::String(_) => Ok(vec![value.clone()]),
        Value::Array(values) if values.iter().all(Value::is_string) => Ok(values.clone()),
        _ => Err(GeneratorError::usage(format!(
            "{CLAUDE_MARKETPLACE} plugin {plugin_name} {field} must be a path string or an array of path strings"
        ))),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "marketplace source and strict-mode decisions form one contract"
)]
fn claude_marketplace_manifests(
    context: &ScanContext<'_>,
    marketplace: &serde_json::Map<String, Value>,
    limitations: &mut Vec<String>,
) -> Result<Vec<PluginManifest>, GeneratorError> {
    if let Some(value) = marketplace.get("name") {
        let name = value.as_str().ok_or_else(|| {
            GeneratorError::usage(format!(
                "{CLAUDE_MARKETPLACE} name must be a kebab-case string"
            ))
        })?;
        if !valid_claude_name(name) {
            return Err(GeneratorError::usage(format!(
                "{CLAUDE_MARKETPLACE} name must be a kebab-case identifier without path separators, control characters, or bidi formatting"
            )));
        }
    }
    let plugins = marketplace
        .get("plugins")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GeneratorError::usage(format!("{CLAUDE_MARKETPLACE} plugins must be an array"))
        })?;
    let marketplace_plugin_root = claude_marketplace_plugin_root(marketplace)?;
    let mut manifests = Vec::new();
    let mut plugin_names = BTreeSet::new();
    for (index, plugin) in plugins.iter().enumerate() {
        let Some(plugin) = plugin.as_object() else {
            return Err(GeneratorError::usage(format!(
                "{CLAUDE_MARKETPLACE} plugins[{index}] must be an object"
            )));
        };
        let name = plugin.get("name").and_then(Value::as_str).ok_or_else(|| {
            GeneratorError::usage(format!(
                "{CLAUDE_MARKETPLACE} plugins[{index}] name must be a non-empty string"
            ))
        })?;
        if !valid_claude_name(name) {
            return Err(GeneratorError::usage(format!(
                "{CLAUDE_MARKETPLACE} plugin name {name:?} must be a kebab-case identifier without path separators, control characters, or bidi formatting"
            )));
        }
        if !plugin_names.insert(name.to_owned()) {
            return Err(GeneratorError::usage(format!(
                "{CLAUDE_MARKETPLACE} contains duplicate plugin name {name}"
            )));
        }
        let strict = plugin
            .get("strict")
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "{CLAUDE_MARKETPLACE} plugin {name} strict must be a boolean"
                    ))
                })
            })
            .transpose()?
            .unwrap_or(true);
        let source_value = plugin.get("source").ok_or_else(|| {
            GeneratorError::usage(format!(
                "{CLAUDE_MARKETPLACE} plugin {name} source must be a string or an object"
            ))
        })?;
        let source = match source_value {
            Value::String(source) => source.as_str(),
            Value::Object(_) => {
                record_uninspected_claude_components(
                    plugin,
                    &format!("marketplace plugin `{name}`"),
                    limitations,
                );
                limitations.push(format!(
                    "Claude marketplace plugin `{name}` has an object source; its component files were not inspected."
                ));
                continue;
            }
            _ => {
                return Err(GeneratorError::usage(format!(
                    "{CLAUDE_MARKETPLACE} plugin {name} source must be a string or an object"
                )));
            }
        };
        record_uninspected_claude_components(
            plugin,
            &format!("marketplace plugin `{name}`"),
            limitations,
        );
        if source == ".."
            || source.starts_with("../")
            || source.starts_with('/')
            || source.contains('\\')
            || source.chars().any(char::is_control)
            || is_windows_drive_path(source)
        {
            return Err(GeneratorError::usage(format!(
                "{CLAUDE_MARKETPLACE} plugin {name} source must stay inside the repository"
            )));
        }
        let Some(source_root) =
            claude_marketplace_source_root(source, marketplace_plugin_root.as_deref(), name)?
        else {
            continue;
        };
        let source_path = repository_path(context, &source_root, "Claude marketplace source")?;
        let metadata = fs::symlink_metadata(&source_path).map_err(|error| {
            GeneratorError::io("inspect Claude marketplace source", &source_path, &error)
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(GeneratorError::usage(format!(
                "{CLAUDE_MARKETPLACE} plugin {name} source is not a directory"
            )));
        }

        let manifest_path = join_plugin_root(&source_root, CLAUDE_MANIFEST);
        let has_manifest = context.file_set.contains(&manifest_path);
        let entry_components = if has_manifest {
            let object = read_json_object(context, &manifest_path)?;
            validate_named_manifest(&object, &manifest_path, ManifestNameKind::Claude)?;
            record_uninspected_claude_components(&object, &manifest_path, limitations);
            if !strict && claude_manifest_declares_components(&object) {
                return Err(GeneratorError::usage(format!(
                    "{CLAUDE_MARKETPLACE} plugin {name} sets strict:false but its plugin manifest declares components"
                )));
            }
            if strict {
                let mut object = object;
                for field in ["skills", "commands"] {
                    merge_marketplace_component_paths(&mut object, plugin, field, name)?;
                }
                object
            } else {
                plugin.clone()
            }
        } else if strict {
            return Err(GeneratorError::usage(format!(
                "Claude marketplace plugin `{name}` is strict but has no local {CLAUDE_MANIFEST}"
            )));
        } else {
            plugin.clone()
        };
        let include_default_skills =
            claude_marketplace_entry_includes_default_skills(context, plugin, &source_root)?;
        let manifest = claude_manifest_at(
            &entry_components,
            context,
            &source_root,
            include_default_skills,
            true,
            if strict {
                &manifest_path
            } else {
                CLAUDE_MARKETPLACE
            },
            SkillProvider::ClaudeMarketplace,
        )?;
        let discovered = discover_skills(context, std::slice::from_ref(&manifest))?;
        if discovered.skills.is_empty() && discovered.commands.is_empty() {
            limitations.push(format!(
                "Claude marketplace plugin `{name}` has no supported local Skills or commands."
            ));
            continue;
        }
        manifests.push(manifest);
    }
    Ok(manifests)
}

fn has_claude_default_components(context: &ScanContext<'_>) -> bool {
    context.files.iter().any(|file| {
        if let Some(relative) = file.strip_prefix(&format!("{DEFAULT_SKILLS_ROOT}/")) {
            return is_skill_definition_file(file) && relative.split('/').count() <= 2;
        }
        if let Some(relative) = file.strip_prefix("commands/") {
            return (relative.split('/').count() == 1 && is_markdown_file(file))
                || (is_skill_definition_file(relative) && relative.split('/').count() >= 2);
        }
        false
    })
}

fn parse_declared_skill_roots(
    value: &Value,
    manifest: &str,
    kind: DeclaredPathKind,
) -> Result<Vec<String>, GeneratorError> {
    parse_declared_paths(value, manifest, "skills", kind)
}

fn parse_kimi_command_roots(
    value: &Value,
    manifest: &str,
    limitations: &mut Vec<String>,
) -> Vec<CommandRoot> {
    let values = match value {
        Value::String(value) => vec![value.as_str()],
        Value::Array(values) if values.iter().all(Value::is_string) => {
            values.iter().filter_map(Value::as_str).collect::<Vec<_>>()
        }
        _ => {
            limitations.push(format!(
                "Kimi {manifest} has an invalid commands field; the CLI warns and ignores it."
            ));
            return Vec::new();
        }
    };
    values
        .into_iter()
        .filter_map(|value| {
            let path = match normalize_skill_root(value, manifest, DeclaredPathKind::KimiCommand) {
                Ok(path) => path,
                Err(error) => {
                    limitations.push(format!(
                        "Kimi {manifest} ignores invalid command path {value:?}: {error}"
                    ));
                    return None;
                }
            };
            Some(CommandRoot {
                path,
                plugin_root: ".".to_owned(),
                recursive_markdown: true,
            })
        })
        .collect()
}

fn parse_declared_paths(
    value: &Value,
    manifest: &str,
    field: &str,
    kind: DeclaredPathKind,
) -> Result<Vec<String>, GeneratorError> {
    let values = match value {
        Value::String(value) => vec![value.as_str()],
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value.as_str().ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "{manifest} {field} entries must be relative path strings"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(GeneratorError::usage(format!(
                "{manifest} {field} must be a path string or an array of path strings"
            )));
        }
    };
    values
        .into_iter()
        // Codex drops a declared `./` entry. Its loader falls back to the
        // default root only when no declared path remains.
        .filter(|value| !(kind == DeclaredPathKind::CodexSkill && *value == "./"))
        .map(|value| normalize_skill_root(value, manifest, kind))
        .collect()
}

fn normalize_skill_root(
    value: &str,
    manifest: &str,
    kind: DeclaredPathKind,
) -> Result<String, GeneratorError> {
    if kind == DeclaredPathKind::ClaudeSkill && value == "." {
        return Ok(".".to_owned());
    }
    let allow_root = matches!(
        kind,
        DeclaredPathKind::ClaudeSkill
            | DeclaredPathKind::ClaudeCommand
            | DeclaredPathKind::KimiSkill
            | DeclaredPathKind::KimiCommand
    );
    if allow_root && value == "./" {
        return Ok(".".to_owned());
    }
    if !value.starts_with("./") {
        return Err(GeneratorError::usage(format!(
            "{manifest} path {value} must start with ./"
        )));
    }
    let value = value.strip_prefix("./").unwrap_or(value);
    let value = value.trim_end_matches('/');
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || is_windows_drive_path(value)
    {
        return Err(GeneratorError::usage(format!(
            "{manifest} has unsafe component path {value}"
        )));
    }
    if (allow_root || kind == DeclaredPathKind::CodexSkill) && value == "." {
        return Ok(".".to_owned());
    }
    let mut components = Vec::new();
    for component in value.split('/') {
        match component {
            "" | ".." => {
                return Err(GeneratorError::usage(format!(
                    "{manifest} path {value} must stay inside the repository"
                )));
            }
            "." => {}
            component => components.push(component),
        }
    }
    if components.is_empty() && kind == DeclaredPathKind::CodexSkill {
        return Ok(".".to_owned());
    }
    if components.is_empty() {
        return Err(GeneratorError::usage(format!(
            "{manifest} path {value} must name a file or directory"
        )));
    }
    Ok(components.join("/"))
}

#[expect(
    clippy::too_many_lines,
    reason = "provider roots are normalized before one deduplicated discovery pass"
)]
fn discover_skills(
    context: &ScanContext<'_>,
    manifests: &[PluginManifest],
) -> Result<DiscoveredSkillFiles, GeneratorError> {
    let mut skill_roots = BTreeSet::new();
    let mut command_roots = BTreeSet::new();
    let root_only_kimi_paths = manifests
        .iter()
        .filter(|manifest| manifest.provider == SkillProvider::Kimi)
        .filter_map(|manifest| manifest.root_skill.as_ref())
        .map(|root| root.path.clone())
        .collect::<BTreeSet<_>>();
    for manifest in manifests {
        skill_roots.extend(manifest.skill_roots.iter().cloned());
        command_roots.extend(manifest.command_roots.iter().cloned());
        if let Some(root_skill) = &manifest.root_skill {
            skill_roots.insert(root_skill.clone());
        }
    }

    let mut discovered = DiscoveredSkillFiles::default();
    for root in skill_roots {
        let path = root.path.as_str();
        let is_root_only_kimi = root_only_kimi_paths.contains(path);
        // The Codex directory walker treats a file-valued root as an empty
        // root. In particular, a root named `SKILL.md` is not itself loaded.
        if root.frontmatter_mode == FrontmatterMode::CodexDefinition
            && context.file_set.contains(path)
        {
            continue;
        }
        if root.frontmatter_mode == FrontmatterMode::ClaudeDefinition
            && context.file_set.contains(path)
        {
            return Err(GeneratorError::usage(format!(
                "Claude skill path {path} must name a directory, not a file"
            )));
        }
        if root.frontmatter_mode == FrontmatterMode::KimiDefinition
            && context.file_set.contains(path)
            && !kimi_definition_is_parseable(context, path, FrontmatterMode::KimiDefinition)?
        {
            continue;
        }
        let kimi_directory_scan =
            if root.frontmatter_mode == FrontmatterMode::KimiDefinition && root.flat_markdown {
                Some(kimi_directory_skills(context, &root)?)
            } else {
                None
            };
        if let Some(scan) = &kimi_directory_scan
            && !scan.blocked_resource_roots.is_empty()
        {
            discovered
                .blocked_kimi_resource_roots
                .entry(path.to_owned())
                .or_default()
                .extend(scan.blocked_resource_roots.iter().cloned());
        }
        if context.file_set.contains(path) {
            if is_skill_definition_file(path) {
                record_skill_file(
                    &mut discovered,
                    path,
                    root.frontmatter_mode,
                    path,
                    &root.plugin_root,
                    None,
                );
                if is_root_only_kimi {
                    let component_roots = discovered
                        .component_roots
                        .entry(path.to_owned())
                        .or_default();
                    component_roots.clear();
                    component_roots.insert(path.to_owned());
                    discovered.root_only_kimi_skills.insert(path.to_owned());
                }
            } else if root.flat_markdown
                && is_markdown_file_for_mode(path, root.frontmatter_mode)
                && !flat_skill_is_shadowed(path, &root.path, context.files)
            {
                let mode = if root.frontmatter_mode == FrontmatterMode::KimiDefinition {
                    FrontmatterMode::KimiFlatDefinition
                } else {
                    FrontmatterMode::FlatDefinition
                };
                if mode == FrontmatterMode::KimiFlatDefinition
                    && !kimi_definition_is_parseable(context, path, mode)?
                {
                    continue;
                }
                record_skill_file(&mut discovered, path, mode, path, &root.plugin_root, None);
            } else {
                return Err(GeneratorError::usage(format!(
                    "skill root {path} is a file, not a supported skill definition or directory"
                )));
            }
            continue;
        }
        for file in context.files.iter().filter(|file| {
            let relative = if path == "." {
                file.as_str()
            } else {
                let Some(relative) = file.strip_prefix(&format!("{path}/")) else {
                    return false;
                };
                relative
            };
            if is_skill_definition_file(relative) {
                if let Some(kimi_directory_scan) = &kimi_directory_scan {
                    return kimi_directory_scan.files.contains(file.as_str());
                }
                if root.frontmatter_mode == FrontmatterMode::CodexDefinition {
                    return codex_skill_file_is_discoverable(relative);
                }
                let depth = relative.split('/').count();
                return if path == "." {
                    depth == 1
                } else if root.require_skill_directory {
                    depth == 2
                } else {
                    depth <= 2
                };
            }
            root.flat_markdown
                && relative.split('/').count() == 1
                && is_markdown_file_for_mode(file, root.frontmatter_mode)
                && !is_skill_definition_file(file)
        }) {
            if is_skill_definition_file(file) {
                record_skill_file(
                    &mut discovered,
                    file,
                    root.frontmatter_mode,
                    path,
                    &root.plugin_root,
                    (root.frontmatter_mode == FrontmatterMode::CodexDefinition).then_some(path),
                );
            } else if !flat_skill_is_shadowed(file, path, context.files) {
                let mode = if root.frontmatter_mode == FrontmatterMode::KimiDefinition {
                    FrontmatterMode::KimiFlatDefinition
                } else {
                    FrontmatterMode::FlatDefinition
                };
                if mode == FrontmatterMode::KimiFlatDefinition
                    && !kimi_definition_is_parseable(context, file, mode)?
                {
                    continue;
                }
                record_skill_file(&mut discovered, file, mode, path, &root.plugin_root, None);
            }
        }
    }
    for root in command_roots {
        let path = root.path.as_str();
        let frontmatter_mode = if root.recursive_markdown {
            FrontmatterMode::KimiCommand
        } else {
            FrontmatterMode::ClaudeCommand
        };
        if context.file_set.contains(path) && is_markdown_file_for_mode(path, frontmatter_mode) {
            record_command_file(
                &mut discovered,
                path,
                frontmatter_mode,
                path,
                &root.plugin_root,
            );
            continue;
        }
        let prefix = if path == "." {
            String::new()
        } else {
            format!("{path}/")
        };
        for file in context.files.iter().filter(|file| {
            let relative = if path == "." {
                file.as_str()
            } else if let Some(relative) = file.strip_prefix(&prefix) {
                relative
            } else {
                return false;
            };
            if root.recursive_markdown {
                is_markdown_file_for_mode(file, FrontmatterMode::KimiCommand)
            } else {
                (relative.split('/').count() == 1 && is_markdown_file(file))
                    || (is_skill_definition_file(relative) && relative.split('/').count() >= 2)
            }
        }) {
            let mode = if root.recursive_markdown {
                FrontmatterMode::KimiCommand
            } else if is_skill_definition_file(file) {
                FrontmatterMode::ClaudeDefinition
            } else {
                FrontmatterMode::ClaudeCommand
            };
            record_command_file(&mut discovered, file, mode, path, &root.plugin_root);
        }
    }
    Ok(discovered)
}

fn kimi_definition_is_parseable(
    context: &ScanContext<'_>,
    file: &str,
    mode: FrontmatterMode,
) -> Result<bool, GeneratorError> {
    let path = repository_path(context, file, "Kimi skill definition")?;
    let text = read_text(&path, "read Kimi skill definition")?;
    Ok(parse_frontmatter(&text, file, mode).is_ok())
}

fn kimi_component_resource_roots(
    discovered: &DiscoveredSkillFiles,
) -> (
    BTreeMap<String, BTreeSet<String>>,
    BTreeSet<String>,
    BTreeSet<String>,
) {
    let mut skill_roots = BTreeMap::new();
    let mut pruned_skill_roots = BTreeSet::new();
    let mut command_roots = BTreeSet::new();
    for (definition, modes) in &discovered.skills {
        if discovered.root_only_kimi_skills.contains(definition) {
            continue;
        }
        if modes.iter().any(|mode| {
            matches!(
                mode,
                FrontmatterMode::KimiDefinition | FrontmatterMode::KimiFlatDefinition
            )
        }) && let Some(roots) = discovered.component_roots.get(definition)
        {
            for root in roots {
                skill_roots.entry(root.clone()).or_insert_with(|| {
                    discovered
                        .blocked_kimi_resource_roots
                        .get(root)
                        .cloned()
                        .unwrap_or_default()
                });
                if modes.contains(&FrontmatterMode::KimiDefinition) {
                    let skill_directory = parent_or_dot(definition);
                    let directory_name = Path::new(&skill_directory)
                        .file_name()
                        .and_then(|name| name.to_str());
                    if skill_directory != *root
                        && path_is_within_root(&skill_directory, root)
                        && directory_name
                            .is_some_and(|name| name == "node_modules" || name.starts_with('.'))
                    {
                        pruned_skill_roots.insert(skill_directory);
                    }
                }
            }
        }
    }
    for (definition, modes) in &discovered.commands {
        if modes.contains(&FrontmatterMode::KimiCommand)
            && let Some(roots) = discovered.component_roots.get(definition)
        {
            command_roots.extend(roots.iter().cloned());
        }
    }
    (skill_roots, pruned_skill_roots, command_roots)
}

fn definition_frontmatter_modes(
    discovered: &DiscoveredSkillFiles,
) -> BTreeMap<String, BTreeSet<FrontmatterMode>> {
    let mut modes_by_definition = BTreeMap::new();
    for (definition, modes) in discovered.skills.iter().chain(&discovered.commands) {
        modes_by_definition
            .entry(definition.clone())
            .or_insert_with(BTreeSet::new)
            .extend(modes.iter().copied());
    }
    modes_by_definition
}

fn kimi_directory_skills(
    context: &ScanContext<'_>,
    root: &SkillRoot,
) -> Result<KimiDirectorySkillScan, GeneratorError> {
    let prefix = if root.path == "." {
        String::new()
    } else {
        format!("{}/", root.path)
    };
    let mut candidates = context
        .files
        .iter()
        .filter_map(|file| {
            let relative = file.strip_prefix(&prefix)?;
            (is_skill_definition_file(relative) && kimi_skill_definition_is_in_scan_scope(relative))
                .then(|| (file.clone(), relative.to_owned()))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, relative)| relative.split('/').count());

    let mut ancestor_skill_files = BTreeSet::new();
    for (_, relative) in &candidates {
        let skill_directory = relative.strip_suffix("/SKILL.md").unwrap_or("");
        let directories = skill_directory.split('/').collect::<Vec<_>>();
        for depth in 1..directories.len() {
            let ancestor = directories[..depth].join("/");
            ancestor_skill_files.insert(join_plugin_root(
                &root.path,
                &format!("{ancestor}/SKILL.md"),
            ));
        }
    }
    let excluded_ancestor_skill_files = if ancestor_skill_files.is_empty() {
        BTreeSet::new()
    } else {
        super::file_walk::excluded_existing_files(
            context.root,
            &ancestor_skill_files,
            context.exclude,
        )?
    };

    let mut sub_skill_permissions = BTreeMap::<String, bool>::new();
    let mut discovered = KimiDirectorySkillScan::default();
    for (file, relative) in candidates {
        let skill_directory = relative.strip_suffix("/SKILL.md").unwrap_or("");
        let directory_components = skill_directory.split('/').collect::<Vec<_>>();
        let blocking_depth = (1..directory_components.len()).find(|depth| {
            let ancestor = directory_components[..*depth].join("/");
            let ancestor_skill_file = join_plugin_root(&root.path, &format!("{ancestor}/SKILL.md"));
            sub_skill_permissions.get(&ancestor) == Some(&false)
                || excluded_ancestor_skill_files.contains(&ancestor_skill_file)
        });
        if let Some(depth) = blocking_depth {
            let blocked_child = directory_components[..=depth].join("/");
            discovered
                .blocked_resource_roots
                .insert(join_plugin_root(&root.path, &blocked_child));
            continue;
        }

        let path = repository_path(context, &file, "Kimi skill definition")?;
        let text = read_text(&path, "read Kimi skill definition")?;
        let Ok(frontmatter) = parse_frontmatter(&text, &file, FrontmatterMode::KimiDefinition)
        else {
            if !skill_directory.is_empty() {
                sub_skill_permissions.insert(skill_directory.to_owned(), false);
            }
            continue;
        };
        if !skill_directory.is_empty() {
            sub_skill_permissions.insert(
                skill_directory.to_owned(),
                frontmatter.kimi_sub_skills_enabled,
            );
        }
        discovered.files.insert(file);
    }
    Ok(discovered)
}

fn codex_skill_file_is_discoverable(relative: &str) -> bool {
    let components = relative.split('/').collect::<Vec<_>>();
    let Some("SKILL.md") = components.last().copied() else {
        return false;
    };
    let directories = &components[..components.len().saturating_sub(1)];
    directories.len() <= CODEX_MAX_SKILL_SCAN_DEPTH
        && directories
            .iter()
            .all(|directory| !directory.starts_with('.'))
}

fn record_skill_file(
    discovered: &mut DiscoveredSkillFiles,
    file: &str,
    mode: FrontmatterMode,
    component_root: &str,
    plugin_root: &str,
    codex_scan_root: Option<&str>,
) {
    discovered
        .skills
        .entry(file.to_owned())
        .or_default()
        .insert(mode);
    let component_root = if component_root == file {
        parent_or_dot(file)
    } else {
        component_root.to_owned()
    };
    record_resource_file(discovered, file, &component_root, plugin_root);
    if let Some(scan_root) = codex_scan_root {
        discovered
            .codex_scan_roots
            .entry(file.to_owned())
            .or_default()
            .insert(scan_root.to_owned());
    }
}

fn record_command_file(
    discovered: &mut DiscoveredSkillFiles,
    file: &str,
    mode: FrontmatterMode,
    component_root: &str,
    plugin_root: &str,
) {
    discovered
        .commands
        .entry(file.to_owned())
        .or_default()
        .insert(mode);
    let component_root = if component_root == file && mode != FrontmatterMode::KimiCommand {
        parent_or_dot(file)
    } else {
        component_root.to_owned()
    };
    record_resource_file(discovered, file, &component_root, plugin_root);
}

fn record_resource_file(
    discovered: &mut DiscoveredSkillFiles,
    file: &str,
    component_root: &str,
    plugin_root: &str,
) {
    discovered
        .resource_roots
        .entry(file.to_owned())
        .or_default()
        .insert(plugin_root.to_owned());
    discovered
        .component_roots
        .entry(file.to_owned())
        .or_default()
        .insert(component_root.to_owned());
}

fn flat_skill_is_shadowed(file: &str, declared_root: &str, files: &[String]) -> bool {
    let relative = if declared_root == "." {
        file
    } else if let Some(relative) = file.strip_prefix(&format!("{declared_root}/")) {
        relative
    } else {
        return false;
    };
    let Some(stem) = Path::new(relative)
        .file_stem()
        .and_then(|stem| stem.to_str())
    else {
        return false;
    };
    let candidate = if declared_root == "." {
        format!("{stem}/SKILL.md")
    } else {
        format!("{declared_root}/{stem}/SKILL.md")
    };
    files.iter().any(|path| path == &candidate)
}

fn is_skill_definition_file(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "SKILL.md")
}

fn kimi_skill_definition_is_in_scan_scope(relative: &str) -> bool {
    let components = relative.split('/').collect::<Vec<_>>();
    let Some("SKILL.md") = components.last().copied() else {
        return false;
    };
    let directories = &components[..components.len() - 1];
    // The CLI walks directories through depth 8 and discovers each walked
    // directory's immediate child Skills before deciding whether to recurse.
    // That permits a Skill directory one level below the deepest walked dir.
    directories.len() <= KIMI_MAX_SKILL_SCAN_DEPTH + 1
        && directories
            .iter()
            .take(directories.len().saturating_sub(1))
            .all(|component| *component != "node_modules" && !component.starts_with('.'))
}

fn is_kimi_sub_skill_flag(mode: FrontmatterMode, key: &str, value: &serde_yaml::Value) -> bool {
    mode == FrontmatterMode::KimiDefinition
        && matches!(key, "has-sub-skill" | "hasSubSkill")
        && value.as_bool().is_some()
}

fn kimi_sub_skills_enabled(mapping: &serde_yaml::Mapping) -> bool {
    ["has-sub-skill", "hasSubSkill"]
        .iter()
        .any(|key| mapping.get(key).and_then(serde_yaml::Value::as_bool) == Some(true))
        || mapping
            .get("metadata")
            .and_then(serde_yaml::Value::as_mapping)
            .is_some_and(|metadata| {
                ["has-sub-skill", "hasSubSkill"]
                    .iter()
                    .any(|key| metadata.get(key).and_then(serde_yaml::Value::as_bool) == Some(true))
            })
}

fn validate_skill_name(name: &str) -> Result<(), GeneratorError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if !valid {
        return Err(GeneratorError::usage(format!(
            "skill name {name} must be 1-64 lowercase letters, numbers, or single hyphen-separated components"
        )));
    }
    Ok(())
}

fn normalize_markdown_destination(target: &str) -> Option<String> {
    // pulldown-cmark has already resolved Markdown backslash escapes and
    // character references. Strip URI components only after that decoding.
    let path = target.split_once('#').map_or(target, |(path, _)| path);
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    let decoded = percent_decode_markdown_uri(path)?;
    let trimmed = decoded.trim_matches(|character: char| character.is_ascii_whitespace());
    (trimmed == decoded && !decoded.contains('\\') && !decoded.chars().any(char::is_control))
        .then_some(decoded)
}

fn percent_decode_markdown_uri(target: &str) -> Option<String> {
    let bytes = target.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let first = hex_value(*bytes.get(index + 1)?)?;
        let second = hex_value(*bytes.get(index + 2)?)?;
        let byte = (first << 4) | second;
        if matches!(byte, b'/' | b'\\') {
            return None;
        }
        decoded.push(byte);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn has_uri_scheme(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.chars().enumerate().all(|(index, character)| {
            character.is_ascii_alphabetic()
                || (index > 0
                    && (character.is_ascii_digit() || matches!(character, '+' | '-' | '.')))
        })
}

fn is_safe_external_uri(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };
    // The scanner has no renderer contract, so it only treats ordinary web
    // destinations and mail links as inert external references.
    ["http", "https", "mailto"]
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
}

fn validate_skills(
    context: &ScanContext<'_>,
    skill_files: &BTreeMap<String, BTreeSet<FrontmatterMode>>,
    codex_repository: Option<&super::file_walk::CodexRepository>,
) -> Result<(), GeneratorError> {
    for (path, modes) in skill_files {
        let path_on_disk = if modes.contains(&FrontmatterMode::CodexDefinition) {
            codex_repository
                .ok_or_else(|| {
                    GeneratorError::usage("Codex skill definition has no repository view")
                })?
                .path(path, context.exclude, "Codex skill definition")?
                .ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "Codex skill definition {path} is not a tracked, in-scope repository file"
                    ))
                })?
        } else {
            repository_path(context, path, "skill definition")?
        };
        let codex_directory_name = modes
            .contains(&FrontmatterMode::CodexDefinition)
            .then(|| path_on_disk.parent())
            .flatten()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        let text = read_text(&path_on_disk, "read skill definition")?;
        for mode in modes {
            let frontmatter = parse_frontmatter(&text, path, *mode)?;
            let skill_definition = matches!(
                *mode,
                FrontmatterMode::LiveDefinition
                    | FrontmatterMode::AntigravityDefinition
                    | FrontmatterMode::CodexDefinition
                    | FrontmatterMode::ClaudeDefinition
                    | FrontmatterMode::KimiDefinition
            );
            if skill_definition
                && Path::new(path)
                    .file_name()
                    .is_none_or(|name| name != "SKILL.md")
            {
                return Err(GeneratorError::usage(format!(
                    "{path} must use the canonical SKILL.md filename"
                )));
            }
            validate_skill_name_for_mode(path, *mode, &frontmatter, codex_directory_name)?;
        }
    }
    Ok(())
}

fn validate_skill_name_for_mode(
    path: &str,
    mode: FrontmatterMode,
    frontmatter: &Frontmatter,
    codex_directory_name: Option<&str>,
) -> Result<(), GeneratorError> {
    let source_directory_name = Path::new(path)
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str());
    let directory_name = if mode == FrontmatterMode::CodexDefinition {
        codex_directory_name
    } else {
        source_directory_name
    };
    let codex_name = if mode == FrontmatterMode::CodexDefinition {
        Some(validate_codex_skill_name(
            frontmatter,
            directory_name,
            path,
        )?)
    } else {
        None
    };
    let name = codex_name
        .as_deref()
        .or_else(|| frontmatter.values.get("name").map(String::as_str))
        .or_else(|| match mode {
            FrontmatterMode::FlatDefinition
            | FrontmatterMode::KimiFlatDefinition
            | FrontmatterMode::ClaudeCommand => {
                Path::new(path).file_stem().and_then(|stem| stem.to_str())
            }
            FrontmatterMode::AntigravityDefinition => directory_name,
            FrontmatterMode::ClaudeDefinition
            | FrontmatterMode::LiveDefinition
            | FrontmatterMode::CodexDefinition
            | FrontmatterMode::KimiDefinition
            | FrontmatterMode::KimiCommand
            | FrontmatterMode::EmbeddedTemplate => None,
        });
    if mode != FrontmatterMode::CodexDefinition
        && !matches!(
            mode,
            FrontmatterMode::ClaudeDefinition
                | FrontmatterMode::ClaudeCommand
                | FrontmatterMode::KimiFlatDefinition
                | FrontmatterMode::KimiCommand
        )
    {
        if let Some(name) = name {
            if mode != FrontmatterMode::KimiDefinition {
                validate_skill_name(name)?;
            } else if name.trim().is_empty() {
                return Err(GeneratorError::usage(format!(
                    "{path} frontmatter requires a non-empty name"
                )));
            }
        } else {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter requires name"
            )));
        }
    }
    if matches!(
        mode,
        FrontmatterMode::LiveDefinition | FrontmatterMode::AntigravityDefinition
    ) && let (Some(name), Some(directory_name)) = (name, directory_name)
        && name != directory_name
    {
        return Err(GeneratorError::usage(format!(
            "{path} frontmatter name does not match skill directory {directory_name}"
        )));
    }
    Ok(())
}

fn validate_codex_skill_name(
    frontmatter: &Frontmatter,
    directory_name: Option<&str>,
    path: &str,
) -> Result<String, GeneratorError> {
    let explicit_name = frontmatter
        .values
        .get("name")
        .map(|name| name.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|name| !name.is_empty());
    let fallback_name = directory_name
        .unwrap_or("skill")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let name = explicit_name.unwrap_or_else(|| {
        if fallback_name.is_empty() {
            "skill".to_owned()
        } else {
            fallback_name
        }
    });
    if name.chars().count() > 64 {
        return Err(GeneratorError::usage(format!(
            "{path} frontmatter name must fit Codex's 64-character limit after sanitizing"
        )));
    }
    Ok(name)
}

#[expect(
    clippy::too_many_lines,
    reason = "link validation keeps URI, path, confinement, and template checks ordered"
)]
fn validate_links(
    context: &ScanContext<'_>,
    discovered: &DiscoveredSkillFiles,
    owned_template_roots: &BTreeSet<String>,
    provider_resource_scopes: &ProviderResourceScopes,
    codex_repository: Option<&super::file_walk::CodexRepository>,
) -> Result<(bool, BTreeSet<String>), GeneratorError> {
    let frontmatter_modes_by_definition = definition_frontmatter_modes(discovered);
    let mut markdown_roots = BTreeMap::<String, MarkdownValidationScopes>::new();
    for (definition, component_roots) in &discovered.component_roots {
        let definition_modes = frontmatter_modes_by_definition
            .get(definition)
            .cloned()
            .unwrap_or_default();
        let codex_definition = definition_modes.contains(&FrontmatterMode::CodexDefinition);
        let other_definition = definition_modes
            .iter()
            .any(|mode| *mode != FrontmatterMode::CodexDefinition);
        let gate_disabled_kimi_resources = frontmatter_modes_by_definition
            .get(definition)
            .is_some_and(|modes| {
                modes.len() == 1 && modes.contains(&FrontmatterMode::KimiDefinition)
            });
        let Some(plugin_roots) = discovered.resource_roots.get(definition) else {
            continue;
        };
        for component_root in component_roots {
            let blocked_resource_roots = discovered.blocked_kimi_resource_roots.get(component_root);
            let prefix = if component_root == "." {
                String::new()
            } else {
                format!("{component_root}/")
            };
            for file in context.files.iter().filter(|file| {
                (component_root == "."
                    || file.as_str() == component_root.as_str()
                    || file.starts_with(&prefix))
                    && (!codex_definition || codex_path_is_scanned_resource(file, component_root))
                    && is_markdown_or_mdx_file(file)
                    && !is_owned_template_path(file, owned_template_roots)
                    && !blocked_resource_roots.is_some_and(|blocked| {
                        is_blocked_kimi_resource(
                            file,
                            gate_disabled_kimi_resources,
                            &discovered.skills,
                            blocked,
                        )
                    })
            }) {
                let scopes = markdown_roots.entry(file.clone()).or_default();
                if codex_definition {
                    scopes
                        .codex_plugin_roots
                        .extend(plugin_roots.iter().cloned());
                }
                if other_definition {
                    scopes
                        .other_plugin_roots
                        .extend(plugin_roots.iter().cloned());
                }
            }
        }
    }

    let mut has_unparsed_html_links = false;
    let mut linked_template_roots = BTreeSet::new();
    for (file, scopes) in markdown_roots {
        let is_component_definition = discovered.component_roots.contains_key(&file);
        let roots = scopes
            .codex_plugin_roots
            .iter()
            .chain(scopes.other_plugin_roots.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        let source_path = if scopes.codex_plugin_roots.is_empty() {
            repository_path(context, &file, "Markdown reference")?
        } else {
            codex_repository
                .ok_or_else(|| {
                    GeneratorError::usage("Codex Markdown reference has no repository view")
                })?
                .path(&file, context.exclude, "Codex Markdown reference")?
                .ok_or_else(|| {
                    GeneratorError::usage(format!(
                    "Codex Markdown reference {file} is not a tracked, in-scope repository file"
                ))
                })?
        };
        if !is_markdown_file(&file) {
            continue;
        }
        let text = read_text(&source_path, "read Markdown reference")?;
        let markdown_frontmatter_mode = if scopes.codex_plugin_roots.is_empty() {
            FrontmatterMode::ClaudeDefinition
        } else {
            FrontmatterMode::CodexDefinition
        };
        let (body, first_body_line) = markdown_body_for_mode(&text, markdown_frontmatter_mode);
        let parser_options = Options::ENABLE_GFM
            | Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES;
        let parser = Parser::new_ext(body, parser_options).into_offset_iter();
        for (event, range) in parser {
            let destination = match event {
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    ..
                }) => {
                    let destination = dest_url.into_string();
                    if link_type == LinkType::Email {
                        format!("mailto:{destination}")
                    } else {
                        destination
                    }
                }
                Event::Start(Tag::Image { dest_url, .. }) => dest_url.into_string(),
                Event::Html(_) | Event::InlineHtml(_) => {
                    has_unparsed_html_links = true;
                    continue;
                }
                _ => continue,
            };
            let line = first_body_line + count_line_breaks(&body[..range.start]);
            if is_windows_drive_path(&destination) {
                return Err(GeneratorError::usage(format!(
                    "unsafe Markdown link {destination} at {file}:{line}"
                )));
            }
            if has_uri_scheme(&destination) {
                if is_safe_external_uri(&destination) {
                    continue;
                }
                return Err(GeneratorError::usage(format!(
                    "unsupported or unsafe Markdown URI {destination} at {file}:{line}"
                )));
            }
            let target = normalize_markdown_destination(&destination).ok_or_else(|| {
                GeneratorError::usage(format!(
                    "unsafe Markdown link {destination} at {file}:{line}"
                ))
            })?;
            if target.is_empty() {
                continue;
            }
            if has_uri_scheme(&target) {
                return Err(GeneratorError::usage(format!(
                    "unsupported or unsafe Markdown URI {target} at {file}:{line}"
                )));
            }
            let resolved = resolve_markdown_path(&file, &target).ok_or_else(|| {
                GeneratorError::usage(format!("unsafe Markdown link {target} at {file}:{line}"))
            })?;
            for root in &roots {
                if !path_is_within_root(&resolved, root) {
                    return Err(GeneratorError::usage(format!(
                        "Markdown link {target} escapes plugin root {root} at {file}:{line}"
                    )));
                }
            }
            if resolved.is_empty() {
                // A valid local link to `.` or `./` refers to the repository
                // root. It has no relative filename to look up in `file_set`.
                continue;
            }
            let codex_scoped_target =
                resource_scope_applies(&provider_resource_scopes.codex, &file, &resolved);
            let kimi_scoped_target =
                resource_scope_applies(&provider_resource_scopes.kimi, &file, &resolved);
            if !codex_scoped_target && !kimi_scoped_target {
                let _target_path = repository_path(context, &resolved, "Markdown link")?;
            }
            // Some skill docs deliberately link to generator placeholders such
            // as `<topic>`. Skip only their missing-file check; the repository
            // and plugin-root checks above still apply.
            let target_exists = if codex_scoped_target || kimi_scoped_target {
                provider_resource_target_exists(
                    context,
                    &file,
                    &resolved,
                    provider_resource_scopes,
                    codex_repository,
                )?
            } else {
                context.file_set.contains(&resolved)
                    || context
                        .files
                        .iter()
                        .any(|candidate| candidate.starts_with(&format!("{resolved}/")))
            };
            if is_markdown_placeholder(&target) && !target_exists {
                continue;
            }
            if !target_exists {
                return Err(GeneratorError::usage(format!(
                    "missing Markdown link {target} at {file}:{line}"
                )));
            }
            if is_component_definition {
                let template_root = owned_template_root_for_path(&resolved, owned_template_roots)
                    .or_else(|| {
                        discovered
                            .component_roots
                            .get(&file)
                            .and_then(|component_roots| {
                                linked_template_root_for_path(
                                    &resolved,
                                    &file,
                                    component_roots,
                                    &discovered.component_roots,
                                    context,
                                )
                            })
                    });
                if let Some(template_root) = template_root
                    && roots
                        .iter()
                        .any(|root| path_is_within_root(&template_root, root))
                {
                    linked_template_roots.insert(template_root);
                }
            }
        }
    }
    Ok((has_unparsed_html_links, linked_template_roots))
}

fn provider_resource_scopes(discovered: &DiscoveredSkillFiles) -> ProviderResourceScopes {
    let mut scopes = ProviderResourceScopes::default();
    for (definition, modes) in definition_frontmatter_modes(discovered) {
        let Some(component_roots) = discovered.component_roots.get(&definition) else {
            continue;
        };
        let Some(plugin_roots) = discovered.resource_roots.get(&definition) else {
            continue;
        };
        let is_codex = modes.contains(&FrontmatterMode::CodexDefinition);
        let is_kimi = modes.iter().any(|mode| {
            matches!(
                mode,
                FrontmatterMode::KimiDefinition
                    | FrontmatterMode::KimiFlatDefinition
                    | FrontmatterMode::KimiCommand
            )
        });
        for component_root in component_roots {
            if is_codex {
                scopes
                    .codex
                    .entry(component_root.clone())
                    .or_default()
                    .extend(plugin_roots.iter().cloned());
                if let Some(scan_roots) = discovered.codex_scan_roots.get(&definition) {
                    scopes
                        .codex_scan_roots
                        .entry(component_root.clone())
                        .or_default()
                        .extend(scan_roots.iter().cloned());
                }
            }
            if is_kimi {
                scopes
                    .kimi
                    .entry(component_root.clone())
                    .or_default()
                    .extend(plugin_roots.iter().cloned());
            }
        }
    }
    scopes
}

fn provider_resource_target_exists(
    context: &ScanContext<'_>,
    source: &str,
    target: &str,
    provider_resource_scopes: &ProviderResourceScopes,
    codex_repository: Option<&super::file_walk::CodexRepository>,
) -> Result<bool, GeneratorError> {
    if resource_scope_applies(&provider_resource_scopes.codex, source, target)
        && codex_repository
            .ok_or_else(|| GeneratorError::usage("Codex Markdown link has no repository view"))?
            .link_exists(
                target,
                context.exclude,
                &provider_resource_scopes
                    .codex_scan_roots
                    .iter()
                    .filter(|(component_root, _)| path_is_within_root(source, component_root))
                    .flat_map(|(_, roots)| roots)
                    .cloned()
                    .collect::<Vec<_>>(),
            )?
    {
        return Ok(true);
    }
    if resource_scope_applies(&provider_resource_scopes.kimi, source, target)
        && super::file_walk::repository_path_exists(context.root, target, context.exclude)?
    {
        return Ok(true);
    }
    Ok(false)
}

fn resource_scope_applies(
    scopes: &BTreeMap<String, BTreeSet<String>>,
    source: &str,
    target: &str,
) -> bool {
    scopes
        .iter()
        .filter(|(component_root, _)| path_is_within_root(source, component_root))
        .flat_map(|(_, plugin_roots)| plugin_roots)
        .any(|plugin_root| path_is_within_root(target, plugin_root))
}

fn codex_path_is_scanned_resource(path: &str, root: &str) -> bool {
    if path == root {
        return false;
    }
    let relative = if root == "." {
        path
    } else if let Some(relative) = path.strip_prefix(&format!("{root}/")) {
        relative
    } else {
        return false;
    };
    let components = relative.split('/').collect::<Vec<_>>();
    let Some(_) = components.last() else {
        return false;
    };
    let directories = &components[..components.len().saturating_sub(1)];
    directories.len() <= CODEX_MAX_SKILL_SCAN_DEPTH
        && directories
            .iter()
            .all(|directory| !directory.starts_with('.'))
}

fn path_is_within_root(path: &str, root: &str) -> bool {
    root == "." || path == root || path.starts_with(&format!("{root}/"))
}

fn owned_template_root_for_path(
    path: &str,
    owned_template_roots: &BTreeSet<String>,
) -> Option<String> {
    owned_template_roots
        .iter()
        .find(|root| path_is_within_root(path, root))
        .cloned()
}

fn owned_template_roots(discovered: &DiscoveredSkillFiles) -> BTreeSet<String> {
    let definitions = discovered
        .skills
        .keys()
        .chain(discovered.commands.keys())
        .filter(|path| is_skill_definition_file(path))
        .filter(|path| {
            !discovered.root_only_kimi_skills.contains(*path)
                || discovered
                    .skills
                    .get(*path)
                    .is_some_and(|modes| modes.len() > 1)
        })
        .cloned()
        .collect::<BTreeSet<_>>();

    definitions
        .iter()
        .map(|definition| {
            let directory = parent_or_dot(definition);
            if directory == "." {
                "templates".to_owned()
            } else {
                format!("{directory}/templates")
            }
        })
        .filter(|template_root| {
            !definitions
                .iter()
                .any(|definition| path_is_within_root(definition, template_root))
        })
        .collect()
}

fn linked_template_root_for_path(
    path: &str,
    definition: &str,
    component_roots: &BTreeSet<String>,
    component_roots_by_definition: &BTreeMap<String, BTreeSet<String>>,
    context: &ScanContext<'_>,
) -> Option<String> {
    let components = path.split('/').collect::<Vec<_>>();
    let template_index = components
        .iter()
        .rposition(|component| component.eq_ignore_ascii_case("templates"))?;
    let template_root = components[..=template_index].join("/");
    let prefix = format!("{template_root}/");
    let has_template_files = !context.file_set.contains(&template_root)
        && context.files.iter().any(|file| file.starts_with(&prefix));
    let contains_component_definition = component_roots_by_definition
        .keys()
        .any(|component| path_is_within_root(component, &template_root));
    let is_sibling = component_roots
        .iter()
        .any(|component_root| is_sibling_template_root(&template_root, definition, component_root));
    (has_template_files && !contains_component_definition && is_sibling).then_some(template_root)
}

fn is_sibling_template_root(template_root: &str, definition: &str, component_root: &str) -> bool {
    let definition_dir = parent_or_dot(definition);
    if component_root == definition_dir {
        let direct_template_root = if component_root == "." {
            "templates".to_owned()
        } else {
            format!("{component_root}/templates")
        };
        return template_root == direct_template_root;
    }
    let collection_root = if component_root == "." {
        if definition_dir == "." {
            return template_root == "templates";
        }
        parent_or_dot(&definition_dir)
    } else if path_is_within_root(&definition_dir, component_root) {
        component_root.to_owned()
    } else {
        return false;
    };
    if path_is_within_root(template_root, &collection_root) {
        let relative = if collection_root == "." {
            template_root
        } else if template_root == collection_root {
            return false;
        } else {
            template_root
                .strip_prefix(&format!("{collection_root}/"))
                .unwrap_or_default()
        };
        let relative_components = relative.split('/').collect::<Vec<_>>();
        return relative_components.len() <= 2
            && relative_components
                .last()
                .is_some_and(|component| component.eq_ignore_ascii_case("templates"));
    }
    false
}

fn parent_or_dot(path: &str) -> String {
    path.rsplit_once('/').map_or_else(
        || ".".to_owned(),
        |(parent, _)| {
            if parent.is_empty() {
                ".".to_owned()
            } else {
                parent.to_owned()
            }
        },
    )
}

fn is_owned_template_path(path: &str, owned_template_roots: &BTreeSet<String>) -> bool {
    owned_template_root_for_path(path, owned_template_roots).is_some()
}

fn has_unparsed_mdx(
    context: &ScanContext<'_>,
    component_roots_by_definition: &BTreeMap<String, BTreeSet<String>>,
    frontmatter_modes_by_definition: &BTreeMap<String, BTreeSet<FrontmatterMode>>,
    skill_files: &BTreeMap<String, BTreeSet<FrontmatterMode>>,
    blocked_kimi_resource_roots: &BTreeMap<String, BTreeSet<String>>,
    owned_template_roots: &BTreeSet<String>,
) -> bool {
    component_roots_by_definition
        .iter()
        .any(|(definition, component_roots)| {
            let gate_disabled_kimi_resources = frontmatter_modes_by_definition
                .get(definition)
                .is_some_and(|modes| {
                    modes.len() == 1 && modes.contains(&FrontmatterMode::KimiDefinition)
                });
            component_roots.iter().any(|root| {
                let blocked_resource_roots = blocked_kimi_resource_roots.get(root);
                let prefix = if root == "." {
                    String::new()
                } else {
                    format!("{root}/")
                };
                context.files.iter().any(|file| {
                    (root == "." || file.starts_with(&prefix))
                        && !is_owned_template_path(file, owned_template_roots)
                        && is_mdx_file(file)
                        && !blocked_resource_roots.is_some_and(|blocked| {
                            is_blocked_kimi_resource(
                                file,
                                gate_disabled_kimi_resources,
                                skill_files,
                                blocked,
                            )
                        })
                })
            })
        })
}

fn is_blocked_kimi_resource(
    file: &str,
    gate_disabled_kimi_resources: bool,
    skill_files: &BTreeMap<String, BTreeSet<FrontmatterMode>>,
    blocked_kimi_resource_roots: &BTreeSet<String>,
) -> bool {
    gate_disabled_kimi_resources
        && !skill_files.contains_key(file)
        && blocked_kimi_resource_roots
            .iter()
            .any(|blocked| path_is_within_root(file, blocked))
}

#[cfg(test)]
fn markdown_body(text: &str) -> (&str, usize) {
    markdown_body_for_mode(text, FrontmatterMode::ClaudeDefinition)
}

fn markdown_body_for_mode(text: &str, mode: FrontmatterMode) -> (&str, usize) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = LineIter::new(text);
    let Some(first) = lines.next() else {
        return (text, 1);
    };
    if frontmatter_line(first) != "---" {
        return (text, 1);
    }
    let mut offset = first.len();
    let yaml_start = offset;
    for line in lines {
        let line_start = offset;
        offset += line.len();
        let marker = frontmatter_line(line);
        if marker == "---" || marker == "..." {
            let parser = frontmatter_parser_config(mode);
            let yaml = serde_yaml::from_str_with_config::<serde_yaml::Value>(
                &text[yaml_start..line_start],
                &parser,
            );
            return if yaml.is_ok_and(|value| value.is_mapping()) {
                let first_body_line = 1 + count_line_breaks(&text[..offset]);
                (&text[offset..], first_body_line)
            } else {
                (text, 1)
            };
        }
    }
    (text, 1)
}

fn is_markdown_file(file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        // `.mdx` is deliberately outside the parser contract. Its contents
        // are reported as unparsed instead of partially validating them.
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn is_markdown_file_for_mode(file: &str, mode: FrontmatterMode) -> bool {
    if matches!(
        mode,
        FrontmatterMode::KimiDefinition
            | FrontmatterMode::KimiFlatDefinition
            | FrontmatterMode::KimiCommand
    ) {
        return is_kimi_markdown_file(file);
    }
    is_markdown_file(file)
}

fn is_kimi_markdown_file(file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension == "md")
}

fn is_mdx_file(file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mdx"))
}

fn is_markdown_or_mdx_file(file: &str) -> bool {
    is_markdown_file(file) || is_mdx_file(file)
}

fn resolve_markdown_path(source: &str, target: &str) -> Option<String> {
    if target.starts_with('/')
        || target.starts_with("//")
        || target.contains('\\')
        || target.chars().any(char::is_control)
        || has_uri_scheme(target)
    {
        return None;
    }
    let mut parts = source
        .rsplit_once('/')
        .map(|(parent, _)| parent.split('/').map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_default();
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            value => parts.push(value.to_owned()),
        }
    }
    Some(parts.join("/"))
}

fn is_windows_drive_path(target: &str) -> bool {
    let bytes = target.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_markdown_placeholder(target: &str) -> bool {
    target.split('/').any(|component| {
        let inner = component
            .strip_prefix('<')
            .and_then(|value| value.strip_suffix('>'));
        inner.is_some_and(|value| {
            let mut characters = value.chars();
            characters
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic())
                && characters.all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                })
        })
    })
}

fn template_files(
    context: &ScanContext<'_>,
    owned_template_roots: &BTreeSet<String>,
    linked_template_roots: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut template_roots = owned_template_roots.clone();
    template_roots.extend(linked_template_roots.iter().cloned());
    let prefixes = template_roots
        .iter()
        .map(|root| format!("{root}/"))
        .collect::<BTreeSet<_>>();
    context
        .files
        .iter()
        .filter(|file| prefixes.iter().any(|prefix| file.starts_with(prefix)))
        .cloned()
        .collect()
}

fn codex_canonical_template_files(
    context: &ScanContext<'_>,
    template_files: &BTreeSet<String>,
    provider_resource_scopes: &ProviderResourceScopes,
    codex_repository: Option<&super::file_walk::CodexRepository>,
) -> Result<BTreeSet<String>, GeneratorError> {
    let canonical_root = fs::canonicalize(context.root)
        .map_err(|error| GeneratorError::io("resolve repository root", context.root, &error))?;
    let mut canonical_files = BTreeSet::new();
    for file in template_files {
        if !resource_scope_applies(&provider_resource_scopes.codex, file, file) {
            continue;
        }
        let Some(target) = codex_repository
            .ok_or_else(|| GeneratorError::usage("Codex template file has no repository view"))?
            .path(file, context.exclude, "Codex template file")?
        else {
            continue;
        };
        let Ok(relative) = target.strip_prefix(&canonical_root) else {
            continue;
        };
        let mut parts = Vec::new();
        for component in relative.components() {
            let Component::Normal(part) = component else {
                continue;
            };
            let Some(part) = part.to_str() else {
                parts.clear();
                break;
            };
            parts.push(part);
        }
        if !parts.is_empty() {
            canonical_files.insert(parts.join("/"));
        }
    }
    Ok(canonical_files)
}

fn validate_templates(
    context: &ScanContext<'_>,
    template_files: &BTreeSet<String>,
    provider_resource_scopes: &ProviderResourceScopes,
    codex_repository: Option<&super::file_walk::CodexRepository>,
) -> Result<(), GeneratorError> {
    for file in template_files {
        let path = if resource_scope_applies(&provider_resource_scopes.codex, file, file) {
            codex_repository
                .ok_or_else(|| GeneratorError::usage("Codex template file has no repository view"))?
                .path(file, context.exclude, "Codex template file")?
                .ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "Codex template file {file} is not a tracked repository file"
                    ))
                })?
        } else {
            repository_path(context, file, "template file")?
        };
        let extension = Path::new(file)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("md")
                if Path::new(file)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md")) =>
            {
                let text = read_text(&path, "read template")?;
                parse_frontmatter(&text, file, FrontmatterMode::EmbeddedTemplate)?;
            }
            Some("json") => {
                let text = read_text(&path, "read template")?;
                parse_json(&text, Path::new(file), "JSON template")?;
            }
            Some("toml") => {
                let text = read_text(&path, "read template")?;
                text.parse::<toml::Table>().map_err(|error| {
                    GeneratorError::usage(format!("invalid TOML template {file}: {error}"))
                })?;
            }
            Some("yaml" | "yml") => {
                let text = read_text(&path, "read template")?;
                let parser = serde_yaml::ParserConfig::default()
                    .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error);
                serde_yaml::from_str_with_config::<serde_yaml::Value>(&text, &parser).map_err(
                    |error| GeneratorError::usage(format!("invalid YAML template {file}: {error}")),
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_frontmatter(
    text: &str,
    path: &str,
    mode: FrontmatterMode,
) -> Result<Frontmatter, GeneratorError> {
    let text = if mode == FrontmatterMode::CodexDefinition {
        text
    } else {
        text.strip_prefix('\u{feff}').unwrap_or(text)
    };
    let Some(yaml) = extract_frontmatter_yaml(text, path, mode)? else {
        return Ok(Frontmatter {
            values: BTreeMap::new(),
            kimi_sub_skills_enabled: false,
        });
    };
    parse_frontmatter_yaml(yaml, path, mode)
}

fn extract_frontmatter_yaml<'a>(
    text: &'a str,
    path: &str,
    mode: FrontmatterMode,
) -> Result<Option<&'a str>, GeneratorError> {
    let mut lines = if mode == FrontmatterMode::CodexDefinition {
        LineIter::new_lf_only(text)
    } else {
        LineIter::new(text)
    };
    let Some(opening_line) = lines.next() else {
        if matches!(
            mode,
            FrontmatterMode::FlatDefinition
                | FrontmatterMode::KimiFlatDefinition
                | FrontmatterMode::KimiCommand
                | FrontmatterMode::ClaudeDefinition
                | FrontmatterMode::ClaudeCommand
        ) {
            return Ok(None);
        }
        return Err(GeneratorError::usage(format!(
            "{path} is missing opening frontmatter delimiter"
        )));
    };
    let opening_delimiter = frontmatter_line(opening_line);
    let opening_delimiter = if mode == FrontmatterMode::CodexDefinition {
        opening_delimiter.trim()
    } else {
        opening_delimiter
    };
    if opening_delimiter != "---" {
        if matches!(
            mode,
            FrontmatterMode::FlatDefinition
                | FrontmatterMode::KimiFlatDefinition
                | FrontmatterMode::KimiCommand
                | FrontmatterMode::ClaudeDefinition
                | FrontmatterMode::ClaudeCommand
        ) {
            return Ok(None);
        }
        return Err(GeneratorError::usage(format!(
            "{path} is missing opening frontmatter delimiter"
        )));
    }
    let yaml_start = opening_line.len();
    let mut offset = yaml_start;
    let mut closed = false;
    let mut yaml_end = yaml_start;
    for raw_line in lines {
        let line_start = offset;
        offset += raw_line.len();
        let line = frontmatter_line(raw_line);
        let line = if mode == FrontmatterMode::CodexDefinition {
            line.trim()
        } else {
            line
        };
        let is_closing_delimiter = if mode == FrontmatterMode::CodexDefinition {
            line == "---"
        } else {
            matches!(line, "---" | "...")
        };
        if is_closing_delimiter {
            yaml_end = line_start;
            closed = true;
            break;
        }
    }
    if !closed
        && matches!(
            mode,
            FrontmatterMode::FlatDefinition
                | FrontmatterMode::KimiFlatDefinition
                | FrontmatterMode::KimiCommand
                | FrontmatterMode::ClaudeDefinition
                | FrontmatterMode::ClaudeCommand
        )
    {
        return Ok(None);
    }
    if !closed {
        return Err(GeneratorError::usage(format!(
            "{path} is missing closing frontmatter delimiter"
        )));
    }
    Ok(Some(&text[yaml_start..yaml_end]))
}

fn parse_frontmatter_yaml(
    yaml_text: &str,
    path: &str,
    mode: FrontmatterMode,
) -> Result<Frontmatter, GeneratorError> {
    let parser = frontmatter_parser_config(mode);
    let mut repaired_yaml_text = None;
    let yaml = match serde_yaml::from_str_with_config::<serde_yaml::Value>(yaml_text, &parser) {
        Ok(yaml) => yaml,
        Err(original_error) if mode == FrontmatterMode::CodexDefinition => {
            let Some(repaired) = repair_codex_frontmatter_scalar_fields(yaml_text) else {
                return Err(invalid_frontmatter_yaml(path, &original_error));
            };
            match serde_yaml::from_str_with_config::<serde_yaml::Value>(&repaired, &parser) {
                Ok(yaml) => {
                    repaired_yaml_text = Some(repaired);
                    yaml
                }
                Err(_) => return Err(invalid_frontmatter_yaml(path, &original_error)),
            }
        }
        Err(error) => return Err(invalid_frontmatter_yaml(path, &error)),
    };
    let parsed_yaml_text = repaired_yaml_text.as_deref().unwrap_or(yaml_text);
    let Some(mapping) = yaml.as_mapping() else {
        return Err(GeneratorError::usage(format!(
            "{path} frontmatter must contain a YAML mapping"
        )));
    };
    reject_yaml_tags(&yaml, path)?;
    let typed_metadata = if matches!(
        mode,
        FrontmatterMode::KimiDefinition
            | FrontmatterMode::KimiFlatDefinition
            | FrontmatterMode::KimiCommand
    ) {
        // Kimi copies arbitrary frontmatter values into skill metadata. Its
        // parser does not constrain nested metadata to string values.
        None
    } else {
        Some(
            serde_yaml::from_str_borrowing_with_config::<TypedFrontmatterMetadata>(
                parsed_yaml_text,
                &parser,
            )
            .map_err(|error| {
                GeneratorError::usage(format!(
                    "{path} frontmatter mapping keys must be YAML strings: {}",
                    frontmatter_yaml_error(&error)
                ))
            })?
            .0,
        )
    };
    let invalid_metadata_value = mode != FrontmatterMode::EmbeddedTemplate
        && typed_metadata
            .as_ref()
            .and_then(Option::as_ref)
            .is_some_and(|metadata| {
                metadata.iter().any(|(key, value)| {
                    let Some(key) = key.as_str() else {
                        return true;
                    };
                    if mode == FrontmatterMode::CodexDefinition {
                        key == "short-description" && !value.is_null() && value.as_str().is_none()
                    } else {
                        value.as_str().is_none() && !is_kimi_sub_skill_flag(mode, key, value)
                    }
                })
            });
    if invalid_metadata_value {
        let message = if mode == FrontmatterMode::CodexDefinition {
            format!("{path} frontmatter metadata.short-description must be a YAML string or null")
        } else {
            format!(
                "{path} frontmatter metadata must use string values, except for Kimi's boolean has-sub-skill flags"
            )
        };
        return Err(GeneratorError::usage(message));
    }
    validate_frontmatter_mapping(mapping, path, mode)?;
    let values = mapping
        .iter()
        .filter_map(|(key, value)| {
            let key = key.as_str();
            let value = match value {
                serde_yaml::Value::String(value) => value.clone(),
                serde_yaml::Value::Bool(value) => value.to_string(),
                _ => return None,
            };
            Some((key.to_owned(), value))
        })
        .collect();
    Ok(Frontmatter {
        values,
        kimi_sub_skills_enabled: mode == FrontmatterMode::KimiDefinition
            && kimi_sub_skills_enabled(mapping),
    })
}

fn frontmatter_parser_config(mode: FrontmatterMode) -> serde_yaml::ParserConfig {
    if mode == FrontmatterMode::CodexDefinition {
        // Codex calls serde_yaml::from_str directly, so YAML anchors and aliases
        // use the parser's default expansion policy. Keep duplicate and merge
        // keys fail-closed; aliases themselves remain supported.
        serde_yaml::ParserConfig::default()
            .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error)
            .merge_key_policy(serde_yaml::MergeKeyPolicy::Error)
    } else {
        serde_yaml::ParserConfig::strict()
            .max_alias_expansions(0)
            .merge_key_policy(serde_yaml::MergeKeyPolicy::Error)
    }
}

fn invalid_frontmatter_yaml(path: &str, error: &serde_yaml::Error) -> GeneratorError {
    GeneratorError::usage(format!(
        "{path} has invalid YAML frontmatter: {}",
        frontmatter_yaml_error(error)
    ))
}

fn repair_codex_frontmatter_scalar_fields(frontmatter: &str) -> Option<String> {
    let mut changed = false;
    let mut block_scalar_indent: Option<usize> = None;
    let mut repaired_lines = Vec::new();
    for line in frontmatter.lines() {
        let indent = line
            .chars()
            .take_while(|character| *character == ' ')
            .count();
        if let Some(block_indent) = block_scalar_indent {
            if line.trim().is_empty() || indent > block_indent {
                repaired_lines.push(line.to_owned());
                continue;
            }
            block_scalar_indent = None;
        }
        let Some((key, value)) = line.split_once(':') else {
            repaired_lines.push(line.to_owned());
            continue;
        };
        if key.trim().is_empty() || !value.chars().next().is_none_or(char::is_whitespace) {
            repaired_lines.push(line.to_owned());
            continue;
        }
        let trimmed_start = value.trim_start();
        let leading_whitespace = &value[..value.len() - trimmed_start.len()];
        let mut scalar = trimmed_start;
        let mut comment = "";
        for (index, character) in trimmed_start.char_indices() {
            if character == '#'
                && (index == 0
                    || trimmed_start[..index]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace))
            {
                let comment_start = trimmed_start[..index].trim_end().len();
                scalar = &trimmed_start[..comment_start];
                comment = &trimmed_start[comment_start..];
                break;
            }
        }
        let scalar = scalar.trim_end();
        let Some(first_char) = scalar.chars().next() else {
            repaired_lines.push(line.to_owned());
            continue;
        };
        if matches!(first_char, '|' | '>') {
            block_scalar_indent = Some(indent);
            repaired_lines.push(line.to_owned());
            continue;
        }
        if matches!(first_char, '\'' | '"') {
            repaired_lines.push(line.to_owned());
            continue;
        }
        let has_colon_separator = scalar
            .chars()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair[0] == ':' && pair[1].is_whitespace());
        let invalid_flow_like_scalar = matches!(first_char, '[' | '{' | '@' | '`')
            && serde_yaml::from_str::<serde_yaml::Value>(scalar).is_err();
        if !has_colon_separator && !invalid_flow_like_scalar {
            repaired_lines.push(line.to_owned());
            continue;
        }
        let quoted_scalar = format!("'{}'", scalar.replace('\'', "''"));
        repaired_lines.push(format!(
            "{key}:{leading_whitespace}{quoted_scalar}{comment}"
        ));
        changed = true;
    }
    changed.then(|| repaired_lines.join("\n"))
}

fn frontmatter_line(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

fn frontmatter_yaml_error(error: &serde_yaml::Error) -> String {
    let mut message = error.to_string();
    if let Some(location) = error.location() {
        let parser_location = format!("line {} column {}", location.line(), location.column());
        let source_location = format!("line {} column {}", location.line() + 1, location.column());
        if let Some((prefix, _)) = message.rsplit_once(&parser_location) {
            message = format!("{prefix}{source_location}");
        }
    }
    message
}

struct LineIter<'a> {
    text: &'a str,
    offset: usize,
    split_bare_cr: bool,
}

impl<'a> LineIter<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            offset: 0,
            split_bare_cr: true,
        }
    }

    fn new_lf_only(text: &'a str) -> Self {
        Self {
            text,
            offset: 0,
            split_bare_cr: false,
        }
    }
}

impl<'a> Iterator for LineIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.text.len() {
            return None;
        }
        let start = self.offset;
        let bytes = self.text.as_bytes();
        let mut end = self.text.len();
        for (index, byte) in bytes.iter().enumerate().skip(start) {
            match byte {
                b'\n' => {
                    end = index + 1;
                    break;
                }
                b'\r' if self.split_bare_cr => {
                    end = if bytes.get(index + 1) == Some(&b'\n') {
                        index + 2
                    } else {
                        index + 1
                    };
                    break;
                }
                _ => {}
            }
        }
        self.offset = end;
        Some(&self.text[start..end])
    }
}

fn count_line_breaks(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                count += 1;
                index += usize::from(bytes.get(index + 1) == Some(&b'\n')) + 1;
            }
            b'\n' => {
                count += 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    count
}

struct TypedFrontmatterMetadata(Option<serde_yaml::MappingAny>);

impl<'de> Deserialize<'de> for TypedFrontmatterMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(TypedFrontmatterMetadataVisitor)
    }
}

struct TypedFrontmatterMetadataVisitor;

impl<'de> Visitor<'de> for TypedFrontmatterMetadataVisitor {
    type Value = TypedFrontmatterMetadata;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("frontmatter with string keys and optional string metadata")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut metadata = None;
        while let Some(key) = map.next_key::<serde_yaml::Value>()? {
            let Some(key) = key.as_str() else {
                return Err(de::Error::custom("frontmatter keys must be YAML strings"));
            };
            if key == "metadata" {
                metadata = Some(map.next_value::<serde_yaml::MappingAny>()?);
            } else {
                let _: IgnoredAny = map.next_value()?;
            }
        }
        Ok(TypedFrontmatterMetadata(metadata))
    }
}

fn reject_yaml_tags(value: &serde_yaml::Value, path: &str) -> Result<(), GeneratorError> {
    match value {
        serde_yaml::Value::Tagged(_) => Err(GeneratorError::usage(format!(
            "{path} frontmatter does not allow YAML tags or aliases"
        ))),
        serde_yaml::Value::Sequence(values) => {
            for value in values {
                reject_yaml_tags(value, path)?;
            }
            Ok(())
        }
        serde_yaml::Value::Mapping(mapping) => {
            for (_, value) in mapping {
                reject_yaml_tags(value, path)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_frontmatter_mapping(
    mapping: &serde_yaml::Mapping,
    path: &str,
    mode: FrontmatterMode,
) -> Result<(), GeneratorError> {
    if mode == FrontmatterMode::EmbeddedTemplate {
        return Ok(());
    }

    validate_frontmatter_name(mapping, path, mode)?;
    validate_frontmatter_description(mapping, path, mode)?;
    validate_frontmatter_optional_fields(mapping, path, mode)?;
    validate_frontmatter_metadata(mapping, path, mode)
}

fn validate_frontmatter_name(
    mapping: &serde_yaml::Mapping,
    path: &str,
    mode: FrontmatterMode,
) -> Result<(), GeneratorError> {
    let name = mapping.get("name").and_then(serde_yaml::Value::as_str);
    let optional = matches!(
        mode,
        FrontmatterMode::FlatDefinition
            | FrontmatterMode::KimiFlatDefinition
            | FrontmatterMode::KimiCommand
            | FrontmatterMode::CodexDefinition
            | FrontmatterMode::ClaudeDefinition
            | FrontmatterMode::ClaudeCommand
            | FrontmatterMode::AntigravityDefinition
    );
    if !optional && name.is_none() {
        return Err(GeneratorError::usage(format!(
            "{path} frontmatter name must be a YAML string"
        )));
    }
    let null_codex_name = mode == FrontmatterMode::CodexDefinition
        && mapping.get("name").is_some_and(serde_yaml::Value::is_null);
    let optional_kimi_name = matches!(
        mode,
        FrontmatterMode::KimiFlatDefinition | FrontmatterMode::KimiCommand
    );
    if mapping.contains_key("name") && name.is_none() && !null_codex_name && !optional_kimi_name {
        return Err(GeneratorError::usage(format!(
            "{path} frontmatter name must be a YAML string"
        )));
    }
    if mode == FrontmatterMode::CodexDefinition {
        return Ok(());
    }
    if let Some(name) = name {
        if matches!(
            mode,
            FrontmatterMode::ClaudeDefinition
                | FrontmatterMode::KimiFlatDefinition
                | FrontmatterMode::KimiCommand
        ) {
            if (name.trim().is_empty()
                && !matches!(
                    mode,
                    FrontmatterMode::KimiFlatDefinition | FrontmatterMode::KimiCommand
                ))
                || name.chars().any(char::is_control)
            {
                return Err(GeneratorError::usage(format!(
                    "{path} frontmatter name must be nonempty and contain no control characters"
                )));
            }
        } else if mode == FrontmatterMode::KimiDefinition {
            if name.trim().is_empty() {
                return Err(GeneratorError::usage(format!(
                    "{path} frontmatter name must be nonempty"
                )));
            }
        } else {
            validate_skill_name(name).map_err(|_| {
                GeneratorError::usage(format!(
                    "{path} frontmatter name {name} is not a valid Agent Skill name"
                ))
            })?;
        }
    }
    Ok(())
}

fn validate_frontmatter_description(
    mapping: &serde_yaml::Mapping,
    path: &str,
    mode: FrontmatterMode,
) -> Result<(), GeneratorError> {
    let optional = matches!(
        mode,
        FrontmatterMode::FlatDefinition
            | FrontmatterMode::KimiFlatDefinition
            | FrontmatterMode::KimiCommand
            | FrontmatterMode::ClaudeDefinition
            | FrontmatterMode::ClaudeCommand
    );
    let description = mapping
        .get("description")
        .and_then(serde_yaml::Value::as_str);
    if !optional && description.is_none() {
        return Err(GeneratorError::usage(format!(
            "{path} frontmatter description must be a YAML string"
        )));
    }
    let optional_kimi_description = matches!(
        mode,
        FrontmatterMode::KimiFlatDefinition | FrontmatterMode::KimiCommand
    );
    if mapping.contains_key("description") && description.is_none() && !optional_kimi_description {
        return Err(GeneratorError::usage(format!(
            "{path} frontmatter description must be a YAML string"
        )));
    }
    let valid_kimi_optional = matches!(
        mode,
        FrontmatterMode::KimiFlatDefinition | FrontmatterMode::KimiCommand
    );
    if description.is_some_and(|description| {
        (!valid_kimi_optional && description.trim().is_empty())
            || (!matches!(
                mode,
                FrontmatterMode::KimiDefinition
                    | FrontmatterMode::CodexDefinition
                    | FrontmatterMode::KimiFlatDefinition
                    | FrontmatterMode::KimiCommand
            ) && description.chars().count() > 1024)
    }) {
        return Err(GeneratorError::usage(format!(
            "{path} frontmatter description must be non-empty and within the provider's supported length"
        )));
    }
    Ok(())
}

fn validate_frontmatter_optional_fields(
    mapping: &serde_yaml::Mapping,
    path: &str,
    mode: FrontmatterMode,
) -> Result<(), GeneratorError> {
    if mode == FrontmatterMode::CodexDefinition {
        return Ok(());
    }
    if matches!(
        mode,
        FrontmatterMode::KimiDefinition | FrontmatterMode::KimiFlatDefinition
    ) {
        if let Some(skill_type) = mapping
            .get("type")
            .and_then(serde_yaml::Value::as_str)
            .map(str::trim)
            .filter(|skill_type| !skill_type.is_empty())
            && !matches!(skill_type, "prompt" | "inline" | "flow" | "reference")
        {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter type {skill_type} is not supported by Kimi; expected prompt, inline, flow, or reference"
            )));
        }
        // The Kimi parser preserves arbitrary frontmatter metadata and treats
        // non-string/blank typed fields as absent.
        return Ok(());
    }
    if mode == FrontmatterMode::KimiCommand {
        return Ok(());
    }
    for key in [
        "license",
        "compatibility",
        "argument-hint",
        "model",
        "context",
        "agent",
    ] {
        if let Some(value) = mapping.get(key)
            && !value.is_string()
        {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter {key} must be a YAML string"
            )));
        }
    }
    if let Some(value) = mapping.get("allowed-tools") {
        let is_claude = matches!(
            mode,
            FrontmatterMode::ClaudeDefinition | FrontmatterMode::ClaudeCommand
        );
        let is_string_list = is_claude
            && value
                .as_sequence()
                .is_some_and(|values| values.iter().all(|value| value.as_str().is_some()));
        if value.as_str().is_none() && !is_string_list {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter allowed-tools must be a string or a Claude string list"
            )));
        }
    }
    if let Some(value) = mapping.get("compatibility")
        && value
            .as_str()
            .is_some_and(|value| value.trim().is_empty() || value.chars().count() > 500)
    {
        return Err(GeneratorError::usage(format!(
            "{path} frontmatter compatibility must contain 1-500 non-whitespace characters"
        )));
    }
    for key in ["user-invocable", "disable-model-invocation"] {
        if let Some(value) = mapping.get(key)
            && frontmatter_boolean(value, mode).is_none()
        {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter {key} must be a YAML boolean"
            )));
        }
    }
    if matches!(
        mode,
        FrontmatterMode::ClaudeDefinition | FrontmatterMode::ClaudeCommand
    ) && let Some(value) = mapping.get("background")
        && frontmatter_boolean(value, mode).is_none()
    {
        return Err(GeneratorError::usage(format!(
            "{path} frontmatter background must use a documented boolean spelling"
        )));
    }
    Ok(())
}

fn frontmatter_boolean(value: &serde_yaml::Value, mode: FrontmatterMode) -> Option<bool> {
    if let Some(value) = value.as_bool() {
        return Some(value);
    }
    if !matches!(
        mode,
        FrontmatterMode::ClaudeDefinition | FrontmatterMode::ClaudeCommand
    ) {
        return None;
    }
    match value {
        serde_yaml::Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        },
        serde_yaml::Value::Number(value) => match value.as_i64() {
            Some(1) => Some(true),
            Some(0) => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn validate_frontmatter_metadata(
    mapping: &serde_yaml::Mapping,
    path: &str,
    mode: FrontmatterMode,
) -> Result<(), GeneratorError> {
    if matches!(
        mode,
        FrontmatterMode::KimiDefinition
            | FrontmatterMode::KimiFlatDefinition
            | FrontmatterMode::KimiCommand
    ) {
        return Ok(());
    }
    if let Some(metadata) = mapping.get("metadata") {
        let metadata = metadata.as_mapping().ok_or_else(|| {
            GeneratorError::usage(format!(
                "{path} frontmatter metadata must be a string-to-string mapping"
            ))
        })?;
        if mode == FrontmatterMode::CodexDefinition {
            if metadata.iter().any(|(key, value)| {
                key == "short-description" && !value.is_null() && value.as_str().is_none()
            }) {
                return Err(GeneratorError::usage(format!(
                    "{path} frontmatter metadata.short-description must be a YAML string or null"
                )));
            }
            return Ok(());
        }
        if metadata.iter().any(|(key, value)| {
            value.as_str().is_none() && !is_kimi_sub_skill_flag(mode, key.as_str(), value)
        }) {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter metadata must be a string-to-string mapping, except for boolean Kimi sub-skill flags"
            )));
        }
    }
    Ok(())
}

fn read_json_object(
    context: &ScanContext<'_>,
    relative: &str,
) -> Result<serde_json::Map<String, Value>, GeneratorError> {
    require_file(context, relative)?;
    let path = repository_path(context, relative, "plugin manifest")?;
    let text = read_text(&path, "read JSON manifest")?;
    let value = parse_json(&text, &path, relative)?;
    value.as_object().cloned().ok_or_else(|| {
        GeneratorError::usage(format!("JSON manifest {relative} must contain an object"))
    })
}

fn parse_json(text: &str, path: &Path, label: &str) -> Result<Value, GeneratorError> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = StrictJsonValue::deserialize(&mut deserializer).map_err(|error| {
        GeneratorError::usage(format!("invalid {label} at {}: {error}", path.display()))
    })?;
    deserializer.end().map_err(|error| {
        GeneratorError::usage(format!("invalid {label} at {}: {error}", path.display()))
    })?;
    Ok(value.0)
}

struct StrictJsonValue(Value);

impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor)
    }
}

struct StrictJsonVisitor;

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = StrictJsonValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value with unique object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(|number| StrictJsonValue(Value::Number(number)))
            .ok_or_else(|| E::custom("JSON number is not finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictJsonValue>()? {
            values.push(value.0);
        }
        Ok(StrictJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key {key}"
                )));
            }
            let value = map.next_value::<StrictJsonValue>()?;
            object.insert(key, value.0);
        }
        Ok(StrictJsonValue(Value::Object(object)))
    }
}

fn string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<&'a str, GeneratorError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| GeneratorError::usage(format!("{path} requires string field `{key}`")))
}

fn require_file(context: &ScanContext<'_>, relative: &str) -> Result<(), GeneratorError> {
    if context.file_set.contains(relative) {
        Ok(())
    } else {
        Err(GeneratorError::usage(format!(
            "skills plugin is missing tracked file `{relative}`"
        )))
    }
}

fn read_text(path: &Path, operation: &str) -> Result<String, GeneratorError> {
    fs::read_to_string(path).map_err(|error| GeneratorError::io(operation, path, &error))
}

fn repository_path(
    context: &ScanContext<'_>,
    relative: &str,
    purpose: &str,
) -> Result<PathBuf, GeneratorError> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute() || relative.contains('\\') {
        return Err(GeneratorError::usage(format!(
            "{purpose} path {relative} must stay inside the repository"
        )));
    }
    let components = relative_path
        .components()
        .map(|component| match component {
            Component::Normal(value) => Ok(PathBuf::from(value)),
            Component::CurDir if relative == "." => Ok(PathBuf::from(".")),
            _ => Err(GeneratorError::usage(format!(
                "{purpose} path {relative} must stay inside the repository"
            ))),
        })
        .collect::<Result<Vec<_>, GeneratorError>>()?;
    let mut current = context.root.to_path_buf();
    if relative == "." {
        return Ok(current);
    }
    for (index, component) in components.iter().enumerate() {
        if component == "." {
            continue;
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(GeneratorError::usage(format!(
                    "{purpose} path {relative} traverses a symbolic link"
                )));
            }
            Ok(metadata) if index + 1 < components.len() && !metadata.is_dir() => {
                return Err(GeneratorError::usage(format!(
                    "{purpose} path {relative} traverses a non-directory component"
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(GeneratorError::io(
                    "inspect repository path",
                    &current,
                    &error,
                ));
            }
        }
    }
    Ok(context.root.join(relative_path))
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;
