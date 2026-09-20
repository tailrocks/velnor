#![expect(
    clippy::panic,
    clippy::expect_used,
    reason = "test helpers panic with context when a regression breaks their precondition"
)]

use super::{
    detect, is_markdown_placeholder, markdown_body, normalize_markdown_destination,
    parse_frontmatter, resolve_markdown_path, validate_codex_skill_name, FrontmatterMode,
};
use crate::s2::provider::{ProviderId, ProviderSet};
use crate::s2::scan::{RepositoryShape, ScanContext};
use crate::s2::{GeneratorError, UnitKind};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
const PORTABLE_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
const ANTIGRAVITY_SCHEMA: &str = "https://antigravity.google/schemas/v1/plugin.json";

fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error}"),
    }
}

struct Fixture {
    root: PathBuf,
    files: BTreeSet<String>,
}

impl Fixture {
    fn empty() -> Self {
        let root = std::env::temp_dir().join(format!(
            "skills-adapter-fixture-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        must(fs::create_dir_all(&root), "create fixture root");
        Self {
            root,
            files: BTreeSet::new(),
        }
    }

    fn claude_plugin() -> Self {
        let mut fixture = Self::empty();
        fixture.add(".claude-plugin/plugin.json", r#"{"name":"example-plugin"}"#);
        fixture.add("skills/example/SKILL.md", &skill("example"));
        fixture
    }

    fn add(&mut self, relative: &str, contents: &str) {
        self.add_bytes(relative, contents.as_bytes());
    }

    fn add_bytes(&mut self, relative: &str, contents: &[u8]) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            must(fs::create_dir_all(parent), "create fixture parent");
        }
        must(fs::write(path, contents), "write fixture file");
        self.files.insert(relative.to_owned());
    }

    fn git(&self, arguments: &[&str]) {
        let output = must(
            Command::new("git")
                .current_dir(&self.root)
                .args(arguments)
                .output(),
            "run git fixture command",
        );
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_detect(&self) -> Result<(BTreeSet<String>, RepositoryShape), GeneratorError> {
        let files = self.files.iter().cloned().collect::<Vec<_>>();
        let file_set = self.files.clone();
        let context = ScanContext {
            root: &self.root,
            files: &files,
            file_set: &file_set,
            exclude: &[],
        };
        let mut shape = empty_shape();
        let hidden = detect(&context, &mut shape)?;
        Ok((hidden, shape))
    }

    fn run_detect_failure(&self) -> (GeneratorError, RepositoryShape) {
        let files = self.files.iter().cloned().collect::<Vec<_>>();
        let file_set = self.files.clone();
        let context = ScanContext {
            root: &self.root,
            files: &files,
            file_set: &file_set,
            exclude: &[],
        };
        let mut shape = empty_shape();
        let error = detect(&context, &mut shape).expect_err("fixture must be rejected");
        (error, shape)
    }

    fn run_scan(&self) -> Result<RepositoryShape, GeneratorError> {
        self.run_scan_with_exclude(&[])
    }

    fn run_scan_with_exclude(&self, exclude: &[String]) -> Result<RepositoryShape, GeneratorError> {
        super::super::scan_shape(
            &self.root,
            &ProviderSet::from([ProviderId::GithubHosted]),
            "main",
            exclude,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn skill(name: &str) -> String {
    format!("---\nname: {name}\ndescription: A valid Agent Skill.\n---\n# {name}\n")
}

fn empty_shape() -> RepositoryShape {
    RepositoryShape {
        files: Vec::new(),
        units: Vec::new(),
        detected: Vec::new(),
        limitations: Vec::new(),
        default_branch: "main".to_owned(),
        providers: ProviderSet::from([ProviderId::GithubHosted]),
    }
}

#[test]
fn no_plugin_manifest_is_a_noop() {
    let mut fixture = Fixture::empty();
    fixture.add("README.md", "# Ordinary repository\n");
    let (hidden, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("ordinary repository is not a plugin: {error}"));
    assert!(hidden.is_empty());
    assert!(shape.detected.is_empty());
}

#[test]
fn valid_portable_plugin_needs_no_catalog_or_generated_docs() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable-plugin"}}"#),
    );
    fixture.add("skills/alpha/SKILL.md", &skill("alpha"));
    fixture.add("skills/beta/SKILL.md", &skill("beta"));

    let (hidden, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("manifest-only plugin is valid: {error}"));
    assert!(hidden.is_empty());
    assert!(shape.detected.contains(&"skills-plugin".to_owned()));
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));
    assert_eq!(
        shape
            .detected
            .iter()
            .filter(|item| item.starts_with("skills-count:"))
            .count(),
        1,
        "the count reports discovered skill definitions once, not a consumer census"
    );
    assert!(!fixture.files.contains("catalog.json"));
    assert!(!fixture.files.contains("docs/index.json"));
    assert!(
        shape.units.is_empty(),
        "plugin metadata is not an executable unit"
    );
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("validated during repository scanning")
            && limitation.contains("no provider-independent CI verification command")
    }));
}

#[test]
fn portable_default_root_requires_each_skill_directory() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable-plugin"}}"#),
    );
    fixture.add("skills/SKILL.md", "not a skill definition\n");

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("the default root contains no root-level skill: {error}"));
    assert!(shape.detected.contains(&"skills-count:0".to_owned()));
    assert!(shape
        .detected
        .contains(&"skills-provider:agent-plugins".to_owned()));
}

#[test]
fn portable_skill_filename_is_case_sensitive() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable-plugin"}}"#),
    );
    fixture.add("skills/example/skill.md", &skill("example"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("case-variant skill filename is not a definition: {error}"));
    assert!(shape.detected.contains(&"skills-count:0".to_owned()));
}

#[test]
fn portable_file_skills_root_invalidates_only_that_component() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable-plugin"}}"#),
    );
    fixture.add("skills", "this path is a file, not a directory\n");
    fixture.add(
        ".kimi-plugin/plugin.json",
        r#"{"name":"kimi-plugin","skills":"./other-skills"}"#,
    );
    fixture.add("other-skills/example/SKILL.md", &skill("example"));

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("invalid Portable Skills component does not stop independent providers: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape
        .detected
        .contains(&"skills-provider:agent-plugins".to_owned()));
    assert!(shape.detected.contains(&"skills-provider:kimi".to_owned()));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("Agent Plugins")
            && limitation.contains("fixed skills root")
            && limitation.contains("component")
    }));
}

#[test]
fn portable_unknown_skills_field_does_not_read_a_file_as_a_root() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(
            r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable-plugin","skills":"./custom"}}"#
        ),
    );
    fixture.add("custom", "not a directory\n");
    fixture.add("skills/default/SKILL.md", &skill("default"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("schema-unknown Portable field is inert: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("field `skills` is outside the published schema")
    }));
}

#[test]
fn agent_plugins_antigravity_and_claude_recover_skills_and_resources_in_output_dirs() {
    for use_git_index in [false, true] {
        let mut portable = Fixture::empty();
        portable.add(
            "plugin.json",
            &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable-plugin"}}"#),
        );
        portable.add("skills/target/SKILL.md", &skill("target"));
        portable.add("skills/target/dist/guide.md", "# Guide\n");
        if use_git_index {
            portable.git(&["init", "-q"]);
            portable.git(&["add", "-f", "."]);
        }
        let shape = portable
            .run_scan()
            .unwrap_or_else(|error| panic!("Portable output-named Skill is found: {error}"));
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));
        assert!(!shape
            .files
            .contains(&"skills/target/dist/guide.md".to_owned()));
        portable.add("skills/target/dist/guide.md", "[missing](missing.md)\n");
        if use_git_index {
            portable.git(&["add", "-f", "skills/target/dist/guide.md"]);
        }
        let error = portable
            .run_scan()
            .expect_err("Markdown under a pruned resource directory is validated");
        assert!(error
            .to_string()
            .contains("missing Markdown link missing.md"));

        let mut antigravity = Fixture::empty();
        antigravity.add(
            "plugin.json",
            &format!(r#"{{"$schema":"{ANTIGRAVITY_SCHEMA}","name":"antigravity-plugin"}}"#),
        );
        antigravity.add("skills/target/SKILL.md", &skill("target"));
        antigravity.add("skills/target/coverage/guide.md", "# Guide\n");
        if use_git_index {
            antigravity.git(&["init", "-q"]);
            antigravity.git(&["add", "-f", "."]);
        }
        let shape = antigravity
            .run_scan()
            .unwrap_or_else(|error| panic!("Antigravity output-named Skill is found: {error}"));
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));
        assert!(!shape
            .files
            .contains(&"skills/target/coverage/guide.md".to_owned()));

        let mut claude = Fixture::empty();
        claude.add(
            ".claude-plugin/plugin.json",
            r#"{"name":"claude-plugin","skills":"./target/skills"}"#,
        );
        claude.add("target/skills/example/SKILL.md", &skill("example"));
        claude.add("target/skills/example/dist/guide.md", "# Guide\n");
        if use_git_index {
            claude.git(&["init", "-q"]);
            claude.git(&["add", "-f", "."]);
        }
        let shape = claude
            .run_scan()
            .unwrap_or_else(|error| panic!("Claude declared output path is found: {error}"));
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));
        assert!(!shape
            .files
            .contains(&"target/skills/example/dist/guide.md".to_owned()));
        claude.add(
            "target/skills/example/dist/guide.md",
            "[missing](missing.md)\n",
        );
        if use_git_index {
            claude.git(&["add", "-f", "target/skills/example/dist/guide.md"]);
        }
        let error = claude
            .run_scan()
            .expect_err("Claude Markdown under a pruned resource directory is validated");
        assert!(error
            .to_string()
            .contains("missing Markdown link missing.md"));
    }
}

#[test]
fn a_skill_named_templates_is_discovered_and_its_own_templates_stay_inert() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable-plugin"}}"#),
    );
    fixture.add("skills/templates/SKILL.md", &skill("templates"));
    fixture.add(
        "skills/templates/templates/package.json",
        r#"{"name":"template"}"#,
    );

    let (hidden, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("a skill directory may be named templates: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(hidden.contains("skills/templates/templates/package.json"));
    assert!(!hidden.contains("skills/templates/SKILL.md"));
}

#[test]
fn skill_count_tracks_actual_discovered_files_and_deduplicates_roots() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"count-plugin","skills":"./extra-skills/"}"#,
    );
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"count-claude-plugin"}"#,
    );
    fixture.add("skills/first/SKILL.md", &skill("first"));
    fixture.add("skills/second/SKILL.md", &skill("second"));
    fixture.add("extra-skills/third/SKILL.md", &skill("third"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("default and declared roots are valid: {error}"));
    assert!(shape.detected.contains(&"skills-count:3".to_owned()));
    assert!(shape.detected.contains(&"skills-provider:codex".to_owned()));
    assert!(shape
        .detected
        .contains(&"skills-provider:claude".to_owned()));

    fixture.add("extra-skills/fourth/SKILL.md", &skill("fourth"));
    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("four discovered skills are valid: {error}"));
    assert!(shape.detected.contains(&"skills-count:4".to_owned()));
}

#[test]
fn portable_manifest_takes_precedence_over_codex_compatibility_manifest() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable-plugin","skills":"./portable-unsupported"}}"#),
    );
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./codex-skills"}"#,
    );
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"claude-plugin","skills":"./claude-skills"}"#,
    );
    fixture.add(
        ".kimi-plugin/plugin.json",
        r#"{"name":"kimi-plugin","skills":"./kimi-skills"}"#,
    );
    fixture.add("skills/portable/SKILL.md", &skill("portable"));
    fixture.add("codex-skills/extra/SKILL.md", &skill("extra"));
    fixture.add("claude-skills/extra/SKILL.md", &skill("extra"));
    fixture.add("kimi-skills/extra/SKILL.md", &skill("extra"));
    fixture.add("portable-unsupported/ignored/SKILL.md", &skill("ignored"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("portable provider uses its fixed root: {error}"));
    assert!(shape.detected.contains(&"skills-count:3".to_owned()));
    assert!(shape
        .detected
        .contains(&"skills-provider:agent-plugins".to_owned()));
    assert!(!shape.detected.contains(&"skills-provider:codex".to_owned()));
    assert!(shape
        .detected
        .contains(&"skills-provider:claude".to_owned()));
    assert!(shape.detected.contains(&"skills-provider:kimi".to_owned()));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("field `skills` is outside the published schema")
            && limitation.contains("not used for plugin discovery")
    }));
}

#[test]
fn portable_manifest_reports_unknown_fields_as_unsupported() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable","skills":"./custom"}}"#),
    );
    fixture.add("skills/default/SKILL.md", &skill("default"));
    fixture.add("custom/ignored/SKILL.md", &skill("ignored"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("unsupported fields are reported: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("field `skills` is outside the published schema")
            && limitation.contains("not used for plugin discovery")
    }));
}

#[test]
fn portable_non_object_extensions_are_reported_and_ignored() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable","extensions":"invalid"}}"#),
    );
    fixture.add("skills/default/SKILL.md", &skill("default"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("non-object extensions are ignored per spec: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("`extensions` is not an object")
            && limitation.contains("component discovery continues")
    }));

    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable","extensions":{{"com.example.client":"invalid"}}}}"#),
    );
    let (error, _) = fixture.run_detect_failure();
    assert!(error
        .to_string()
        .contains("extensions entries must be objects"));
}

#[test]
fn portable_manifest_rejects_invalid_names() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"bad_name"}}"#),
    );
    let (error, shape) = fixture.run_detect_failure();
    assert!(shape.detected.is_empty());
    assert!(error.to_string().contains("plugin.json"));
}

#[test]
fn portable_manifest_version_must_be_a_string_when_present() {
    for version in ["null", "1", "[]", "false"] {
        let mut fixture = Fixture::empty();
        fixture.add(
            "plugin.json",
            &format!(r#"{{"$schema":"{PORTABLE_SCHEMA}","name":"portable","version":{version}}}"#),
        );

        let (error, _) = fixture.run_detect_failure();
        assert!(error.to_string().contains("version must be a string"));
    }
}

#[test]
fn antigravity_manifest_matches_the_local_compatibility_contract() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(
            r#"{{"$schema":"{ANTIGRAVITY_SCHEMA}","name":"example-rust-plugin","description":"Rust workflows","keywords":["rust","cargo"],"homepage":"https://example.test/rust-plugin","repository":"https://example.test/rust-plugin"}}"#
        ),
    );
    fixture.add("skills/rust-review/SKILL.md", &skill("rust-review"));

    let (hidden, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("Antigravity manifest is recognized: {error}"));
    assert!(hidden.is_empty());
    assert!(shape.detected.contains(&"skills-plugin".to_owned()));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(!shape
        .detected
        .contains(&"skills-provider:claude".to_owned()));
    assert!(!fixture.files.contains("catalog.json"));
    assert!(!fixture.files.contains("docs/index.json"));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("local compatibility contract")
            && limitation.contains("full published-schema validation is not performed")
    }));
}

#[test]
fn schema_less_root_manifest_uses_narrow_antigravity_compatibility_contract() {
    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            "plugin.json",
            r#"{"name":"local-plugin","description":"A local Antigravity-compatible plugin"}"#,
        );
        fixture.add("skills/target/SKILL.md", &skill("target"));
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "-f", "."]);
        }

        let (_, shape) = fixture.run_detect().unwrap_or_else(|error| {
            panic!("schema-less Antigravity manifest with a CLI name is supported: {error}")
        });
        assert!(shape
            .detected
            .contains(&"skills-provider:antigravity".to_owned()));
        assert!(!shape
            .detected
            .contains(&"skills-provider:claude".to_owned()));
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));
        assert!(shape.limitations.iter().any(|limitation| {
            limitation.contains("without $schema")
                && limitation.contains("direct skills/<name>/SKILL.md")
                && limitation.contains("not classified")
        }));
    }
}

#[test]
fn schema_less_generic_root_manifest_is_not_classified_without_skill_layout() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        r#"{"name":"generic-plugin","description":"Generic metadata"}"#,
    );

    let (hidden, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("ambiguous generic metadata is ignored: {error}"));
    assert!(hidden.is_empty());
    assert!(!shape.detected.contains(&"skills-plugin".to_owned()));
    assert!(!shape
        .detected
        .contains(&"skills-provider:antigravity".to_owned()));
    assert!(shape
        .limitations
        .iter()
        .any(|limitation| limitation.contains("is not classified as Antigravity")));
}

#[test]
fn schema_less_antigravity_layout_rejects_unknown_provider_fields() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        r#"{"name":"generic-plugin","version":"1.0.0"}"#,
    );
    fixture.add("skills/review/SKILL.md", &skill("review"));

    let (error, _) = fixture.run_detect_failure();
    assert!(error
        .to_string()
        .contains("unsupported Antigravity field `version`"));
}

#[test]
fn antigravity_directory_skills_may_derive_name_from_folder() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{ANTIGRAVITY_SCHEMA}","name":"demo-plugin"}}"#),
    );
    fixture.add(
        "skills/review/SKILL.md",
        "---\ndescription: Reviews code changes.\n---\n# Review\n",
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("Antigravity skill names default to their folder: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn antigravity_directory_skills_still_require_description() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{ANTIGRAVITY_SCHEMA}","name":"demo-plugin"}}"#),
    );
    fixture.add(
        "skills/review/SKILL.md",
        "---\nname: review\n---\n# Review\n",
    );

    let (error, _) = fixture.run_detect_failure();
    assert!(error.to_string().contains("description"));
}

#[test]
fn antigravity_plugin_collects_codex_claude_and_kimi_sidecars() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        &format!(r#"{{"$schema":"{ANTIGRAVITY_SCHEMA}","name":"demo-plugin"}}"#),
    );
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"demo-codex","skills":"./codex-skills"}"#,
    );
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"demo-claude","skills":["./claude-skills"],"commands":["./commands"]}"#,
    );
    fixture.add(
        ".kimi-plugin/plugin.json",
        r#"{"name":"demo-kimi","skills":"./kimi-skills"}"#,
    );
    fixture.add("skills/antigravity/SKILL.md", &skill("antigravity"));
    fixture.add("codex-skills/codex/SKILL.md", &skill("codex"));
    fixture.add("claude-skills/claude/SKILL.md", &skill("claude"));
    fixture.add("kimi-skills/kimi/SKILL.md", &skill("kimi"));
    fixture.add("commands/deploy.md", "# Deploy\nRun deployment workflow.\n");

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("all provider manifests contribute roots: {error}"));
    assert!(shape.detected.contains(&"skills-count:4".to_owned()));
    assert!(shape.detected.contains(&"commands-count:1".to_owned()));
    for provider in ["antigravity", "claude", "codex", "kimi"] {
        assert!(
            shape
                .detected
                .contains(&format!("skills-provider:{provider}")),
            "missing source identity for {provider}"
        );
    }
}

#[test]
fn claude_plugin_needs_no_marketplace_or_other_provider_manifest() {
    let fixture = Fixture::claude_plugin();
    let (hidden, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("Claude plugin manifest is sufficient: {error}"));
    assert!(hidden.is_empty());
    assert!(shape.detected.contains(&"skills-plugin".to_owned()));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(!fixture.files.contains(".claude-plugin/marketplace.json"));
    assert!(!fixture.files.contains(".codex-plugin/plugin.json"));
    assert!(!fixture.files.contains("kimi.plugin.json"));
}

#[test]
fn claude_plugin_names_must_start_with_a_lowercase_letter() {
    let mut fixture = Fixture::empty();
    fixture.add(".claude-plugin/plugin.json", r#"{"name":"1plugin"}"#);
    fixture.add("skills/example/SKILL.md", &skill("example"));

    let (error, _) = fixture.run_detect_failure();
    assert!(error
        .to_string()
        .contains("name does not match the provider's identifier rules"));
}

#[test]
fn claude_plugin_skill_frontmatter_can_override_the_directory_name_or_be_absent() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: review:carefully\ndescription: Review changes.\n---\n# Review\n",
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("Claude plugin name may override its directory: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));

    fixture.add(
        "skills/example/SKILL.md",
        "Review the repository carefully.\n",
    );
    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("Claude may use a skill without YAML frontmatter: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn claude_skill_thematic_break_keeps_markdown_link_validation_active() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add("skills/example/SKILL.md", "---\n[missing](missing.md)\n");

    let (error, _) = fixture.run_detect_failure();
    assert!(error.to_string().contains("missing.md"));
    assert!(error.to_string().contains("skills/example/SKILL.md:2"));
    assert!(!error.to_string().contains("frontmatter"));
}

#[test]
fn claude_skill_closed_but_invalid_frontmatter_fails() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add("skills/example/SKILL.md", "---\nname: [\n---\n# Broken\n");

    let (error, _) = fixture.run_detect_failure();
    assert!(error.to_string().contains("invalid YAML frontmatter"));
}

#[test]
fn claude_skill_frontmatter_still_rejects_yaml_aliases() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: &skill_name example\ndescription: *skill_name\n---\n# Example\n",
    );

    let error = fixture
        .run_scan()
        .expect_err("Claude definitions keep the strict no-alias parser policy");
    assert!(error.to_string().contains("invalid YAML frontmatter"));
}

#[test]
fn marketplace_catalog_without_local_plugin_components_is_not_a_skill_plugin() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","owner":{"name":"Example"},"plugins":[]}"#,
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("a marketplace catalog is not a local plugin: {error}"));
    assert!(shape.detected.is_empty());
}

#[test]
fn claude_marketplace_strict_false_local_source_can_define_a_plugin_without_plugin_json() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","owner":{"name":"Example"},"plugins":[{"name":"local-tool","source":"./plugins/local-tool","strict":false}]}"#,
    );
    fixture.add(
        "plugins/local-tool/skills/review/SKILL.md",
        &skill("review"),
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("local strict-false entry defines the plugin: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape
        .detected
        .contains(&"skills-provider:claude-marketplace".to_owned()));
}

#[cfg(unix)]
#[test]
fn claude_marketplace_rejects_symlinked_local_source_roots() {
    use std::os::unix::fs::symlink;

    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".claude-plugin/marketplace.json",
            r#"{"name":"catalog","owner":{"name":"Example"},"plugins":[{"name":"local-tool","source":"./plugins/local-tool","strict":false}]}"#,
        );
        let outside = fixture.root.with_file_name(format!(
            "{}-outside",
            fixture
                .root
                .file_name()
                .expect("fixture root has a basename")
                .to_string_lossy()
        ));
        let _ = fs::remove_dir_all(&outside);
        must(
            fs::create_dir_all(outside.join("skills/review")),
            "create external marketplace source",
        );
        must(
            fs::write(outside.join("skills/review/SKILL.md"), skill("review")),
            "write external marketplace skill",
        );
        must(
            fs::create_dir_all(fixture.root.join("plugins")),
            "create marketplace source parent",
        );
        must(
            symlink(&outside, fixture.root.join("plugins/local-tool")),
            "symlink marketplace source outside the repository",
        );
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "."]);
        }

        let error = fixture
            .run_scan()
            .expect_err("marketplace source symlinks must not expose outside files");
        assert!(error.to_string().contains("symbolic link"));
        must(
            fs::remove_dir_all(&outside),
            "remove external marketplace source",
        );
    }
}

#[test]
fn claude_marketplace_strict_local_source_requires_plugin_manifest() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","owner":{"name":"Example"},"plugins":[{"name":"local-tool","source":"./plugins/local-tool","strict":true}]}"#,
    );
    fixture.add(
        "plugins/local-tool/skills/review/SKILL.md",
        &skill("review"),
    );

    let (error, _) = fixture.run_detect_failure();
    assert!(error
        .to_string()
        .contains("is strict but has no local .claude-plugin/plugin.json"));
}

#[test]
fn claude_marketplace_resolves_plugin_root_and_merges_strict_entry_paths() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","metadata":{"pluginRoot":"./plugins"},"plugins":[{"name":"formatter","source":"formatter","skills":"./marketplace-skills","commands":"./marketplace-commands"}]}"#,
    );
    fixture.add(
        "plugins/formatter/.claude-plugin/plugin.json",
        r#"{"name":"formatter","skills":"./manifest-skills","commands":"./manifest-commands"}"#,
    );
    fixture.add(
        "plugins/formatter/skills/default/SKILL.md",
        &skill("default"),
    );
    fixture.add(
        "plugins/formatter/manifest-skills/from-manifest/SKILL.md",
        &skill("from-manifest"),
    );
    fixture.add(
        "plugins/formatter/marketplace-skills/from-marketplace/SKILL.md",
        &skill("from-marketplace"),
    );
    fixture.add(
        "plugins/formatter/manifest-commands/manifest.md",
        "# Manifest command\n",
    );
    fixture.add(
        "plugins/formatter/marketplace-commands/marketplace.md",
        "# Marketplace command\n",
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("strict marketplace paths merge locally: {error}"));
    assert!(shape.detected.contains(&"skills-count:3".to_owned()));
    assert!(shape.detected.contains(&"commands-count:2".to_owned()));
    assert!(shape
        .detected
        .contains(&"skills-provider:claude-marketplace".to_owned()));
}

#[test]
fn claude_marketplace_plugin_root_accepts_bare_dot_as_marketplace_root() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","metadata":{"pluginRoot":"."},"plugins":[{"name":"formatter","source":"formatter"}]}"#,
    );
    fixture.add(
        "formatter/.claude-plugin/plugin.json",
        r#"{"name":"formatter"}"#,
    );
    fixture.add("formatter/skills/review/SKILL.md", &skill("review"));

    let (_, shape) = fixture.run_detect().unwrap_or_else(|error| {
        panic!("Velnor resolves a bare source under pluginRoot `.`: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape
        .detected
        .contains(&"skills-provider:claude-marketplace".to_owned()));
}

#[test]
fn claude_marketplace_plugin_root_accepts_bare_directory_names_with_underscore_prefix() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","metadata":{"pluginRoot":"./plugins"},"plugins":[{"name":"formatter","source":"_formatter"}]}"#,
    );
    fixture.add(
        "plugins/_formatter/.claude-plugin/plugin.json",
        r#"{"name":"formatter"}"#,
    );
    fixture.add(
        "plugins/_formatter/skills/review/SKILL.md",
        &skill("review"),
    );

    let (_, shape) = fixture.run_detect().unwrap_or_else(|error| {
        panic!("a bare single-component source is resolved under pluginRoot: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape
        .detected
        .contains(&"skills-provider:claude-marketplace".to_owned()));
}

#[test]
fn claude_marketplace_rejects_duplicate_and_unsafe_names() {
    let cases = [
        (
            r#"{"name":"market\u0001place","plugins":[]}"#,
            "kebab-case identifier",
        ),
        (
            r#"{"name":"market\u202eplace","plugins":[]}"#,
            "kebab-case identifier",
        ),
        (
            r#"{"plugins":[{"name":"tool\u0001name","source":"./tool"}]}"#,
            "kebab-case identifier",
        ),
        (
            r#"{"plugins":[{"name":"tool\u202ename","source":"./tool"}]}"#,
            "kebab-case identifier",
        ),
        (
            r#"{"plugins":[{"name":"../tool","source":"./tool"}]}"#,
            "kebab-case identifier",
        ),
        (
            r#"{"plugins":[{"name":"same","source":"./one","strict":false},{"name":"same","source":"./two","strict":false}]}"#,
            "duplicate plugin name",
        ),
    ];
    for (marketplace, expected_error) in cases {
        let mut fixture = Fixture::empty();
        fixture.add(".claude-plugin/marketplace.json", marketplace);
        fixture.add("one/.keep", "\n");
        fixture.add("two/.keep", "\n");
        let (error, _) = fixture.run_detect_failure();
        assert!(
            error.to_string().contains(expected_error),
            "expected {expected_error:?} in error, got: {error}"
        );
    }
}

#[test]
fn claude_marketplace_strict_false_rejects_all_declared_component_paths() {
    for component_fields in [
        r#""skills":"./skills""#,
        r#""commands":"./commands""#,
        r#""agents":"./agents""#,
        r#""hooks":"./hooks""#,
        r#""mcpServers":{"server":{"command":"server"}}"#,
        r#""lspServers":"./lsp""#,
        r#""outputStyles":"./output-styles""#,
        r#""workflows":"./workflows""#,
        r#""channels":[{"server":"telegram","userConfig":{}}]"#,
        r#""themes":"./themes""#,
        r#""monitors":"./monitors""#,
        r#""experimental":{"themes":"./themes"}"#,
        r#""experimental":{"monitors":"./monitors"}"#,
    ] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".claude-plugin/marketplace.json",
            r#"{"name":"catalog","plugins":[{"name":"local-tool","source":"./plugins/local-tool","strict":false}]}"#,
        );
        fixture.add(
            "plugins/local-tool/.claude-plugin/plugin.json",
            &format!(r#"{{"name":"local-tool",{component_fields}}}"#),
        );
        fixture.add(
            "plugins/local-tool/skills/review/SKILL.md",
            &skill("review"),
        );

        let (error, _) = fixture.run_detect_failure();
        assert!(
            error.to_string().contains("declares components"),
            "strict:false accepted component fields {component_fields}: {error}"
        );
    }
}

#[test]
fn claude_workflows_and_experimental_components_are_reported_uninspected() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"claude-plugin","workflows":"./workflows","themes":"./themes","monitors":"./monitors","channels":[{"server":"telegram","userConfig":{}}],"experimental":{"themes":"./experimental-themes","monitors":"./experimental-monitors"}}"#,
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("recognized Claude components are reported: {error}"));
    for field in [
        "workflows",
        "themes",
        "monitors",
        "channels",
        "experimental.themes",
        "experimental.monitors",
    ] {
        assert!(
            shape.limitations.iter().any(
                |limitation| limitation.contains(field) && limitation.contains("not inspected")
            ),
            "missing limitation for {field}: {:?}",
            shape.limitations
        );
    }
}

#[test]
fn claude_marketplace_root_skill_path_normalizes_dot_component() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","plugins":[{"name":"root-plugin","source":"./plugins/tool","skills":["./"]}]}"#,
    );
    fixture.add(
        "plugins/tool/.claude-plugin/plugin.json",
        r#"{"name":"root-plugin"}"#,
    );
    fixture.add("plugins/tool/SKILL.md", &skill("root-skill"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("marketplace root skill is discovered: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape
        .detected
        .contains(&"skills-provider:claude-marketplace".to_owned()));
}

#[test]
fn claude_marketplace_root_source_preserves_full_default_skills_scan() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","plugins":[{"name":"root-plugin","source":"./","strict":false,"skills":["./"]}]}"#,
    );
    fixture.add("skills/review/SKILL.md", &skill("review"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("marketplace root path keeps the default scan: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn claude_strict_root_source_keeps_plugin_manifest_skills_additive_to_default() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","plugins":[{"name":"root-plugin","source":"./","strict":true}]}"#,
    );
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"root-plugin","skills":"./custom-skills"}"#,
    );
    fixture.add("skills/default/SKILL.md", &skill("default"));
    fixture.add("custom-skills/custom/SKILL.md", &skill("custom"));

    let (_, shape) = fixture.run_detect().unwrap_or_else(|error| {
        panic!("plugin.json roots supplement default skills/ at marketplace root: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));
}

#[test]
fn claude_marketplace_root_source_falls_back_when_declared_paths_are_missing() {
    let mut with_default_skill = Fixture::empty();
    with_default_skill.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","plugins":[{"name":"root-plugin","source":"./","strict":false,"skills":["./missing"]}]}"#,
    );
    with_default_skill.add("skills/review/SKILL.md", &skill("review"));
    let (_, shape) = with_default_skill
        .run_detect()
        .unwrap_or_else(|error| panic!("missing path falls back to default skills: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));

    let mut with_root_skill = Fixture::empty();
    with_root_skill.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","plugins":[{"name":"root-plugin","source":"./","strict":false,"skills":["./missing"]}]}"#,
    );
    with_root_skill.add("SKILL.md", &skill("root-skill"));
    let (_, shape) = with_root_skill
        .run_detect()
        .unwrap_or_else(|error| panic!("missing path also permits root skill fallback: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn claude_marketplace_remote_sources_are_not_claimed_as_local_plugins() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","owner":{"name":"Example"},"plugins":[{"name":"remote-tool","source":{"source":"github","repo":"owner/tool"}}]}"#,
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("remote catalog entries are not read locally: {error}"));
    assert!(shape.detected.is_empty());
    assert!(shape
        .limitations
        .iter()
        .any(|limitation| limitation.contains("component files were not inspected")));
}

#[test]
fn claude_marketplace_source_must_be_a_string_or_object() {
    for plugin in [
        r#"{"name":"missing-source"}"#,
        r#"{"name":"numeric-source","source":7}"#,
        r#"{"name":"null-source","source":null}"#,
        r#"{"name":"array-source","source":[]}"#,
    ] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".claude-plugin/marketplace.json",
            &format!(r#"{{"name":"catalog","plugins":[{plugin}]}}"#),
        );

        let (error, _) = fixture.run_detect_failure();
        assert!(error
            .to_string()
            .contains("source must be a string or an object"));
    }
}

#[test]
fn claude_marketplace_rejects_bare_source_paths_with_slashes() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","metadata":{"pluginRoot":"./plugins"},"plugins":[{"name":"invalid","source":"team-a/formatter"}]}"#,
    );

    let (error, _) = fixture.run_detect_failure();
    assert!(error
        .to_string()
        .contains("source containing `/` must start with `./`"));
}

#[test]
fn claude_marketplace_requires_dot_slash_for_marketplace_root_source() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","plugins":[{"name":"invalid-root","source":"."}]}"#,
    );

    let (error, _) = fixture.run_detect_failure();
    assert!(error
        .to_string()
        .contains("source `.` is invalid; use `./` for the marketplace root"));
}

#[test]
fn claude_marketplace_rejects_bare_source_without_plugin_root() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","plugins":[{"name":"invalid","source":"formatter"}]}"#,
    );

    let (error, _) = fixture.run_detect_failure();
    assert!(error
        .to_string()
        .contains("bare source `formatter` requires metadata.pluginRoot"));
}

#[test]
fn claude_marketplace_plugin_root_accepts_unicode_bare_directory_names() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","metadata":{"pluginRoot":"./plugins"},"plugins":[{"name":"formatter","source":"格式化"}]}"#,
    );
    fixture.add(
        "plugins/格式化/.claude-plugin/plugin.json",
        r#"{"name":"formatter"}"#,
    );
    fixture.add("plugins/格式化/skills/review/SKILL.md", &skill("review"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("a Unicode bare directory name resolves locally: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn claude_marketplace_local_source_cannot_escape_the_repository() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","owner":{"name":"Example"},"plugins":[{"name":"unsafe","source":"../outside","strict":false}]}"#,
    );
    let (error, shape) = fixture.run_detect_failure();
    assert!(shape.detected.is_empty());
    assert!(error.to_string().contains("stay inside the repository"));
}

#[test]
fn claude_skills_are_additive_and_declared_command_paths_can_keep_the_default() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"claude-plugin","skills":["./extra-skills/"],"commands":["./commands","./slash"]}"#,
    );
    fixture.add("skills/default/SKILL.md", &skill("default"));
    fixture.add("extra-skills/extra/SKILL.md", &skill("extra"));
    fixture.add(
        "commands/default.md",
        "# Default command\nDefault Claude command.\n",
    );
    fixture.add("slash/run.md", "# Run\nA valid command.\n");
    fixture.add("SKILL.md", "invalid root fallback must stay suppressed\n");

    let (_, shape) = fixture.run_detect().unwrap_or_else(|error| {
        panic!("explicit default and custom Claude paths are valid: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));
    assert!(shape.detected.contains(&"commands-count:2".to_owned()));
}

#[test]
fn claude_skill_path_accepts_bare_dot_and_scans_root_skill_resources() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"claude-plugin","skills":"."}"#,
    );
    fixture.add("skills/default/SKILL.md", &skill("default"));
    fixture.add(
        "SKILL.md",
        "---\nname: root-skill\ndescription: Root skill.\n---\n[Guide](references/guide.md)\n",
    );
    fixture.add("references/guide.md", "# Guide\n");

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("Claude skills accepts . for its plugin root and validates resources: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));
}

#[test]
fn claude_skill_path_array_accepts_bare_dot_and_scans_root_skill_resources() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"claude-plugin","skills":["."]}"#,
    );
    fixture.add("skills/default/SKILL.md", &skill("default"));
    fixture.add(
        "SKILL.md",
        "---\nname: root-skill\ndescription: Root skill.\n---\n[Guide](references/guide.md)\n",
    );
    fixture.add("references/guide.md", "# Guide\n");

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("Claude skills array accepts . for its plugin root: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));
}

#[test]
fn claude_declared_skill_path_must_name_a_directory() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"claude-plugin","skills":"./skills/review/SKILL.md"}"#,
    );
    fixture.add("skills/review/SKILL.md", &skill("review"));

    let (error, _) = fixture.run_detect_failure();
    assert!(error
        .to_string()
        .contains("must name a directory, not a file"));
}

#[test]
fn claude_declared_command_paths_replace_the_default_root() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"claude-plugin","commands":"./slash"}"#,
    );
    fixture.add(
        "commands/default.md",
        "# Default command\nThis command is ignored when a custom root is declared.\n",
    );
    fixture.add("slash/run.md", "# Run\nA custom command.\n");

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("declared command paths replace commands/: {error}"));
    assert!(shape.detected.contains(&"commands-count:1".to_owned()));
}

#[test]
fn claude_command_root_path_discovers_root_markdown_commands() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"claude-plugin","commands":"./"}"#,
    );
    fixture.add("deploy.md", "# Deploy\nA root-level slash command.\n");

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("Claude command root ./ is valid: {error}"));
    assert!(shape.detected.contains(&"commands-count:1".to_owned()));
}

#[test]
fn claude_skill_root_fallback_requires_skills_directory_to_be_absent() {
    let mut fixture = Fixture::empty();
    fixture.add(".claude-plugin/plugin.json", r#"{"name":"claude-plugin"}"#);
    fixture.add(
        "skills/README.md",
        "A skills directory suppresses root fallback.\n",
    );
    fixture.add("SKILL.md", "invalid root skill must stay ignored\n");

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("any skills/ content suppresses root fallback: {error}"));
    assert!(shape.detected.contains(&"skills-count:0".to_owned()));
}

#[test]
fn claude_command_delimited_frontmatter_must_be_valid_yaml_mapping() {
    for contents in [
        "---\nname: [\n---\n# Broken\n",
        "---\njust a scalar\n---\n# Not a mapping\n",
    ] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".claude-plugin/plugin.json",
            r#"{"name":"claude-plugin","commands":"./commands"}"#,
        );
        fixture.add("commands/broken.md", contents);
        let (error, _) = fixture.run_detect_failure();
        assert!(error.to_string().contains("frontmatter"));
    }
}

#[test]
fn claude_command_thematic_break_keeps_markdown_link_validation_active() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"claude-plugin","commands":"./commands"}"#,
    );
    fixture.add("commands/broken.md", "---\n[missing](missing.md)\n");

    let (error, _) = fixture.run_detect_failure();
    assert!(error.to_string().contains("missing.md"));
    assert!(error.to_string().contains("commands/broken.md:2"));
    assert!(!error.to_string().contains("frontmatter"));
}

#[test]
fn claude_command_directories_can_contain_nested_skill_definitions() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"claude-plugin","commands":"./slash"}"#,
    );
    fixture.add(
        "slash/review/SKILL.md",
        "---\nname: review\ndescription: Review changes.\n---\n# Review\n",
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("nested Claude command skill is valid: {error}"));
    assert!(shape.detected.contains(&"commands-count:1".to_owned()));
}

#[test]
fn claude_nested_command_skill_definitions_are_discovered() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"claude-plugin","commands":"./commands"}"#,
    );
    fixture.add("commands/slash/review/SKILL.md", &skill("review"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("nested command Skills are valid: {error}"));
    assert!(shape.detected.contains(&"commands-count:1".to_owned()));
}

#[test]
fn claude_default_layout_works_without_manifest_and_ignores_skills_readme() {
    let mut fixture = Fixture::empty();
    fixture.add("skills/example/SKILL.md", &skill("example"));
    fixture.add("skills/README.md", "ordinary documentation, not a skill\n");
    fixture.add("commands/deploy.md", "# Deploy\nA slash command.\n");

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("Claude default paths need no manifest: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape.detected.contains(&"commands-count:1".to_owned()));
}

#[test]
fn claude_root_skill_is_only_a_fallback_when_default_skills_are_absent() {
    let mut fixture = Fixture::empty();
    fixture.add(".claude-plugin/plugin.json", r#"{"name":"claude-plugin"}"#);
    fixture.add("skills/example/SKILL.md", &skill("example"));
    fixture.add("SKILL.md", "invalid root skill must be suppressed\n");

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("default skills suppress root fallback: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn claude_root_skill_fallback_works_with_and_without_manifest() {
    for with_manifest in [true, false] {
        let mut fixture = Fixture::empty();
        if with_manifest {
            fixture.add(".claude-plugin/plugin.json", r#"{"name":"claude-plugin"}"#);
        }
        fixture.add("SKILL.md", &skill("root-skill"));
        let (_, shape) = fixture
            .run_detect()
            .unwrap_or_else(|error| panic!("Claude root skill fallback is valid: {error}"));
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    }
}

#[test]
fn claude_root_skill_fallback_ignores_untracked_empty_skills_directory_in_git_scans() {
    let mut fixture = Fixture::empty();
    fixture.add(".claude-plugin/plugin.json", r#"{"name":"claude-plugin"}"#);
    fixture.add("SKILL.md", &skill("root-skill"));
    fixture.git(&["init", "-q"]);
    fixture.git(&["add", ".claude-plugin/plugin.json", "SKILL.md"]);
    must(
        fs::create_dir_all(fixture.root.join("skills")),
        "create untracked empty default skills directory",
    );

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("untracked empty paths do not change Git scans: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn codex_skills_array_replaces_default_roots_and_keeps_each_declared_root() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":["./custom-one","./custom-two"]}"#,
    );
    fixture.add(
        "skills/default/SKILL.md",
        "invalid default skill must be ignored\n",
    );
    fixture.add("custom-one/one/SKILL.md", &skill("one"));
    fixture.add("custom-two/two/SKILL.md", &skill("two"));

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("Codex explicit roots replace the default skills root: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));
    assert!(shape.detected.contains(&"skills-provider:codex".to_owned()));
}

#[test]
fn codex_skills_array_accepts_dot_slash_dot_as_the_plugin_root() {
    for root in ["./.", "././"] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            &format!(r#"{{"name":"codex-plugin","skills":["{root}"]}}"#),
        );
        fixture.add("SKILL.md", &skill("root-skill"));
        fixture.add("skills/default/SKILL.md", &skill("default"));

        let shape = fixture
            .run_scan()
            .unwrap_or_else(|error| panic!("Codex accepts `{root}` as its plugin root: {error}"));
        assert!(shape.detected.contains(&"skills-count:2".to_owned()));
    }
}

#[test]
fn codex_skill_roots_recurse_to_depth_six_and_prune_hidden_directories() {
    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./custom"}"#,
        );
        fixture.add("custom/group/name/SKILL.md", &skill("name"));
        fixture.add("custom/one/two/three/four/five/six/SKILL.md", &skill("six"));
        fixture.add(
            "custom/one/two/three/four/five/six/seven/SKILL.md",
            "outside Codex's depth limit\n",
        );
        fixture.add(
            "custom/.hidden/skill/SKILL.md",
            "hidden Codex skills are not discovered\n",
        );
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "-f", "."]);
        }

        let shape = fixture.run_scan().unwrap_or_else(|error| {
            panic!("Codex recursively scans visible paths through directory depth six: {error}")
        });
        assert!(shape.detected.contains(&"skills-count:2".to_owned()));
    }
}

#[test]
fn codex_default_root_scans_its_root_skill_and_node_modules() {
    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(".codex-plugin/plugin.json", r#"{"name":"codex-plugin"}"#);
        fixture.add("skills/SKILL.md", &skill("root-skill"));
        fixture.add("skills/node_modules/pkg/SKILL.md", &skill("pkg"));
        fixture.add("skills/one/two/three/four/five/six/SKILL.md", &skill("six"));
        fixture.add(
            "skills/one/two/three/four/five/six/seven/SKILL.md",
            "outside Codex's depth limit\n",
        );
        fixture.add(
            "skills/.hidden/skill/SKILL.md",
            "hidden Codex skills are not discovered\n",
        );
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "."]);
        }

        let shape = fixture.run_scan().unwrap_or_else(|error| {
            panic!("Codex's default root recursively discovers its visible skills: {error}")
        });
        assert!(shape.detected.contains(&"skills-count:3".to_owned()));
    }
}

#[test]
fn codex_file_valued_skill_roots_are_empty() {
    for use_git_index in [false, true] {
        let mut default_file = Fixture::empty();
        default_file.add(".codex-plugin/plugin.json", r#"{"name":"codex-plugin"}"#);
        default_file.add("skills", "A file cannot be scanned as the default root.\n");
        if use_git_index {
            default_file.git(&["init", "-q"]);
            default_file.git(&["add", "."]);
        }
        let shape = default_file.run_scan().unwrap_or_else(|error| {
            panic!("a regular file at the default root is ignored: {error}")
        });
        assert!(shape.detected.contains(&"skills-count:0".to_owned()));

        let mut explicit_file = Fixture::empty();
        explicit_file.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./custom/SKILL.md"}"#,
        );
        explicit_file.add("custom/SKILL.md", &skill("explicit-file"));
        if use_git_index {
            explicit_file.git(&["init", "-q"]);
            explicit_file.git(&["add", "."]);
        }
        let shape = explicit_file.run_scan().unwrap_or_else(|error| {
            panic!("an explicit file path is an empty Codex root: {error}")
        });
        assert!(shape.detected.contains(&"skills-count:0".to_owned()));
    }
}

#[test]
fn codex_file_skill_root_is_still_resolvable_as_a_link_target() {
    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":["./file-root/SKILL.md","./valid"]}"#,
        );
        fixture.add(
            "file-root/SKILL.md",
            "this declared file root is not parsed as a skill\n",
        );
        fixture.add(
            "valid/SKILL.md",
            "---\nname: valid\ndescription: Valid skill.\n---\n[File](../file-root/SKILL.md)\n",
        );
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "."]);
        }

        let shape = fixture.run_scan().unwrap_or_else(|error| {
            panic!("the file root stays empty while a separate link can resolve it: {error}")
        });
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));

        let exclude = ["file-root/SKILL.md".to_owned()];
        let error = fixture
            .run_scan_with_exclude(&exclude)
            .expect_err("an excluded file root cannot satisfy a separate skill link");
        assert!(error
            .to_string()
            .contains("missing Markdown link ../file-root/SKILL.md"));
    }
}

#[test]
fn codex_names_fall_back_to_directory_and_allow_explicit_overrides() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./custom"}"#,
    );
    fixture.add(
        "custom/derived-name/SKILL.md",
        "---\ndescription: Name comes from this directory.\n---\n",
    );
    fixture.add(
        "custom/blank-name/SKILL.md",
        "---\nname: \"   \"\ndescription: Blank names use the directory.\n---\n",
    );
    fixture.add(
        "custom/null-name/SKILL.md",
        "---\nname: null\ndescription: Null names use the directory.\n---\n",
    );
    fixture.add(
        "custom/folder-name/SKILL.md",
        "---\nname: \"Explicit Override\"\ndescription: Explicit names can differ from the folder.\n---\n",
    );

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("Codex skill names follow runtime fallback rules: {error}"));
    assert!(shape.detected.contains(&"skills-count:4".to_owned()));
}

#[test]
fn codex_whitespace_only_directory_name_uses_runtime_skill_fallback() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./custom"}"#,
    );
    fixture.add(
        "custom/   /SKILL.md",
        "---\ndescription: A whitespace-only folder name.\n---\n",
    );
    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("Codex sanitizes an empty directory fallback to `skill`: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));

    let frontmatter = parse_frontmatter(
        "---\ndescription: A whitespace-only folder name.\n---\n",
        "custom/   /SKILL.md",
        FrontmatterMode::CodexDefinition,
    )
    .unwrap_or_else(|error| panic!("valid Codex frontmatter: {error}"));
    assert_eq!(
        validate_codex_skill_name(&frontmatter, Some("   "), "custom/   /SKILL.md")
            .unwrap_or_else(|error| panic!("runtime fallback name: {error}")),
        "skill"
    );
}

#[test]
fn codex_directory_name_fallback_obeys_the_64_character_limit() {
    let long_directory = "a".repeat(65);
    for name_field in ["", "name: \"   \"\n", "name: null\n"] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./custom"}"#,
        );
        fixture.add(
            &format!("custom/{long_directory}/SKILL.md"),
            &format!("---\n{name_field}description: Valid skill.\n---\n"),
        );

        let error = fixture
            .run_scan()
            .expect_err("a fallback directory name longer than 64 characters is invalid");
        assert!(error.to_string().contains("64-character limit"));
    }

    let mut explicit_name = Fixture::empty();
    explicit_name.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./custom"}"#,
    );
    explicit_name.add(
        &format!("custom/{long_directory}/SKILL.md"),
        "---\nname: Short Name\ndescription: Valid skill.\n---\n",
    );
    let shape = explicit_name.run_scan().unwrap_or_else(|error| {
        panic!("an explicit short name overrides the long directory fallback: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn codex_scalar_repair_and_frontmatter_delimiters_match_runtime() {
    let colon_scalar = "---\nname: example\ndescription: Build for AWS: ECS\n---\n";
    let parsed = parse_frontmatter(
        colon_scalar,
        "skills/example/SKILL.md",
        FrontmatterMode::CodexDefinition,
    )
    .unwrap_or_else(|error| panic!("Codex repairs colon-rich plain scalar text: {error}"));
    assert_eq!(
        parsed.values.get("description").map(String::as_str),
        Some("Build for AWS: ECS")
    );
    let strict_error = parse_frontmatter(
        colon_scalar,
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition,
    )
    .expect_err("the repair must remain Codex-specific");
    assert!(strict_error
        .to_string()
        .contains("invalid YAML frontmatter"));

    let yaml_document_end = "---\nname: example\ndescription: Valid skill.\n...\n";
    let delimiter_error = parse_frontmatter(
        yaml_document_end,
        "skills/example/SKILL.md",
        FrontmatterMode::CodexDefinition,
    )
    .expect_err("Codex requires its own closing `---` delimiter");
    assert!(delimiter_error
        .to_string()
        .contains("missing closing frontmatter delimiter"));
    parse_frontmatter(
        "---\nname: example\ndescription: Valid skill.\n---\n",
        "skills/example/SKILL.md",
        FrontmatterMode::CodexDefinition,
    )
    .unwrap_or_else(|error| panic!("Codex accepts a closing `---`: {error}"));
    parse_frontmatter(
        " \t--- \t\nname: example\ndescription: Valid skill.\n \t--- \t\n",
        "skills/example/SKILL.md",
        FrontmatterMode::CodexDefinition,
    )
    .unwrap_or_else(|error| panic!("Codex trims frontmatter delimiter lines: {error}"));
    let bom_prefixed = "\u{feff}---\nname: example\ndescription: Valid skill.\n---\n";
    let bom_error = parse_frontmatter(
        bom_prefixed,
        "skills/example/SKILL.md",
        FrontmatterMode::CodexDefinition,
    )
    .expect_err("Codex does not strip a BOM before its opening delimiter");
    assert!(bom_error
        .to_string()
        .contains("missing opening frontmatter delimiter"));
    parse_frontmatter(
        bom_prefixed,
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition,
    )
    .unwrap_or_else(|error| panic!("other providers retain BOM handling: {error}"));
    parse_frontmatter(
        yaml_document_end,
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition,
    )
    .unwrap_or_else(|error| panic!("other providers retain YAML `...` closure: {error}"));
}

#[test]
fn codex_run_scan_uses_runtime_frontmatter_delimiter_rules() {
    let padded = " \t--- \t\nname: example\ndescription: Valid skill.\n \t--- \t\n";
    let mut padded_fixture = Fixture::empty();
    padded_fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./skills"}"#,
    );
    padded_fixture.add("skills/example/SKILL.md", padded);
    let shape = padded_fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("Codex accepts padded frontmatter delimiters: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));

    let mut bom_fixture = Fixture::empty();
    bom_fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./skills"}"#,
    );
    bom_fixture.add(
        "skills/example/SKILL.md",
        "\u{feff}---\nname: example\ndescription: Valid skill.\n---\n",
    );
    let error = bom_fixture
        .run_scan()
        .expect_err("Codex rejects BOM before the opening frontmatter delimiter");
    assert!(error
        .to_string()
        .contains("missing opening frontmatter delimiter"));

    let bare_cr = "---\rname: example\rdescription: Valid skill.\r---\r";
    let parser_error = parse_frontmatter(
        bare_cr,
        "skills/example/SKILL.md",
        FrontmatterMode::CodexDefinition,
    )
    .expect_err("Codex uses Rust line semantics and does not split on bare CR");
    assert!(parser_error
        .to_string()
        .contains("missing opening frontmatter delimiter"));

    let mut bare_cr_fixture = Fixture::empty();
    bare_cr_fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./skills"}"#,
    );
    bare_cr_fixture.add("skills/example/SKILL.md", bare_cr);
    let error = bare_cr_fixture
        .run_scan()
        .expect_err("Codex rejects a bare-CR-only frontmatter document");
    assert!(error
        .to_string()
        .contains("missing opening frontmatter delimiter"));
}

#[test]
fn codex_frontmatter_accepts_runtime_yaml_aliases() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./skills"}"#,
    );
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: &description \"[Not a link](missing.md)\"\nmetadata:\n  short-description: *description\n---\n[Guide](references/guide.md)\n",
    );
    fixture.add("skills/example/references/guide.md", "# Guide\n");

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("Codex accepts aliases and strips aliased YAML frontmatter: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn codex_ignores_unknown_frontmatter_fields_like_runtime() {
    let frontmatter = "---\nname: example\ndescription: Valid skill.\nargument-hint: [duration]\nmetadata:\n  short-description: null\n  custom: [x]\n---\n";
    parse_frontmatter(
        frontmatter,
        "skills/example/SKILL.md",
        FrontmatterMode::CodexDefinition,
    )
    .unwrap_or_else(|error| panic!("Codex ignores unknown fields and metadata: {error}"));

    let other_provider_error = parse_frontmatter(
        "---\nname: example\ndescription: Valid skill.\nargument-hint: [duration]\n---\n",
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition,
    )
    .expect_err("non-Codex providers keep their optional-field contract");
    assert!(other_provider_error
        .to_string()
        .contains("argument-hint must be a YAML string"));

    let null_metadata = "---\nname: example\ndescription: Valid skill.\nmetadata: null\n---\n";
    parse_frontmatter(
        null_metadata,
        "skills/example/SKILL.md",
        FrontmatterMode::CodexDefinition,
    )
    .expect_err("Codex metadata is a typed object and rejects explicit null");

    let invalid_known_metadata =
        "---\nname: example\ndescription: Valid skill.\nmetadata:\n  short-description: [x]\n---\n";
    let codex_error = parse_frontmatter(
        invalid_known_metadata,
        "skills/example/SKILL.md",
        FrontmatterMode::CodexDefinition,
    )
    .expect_err("Codex still type-checks its known metadata field");
    assert!(codex_error
        .to_string()
        .contains("metadata.short-description"));
}

#[test]
fn codex_does_not_apply_the_non_codex_description_length_cap() {
    let long_description = "d".repeat(1500);
    let frontmatter = format!("---\nname: example\ndescription: {long_description}\n---\n");
    parse_frontmatter(
        &frontmatter,
        "skills/example/SKILL.md",
        FrontmatterMode::CodexDefinition,
    )
    .unwrap_or_else(|error| panic!("Codex only requires a non-empty description: {error}"));
    let error = parse_frontmatter(
        &frontmatter,
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition,
    )
    .expect_err("the 1024-character bound remains for the other provider");
    assert!(error.to_string().contains("supported length"));
}

#[cfg(unix)]
#[test]
fn codex_fallback_names_use_sanitized_canonical_directory_basename() {
    use std::os::unix::fs::symlink;

    for use_git_index in [false, true] {
        for (canonical_name, visible_alias, should_fail) in [
            ("z".repeat(65), "short-alias".to_owned(), true),
            ("canonical  short".to_owned(), "x".repeat(65), false),
        ] {
            let mut fixture = Fixture::empty();
            fixture.add(
                ".codex-plugin/plugin.json",
                r#"{"name":"codex-plugin","skills":"./skills"}"#,
            );
            fixture.add(
                &format!("components/{canonical_name}/SKILL.md"),
                "---\ndescription: Derive fallback from the canonical directory.\n---\n",
            );
            must(
                fs::create_dir_all(fixture.root.join("skills")),
                "create Codex skill root for directory alias",
            );
            must(
                symlink(
                    fixture.root.join(format!("components/{canonical_name}")),
                    fixture.root.join(format!("skills/{visible_alias}")),
                ),
                "add Codex directory alias for fallback name",
            );
            if use_git_index {
                fixture.git(&["init", "-q"]);
                fixture.git(&["add", "-f", "."]);
            }

            let result = fixture.run_scan();
            if should_fail {
                let error = result.expect_err(
                    "a long canonical target basename must not be replaced by a short alias",
                );
                assert!(error.to_string().contains("64-character limit"));
            } else {
                let shape = result.unwrap_or_else(|error| {
                    panic!("the sanitized canonical basename fits the limit: {error}")
                });
                assert!(shape.detected.contains(&"skills-count:1".to_owned()));
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn codex_template_aliases_suppress_the_visible_canonical_target_too() {
    use std::os::unix::fs::symlink;

    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./skills"}"#,
        );
        fixture.add(
            "components/physical-skill/SKILL.md",
            &skill("physical-skill"),
        );
        fixture.add(
            "components/physical-skill/templates/package.json",
            r#"{"name":"template-only-package"}"#,
        );
        must(
            fs::create_dir_all(fixture.root.join("skills")),
            "create Codex skill alias root",
        );
        must(
            symlink(
                fixture.root.join("components/physical-skill"),
                fixture.root.join("skills/linked-skill"),
            ),
            "add Codex skill directory alias",
        );
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "-f", "."]);
        }

        let (ignored, shape) = fixture.run_detect().unwrap_or_else(|error| {
            panic!("a template through a Codex alias is validated: {error}")
        });
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));
        assert!(ignored.contains("skills/linked-skill/templates/package.json"));
        assert!(ignored.contains("components/physical-skill/templates/package.json"));
    }
}

#[test]
fn codex_declared_node_modules_root_recovers_skills_and_resources() {
    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./node_modules/pkg/skills"}"#,
        );
        fixture.add(
            "skills/default/SKILL.md",
            "the default root must not be scanned\n",
        );
        fixture.add(
            "node_modules/pkg/skills/one/two/three/four/five/six/SKILL.md",
            "---\nname: \"Custom Name\"\ndescription: Codex skill.\n---\n[Guide](references/guide.md)\n",
        );
        fixture.add(
            "node_modules/pkg/skills/one/two/three/four/five/six/references/guide.md",
            "# Guide\n",
        );
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "."]);
        }

        let shape = fixture.run_scan().unwrap_or_else(|error| {
            panic!("Codex recovers explicit roots under node_modules: {error}")
        });
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));

        let exclude_skill =
            ["node_modules/pkg/skills/one/two/three/four/five/six/SKILL.md".to_owned()];
        let shape = fixture
            .run_scan_with_exclude(&exclude_skill)
            .unwrap_or_else(|error| panic!("an excluded Codex definition is ignored: {error}"));
        assert!(shape.detected.contains(&"skills-count:0".to_owned()));

        let exclude = [
            "node_modules/pkg/skills/one/two/three/four/five/six/references/guide.md".to_owned(),
        ];
        let error = fixture
            .run_scan_with_exclude(&exclude)
            .expect_err("excluded Codex resources must not satisfy local links");
        assert!(error
            .to_string()
            .contains("missing Markdown link references/guide.md"));
    }
}

#[test]
fn codex_depth_six_skill_can_link_to_node_modules_without_expanding_discovery() {
    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./skills"}"#,
        );
        fixture.add(
            "skills/one/two/three/four/five/six/SKILL.md",
            "---\nname: depth-six\ndescription: Depth boundary skill.\n---\n[Package](../../../../../../../node_modules/pkg/reference.md)\n",
        );
        fixture.add(
            "skills/one/two/three/four/five/six/seven/SKILL.md",
            "not discovered beyond depth six\n",
        );
        fixture.add("node_modules/pkg/reference.md", "# Package reference\n");
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "."]);
        }

        let shape = fixture.run_scan().unwrap_or_else(|error| {
            panic!("a depth-six skill resolves its separately checked node_modules link: {error}")
        });
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));

        let exclude = ["node_modules/pkg/reference.md".to_owned()];
        let error = fixture
            .run_scan_with_exclude(&exclude)
            .expect_err("an excluded linked node_modules target cannot satisfy its link");
        assert!(error
            .to_string()
            .contains("missing Markdown link ../../../../../../../node_modules/pkg/reference.md"));
    }
}

#[test]
fn codex_hidden_link_targets_under_declared_roots_do_not_satisfy_links() {
    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./skills"}"#,
        );
        fixture.add(
            "skills/example/SKILL.md",
            "---\nname: example\ndescription: Example skill.\n---\n[Hidden](references/.hidden/guide.md)\n",
        );
        fixture.add("skills/example/references/.hidden/guide.md", "# Hidden\n");
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "."]);
        }

        let error = fixture
            .run_scan()
            .expect_err("hidden subdirectories are pruned from Codex link recovery");
        assert!(error
            .to_string()
            .contains("missing Markdown link references/.hidden/guide.md"));
    }
}

#[cfg(unix)]
#[test]
fn codex_hidden_directory_aliases_do_not_satisfy_explicit_links() {
    use std::os::unix::fs::symlink;

    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./skills"}"#,
        );
        fixture.add(
            "skills/example/SKILL.md",
            "---\nname: example\ndescription: Example skill.\n---\n[Hidden alias](references/.hidden-alias/guide.md)\n",
        );
        fixture.add(
            "skills/example/references/visible/guide.md",
            "# The canonical resource is visible.\n",
        );
        must(
            symlink(
                "visible",
                fixture.root.join("skills/example/references/.hidden-alias"),
            ),
            "add hidden directory alias to visible resource",
        );
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "."]);
        }

        let error = fixture
            .run_scan()
            .expect_err("a hidden visible alias is pruned even when its target exists");
        assert!(error
            .to_string()
            .contains("missing Markdown link references/.hidden-alias/guide.md"));
    }
}

#[cfg(unix)]
#[test]
fn codex_hidden_external_directory_aliases_do_not_satisfy_links() {
    use std::os::unix::fs::symlink;

    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./skills"}"#,
        );
        fixture.add(
            "skills/example/SKILL.md",
            "---\nname: example\ndescription: Example skill.\n---\n[External](references/.outside/guide.md)\n",
        );
        let outside = fixture.root.with_file_name(format!(
            "{}-outside",
            fixture
                .root
                .file_name()
                .expect("fixture root has a basename")
                .to_string_lossy()
        ));
        must(fs::create_dir_all(&outside), "create external target");
        must(
            fs::write(outside.join("guide.md"), "# Must not be read\n"),
            "write external target",
        );
        must(
            fs::create_dir_all(fixture.root.join("skills/example/references")),
            "create link parent",
        );
        must(
            symlink(
                &outside,
                fixture.root.join("skills/example/references/.outside"),
            ),
            "add hidden external directory alias",
        );
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "."]);
        }

        let result = fixture.run_scan();
        let _ = fs::remove_dir_all(&outside);
        let error = result.expect_err("hidden external resources cannot satisfy links");
        assert!(error
            .to_string()
            .contains("missing Markdown link references/.outside/guide.md"));
    }
}

#[cfg(unix)]
#[test]
fn codex_follows_in_repository_directory_symlinks_through_the_visible_alias() {
    use std::os::unix::fs::symlink;

    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./node_modules/pkg/skills"}"#,
        );
        fixture.add(
            "actual/.hidden/skill/SKILL.md",
            "---\nname: Alias Skill\ndescription: Read through an in-repository directory link.\n---\n[Guide](references/guide.md)\n",
        );
        fixture.add(
            "actual/.hidden/skill/references/guide.md",
            "# Target exists\n",
        );
        must(
            fs::create_dir_all(fixture.root.join("node_modules/pkg/skills")),
            "create Codex node_modules skill root",
        );
        must(
            symlink(
                fixture.root.join("actual/.hidden/skill"),
                fixture.root.join("node_modules/pkg/skills/linked"),
            ),
            "add in-repository Codex skill alias",
        );
        must(
            symlink(
                fixture.root.join("actual/.hidden/skill"),
                fixture.root.join("node_modules/pkg/skills/.hidden-alias"),
            ),
            "add a hidden Codex alias to a valid skill",
        );
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "."]);
        }

        let shape = fixture.run_scan().unwrap_or_else(|error| {
            panic!("Codex follows a directory link and reads skill/resources safely: {error}")
        });
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));

        let alias_skill_excluded = ["node_modules/pkg/skills/linked/SKILL.md".to_owned()];
        let shape = fixture
            .run_scan_with_exclude(&alias_skill_excluded)
            .unwrap_or_else(|error| panic!("the visible skill alias is excluded: {error}"));
        assert!(shape.detected.contains(&"skills-count:0".to_owned()));

        let alias_resource_excluded =
            ["node_modules/pkg/skills/linked/references/guide.md".to_owned()];
        let error = fixture
            .run_scan_with_exclude(&alias_resource_excluded)
            .expect_err("an excluded alias resource cannot satisfy a Markdown link");
        assert!(error
            .to_string()
            .contains("missing Markdown link references/guide.md"));
    }
}

#[cfg(unix)]
#[test]
fn codex_directory_symlink_rejects_canonical_targets_outside_repository() {
    use std::os::unix::fs::symlink;

    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./external-skills"}"#,
    );
    let external_root = fixture.root.with_extension("external-skills");
    let _ = fs::remove_dir_all(&external_root);
    must(
        fs::create_dir_all(&external_root),
        "create external Codex skill target",
    );
    must(
        fs::write(external_root.join("SKILL.md"), skill("outside").as_bytes()),
        "write external Codex skill",
    );
    must(
        symlink(&external_root, fixture.root.join("external-skills")),
        "add external Codex directory alias",
    );

    let error = fixture
        .run_scan()
        .expect_err("Codex roots cannot resolve outside the repository");
    assert!(error.to_string().contains("outside") || error.to_string().contains("repository"));
    must(
        fs::remove_dir_all(external_root),
        "remove external Codex target",
    );
}

#[cfg(unix)]
#[test]
fn codex_declared_root_alias_to_repository_root_is_scanned_once() {
    use std::os::unix::fs::symlink;

    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./root-alias"}"#,
        );
        fixture.add("skills/example/SKILL.md", &skill("root-alias"));
        must(
            symlink(&fixture.root, fixture.root.join("root-alias")),
            "add a Codex root alias to the repository root",
        );
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "-f", "."]);
        }

        let shape = fixture.run_scan().unwrap_or_else(|error| {
            panic!("the initial Codex root alias may resolve to the repository root: {error}")
        });
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    }
}

#[cfg(unix)]
#[test]
fn codex_exclusion_of_first_directory_alias_does_not_hide_a_later_alias() {
    use std::os::unix::fs::symlink;

    for use_git_index in [false, true] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            r#"{"name":"codex-plugin","skills":"./aliases"}"#,
        );
        fixture.add("actual/SKILL.md", &skill("shared-alias"));
        must(
            fs::create_dir_all(fixture.root.join("aliases")),
            "create Codex alias directory",
        );
        for alias in ["a-first", "b-second"] {
            must(
                symlink(
                    fixture.root.join("actual"),
                    fixture.root.join(format!("aliases/{alias}")),
                ),
                "add sibling Codex directory alias",
            );
        }
        if use_git_index {
            fixture.git(&["init", "-q"]);
            fixture.git(&["add", "-f", "."]);
        }

        let exclude = ["aliases/a-first/SKILL.md".to_owned()];
        let shape = fixture
            .run_scan_with_exclude(&exclude)
            .unwrap_or_else(|error| {
                panic!("a later alias remains visible after the first alias is excluded: {error}")
            });
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    }
}

#[cfg(unix)]
#[test]
fn codex_skips_symlinked_skill_files_and_stops_directory_cycles() {
    use std::os::unix::fs::symlink;

    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./skills"}"#,
    );
    fixture.add("real-skill/SKILL.md", &skill("linked-file"));
    fixture.add("skills/cycle/SKILL.md", &skill("cycle"));
    must(
        fs::create_dir_all(fixture.root.join("skills/file-link")),
        "create directory for Codex file symlink",
    );
    must(
        symlink(
            fixture.root.join("real-skill/SKILL.md"),
            fixture.root.join("skills/file-link/SKILL.md"),
        ),
        "add a file symlink under Codex root",
    );
    must(
        symlink(
            fixture.root.join("skills"),
            fixture.root.join("skills/cycle/back-to-root"),
        ),
        "add a directory cycle under Codex root",
    );

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("Codex symlink cycle terminates safely: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[cfg(unix)]
#[test]
fn codex_file_symlinks_do_not_satisfy_linked_resources() {
    use std::os::unix::fs::symlink;

    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./skills"}"#,
    );
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: Example\ndescription: Checks linked resources.\n---\n[Guide](references/guide.md)\n",
    );
    fixture.add("real-guide.md", "# Guide\n");
    must(
        fs::create_dir_all(fixture.root.join("skills/example/references")),
        "create Codex skill resource directory",
    );
    must(
        symlink(
            fixture.root.join("real-guide.md"),
            fixture.root.join("skills/example/references/guide.md"),
        ),
        "add symlinked Codex resource file",
    );

    let error = fixture
        .run_scan()
        .expect_err("file symlinks cannot satisfy a Codex resource link");
    assert!(error
        .to_string()
        .contains("missing Markdown link references/guide.md"));
}

#[cfg(unix)]
#[test]
fn codex_git_index_does_not_use_untracked_files_through_a_tracked_directory_link() {
    use std::os::unix::fs::symlink;

    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":"./node_modules/pkg/skills"}"#,
    );
    fixture.add(
        "actual/.hidden/skill/SKILL.md",
        "---\nname: Alias Skill\ndescription: Read through an in-repository directory link.\n---\n[Guide](references/guide.md)\n",
    );
    let untracked_resource = fixture
        .root
        .join("actual/.hidden/skill/references/guide.md");
    must(
        fs::create_dir_all(untracked_resource.parent().unwrap_or(&fixture.root)),
        "create untracked Codex resource directory",
    );
    must(
        fs::create_dir_all(fixture.root.join("node_modules/pkg/skills")),
        "create Codex node_modules skill root",
    );
    must(
        symlink(
            fixture.root.join("actual/.hidden/skill"),
            fixture.root.join("node_modules/pkg/skills/linked"),
        ),
        "add tracked Codex skill alias",
    );
    fixture.git(&["init", "-q"]);
    fixture.git(&["add", "-f", "."]);
    must(
        fs::write(&untracked_resource, "# Untracked guide\n"),
        "write untracked Codex linked resource",
    );

    let error = fixture
        .run_scan()
        .expect_err("an untracked Git file cannot satisfy a Codex link");
    assert!(error
        .to_string()
        .contains("missing Markdown link references/guide.md"));

    fixture.git(&["add", "actual/.hidden/skill/references/guide.md"]);
    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("a tracked canonical target resolves through its alias: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn codex_legacy_names_and_optional_fields_follow_runtime_defaults() {
    for (manifest, expected_count) in [
        ("{}", 1),
        (r#"{"name":"","version":null,"skills":null}"#, 1),
        (r#"{"name":"Codex Plugin","skills":"./custom-skills"}"#, 1),
    ] {
        let mut fixture = Fixture::empty();
        fixture.add(".codex-plugin/plugin.json", manifest);
        fixture.add("skills/default/SKILL.md", &skill("default"));
        fixture.add("custom-skills/custom/SKILL.md", &skill("custom"));

        let shape = fixture.run_scan().unwrap_or_else(|error| {
            panic!("Codex legacy parser accepts its name defaults and fields: {error}")
        });
        assert!(shape
            .detected
            .contains(&format!("skills-count:{expected_count}")));
        assert!(shape.detected.contains(&"skills-provider:codex".to_owned()));
    }
}

#[test]
fn codex_empty_skills_array_uses_the_default_root() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":[]}"#,
    );
    fixture.add("skills/default/SKILL.md", &skill("default"));
    fixture.add(
        "custom-skills/custom/SKILL.md",
        "an unselected explicit root must not be parsed\n",
    );

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("Codex's empty skills array selects the default skills root: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn codex_name_must_be_a_string_when_present() {
    for name in ["null", "7", "[]"] {
        let mut fixture = Fixture::empty();
        fixture.add(
            ".codex-plugin/plugin.json",
            &format!(r#"{{"name":{name}}}"#),
        );

        let (error, _) = fixture.run_detect_failure();
        assert!(
            error
                .to_string()
                .contains("name must be a string when present"),
            "non-string Codex name {name} must fail: {error}"
        );
    }
}

#[test]
fn codex_skills_array_entries_must_be_relative_path_strings() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"codex-plugin","skills":["./custom",7]}"#,
    );
    let (error, _) = fixture.run_detect_failure();
    assert!(error
        .to_string()
        .contains("skills entries must be relative path strings"));
}

#[test]
fn kimi_root_manifest_supersedes_compat_manifest_and_declared_roots() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skillpacks"}"#,
    );
    fixture.add(
        ".kimi-plugin/plugin.json",
        "not JSON; the root manifest wins",
    );
    fixture.add("skillpacks/example/SKILL.md", &skill("example"));
    fixture.add(
        "SKILL.md",
        "not a discovered root skill when skills is explicit\n",
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("root Kimi manifest takes precedence: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape.detected.contains(&"skills-provider:kimi".to_owned()));
}

#[test]
fn kimi_documented_sidecar_manifest_is_detected_as_kimi() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".kimi-plugin/plugin.json",
        r#"{"name":"sidecar-plugin","skills":"./skills"}"#,
    );
    fixture.add("skills/example/SKILL.md", &skill("example"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("the documented Kimi sidecar is valid: {error}"));
    assert!(shape.detected.contains(&"skills-provider:kimi".to_owned()));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(!shape
        .detected
        .contains(&"skills-provider:kimi-compatibility".to_owned()));
}

#[test]
fn kimi_root_skill_fallback_is_used_when_skills_is_omitted() {
    let mut fixture = Fixture::empty();
    fixture.add("kimi.plugin.json", r#"{"name":"root-skill-plugin"}"#);
    fixture.add("SKILL.md", &skill("root-skill"));
    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("root skill fallback is valid: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn kimi_root_skill_fallback_scans_only_the_root_skill_document() {
    let mut fixture = Fixture::empty();
    fixture.add("kimi.plugin.json", r#"{"name":"root-skill-plugin"}"#);
    fixture.add("SKILL.md", &skill("root-skill"));
    fixture.add("docs/unrelated.md", "[broken](missing-resource.md)\n");
    fixture.add("skills/inferred/SKILL.md", "invalid frontmatter\n");

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("root-only fallback must ignore unrelated repository content: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn kimi_root_skill_skips_missing_metadata_without_failing_plugin_scan() {
    let mut missing_name = Fixture::empty();
    missing_name.add("kimi.plugin.json", r#"{"name":"root-skill-plugin"}"#);
    missing_name.add(
        "SKILL.md",
        "---\ndescription: Root skill.\n---\n# Root skill\n",
    );
    let (_, shape) = missing_name
        .run_detect()
        .unwrap_or_else(|error| panic!("Kimi skips a root Skill without a name: {error}"));
    assert!(shape.detected.contains(&"skills-count:0".to_owned()));

    let mut missing_description = Fixture::empty();
    missing_description.add("kimi.plugin.json", r#"{"name":"root-skill-plugin"}"#);
    missing_description.add("SKILL.md", "---\nname: root-skill\n---\n# Root skill\n");
    let (_, shape) = missing_description
        .run_detect()
        .unwrap_or_else(|error| panic!("Kimi skips a root Skill without a description: {error}"));
    assert!(shape.detected.contains(&"skills-count:0".to_owned()));

    let mut named = Fixture::empty();
    named.add("kimi.plugin.json", r#"{"name":"root-skill-plugin"}"#);
    named.add(
        "SKILL.md",
        "---\nname: root-skill\ndescription: Root skill.\n---\n# Root skill\n",
    );
    let (_, shape) = named
        .run_detect()
        .unwrap_or_else(|error| panic!("root skill has its required metadata: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn kimi_manifest_skills_paths_are_directory_roots() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":["./skills/quick-start.md","./skilldirs/lookup"]}"#,
    );
    fixture.add(
        "skills/quick-start.md",
        "# Quick start\nUse this workflow.\n",
    );
    fixture.add("skilldirs/lookup/SKILL.md", &skill("lookup"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("declared Kimi skill directories are discovered: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("ignores Skills path skills/quick-start.md")
            && limitation.contains("directory roots only")
    }));
}

#[test]
fn kimi_skill_and_command_roots_require_dot_slash_prefix() {
    let mut skill_root = Fixture::empty();
    skill_root.add("kimi.plugin.json", r#"{"name":"kimi-plugin","skills":"."}"#);
    let (error, _) = skill_root.run_detect_failure();
    assert!(error.to_string().contains("must start with ./"));

    let mut command_root = Fixture::empty();
    command_root.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","commands":"."}"#,
    );
    command_root.add("commands/deploy.md", "# Deploy\n");
    let (_, shape) = command_root
        .run_detect()
        .unwrap_or_else(|error| panic!("Kimi ignores an invalid command path: {error}"));
    assert!(!shape.detected.contains(&"commands-count:1".to_owned()));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("invalid command path") && limitation.contains("must start with ./")
    }));
}

#[test]
fn kimi_ordinary_skill_can_link_to_a_tracked_node_modules_resource() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: Example Skill.\n---\n[Reference](node_modules/pkg/reference.md)\n",
    );
    fixture.add(
        "skills/example/node_modules/pkg/reference.md",
        "# Reference\n",
    );
    fixture.git(&["init", "-q"]);
    fixture.git(&["add", "."]);

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("a tracked Kimi resource beneath node_modules exists: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn kimi_directory_skills_require_metadata_but_allow_custom_names() {
    let mut valid = Fixture::empty();
    valid.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skilldirs"}"#,
    );
    valid.add(
        "skilldirs/Lookup/SKILL.md",
        "---\nname: custom-reviewer\ndescription: A valid Kimi skill.\n---\n",
    );
    valid.add(
        "skilldirs/derived/SKILL.md",
        "---\nname: derived\ndescription: Another valid Kimi skill.\n---\n",
    );
    let (_, shape) = valid.run_detect().unwrap_or_else(|error| {
        panic!("Kimi frontmatter names need not match directories: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));

    let mut invalid_metadata = Fixture::empty();
    invalid_metadata.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skilldirs"}"#,
    );
    invalid_metadata.add(
        "skilldirs/lookup/SKILL.md",
        "---\ndescription: [invalid]\n---\n# Lookup\n",
    );
    invalid_metadata.add("skilldirs/valid/SKILL.md", &skill("valid"));
    let (_, shape) = invalid_metadata
        .run_detect()
        .unwrap_or_else(|error| panic!("an invalid Kimi Skill is skipped: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));

    let mut missing_description = Fixture::empty();
    missing_description.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skilldirs"}"#,
    );
    missing_description.add(
        "skilldirs/lookup/SKILL.md",
        "---\nname: lookup\n---\n# Lookup\n",
    );
    missing_description.add("skilldirs/valid/SKILL.md", &skill("valid"));
    let (_, shape) = missing_description
        .run_detect()
        .unwrap_or_else(|error| panic!("a Kimi Skill missing description is skipped: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn kimi_skills_root_dot_finds_root_and_nested_directory_skills() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./"}"#,
    );
    fixture.add("SKILL.md", &skill("root-skill"));
    fixture.add("groups/review/SKILL.md", &skill("nested-review"));
    fixture.add(".hidden/SKILL.md", &skill("hidden-direct"));
    fixture.add("node_modules/SKILL.md", &skill("node-modules-direct"));
    fixture.add(".hidden/ignored/SKILL.md", "invalid frontmatter\n");
    fixture.add("node_modules/ignored/SKILL.md", "invalid frontmatter\n");

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("Kimi accepts ./ and nested directory skills: {error}"));
    assert!(shape.detected.contains(&"skills-count:4".to_owned()));
}

#[test]
fn kimi_run_scan_recovers_direct_node_modules_skill_from_pruned_walk() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    fixture.add("skills/node_modules/SKILL.md", &skill("bundled"));

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("Kimi direct node_modules skill is discovered: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn kimi_run_scan_recovers_flat_skills_under_generic_pruned_declared_roots() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./target/skills"}"#,
    );
    fixture.add("target/skills/quick.md", "# Quick\nA flat skill.\n");
    fixture.git(&["init", "-q"]);
    fixture.git(&["add", "."]);

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("Kimi scans flat skills from a declared root under target/: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn kimi_run_scan_recovers_resources_for_direct_skill_in_pruned_directory() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    fixture.add(
        "skills/node_modules/SKILL.md",
        "---\nname: bundled\ndescription: Bundled Skill.\n---\n[Guide](references/guide.md)\n",
    );
    fixture.add(
        "skills/node_modules/references/guide.md",
        "[missing](missing.md)\n",
    );
    fixture.add(
        "skills/node_modules/pkg/SKILL.md",
        "invalid nested Skill frontmatter\n",
    );
    fixture.add(
        "skills/.hidden/SKILL.md",
        "---\nname: hidden\ndescription: Hidden Skill.\n---\n[Guide](references/guide.md)\n",
    );
    fixture.add("skills/.hidden/references/guide.md", "# Hidden guide\n");
    fixture.add(
        "skills/node_modules/templates/config.json",
        "{\"valid\": true}\n",
    );
    fixture.add(
        "skills/node_modules/templates/SKILL.md",
        "---\nname: template\n---\n",
    );
    fixture.git(&["init", "-q"]);
    fixture.git(&["add", "."]);

    let error = fixture
        .run_scan()
        .expect_err("Markdown in a direct pruned-directory Skill must be validated");
    assert!(error.to_string().contains("references/guide.md"));
    assert!(error.to_string().contains("missing Markdown link"));

    fixture.add("skills/node_modules/references/guide.md", "# Guide\n");
    fixture.add(
        "skills/.hidden/references/guide.md",
        "[missing](missing.md)\n",
    );
    let error = fixture
        .run_scan()
        .expect_err("resources beside a direct hidden Skill must be validated");
    assert!(error.to_string().contains(".hidden/references/guide.md"));

    fixture.add("skills/.hidden/references/guide.md", "# Hidden guide\n");
    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("unopted content beneath the pruned Skill must stay out of scope: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));

    fixture.add(
        "skills/node_modules/templates/config.json",
        "{ invalid JSON",
    );
    let error = fixture
        .run_scan()
        .expect_err("templates beside a direct pruned-directory Skill must be validated");
    assert!(error.to_string().contains("invalid JSON template"));
    assert!(error
        .to_string()
        .contains("skills/node_modules/templates/config.json"));
}

#[test]
fn kimi_pruned_skill_templates_validate_toml_and_yaml() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    fixture.add("skills/node_modules/SKILL.md", &skill("bundled"));
    fixture.add(
        "skills/node_modules/templates/settings.toml",
        "enabled = true\n",
    );
    fixture.add(
        "skills/node_modules/templates/settings.yaml",
        "enabled: true\n",
    );
    fixture.add(
        "skills/node_modules/templates/settings.yml",
        "enabled: true\n",
    );
    fixture.git(&["init", "-q"]);
    fixture.git(&["add", "."]);

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("valid TOML and YAML templates beside a direct Skill are accepted: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));

    fixture.add(
        "skills/node_modules/templates/settings.toml",
        "enabled = true\nenabled = false\n",
    );
    let error = fixture
        .run_scan()
        .expect_err("TOML templates beside a direct pruned Skill must be validated");
    assert!(error.to_string().contains("invalid TOML template"));
    assert!(error
        .to_string()
        .contains("skills/node_modules/templates/settings.toml"));

    fixture.add(
        "skills/node_modules/templates/settings.toml",
        "enabled = true\n",
    );
    fixture.add(
        "skills/node_modules/templates/settings.yaml",
        "enabled: true\nenabled: false\n",
    );
    let error = fixture
        .run_scan()
        .expect_err("YAML templates beside a direct pruned Skill must be validated");
    assert!(error.to_string().contains("invalid YAML template"));
    assert!(error
        .to_string()
        .contains("skills/node_modules/templates/settings.yaml"));
}

#[test]
fn kimi_pruned_skill_resources_respect_scan_excludes() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    fixture.add("skills/node_modules/SKILL.md", &skill("bundled"));
    fixture.add(
        "skills/node_modules/references/guide.md",
        "[missing](not-there.md)\n",
    );
    fixture.git(&["init", "-q"]);
    fixture.git(&["add", "."]);

    let error = fixture
        .run_scan()
        .expect_err("an included resource in a pruned Skill must be link-validated");
    assert!(error.to_string().contains("references/guide.md"));
    assert!(error.to_string().contains("missing Markdown link"));

    let shape = fixture
        .run_scan_with_exclude(&["skills/node_modules/references/guide.md".to_owned()])
        .unwrap_or_else(|error| panic!("excluded pruned-Skill resource stays excluded: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(!shape
        .files
        .contains(&"skills/node_modules/references/guide.md".to_owned()));
}

#[test]
fn kimi_pruned_child_resources_follow_the_parent_nested_skill_gate() {
    let mut disabled = Fixture::empty();
    disabled.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    disabled.add("skills/package/SKILL.md", &skill("package"));
    disabled.add(
        "skills/package/node_modules/SKILL.md",
        "unparseable frontmatter outside the parent's nested-Skill scope\n",
    );
    disabled.add(
        "skills/package/node_modules/references/guide.md",
        "[missing](not-there.md)\n",
    );
    disabled.git(&["init", "-q"]);
    disabled.git(&["add", "."]);

    let shape = disabled
        .run_scan()
        .unwrap_or_else(|error| panic!("unopted child resources stay out of scope: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));

    let mut enabled = Fixture::empty();
    enabled.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    enabled.add(
        "skills/package/SKILL.md",
        "---\nname: package\ndescription: Package Skill.\nhas-sub-skill: true\n---\n# Package\n",
    );
    enabled.add(
        "skills/package/node_modules/SKILL.md",
        "---\nname: child\ndescription: Child Skill.\n---\n[Guide](references/guide.md)\n",
    );
    enabled.add(
        "skills/package/node_modules/references/guide.md",
        "[missing](not-there.md)\n",
    );
    enabled.git(&["init", "-q"]);
    enabled.git(&["add", "."]);

    let error = enabled
        .run_scan()
        .expect_err("opted-in pruned child resources must be link-validated");
    assert!(error
        .to_string()
        .contains("node_modules/references/guide.md"));
    assert!(error.to_string().contains("missing Markdown link"));
}

#[test]
fn kimi_pruned_github_skill_resources_are_recovered() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    fixture.add(
        "skills/.github/SKILL.md",
        "---\nname: generated-skill\ndescription: Generated Skill.\n---\n[Guide](references/guide.md)\n",
    );
    fixture.add(
        "skills/.github/references/guide.md",
        "[missing](not-there.md)\n",
    );
    fixture.git(&["init", "-q"]);
    fixture.git(&["add", "."]);

    let error = fixture
        .run_scan()
        .expect_err("a direct Skill in generic-pruned .github still owns its resources");
    assert!(error.to_string().contains(".github/references/guide.md"));
    assert!(error.to_string().contains("missing Markdown link"));
}

#[cfg(unix)]
#[test]
fn kimi_pruned_skill_resource_recovery_does_not_follow_symlinks() {
    use std::os::unix::fs::symlink;

    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    fixture.add("skills/node_modules/SKILL.md", &skill("bundled"));
    fixture.add("outside/references/guide.md", "[missing](not-there.md)\n");
    fixture.add("outside/templates/config.json", "{ invalid JSON\n");
    must(
        fs::create_dir_all(fixture.root.join("skills/node_modules/references")),
        "create pruned-Skill resource directory",
    );
    must(
        symlink(
            fixture.root.join("outside/references/guide.md"),
            fixture.root.join("skills/node_modules/references/guide.md"),
        ),
        "link a pruned-Skill resource file outside the component",
    );
    must(
        symlink(
            fixture.root.join("outside/templates"),
            fixture.root.join("skills/node_modules/templates"),
        ),
        "link a pruned-Skill template directory outside the component",
    );
    fixture.git(&["init", "-q"]);
    fixture.git(&["add", "."]);

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("pruned-Skill recovery skips symlinks: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn kimi_run_scan_keeps_a_direct_command_file_scope_narrow() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","commands":"./commands/run.md"}"#,
    );
    fixture.add("commands/run.md", "# Run\n");
    fixture.add("commands/unrelated.md", "[missing](missing.md)\n");
    fixture.git(&["init", "-q"]);
    fixture.git(&["add", "."]);

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("one Kimi command file must not scan siblings: {error}"));
    assert!(shape.detected.contains(&"commands-count:1".to_owned()));

    fixture.add("commands/run.md", "[missing](missing.md)\n");
    let error = fixture
        .run_scan()
        .expect_err("the declared Kimi command file must still validate its links");
    assert!(error.to_string().contains("commands/run.md"));
    assert!(error.to_string().contains("missing Markdown link"));
}

#[test]
fn kimi_run_scan_recovers_flat_skills_and_recursive_commands_under_node_modules() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./node_modules/pkg/skills","commands":"./node_modules/pkg/commands"}"#,
    );
    fixture.add(
        "node_modules/pkg/skills/quick.md",
        "# Quick\nA flat skill.\n",
    );
    fixture.add(
        "node_modules/pkg/skills/.hidden.md",
        "# Hidden\nAlso a top-level flat skill.\n",
    );
    fixture.add(
        "node_modules/pkg/skills/ignored.MD",
        "# Ignored uppercase extension.\n",
    );
    fixture.add(
        "node_modules/pkg/skills/nested/not-a-flat-skill.md",
        "# Nested Markdown\nNot a top-level flat skill.\n",
    );
    fixture.add(
        "node_modules/pkg/skills/directory/SKILL.md",
        &skill("directory"),
    );
    fixture.add("node_modules/pkg/commands/deploy.md", "# Deploy\n");
    fixture.add(
        "node_modules/pkg/commands/.hidden/review.md",
        "# Hidden command\n",
    );
    fixture.add(
        "node_modules/pkg/commands/node_modules/tool.md",
        "# Package command\n",
    );
    fixture.add(
        "node_modules/pkg/commands/deep/path/cleanup.md",
        "# Deep command\n",
    );

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("Kimi declared roots recover their provider-defined files: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:3".to_owned()));
    assert!(shape.detected.contains(&"commands-count:4".to_owned()));

    let excluded = fixture
        .run_scan_with_exclude(&["node_modules/pkg/commands/node_modules/tool.md".to_owned()])
        .unwrap_or_else(|error| panic!("Kimi command recovery applies scan excludes: {error}"));
    assert!(excluded.detected.contains(&"skills-count:3".to_owned()));
    assert!(excluded.detected.contains(&"commands-count:3".to_owned()));
}

#[test]
fn kimi_run_scan_ignores_a_declared_skill_root_that_is_a_file_under_node_modules() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./node_modules/pkg/skill.md"}"#,
    );
    fixture.add("node_modules/pkg/skill.md", "# Not a directory root\n");

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("Kimi ignores direct file skill roots: {error}"));
    assert!(shape.detected.contains(&"skills-count:0".to_owned()));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("accepts directory roots only")
            && limitation.contains("node_modules/pkg/skill.md")
    }));
}

#[test]
fn kimi_run_scan_checks_resources_from_recovered_node_modules_skills() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills/node_modules"}"#,
    );
    fixture.add(
        "skills/node_modules/pkg/SKILL.md",
        "---\nname: package-skill\ndescription: Package skill.\n---\n[Guide](../resources/guide.md)\n",
    );
    fixture.add(
        "skills/node_modules/resources/guide.md",
        "# Package guide\n",
    );
    fixture.add(
        "skills/node_modules/pkg/references/extra.md",
        "# Extra reference\n",
    );

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("a recovered Skill can link to a sibling resource: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));

    fixture.add(
        "skills/node_modules/pkg/references/extra.md",
        "[missing](missing.md)\n",
    );
    let error = fixture
        .run_scan()
        .expect_err("linked support Markdown must validate local links");
    assert!(error.to_string().contains("references/extra.md"));
    assert!(error.to_string().contains("missing Markdown link"));
}

#[test]
fn kimi_recovered_skill_templates_are_loaded_and_validated() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./node_modules/pkg/skills"}"#,
    );
    fixture.add("node_modules/pkg/skills/review/SKILL.md", &skill("review"));
    fixture.add(
        "node_modules/pkg/skills/review/templates/config.json",
        "{ invalid JSON",
    );

    let error = fixture
        .run_scan()
        .expect_err("recovered template files must be validated");
    assert!(error
        .to_string()
        .contains("invalid JSON template at node_modules/pkg/skills/review/templates/config.json"));

    fixture.add(
        "node_modules/pkg/skills/review/templates/config.json",
        r#"{"valid":true}"#,
    );
    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("valid recovered template is accepted: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(!shape
        .files
        .iter()
        .any(|file| { file == "node_modules/pkg/skills/review/templates/config.json" }));
}

#[test]
fn kimi_run_scan_does_not_recover_excluded_node_modules_skill() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    fixture.add("skills/node_modules/SKILL.md", "invalid frontmatter\n");

    let shape = fixture
        .run_scan_with_exclude(&["skills/node_modules/SKILL.md".to_owned()])
        .unwrap_or_else(|error| panic!("excluded Kimi Skill stays excluded: {error}"));
    assert!(!shape
        .files
        .contains(&"skills/node_modules/SKILL.md".to_owned()));
    assert!(shape.detected.contains(&"skills-count:0".to_owned()));
}

#[test]
fn kimi_excluded_parent_skill_still_gates_nested_skills() {
    let mut excluded_parent = Fixture::empty();
    excluded_parent.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    excluded_parent.add(
        "skills/parent/SKILL.md",
        "---\nname: parent\ndescription: Parent skill.\n---\n# Parent\n",
    );
    excluded_parent.add("skills/parent/child/SKILL.md", &skill("child"));

    let shape = excluded_parent
        .run_scan_with_exclude(&["skills/parent/SKILL.md".to_owned()])
        .unwrap_or_else(|error| panic!("excluded parent remains a nested-skill gate: {error}"));
    assert!(shape.detected.contains(&"skills-count:0".to_owned()));
    assert!(!shape.files.contains(&"skills/parent/SKILL.md".to_owned()));

    let mut ordinary_group = Fixture::empty();
    ordinary_group.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    ordinary_group.add("skills/group/child/SKILL.md", &skill("child"));
    let shape = ordinary_group
        .run_scan_with_exclude(&["skills/group/SKILL.md".to_owned()])
        .unwrap_or_else(|error| panic!("absent grouping Skill does not gate child: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn kimi_run_scan_honors_nested_flags_for_declared_root_inside_node_modules() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./node_modules/example/skills"}"#,
    );
    fixture.add(
        "node_modules/example/skills/SKILL.md",
        "---\nname: package-root\ndescription: Package root.\n---\n# Package root\n",
    );
    fixture.add(
        "node_modules/example/skills/review/SKILL.md",
        "---\nname: review\ndescription: Review skill.\nhas-sub-skill: true\n---\n# Review\n",
    );
    fixture.add(
        "node_modules/example/skills/review/nested/SKILL.md",
        "---\nname: nested\ndescription: Nested skill.\n---\n# Nested\n",
    );
    fixture.add(
        "node_modules/example/skills/review/nested/deeper/SKILL.md",
        "invalid frontmatter\n",
    );

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("Kimi root skill does not gate its direct child, but child flags do: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:3".to_owned()));
}

#[test]
fn kimi_run_scan_enters_declared_node_modules_root_and_follows_nested_flags() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./node_modules"}"#,
    );
    fixture.add("node_modules/SKILL.md", &skill("node-modules-root"));
    fixture.add(
        "node_modules/example/SKILL.md",
        "---\nname: package-root\ndescription: Package root.\nhas-sub-skill: true\n---\n# Package root\n",
    );
    fixture.add(
        "node_modules/example/sub/SKILL.md",
        "---\nname: sub\ndescription: Sub skill.\nhas-sub-skill: true\n---\n# Sub\n",
    );
    fixture.add(
        "node_modules/example/sub/nested/SKILL.md",
        "---\nname: nested\ndescription: Nested skill.\n---\n# Nested\n",
    );
    fixture.add(
        "node_modules/example/sub/nested/too-deep/SKILL.md",
        "invalid frontmatter\n",
    );
    fixture.add(
        "node_modules/blocked/SKILL.md",
        "---\nname: blocked\ndescription: No nested opt-in.\n---\n# Blocked\n",
    );
    fixture.add("node_modules/blocked/sub/SKILL.md", "invalid frontmatter\n");

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("Kimi declared node_modules root follows opted-in Skills: {error}")
    });
    assert!(
        shape.detected.contains(&"skills-count:5".to_owned()),
        "detected={:?} files={:?}",
        shape.detected,
        shape.files
    );
}

#[test]
fn kimi_run_scan_declared_package_root_checks_child_before_nested_opt_in() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./node_modules/pkg"}"#,
    );
    fixture.add(
        "node_modules/pkg/SKILL.md",
        "---\nname: package-root\ndescription: Package root.\n---\n# Package root\n",
    );
    fixture.add(
        "node_modules/pkg/sub/SKILL.md",
        "---\nname: sub\ndescription: Child Skill.\nhas-sub-skill: true\n---\n# Child\n",
    );
    fixture.add(
        "node_modules/pkg/sub/nested/SKILL.md",
        "---\nname: nested\ndescription: Opted-in grandchild.\n---\n# Nested\n",
    );
    fixture.add(
        "node_modules/pkg/sub/nested/deeper/SKILL.md",
        "invalid frontmatter\n",
    );
    fixture.add(
        "node_modules/pkg/no-nested/SKILL.md",
        "---\nname: no-nested\ndescription: Direct child without opt-in.\n---\n# Direct child\n",
    );
    fixture.add(
        "node_modules/pkg/no-nested/ignored/SKILL.md",
        "invalid frontmatter\n",
    );

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("Kimi scans immediate children before their parent opt-in: {error}")
    });
    assert!(
        shape.detected.contains(&"skills-count:4".to_owned()),
        "detected={:?} files={:?}",
        shape.detected,
        shape.files
    );
}

#[test]
fn kimi_run_scan_respects_excludes_inside_declared_node_modules_root() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./node_modules"}"#,
    );
    fixture.add(
        "node_modules/example/SKILL.md",
        "---\nname: example\ndescription: Example skill.\n---\n# Example\n",
    );

    let shape = fixture
        .run_scan_with_exclude(&["node_modules/example/SKILL.md".to_owned()])
        .unwrap_or_else(|error| panic!("excluded Kimi path stays excluded: {error}"));
    assert!(!shape
        .files
        .contains(&"node_modules/example/SKILL.md".to_owned()));
    assert!(shape.detected.contains(&"skills-count:0".to_owned()));
}

#[test]
fn kimi_declared_roots_match_the_cli_depth_limit() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skilldirs"}"#,
    );
    fixture.add(
        "skilldirs/depth/one/two/three/four/five/six/seven/SKILL.md",
        &skill("within-depth"),
    );
    fixture.add(
        "skilldirs/deepest/one/two/three/four/five/six/seven/eight/SKILL.md",
        &skill("deepest-direct-child"),
    );
    fixture.add(
        "skilldirs/over/one/two/three/four/five/six/seven/eight/nine/SKILL.md",
        "invalid frontmatter\n",
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("Kimi scan depth matches the CLI limit: {error}"));
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));
}

#[test]
fn kimi_nested_directory_skills_require_parent_opt_in() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    fixture.add("skills/group/parent/SKILL.md", &skill("parent"));
    fixture.add(
        "skills/group/parent/child/SKILL.md",
        "invalid ignored child frontmatter\n",
    );
    fixture.add("skills/group/sibling/SKILL.md", &skill("sibling"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("disabled nested Skills are ignored: {error}"));
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));
}

#[test]
fn kimi_unopted_nested_skill_and_support_markdown_do_not_expand_validation_scope() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    fixture.add("skills/parent/SKILL.md", &skill("parent"));
    fixture.add(
        "skills/parent/child/SKILL.md",
        "---\nname: child\ndescription: Unopted child.\n---\n[missing](not-there.md)\n",
    );
    fixture.add(
        "skills/parent/child/references/guide.md",
        "[missing](also-not-there.md)\n",
    );
    fixture.add(
        "skills/parent/child/templates/package.json",
        "not JSON and outside the accepted parent skill's template root",
    );
    fixture.add(
        "packages/app/templates/package.json",
        r#"{"name":"app-template","packageManager":"bun@1.2.3","scripts":{"test":"bun test"}}"#,
    );
    fixture.add("packages/app/templates/bun.lock", "{}\n");
    fixture.add(
        "packages/app/templates/index.ts",
        "export const app = true;\n",
    );

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("unopted nested content does not enter Kimi validation: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape
        .units
        .iter()
        .any(|unit| unit.kind == UnitKind::Bun && unit.root == "packages/app/templates"));
    assert!(!shape
        .units
        .iter()
        .any(|unit| unit.root.starts_with("skills/parent/child/templates")));
}

#[test]
fn kimi_subskill_flags_at_root_or_metadata_enable_only_the_next_skill_level() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    fixture.add(
        "skills/kebab-parent/SKILL.md",
        "---\nname: kebab-parent\ndescription: Parent.\nmetadata:\n  has-sub-skill: true\n---\n# Parent\n",
    );
    fixture.add("skills/kebab-parent/child/SKILL.md", &skill("kebab-child"));
    fixture.add(
        "skills/camel-parent/SKILL.md",
        "---\nname: camel-parent\ndescription: Parent.\nmetadata:\n  hasSubSkill: true\n---\n# Parent\n",
    );
    fixture.add("skills/camel-parent/child/SKILL.md", &skill("camel-child"));
    fixture.add(
        "skills/root-kebab-parent/SKILL.md",
        "---\nname: root-kebab-parent\ndescription: Parent.\nhas-sub-skill: true\n---\n# Parent\n",
    );
    fixture.add(
        "skills/root-kebab-parent/child/SKILL.md",
        &skill("root-kebab-child"),
    );
    fixture.add(
        "skills/root-camel-parent/SKILL.md",
        "---\nname: root-camel-parent\ndescription: Parent.\nhasSubSkill: true\n---\n# Parent\n",
    );
    fixture.add(
        "skills/root-camel-parent/child/SKILL.md",
        &skill("root-camel-child"),
    );
    fixture.add(
        "skills/kebab-parent/child/grandchild/SKILL.md",
        "invalid grandchild without child opt-in\n",
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("both Kimi metadata aliases enable nested Skills: {error}"));
    assert!(shape.detected.contains(&"skills-count:8".to_owned()));
    assert!(shape.detected.contains(&"skills-provider:kimi".to_owned()));
    assert!(!shape
        .detected
        .contains(&"skills-provider:claude".to_owned()));
}

#[test]
fn kimi_flat_markdown_skills_allow_optional_frontmatter_fields() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./flat-skills"}"#,
    );
    fixture.add(
        "flat-skills/quick-start.md",
        "---\nname: quick-start\n---\n# Quick start\n",
    );
    fixture.add("flat-skills/summary.md", "# Summary\nA flat Kimi skill.\n");

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("flat skills may omit frontmatter fields: {error}"));
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));
}

#[test]
fn kimi_flat_skills_and_commands_require_lowercase_md_extension() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./flat-skills","commands":"./commands"}"#,
    );
    fixture.add("flat-skills/kept.md", "# Kept skill\n");
    fixture.add("flat-skills/ignored.MD", "# Ignored skill\n");
    fixture.add("commands/kept.md", "# Kept command\n");
    fixture.add("commands/ignored.MD", "# Ignored command\n");

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("Kimi Markdown discovery is case-sensitive: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape.detected.contains(&"commands-count:1".to_owned()));
}

#[test]
fn kimi_skill_types_match_the_pinned_cli_parser() {
    let mut supported = Fixture::empty();
    supported.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    supported.add(
        "skills/reference/SKILL.md",
        "---\nname: reference\ndescription: A supported reference skill.\ntype: ' reference '\n---\n# Reference\n",
    );
    let (_, shape) = supported
        .run_detect()
        .unwrap_or_else(|error| panic!("Kimi supports the reference skill type: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));

    let mut unsupported = Fixture::empty();
    unsupported.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    unsupported.add(
        "skills/unsupported/SKILL.md",
        "---\nname: unsupported\ndescription: Unsupported type.\ntype: typo\n---\n# Unsupported\n",
    );
    unsupported.add(
        "skills/kept/SKILL.md",
        "---\nname: kept\ndescription: A supported sibling.\ntype: reference\n---\n# Kept\n",
    );
    let (_, shape) = unsupported.run_detect().unwrap_or_else(|error| {
        panic!("an unsupported Kimi skill type skips only that skill: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));

    let mut non_string = Fixture::empty();
    non_string.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    non_string.add(
        "skills/non-string/SKILL.md",
        "---\nname: non-string\ndescription: Non-string type is omitted.\ntype: true\nmetadata:\n  custom: [one, two]\n---\n# Valid\n",
    );
    let (_, shape) = non_string.run_detect().unwrap_or_else(|error| {
        panic!("Kimi omits a non-string type and preserves arbitrary metadata: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));

    let mut empty = Fixture::empty();
    empty.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skills"}"#,
    );
    empty.add(
        "skills/empty/SKILL.md",
        "---\nname: empty\ndescription: Empty type is omitted.\ntype: '   '\n---\n# Empty\n",
    );
    let (_, shape) = empty
        .run_detect()
        .unwrap_or_else(|error| panic!("Kimi omits a blank skill type: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn kimi_manifest_trims_name_ignores_non_string_version_and_bad_commands() {
    for commands in ["7", r#"["./commands", 7]"#] {
        let mut fixture = Fixture::empty();
        fixture.add(
            "kimi.plugin.json",
            &format!(
                r#"{{"name":" kimi-plugin ","version":7,"skills":"./skills","commands":{commands}}}"#
            ),
        );
        fixture.add(
            "skills/quick-start.md",
            "---\nname: 7\ndescription: false\ncustom: [one, two]\nmetadata: {custom: [nested]}\n---\n# Quick start\n",
        );
        fixture.add("commands/ignored.md", "# Not declared\n");

        let (_, shape) = fixture.run_detect().unwrap_or_else(|error| {
            panic!("Kimi trims manifest names and ignores invalid optional fields: {error}")
        });
        assert!(shape.detected.contains(&"skills-count:1".to_owned()));
        assert!(!shape.detected.contains(&"commands-count:1".to_owned()));
        assert!(shape.limitations.iter().any(|limitation| {
            limitation.contains("invalid commands field")
                || limitation.contains("invalid command path")
        }));
    }
}

#[test]
fn flat_kimi_definition_does_not_own_the_collection_templates_directory() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./flat-skills"}"#,
    );
    fixture.add("flat-skills/summary.md", "# Summary\nA flat Kimi skill.\n");
    fixture.add(
        "flat-skills/templates/package.json",
        r#"{"name":"example","packageManager":"bun@1.2.3","scripts":{"test":"bun test"}}"#,
    );
    fixture.add("flat-skills/templates/bun.lock", "{}\n");
    fixture.add(
        "flat-skills/templates/src.ts",
        "export const example = true;\n",
    );

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("flat Skills collection and package both scan: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(shape
        .units
        .iter()
        .any(|unit| unit.kind == UnitKind::Bun && unit.root == "flat-skills/templates"));
}

#[test]
fn kimi_commands_collect_nested_markdown_and_accept_optional_frontmatter() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","commands":["./commands/","./extra.md"]}"#,
    );
    fixture.add("commands/deploy.md", "# Deploy\nDeploy the service.\n");
    fixture.add(
        "commands/frontend/component.md",
        "---\ndescription: Build it.\n---\nPrompt.\n",
    );
    fixture.add("extra.md", "---\nname: extras/command\n---\nPrompt.\n");

    let (_, shape) = fixture.run_detect().unwrap_or_else(|error| {
        panic!("Kimi commands recurse and allow optional metadata: {error}")
    });
    assert!(shape.detected.contains(&"commands-count:3".to_owned()));
}

#[test]
fn kimi_flat_markdown_starting_with_thematic_break_needs_no_frontmatter() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./flat-skills"}"#,
    );
    fixture.add(
        "flat-skills/section.md",
        "---\n## First section\n\nA flat skill without frontmatter.\n",
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("a flat Markdown skill may begin with a rule: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn kimi_directory_skill_wins_over_same_name_flat_markdown() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":"./skillpacks"}"#,
    );
    fixture.add("skillpacks/lookup.md", "---\ninvalid: [\n---\n");
    fixture.add("skillpacks/lookup/SKILL.md", &skill("lookup"));

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("directory-form skill has precedence: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn root_plugin_manifest_requires_schema_and_name() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "plugin.json",
        r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json"}"#,
    );
    let (error, shape) = fixture.run_detect_failure();
    assert!(shape.detected.is_empty());
    assert!(error.to_string().contains("plugin.json"));
}

#[test]
fn duplicate_manifest_json_keys_are_rejected() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/plugin.json",
        r#"{"name":"first","name":"second"}"#,
    );
    let (error, shape) = fixture.run_detect_failure();
    assert!(shape.detected.is_empty());
    assert!(error.to_string().contains("duplicate JSON object key"));
}

#[test]
fn agent_skill_frontmatter_accepts_optional_standard_and_extension_fields() {
    let parsed = parse_frontmatter(
        "\u{feff}---\r\nname: example\r\ndescription: >-\r\n  First line\r\n  second line\r\nlicense: MIT\r\ncompatibility: Any local tool runner.\r\nallowed-tools: Bash(git:*)\r\nmetadata:\r\n  author: example\r\n  revision: v2\r\nuser-invocable: false\r\nfuture-field:\r\n  nested: accepted\r\n...\r\n# Example\r\n",
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition,
    )
    .unwrap_or_else(|error| panic!("valid optional frontmatter fields parse: {error}"));
    assert_eq!(parsed.values["name"], "example");
    assert_eq!(parsed.values["description"], "First line second line");
    assert_eq!(parsed.values["user-invocable"], "false");
}

#[test]
fn claude_boolean_frontmatter_fields_accept_documented_yaml_spellings() {
    for spelling in [
        "true", "false", "yes", "no", "on", "off", "1", "0", "YES", "Off",
    ] {
        let content = format!(
            "---\nname: example\ndescription: Example.\nuser-invocable: {spelling}\ndisable-model-invocation: {spelling}\nbackground: {spelling}\n---\n"
        );
        for mode in [
            FrontmatterMode::ClaudeDefinition,
            FrontmatterMode::ClaudeCommand,
        ] {
            parse_frontmatter(&content, "claude/SKILL.md", mode).unwrap_or_else(|error| {
                panic!("Claude accepts documented boolean spelling {spelling}: {error}")
            });
        }
    }

    let invalid = "---\nuser-invocable: maybe\n---\n";
    assert!(parse_frontmatter(
        invalid,
        "claude/SKILL.md",
        FrontmatterMode::ClaudeDefinition,
    )
    .is_err());
    assert!(parse_frontmatter(
        invalid,
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition
    )
    .is_err());
    parse_frontmatter(
        "---\ncontext: fork\nbackground: false\n---\n",
        "claude/SKILL.md",
        FrontmatterMode::ClaudeDefinition,
    )
    .unwrap_or_else(|error| panic!("Claude background accepts a YAML boolean: {error}"));
}

#[test]
fn claude_background_frontmatter_must_be_a_yaml_boolean() {
    let invalid = "---\ncontext: fork\nbackground: maybe\n---\n";
    let error = parse_frontmatter(
        invalid,
        "claude/SKILL.md",
        FrontmatterMode::ClaudeDefinition,
    )
    .expect_err("background must be boolean");
    assert!(error
        .to_string()
        .contains("background must use a documented boolean spelling"));
}

#[test]
fn claude_allowed_tools_accepts_yaml_string_lists() {
    let content = "---\nallowed-tools: [Read, Grep]\n---\n";
    for mode in [
        FrontmatterMode::ClaudeDefinition,
        FrontmatterMode::ClaudeCommand,
    ] {
        parse_frontmatter(content, "claude/SKILL.md", mode)
            .unwrap_or_else(|error| panic!("Claude accepts a YAML allowed-tools list: {error}"));
    }
    assert!(parse_frontmatter(
        content,
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition
    )
    .is_err());

    let invalid = "---\nallowed-tools: [Read, 7]\n---\n";
    assert!(parse_frontmatter(
        invalid,
        "claude/SKILL.md",
        FrontmatterMode::ClaudeDefinition,
    )
    .is_err());
}

#[test]
fn frontmatter_metadata_block_and_flow_maps_require_string_keys_and_values() {
    for frontmatter in [
        "---\nname: example\ndescription: Valid.\nmetadata:\n  true: value\n---\n",
        "---\nname: example\ndescription: Valid.\nmetadata: {7: value}\n---\n",
        "---\nname: example\ndescription: Valid.\nmetadata:\n  key: 7\n---\n",
        "---\nname: example\ndescription: Valid.\nmetadata: {key: false}\n---\n",
        "---\nname: example\ndescription: Valid.\nmetadata:\n  ? [complex, key]\n  : value\n---\n",
    ] {
        let error = parse_frontmatter(
            frontmatter,
            "skills/example/SKILL.md",
            FrontmatterMode::LiveDefinition,
        )
        .expect_err("non-string metadata keys and values must fail");
        assert!(error.to_string().contains("metadata") || error.to_string().contains("keys"));
    }

    for frontmatter in [
        "---\nname: example\ndescription: Valid.\nmetadata:\n  key: value\n  \"true\": \"false\"\n---\n",
        "---\nname: example\ndescription: Valid.\nmetadata: {key: value, \"7\": \"true\"}\n---\n",
    ] {
        assert!(parse_frontmatter(
            frontmatter,
            "skills/example/SKILL.md",
            FrontmatterMode::LiveDefinition,
        )
        .is_ok());
    }
}

#[test]
fn agent_skill_name_constraints_and_required_fields_are_enforced() {
    for (field, content) in [
        (
            "name",
            "---\nname: Example\ndescription: Valid description.\n---\n",
        ),
        (
            "name",
            "---\nname: under_score\ndescription: Valid description.\n---\n",
        ),
        (
            "description",
            "---\nname: example\ndescription: \"\"\n---\n",
        ),
    ] {
        let error = parse_frontmatter(
            content,
            "skills/example/SKILL.md",
            FrontmatterMode::LiveDefinition,
        )
        .expect_err("invalid required frontmatter must fail");
        assert!(error.to_string().contains(field));
    }
}

#[test]
fn duplicate_frontmatter_keys_are_rejected() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\nname: duplicate\ndescription: Valid.\n---\n",
    );
    let (error, shape) = fixture.run_detect_failure();
    assert!(shape.detected.is_empty());
    assert!(error.to_string().contains("duplicate key"));
}

#[test]
fn frontmatter_yaml_syntax_diagnostics_use_original_file_lines() {
    let error = parse_frontmatter(
        "---\nname: example\ndescription: Valid.\nbroken: [\n---\n",
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition,
    )
    .expect_err("malformed frontmatter YAML must fail");
    assert!(error.to_string().contains("line 4"), "{error}");
}

#[test]
fn markdown_frontmatter_is_removed_only_when_it_is_a_yaml_mapping() {
    let with_frontmatter = "---\ntitle: guide\n---\n[link](target.md)\n";
    assert_eq!(markdown_body(with_frontmatter), ("[link](target.md)\n", 4));

    let thematic_breaks = "---\n# A Markdown heading\n---\n[link](target.md)\n";
    assert_eq!(markdown_body(thematic_breaks), (thematic_breaks, 1));
}

#[test]
fn bare_cr_frontmatter_is_parsed_and_original_link_line_is_preserved() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/references/broken.md",
        "---\rtitle: Bare CR\r---\r# Broken\r[missing](missing.md)\r",
    );
    let (error, _) = fixture.run_detect_failure();
    assert!(error.to_string().contains("references/broken.md:5"));

    let parsed = parse_frontmatter(
        "---\rname: example\rdescription: Bare CR frontmatter.\r---\r",
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition,
    )
    .unwrap_or_else(|error| panic!("bare CR lines are valid: {error}"));
    assert_eq!(parsed.values["name"], "example");
}

#[test]
fn indented_horizontal_rule_inside_frontmatter_block_scalar_is_not_a_delimiter() {
    let parsed = parse_frontmatter(
        "---\nname: example\ndescription: |\n  first line\n  ---\n  last line\n---\n",
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition,
    )
    .unwrap_or_else(|error| panic!("indented scalar delimiter is content: {error}"));
    assert_eq!(parsed.values["description"], "first line\n---\nlast line\n");
}

#[test]
fn compatibility_frontmatter_obeys_the_one_to_five_hundred_character_limit() {
    for compatibility in ["", "   "] {
        let content = format!(
            "---\nname: example\ndescription: Valid.\ncompatibility: \"{compatibility}\"\n---\n"
        );
        let error = parse_frontmatter(
            &content,
            "skills/example/SKILL.md",
            FrontmatterMode::LiveDefinition,
        )
        .expect_err("empty compatibility must fail");
        assert!(error.to_string().contains("compatibility"));
    }
    for length in [1, 500] {
        let compatibility = "x".repeat(length);
        let content = format!(
            "---\nname: example\ndescription: Valid.\ncompatibility: \"{compatibility}\"\n---\n"
        );
        assert!(parse_frontmatter(
            &content,
            "skills/example/SKILL.md",
            FrontmatterMode::LiveDefinition,
        )
        .is_ok());
    }
    let compatibility = "x".repeat(501);
    let content = format!(
        "---\nname: example\ndescription: Valid.\ncompatibility: \"{compatibility}\"\n---\n"
    );
    let error = parse_frontmatter(
        &content,
        "skills/example/SKILL.md",
        FrontmatterMode::LiveDefinition,
    )
    .expect_err("compatibility above 500 characters must fail");
    assert!(error.to_string().contains("compatibility"));
}

#[test]
fn commonmark_reference_escaped_angle_and_multiline_links_resolve() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: A valid Agent Skill.\n---\n\
         [reference][doc]\n\
         [a multiline\nlabel](./templates/multiline.md)\n\
         [escaped](./templates/diagram_\\(v1\\).md)\n\
         [angle](<./templates/angle.md>)\n\
         [percent](space%20name.md?view=full#section)\n\
         [external](https://example.invalid/path) and [anchor](#details)\n\
         [mail](mailto:skill@example.test)\n\n\
         [doc]:\n\
           <./templates/reference.md>\n\
           \"title\"\n\n\
         Inline `[ignored](missing-inline.md)` is code.\n\
         ~~~md\n\
         [ignored](missing-fenced.md)\n\
         ~~~\n",
    );
    for path in [
        "skills/example/templates/multiline.md",
        "skills/example/templates/diagram_(v1).md",
        "skills/example/templates/angle.md",
        "skills/example/templates/reference.md",
        "skills/example/space name.md",
    ] {
        fixture.add(path, "# Target\n");
    }

    let (hidden, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("CommonMark links resolve: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(hidden.contains("skills/example/templates/reference.md"));
}

#[test]
fn commonmark_email_autolinks_are_allowed_mail_uris() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: A valid Agent Skill.\n---\nContact <user@example.test>.\n",
    );

    fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("CommonMark email autolink is a safe mail URI: {error}"));
}

#[test]
fn raw_html_link_attributes_are_reported_as_unvalidated() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/references/html.md",
        "<a href=\"javascript:alert(1)\">unsafe link</a>\n",
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("raw HTML limitation is reported: {error}"));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("Raw HTML URL attributes") && limitation.contains("not link-validated")
    }));
}

#[test]
fn raw_html_svg_and_responsive_image_urls_are_reported_as_unvalidated() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/references/html.md",
        "<svg><use xlink:href=\"javascript:alert(1)\"/></svg>\n<img srcset=\"javascript:alert(1) 1x\">\n",
    );
    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("raw HTML URLs are reported as unvalidated: {error}"));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("Raw HTML URL attributes") && limitation.contains("not link-validated")
    }));
}

#[test]
fn mdx_content_including_markdown_and_jsx_links_is_unparsed_and_unvalidated() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/references/guide.mdx",
        "[unsafe markdown link](javascript:alert(1))\n<Link href=\"javascript:alert(1)\" />\n",
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("MDX is outside the parsed contract: {error}"));
    assert!(shape.limitations.iter().any(|limitation| {
        limitation.contains("MDX file content")
            && limitation.contains("not parsed or link-validated")
    }));
}

#[test]
fn markdown_frontmatter_links_keep_original_file_line_numbers() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/references/broken.md",
        "---\ntitle: Broken reference\n---\n# Broken\n[missing](missing.md)\n",
    );
    let (error, _) = fixture.run_detect_failure();
    assert!(error.to_string().contains("references/broken.md:5"));
}

#[test]
fn kimi_root_skill_fallback_does_not_walk_sibling_resources_or_mdx() {
    let mut fixture = Fixture::empty();
    fixture.add("kimi.plugin.json", r#"{"name":"root-skill"}"#);
    fixture.add("SKILL.md", &skill("root-skill"));
    fixture.add("references/unsafe.md", "[unsafe](javascript:alert(1))\n");
    fixture.add(
        "references/component.mdx",
        "<Link href=\"javascript:alert(1)\" />\n",
    );
    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("root-only fallback ignores sibling resources: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(!shape
        .limitations
        .iter()
        .any(|limitation| limitation.contains("MDX file content")));
}

#[test]
fn root_level_skill_links_to_repository_root_resolve() {
    let mut fixture = Fixture::empty();
    fixture.add("kimi.plugin.json", r#"{"name":"root-skill"}"#);
    fixture.add(
        "SKILL.md",
        "---\nname: root-skill\ndescription: Root skill.\n---\n[repo](.) [repo slash](./)\n",
    );

    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("root-relative links resolve to the repository: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
}

#[test]
fn case_variant_skill_filename_is_not_discovered() {
    let mut fixture = Fixture::empty();
    fixture.add(".claude-plugin/plugin.json", r#"{"name":"example-plugin"}"#);
    fixture.add("skills/example/skill.md", &skill("example"));
    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("case-variant filename is not a Claude Skill: {error}"));
    assert!(shape.detected.contains(&"skills-count:0".to_owned()));
}

#[test]
fn horizontal_rule_document_is_not_consumed_as_frontmatter() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/references/thematic.md",
        "---\n[missing](missing.md)\n---\n",
    );
    let (error, _) = fixture.run_detect_failure();
    assert!(error.to_string().contains("missing Markdown link"));
}

#[test]
fn reference_link_to_missing_target_is_rejected() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: Valid.\n---\n[missing][missing-doc]\n\n[missing-doc]: ./references/missing.md\n",
    );
    let (error, shape) = fixture.run_detect_failure();
    assert!(shape.detected.is_empty());
    assert!(error.to_string().contains("missing Markdown link"));
}

#[test]
fn markdown_links_must_stay_inside_the_declared_plugin_root() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".claude-plugin/marketplace.json",
        r#"{"name":"catalog","owner":{"name":"Example"},"plugins":[{"name":"local-tool","source":"./plugins/local-tool","strict":false}]}"#,
    );
    fixture.add(
        "plugins/local-tool/skills/example/SKILL.md",
        "---\nname: example\ndescription: Valid.\n---\n[outside plugin](../../../outside.md)\n",
    );
    fixture.add("plugins/outside.md", "# Outside the plugin source\n");
    let (error, _) = fixture.run_detect_failure();
    assert!(error
        .to_string()
        .contains("escapes plugin root plugins/local-tool"));
}

#[test]
fn skill_links_can_reference_sibling_resources_inside_the_plugin_root() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: Valid.\n---\n[sibling templates](../sibling/templates/)\n",
    );
    fixture.add("skills/sibling/templates/README.md", "# Shared templates\n");
    fixture.add(
        "skills/sibling/templates/package.json",
        r#"{"name":"shared-template","packageManager":"bun@1.2.3","scripts":{"test":"bun test"}}"#,
    );
    fixture.add("skills/sibling/templates/bun.lock", "{}\n");
    fixture.add(
        "skills/sibling/templates/index.ts",
        "export const template = true;\n",
    );

    let (hidden, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("plugin-root sibling resource is valid: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(hidden.contains("skills/sibling/templates/README.md"));
    assert!(hidden.contains("skills/sibling/templates/package.json"));
}

#[test]
fn link_to_a_sibling_skill_named_templates_does_not_hide_that_skill_or_its_code() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: Valid.\n---\n[templates skill](../templates/)\n",
    );
    fixture.add("skills/templates/SKILL.md", &skill("templates"));
    fixture.add(
        "skills/templates/package.json",
        r#"{"name":"template-tool","packageManager":"bun@1.2.3","scripts":{"test":"bun test"}}"#,
    );
    fixture.add("skills/templates/bun.lock", "{}\n");
    fixture.add("skills/templates/index.ts", "export const tool = true;\n");

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("linked templates-named skill stays discoverable: {error}"));
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));
    assert!(shape
        .units
        .iter()
        .any(|unit| unit.kind == UnitKind::Bun && unit.root == "skills/templates"));
}

#[test]
fn skill_link_to_unrelated_package_templates_does_not_hide_product_inputs() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: Valid.\n---\n[config](../../packages/app/templates/config.json)\n",
    );
    fixture.add("packages/app/templates/config.json", r#"{"name":"app"}"#);
    fixture.add(
        "packages/app/templates/src.ts",
        "export const appTemplate = true;\n",
    );

    let (hidden, _) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("linked package files remain valid references: {error}"));
    assert!(!hidden.contains("packages/app/templates/config.json"));
    assert!(!hidden.contains("packages/app/templates/src.ts"));
}

#[test]
fn root_skill_link_to_package_templates_keeps_them_visible_to_full_scan() {
    let mut fixture = Fixture::empty();
    fixture.add("kimi.plugin.json", r#"{"name":"root-skill-plugin"}"#);
    fixture.add(
        "SKILL.md",
        "---\nname: root-skill\ndescription: Root skill.\n---\n[config](packages/templates/config.json)\n",
    );
    fixture.add(
        "packages/templates/package.json",
        r#"{"name":"package","packageManager":"bun@1.2.3","scripts":{"test":"bun test"}}"#,
    );
    fixture.add("packages/templates/config.json", r#"{"app":true}"#);
    fixture.add("packages/templates/bun.lock", "{}\n");
    fixture.add("packages/templates/src.ts", "export const app = true;\n");

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("root skill support links scan cleanly: {error}"));
    assert!(shape
        .units
        .iter()
        .any(|unit| unit.kind == UnitKind::Bun && unit.root == "packages/templates"));
    assert!(shape
        .files
        .contains(&"packages/templates/package.json".to_owned()));
}

#[test]
fn direct_and_flat_skill_roots_cannot_claim_package_templates() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "kimi.plugin.json",
        r#"{"name":"kimi-plugin","skills":["./skills","./flat-skills"]}"#,
    );
    fixture.add(
        "skills/SKILL.md",
        "---\nname: direct-skill\ndescription: A direct root skill.\n---\n[config](../packages/templates/config.json)\n",
    );
    fixture.add(
        "flat-skills/guide.md",
        "# Guide\n[config](../packages/templates/config.json)\n",
    );
    fixture.add(
        "packages/templates/package.json",
        r#"{"name":"product","packageManager":"bun@1.2.3","scripts":{"test":"bun test"}}"#,
    );
    fixture.add("packages/templates/config.json", r#"{"app":true}"#);
    fixture.add("packages/templates/bun.lock", "{}\n");
    fixture.add("packages/templates/index.ts", "export const app = true;\n");

    let shape = fixture.run_scan().unwrap_or_else(|error| {
        panic!("direct and flat Skill links keep packages visible: {error}")
    });
    assert!(shape.detected.contains(&"skills-count:2".to_owned()));
    assert!(shape
        .units
        .iter()
        .any(|unit| unit.kind == UnitKind::Bun && unit.root == "packages/templates"));
}

#[test]
fn support_markdown_cannot_claim_sibling_templates_as_inert() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/references/guide.md",
        "[template examples](../../sibling/templates/)\n",
    );
    fixture.add("skills/sibling/templates/README.md", "# Shared templates\n");
    fixture.add(
        "skills/sibling/templates/package.json",
        r#"{"name":"sibling-package","packageManager":"bun@1.2.3","scripts":{"test":"bun test"}}"#,
    );
    fixture.add("skills/sibling/templates/bun.lock", "{}\n");
    fixture.add(
        "skills/sibling/templates/index.ts",
        "export const template = true;\n",
    );

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("support Markdown links are validated: {error}"));
    assert!(
        shape
            .units
            .iter()
            .any(|unit| { unit.kind == UnitKind::Bun && unit.root == "skills/sibling/templates" }),
        "only a skill or command definition can mark a linked directory inert"
    );
}

#[cfg(unix)]
#[test]
fn markdown_support_files_cannot_traverse_symlink_directories() {
    use std::os::unix::fs::symlink;

    let mut fixture = Fixture::claude_plugin();
    must(
        fs::create_dir_all(fixture.root.join("outside")),
        "create outside target directory",
    );
    must(
        fs::write(fixture.root.join("outside/target.md"), "# Outside\n"),
        "write outside target",
    );
    must(
        symlink("../../outside", fixture.root.join("skills/example/escape")),
        "create skill-root symlink",
    );
    fixture.add(
        "skills/example/references/guide.md",
        "[outside](../escape/target.md)\n",
    );
    fixture
        .files
        .insert("skills/example/escape/target.md".to_owned());

    let (error, _) = fixture.run_detect_failure();
    assert!(error.to_string().contains("symbolic link"));
}

#[test]
fn entity_escaped_traversal_and_unsafe_destinations_are_rejected() {
    for destination in [
        "../../../outside.md",
        "../../../&#46;&#46;/outside.md",
        "../../../%2e%2e/outside.md",
        "/absolute.md",
        "//host/path",
        "..\\outside.md",
        "bad%2.md",
        "topic<name>/README.md",
    ] {
        let mut fixture = Fixture::claude_plugin();
        fixture.add(
            "skills/example/SKILL.md",
            &format!("---\nname: example\ndescription: Valid.\n---\n[unsafe]({destination})\n"),
        );
        let (error, shape) = fixture.run_detect_failure();
        assert!(shape.detected.is_empty(), "unsafe link emitted evidence");
        assert!(
            error.to_string().contains("Markdown"),
            "{destination}: {error}"
        );
    }
}

#[test]
fn markdown_placeholder_links_skip_only_missing_file_checks() {
    let mut valid = Fixture::claude_plugin();
    valid.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: Valid.\n---\n[generated](references/%3Ctopic%3E/README.md)\n",
    );
    assert!(
        valid.run_detect().is_ok(),
        "a confined placeholder may be absent"
    );

    let mut escaping = Fixture::claude_plugin();
    escaping.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: Valid.\n---\n[generated](%3Ctopic%3E/../../../../other/SKILL.md)\n",
    );
    let (error, shape) = escaping.run_detect_failure();
    assert!(
        shape.detected.is_empty(),
        "unsafe placeholder emitted evidence"
    );
    assert!(
        error.to_string().contains("unsafe Markdown link"),
        "{error}"
    );
}

#[test]
fn unsupported_uri_schemes_fail_closed() {
    for destination in [
        "javascript:alert(1)",
        "vbscript:msgbox(1)",
        "file:///etc/passwd",
        "data:image/png;base64,AAAA",
        "custom+demo:opaque",
        "intent://example",
        "javas&#99;ript:alert(1)",
        "javascript%3Aalert(1)",
        "https%3A/../../outside.md",
    ] {
        let mut fixture = Fixture::claude_plugin();
        fixture.add(
            "skills/example/SKILL.md",
            &format!("---\nname: example\ndescription: Valid.\n---\n[link]({destination})\n"),
        );
        let (error, _) = fixture.run_detect_failure();
        assert!(
            error
                .to_string()
                .contains("unsupported or unsafe Markdown URI"),
            "{destination}: {error}"
        );
    }
}

#[test]
fn decoded_leading_whitespace_cannot_turn_a_script_uri_into_a_local_path() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/ javascript:alert(1)",
        "A decoy local file for the old path-resolution bypass.\n",
    );
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: Valid.\n---\n[link](&#32;javascript:alert(1))\n",
    );
    let (error, _) = fixture.run_detect_failure();
    assert!(error.to_string().contains("Markdown"));
}

#[test]
fn only_http_https_and_mailto_are_supported_external_uris() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: Valid.\n---\n[http](HTTP://example.test/a) [https](https://example.test/b) [email](mailto:skill@example.test)\n",
    );
    assert!(fixture.run_detect().is_ok());
}

#[test]
fn links_inside_gfm_tables_are_validated() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: Valid.\n---\n| target |\n| --- |\n| [unsafe](javascript:alert(1)) |\n",
    );
    let (error, _) = fixture.run_detect_failure();
    assert!(error
        .to_string()
        .contains("unsupported or unsafe Markdown URI"));
}

#[test]
fn bare_gfm_autolink_literals_are_reported_as_outside_the_parser_contract() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/SKILL.md",
        "---\nname: example\ndescription: Valid.\n---\nwww.example.test\n",
    );
    let (_, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("bare text is outside parsed link syntax: {error}"));
    assert!(shape
        .limitations
        .iter()
        .any(|limitation| { limitation.contains("bare GFM autolink literals are not parsed") }));
}

#[test]
fn markdown_destination_normalization_and_repository_resolution_are_stable() {
    assert_eq!(
        normalize_markdown_destination("foo%20bar.md?view=full#section"),
        Some("foo bar.md".to_owned())
    );
    assert!(normalize_markdown_destination("foo%2Fbar.md").is_none());
    assert!(normalize_markdown_destination("bad%2.md").is_none());
    assert!(normalize_markdown_destination(" javascript:alert(1)").is_none());
    assert_eq!(
        resolve_markdown_path("skills/example/SKILL.md", "references/policy.md"),
        Some("skills/example/references/policy.md".to_owned())
    );
    assert_eq!(
        resolve_markdown_path("skills/example/SKILL.md", "../../../outside.md"),
        None
    );
    assert!(is_markdown_placeholder("research/<topic>/README.md"));
    assert!(!is_markdown_placeholder("pure-rust-macos-ui/README.md"));
}

#[test]
fn template_paths_are_excluded_and_binary_assets_are_not_parsed_as_text() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/templates/package.json",
        r#"{"name":"template"}"#,
    );
    fixture.add("skills/example/templates/config.YAML", "name: template\n");
    fixture.add(
        "skills/example/templates/nested/SKILL.md",
        "---\nname: <skill-name>\ndescription: A template.\n---\n",
    );
    fixture.add_bytes(
        "skills/example/templates/images/icon.png",
        &[0x89, b'P', b'N', b'G', 0x00, 0xff],
    );
    fixture.add_bytes(
        "scripts/helper/templates/font.woff2",
        &[0x00, 0xff, 0x81, 0x00],
    );
    fixture.add(
        "skills/example/helpers/package.json",
        r#"{"name":"real-helper","packageManager":"bun@1.2.3"}"#,
    );

    let (hidden, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("template assets are safely classified: {error}"));
    assert!(hidden.contains("skills/example/templates/package.json"));
    assert!(hidden.contains("skills/example/templates/config.YAML"));
    assert!(hidden.contains("skills/example/templates/nested/SKILL.md"));
    assert!(hidden.contains("skills/example/templates/images/icon.png"));
    assert!(!hidden.contains("scripts/helper/templates/font.woff2"));
    assert!(!hidden.contains("skills/example/helpers/package.json"));
    assert!(shape.units.is_empty());
}

#[test]
fn supported_text_template_variants_are_validated_and_hidden() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/templates/SKILL.md",
        "---\nname: <skill-name>\ndescription: A skill template.\nmetadata:\n  hasSubSkill: true\n---\n# <skill-name>\n",
    );
    fixture.add(
        "skills/example/templates/config.json",
        r#"{"enabled":true}"#,
    );
    fixture.add("skills/example/templates/config.toml", "enabled = true\n");
    fixture.add("skills/example/templates/config.yaml", "enabled: true\n");

    let (hidden, _) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("supported template formats parse: {error}"));
    for path in [
        "skills/example/templates/SKILL.md",
        "skills/example/templates/config.json",
        "skills/example/templates/config.toml",
        "skills/example/templates/config.yaml",
    ] {
        assert!(hidden.contains(path), "{path} must remain inert");
    }
}

#[test]
fn explicitly_declared_templates_directory_can_be_a_skill_root() {
    let mut fixture = Fixture::empty();
    fixture.add(
        ".codex-plugin/plugin.json",
        r#"{"name":"custom-skill-root","skills":"./templates"}"#,
    );
    fixture.add("templates/example/SKILL.md", &skill("example"));

    let (hidden, shape) = fixture
        .run_detect()
        .unwrap_or_else(|error| panic!("declared templates path is a component root: {error}"));
    assert!(shape.detected.contains(&"skills-count:1".to_owned()));
    assert!(!hidden.contains("templates/example/SKILL.md"));
}

#[test]
fn invalid_supported_template_formats_fail_closed() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add("skills/example/templates/package.json", "{\n");
    let (error, shape) = fixture.run_detect_failure();
    assert!(shape.detected.is_empty());
    assert!(error.to_string().contains("invalid JSON template"));
}

#[test]
fn duplicate_yaml_template_keys_are_rejected() {
    let mut fixture = Fixture::claude_plugin();
    fixture.add(
        "skills/example/templates/config.yaml",
        "name: first\nname: second\n",
    );
    let (error, shape) = fixture.run_detect_failure();
    assert!(shape.detected.is_empty());
    assert!(error.to_string().contains("invalid YAML template"));
}

#[test]
fn full_scan_keeps_skill_templates_inert_and_unrelated_templates_visible() {
    let mut fixture = Fixture::claude_plugin();
    let helper = "skills/example/helpers";
    fixture.add(
        &format!("{helper}/package.json"),
        "{\"name\":\"helper\",\"packageManager\":\"bun@1.2.3\",\"scripts\":{\"check\":\"bun test\"}}",
    );
    fixture.add(&format!("{helper}/bun.lock"), "{}\n");
    fixture.add(
        &format!("{helper}/index.ts"),
        "export const helper = true;\n",
    );
    fixture.add(
        "skills/example/templates/package.json",
        r#"{"name":"template","packageManager":"bun@1.2.3"}"#,
    );
    fixture.add("skills/example/templates/bun.lock", "{}\n");
    fixture.add(
        "skills/tools/templates/package.json",
        r#"{"name":"independent-tool","packageManager":"bun@1.2.3","scripts":{"test":"bun test"}}"#,
    );
    fixture.add("skills/tools/templates/bun.lock", "{}\n");
    fixture.add(
        "skills/tools/templates/index.ts",
        "export const tool = true;\n",
    );
    fixture.add(
        "packages/foo/templates/package.json",
        r#"{"name":"monorepo-package","packageManager":"bun@1.2.3","scripts":{"test":"bun test"}}"#,
    );
    fixture.add("packages/foo/templates/bun.lock", "{}\n");
    fixture.add(
        "packages/foo/templates/index.ts",
        "export const packageTemplate = true;\n",
    );

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("scan real helpers and templates: {error}"));
    assert!(shape
        .units
        .iter()
        .any(|unit| { unit.kind == UnitKind::Bun && unit.root == "skills/example/helpers" }));
    assert!(shape
        .units
        .iter()
        .any(|unit| { unit.kind == UnitKind::Bun && unit.root == "packages/foo/templates" }));
    assert!(shape
        .units
        .iter()
        .any(|unit| { unit.kind == UnitKind::Bun && unit.root == "skills/tools/templates" }));
    assert!(!shape
        .units
        .iter()
        .any(|unit| { unit.root.starts_with("skills/example/templates") }));
    assert!(shape
        .files
        .contains(&"packages/foo/templates/package.json".to_owned()));
}

#[test]
fn ordinary_language_projects_keep_their_units_without_plugin_metadata() {
    let mut fixture = Fixture::empty();
    fixture.add(
        "Cargo.toml",
        "[package]\nname = \"control\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    fixture.add("rust-toolchain.toml", "[toolchain]\nchannel = \"stable\"\n");
    fixture.add("src/lib.rs", "pub fn control() {}\n");
    fixture.add(
        "package.json",
        r#"{"name":"control","packageManager":"bun@1.2.3","scripts":{"test":"bun test"}}"#,
    );
    fixture.add("bun.lock", "{}\n");

    let shape = fixture
        .run_scan()
        .unwrap_or_else(|error| panic!("scan ordinary language projects: {error}"));
    assert!(shape.units.iter().any(|unit| unit.kind == UnitKind::Rust));
    assert!(shape.units.iter().any(|unit| unit.kind == UnitKind::Bun));
    assert!(!shape.detected.iter().any(|item| item == "skills-plugin"));
}
