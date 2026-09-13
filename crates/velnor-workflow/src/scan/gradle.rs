//! Gradle detector: one verification unit per included module, plus one root
//! unit per `settings.gradle(.kts)` workspace.
//!
//! Nested `build.gradle(.kts)` files inside a settings workspace are modules,
//! not independent Gradle roots. Module commands run through the workspace
//! wrapper as `./gradlew :<path>:check`. `depends_on` is recovered from
//! `project(":…")` and typesafe `projects.fooBar` accessors. A `build.gradle`
//! that is not under a settings workspace stays a standalone root.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::file_walk::{is_test_support_path, join_repo_path, path_prefix};
use super::{unit, RepositoryShape, ScanContext};
use crate::{shell_change_dir, CachePurpose, CacheSpec, GeneratorError, UnitKind};

pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<(), GeneratorError> {
    let settings_roots = settings_roots(context.file_set);
    let mut covered = BTreeSet::new();
    for settings_root in &settings_roots {
        detect_workspace(context, shape, settings_root, &mut covered)?;
    }
    detect_standalone_roots(context, shape, &covered);
    Ok(())
}

fn settings_roots(file_set: &BTreeSet<String>) -> Vec<String> {
    let mut roots = BTreeSet::new();
    for file in file_set {
        if is_test_support_path(file) {
            continue;
        }
        if matches!(
            file.rsplit('/').next(),
            Some("settings.gradle" | "settings.gradle.kts")
        ) {
            roots.insert(parent_of(file));
        }
    }
    roots.into_iter().collect()
}

fn detect_workspace(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
    settings_root: &str,
    covered: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    let settings_path = settings_file(context.file_set, settings_root);
    let settings_source = read_repo_file(context.root, &settings_path)?;
    let modules = parse_include_modules(&settings_source);
    let gradle = gradle_command(context.file_set, settings_root);
    let command_prefix = shell_change_dir(settings_root);
    shape.units.push(unit(
        UnitKind::Gradle,
        settings_root,
        workspace_watch(settings_root),
        vec![format!("{command_prefix}{gradle} check --no-daemon")],
        Some(workspace_cache(settings_root)),
    ));
    covered.insert(settings_root.to_owned());

    let mut module_ids = BTreeMap::new();
    let mut pending = Vec::new();
    for module in &modules {
        let project_path = module.replace(':', "/");
        let module_root = join_repo_path(settings_root, &project_path);
        covered.insert(module_root.clone());
        let gradle_path = format!(":{module}");
        let module_unit = unit(
            UnitKind::Gradle,
            &module_root,
            module_watch(&module_root),
            vec![format!(
                "{command_prefix}{gradle} {gradle_path}:check --no-daemon"
            )],
            Some(workspace_cache(settings_root)),
        );
        module_ids.insert(module.clone(), module_unit.id.clone());
        pending.push((module.clone(), module_root, module_unit));
    }

    for (_module, module_root, mut module_unit) in pending {
        let mut dependencies = BTreeSet::new();
        for build_file in module_build_files(context.file_set, &module_root) {
            let source = read_repo_file(context.root, &build_file)?;
            for dep in parse_project_deps(&source, &module_ids) {
                if dep != module_unit.id {
                    dependencies.insert(dep);
                }
            }
        }
        module_unit.depends_on = dependencies.into_iter().collect();
        shape.units.push(module_unit);
    }
    Ok(())
}

fn detect_standalone_roots(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
    covered: &BTreeSet<String>,
) {
    let mut standalone = BTreeSet::new();
    for file in context.file_set {
        if is_test_support_path(file) {
            continue;
        }
        if matches!(
            file.rsplit('/').next(),
            Some("build.gradle" | "build.gradle.kts")
        ) {
            let root = parent_of(file);
            if covered.iter().any(|settings| is_under(&root, settings)) {
                continue;
            }
            standalone.insert(root);
        }
    }
    for root in standalone {
        let prefix = path_prefix(&root);
        let command_prefix = shell_change_dir(&root);
        let gradle = gradle_command(context.file_set, &root);
        shape.units.push(unit(
            UnitKind::Gradle,
            &root,
            vec![
                format!("{prefix}**/*.gradle"),
                format!("{prefix}**/*.gradle.kts"),
                format!("{prefix}gradle/**"),
                format!("{prefix}src/**"),
            ],
            vec![format!("{command_prefix}{gradle} check --no-daemon")],
            Some(workspace_cache(&root)),
        ));
    }
}

fn workspace_watch(root: &str) -> Vec<String> {
    let prefix = path_prefix(root);
    vec![
        format!("{prefix}settings.gradle"),
        format!("{prefix}settings.gradle.kts"),
        format!("{prefix}build.gradle"),
        format!("{prefix}build.gradle.kts"),
        format!("{prefix}gradlew"),
        format!("{prefix}gradlew.bat"),
        format!("{prefix}gradle/**"),
    ]
}

fn module_watch(root: &str) -> Vec<String> {
    let prefix = path_prefix(root);
    vec![
        format!("{prefix}**/*.gradle"),
        format!("{prefix}**/*.gradle.kts"),
        format!("{prefix}src/**"),
    ]
}

fn workspace_cache(root: &str) -> CacheSpec {
    CacheSpec {
        key_files: vec![
            join_repo_path(root, "gradle/wrapper/gradle-wrapper.properties"),
            join_repo_path(root, "gradle/libs.versions.toml"),
        ],
        paths: vec![
            "~/.gradle/caches".to_owned(),
            "~/.gradle/wrapper".to_owned(),
        ],
        purpose: CachePurpose::Generic,
        mbx_output_cache_justification: None,
        mutable_mount_seed: false,
    }
}

fn gradle_command(file_set: &BTreeSet<String>, root: &str) -> &'static str {
    if file_set.contains(&join_repo_path(root, "gradlew")) {
        "./gradlew"
    } else {
        "gradle"
    }
}

fn settings_file(file_set: &BTreeSet<String>, root: &str) -> String {
    let kts = join_repo_path(root, "settings.gradle.kts");
    if file_set.contains(&kts) {
        kts
    } else {
        join_repo_path(root, "settings.gradle")
    }
}

fn module_build_files(file_set: &BTreeSet<String>, module_root: &str) -> Vec<String> {
    ["build.gradle.kts", "build.gradle"]
        .into_iter()
        .map(|name| join_repo_path(module_root, name))
        .filter(|path| file_set.contains(path))
        .collect()
}

fn read_repo_file(root: &Path, relative: &str) -> Result<String, GeneratorError> {
    let path = root.join(relative);
    fs::read_to_string(&path).map_err(|error| GeneratorError::io("read Gradle file", &path, &error))
}

fn parent_of(file: &str) -> String {
    match file.rsplit_once('/') {
        Some((parent, _)) => parent.to_owned(),
        None => ".".to_owned(),
    }
}

fn is_under(path: &str, root: &str) -> bool {
    path == root || path.starts_with(&format!("{root}/"))
}

fn parse_include_modules(source: &str) -> Vec<String> {
    let mut modules = Vec::new();
    let mut rest = source;
    while let Some(idx) = rest.find("include") {
        let before = &rest[..idx];
        let after = &rest[idx + 7..];
        let boundary = before
            .chars()
            .next_back()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
        if !boundary || after.starts_with("Build") {
            rest = after;
            continue;
        }
        let trimmed = after.trim_start();
        let body = if let Some(stripped) = trimmed.strip_prefix('(') {
            let Some((body, consumed)) = extract_until_matching_paren(stripped) else {
                rest = after;
                continue;
            };
            rest = &trimmed[consumed + 1..];
            body
        } else {
            let line = trimmed.lines().next().unwrap_or("");
            rest = &trimmed[line.len()..];
            line
        };
        for name in quoted_strings(body) {
            let name = name.trim_start_matches(':');
            if !name.is_empty() && !modules.iter().any(|existing| existing == name) {
                modules.push(name.to_owned());
            }
        }
    }
    modules
}

fn extract_until_matching_paren(source: &str) -> Option<(&str, usize)> {
    let mut depth = 1_i32;
    for (idx, ch) in source.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&source[..idx], idx + 1));
                }
            }
            _ => {}
        }
    }
    None
}

fn quoted_strings(source: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut chars = source.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch != '"' && ch != '\'' {
            continue;
        }
        let quote = ch;
        let mut value = String::new();
        for (_, next) in chars.by_ref() {
            if next == quote {
                values.push(value);
                break;
            }
            value.push(next);
        }
    }
    values
}

fn parse_project_deps(source: &str, modules: &BTreeMap<String, String>) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    for name in quoted_strings(source) {
        let name = name.trim_start_matches(':');
        if let Some(id) = modules.get(name) {
            deps.insert(id.clone());
        }
    }
    for (name, id) in modules {
        let accessor = typesafe_accessor(name);
        let needle = format!("projects.{accessor}");
        if contains_identifier(source, &needle) {
            deps.insert(id.clone());
        }
    }
    deps
}

fn typesafe_accessor(module: &str) -> String {
    let mut out = String::new();
    let mut capitalize = false;
    for ch in module.chars() {
        if ch == '-' || ch == '_' || ch == ':' {
            capitalize = true;
            continue;
        }
        if capitalize {
            for upper in ch.to_uppercase() {
                out.push(upper);
            }
            capitalize = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn contains_identifier(source: &str, needle: &str) -> bool {
    let mut rest = source;
    while let Some(idx) = rest.find(needle) {
        let after = rest.get(idx + needle.len()..).unwrap_or("");
        let next = after.chars().next();
        if next.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_') {
            return true;
        }
        rest = after;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{parse_include_modules, parse_project_deps, typesafe_accessor};
    use std::collections::BTreeMap;

    #[test]
    fn include_parses_multiline_kotlin_settings() {
        let source = r#"
include(
    "bitcoin-domain",
    "bitcoin-flyway",
    "crypto-utils",
)
"#;
        assert_eq!(
            parse_include_modules(source),
            vec!["bitcoin-domain", "bitcoin-flyway", "crypto-utils"]
        );
    }

    #[test]
    fn include_ignores_include_build() {
        let source = r#"
includeBuild("build-logic")
include("app")
"#;
        assert_eq!(parse_include_modules(source), vec!["app"]);
    }

    #[test]
    fn typesafe_accessors_match_kebab_case_modules() {
        assert_eq!(typesafe_accessor("bitcoin-domain"), "bitcoinDomain");
        assert_eq!(typesafe_accessor("bitcoin-flyway"), "bitcoinFlyway");
        assert_eq!(typesafe_accessor("crypto-utils"), "cryptoUtils");
        assert_eq!(
            typesafe_accessor("tailrocks-jooq-utils"),
            "tailrocksJooqUtils"
        );
    }

    #[test]
    #[expect(clippy::unwrap_used, reason = "fixture setup fails the test")]
    fn workspace_scan_emits_wrapper_module_commands_and_depends_on() {
        use super::super::scan_shape;
        use crate::RunnerMode;
        use std::fs;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "velnor-gradle-scan-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("app/src")).unwrap();
        fs::create_dir_all(root.join("lib/src")).unwrap();
        fs::write(
            root.join("settings.gradle.kts"),
            "include(\"lib\", \"app\")\n",
        )
        .unwrap();
        fs::write(root.join("gradlew"), "#!/bin/sh\n").unwrap();
        fs::write(root.join("build.gradle.kts"), "tasks {}\n").unwrap();
        fs::write(
            root.join("lib/build.gradle.kts"),
            "plugins { `java-library` }\n",
        )
        .unwrap();
        fs::write(
            root.join("app/build.gradle.kts"),
            "dependencies { implementation(projects.lib) }\n",
        )
        .unwrap();
        let shape = scan_shape(&root, RunnerMode::Velnor, "main", &[]).unwrap();
        let app = shape
            .units
            .iter()
            .find(|unit| unit.id == "gradle-app")
            .unwrap();
        let lib = shape
            .units
            .iter()
            .find(|unit| unit.id == "gradle-lib")
            .unwrap();
        let workspace = shape.units.iter().find(|unit| unit.id == "gradle").unwrap();
        assert!(workspace
            .pr_commands
            .iter()
            .any(|command| command.contains("./gradlew check")));
        assert!(lib
            .pr_commands
            .iter()
            .any(|command| command.contains("./gradlew :lib:check")));
        assert!(app
            .pr_commands
            .iter()
            .any(|command| command.contains("./gradlew :app:check")));
        assert!(app.depends_on.contains(&"gradle-lib".to_owned()));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_deps_read_typesafe_and_colon_forms() {
        let mut modules = BTreeMap::new();
        modules.insert(
            "bitcoin-domain".to_owned(),
            "gradle-backend-bitcoin-domain".to_owned(),
        );
        modules.insert(
            "crypto-utils".to_owned(),
            "gradle-backend-crypto-utils".to_owned(),
        );
        let source = r#"
dependencies {
    api(projects.bitcoinDomain)
    implementation(projects.cryptoUtils)
    testImplementation(project(":bitcoin-domain"))
}
"#;
        let deps = parse_project_deps(source, &modules);
        assert!(deps.contains("gradle-backend-bitcoin-domain"));
        assert!(deps.contains("gradle-backend-crypto-utils"));
    }
}
