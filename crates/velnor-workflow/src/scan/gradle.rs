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
use crate::{
    shell_change_dir, CachePurpose, CacheSpec, GeneratorError, Unit, UnitKind, UnitService,
};

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
    let workspace_index = shape.units.len();
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

    let mut pending_sources = Vec::new();
    for (_module, module_root, mut module_unit) in pending {
        let mut dependencies = BTreeSet::new();
        let mut sources = String::new();
        for build_file in module_build_files(context.file_set, &module_root) {
            let source = read_repo_file(context.root, &build_file)?;
            sources.push_str(&source);
            sources.push('\n');
            for dep in parse_project_deps(&source, &module_ids) {
                if dep != module_unit.id {
                    dependencies.insert(dep);
                }
            }
        }
        module_unit.depends_on = dependencies.into_iter().collect();
        pending_sources.push((module_unit, sources));
    }
    let workspace_sources = pending_sources
        .iter()
        .map(|(_, sources)| sources.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for (mut module_unit, sources) in pending_sources {
        if let Some(service) = postgres_service(&sources, &workspace_sources) {
            module_unit.services.push(service);
            prepend_schema_tasks(&mut module_unit);
        }
        shape.units.push(module_unit);
    }
    if let Some(service) = shape.units[workspace_index + 1..]
        .iter()
        .find_map(|module| module.services.first().cloned())
    {
        shape.units[workspace_index].services.push(service);
        prepend_schema_tasks(&mut shape.units[workspace_index]);
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum GradleToken {
    Identifier(String),
    String(String),
    OpenParen,
    CloseParen,
    Comma,
    Dot,
    Other(char),
}

/// Tokenize only the syntax needed for dependency discovery. Comments,
/// malformed strings, and malformed block comments terminate the useful
/// region instead of becoming accidental project edges.
fn gradle_tokens(source: &str) -> Vec<GradleToken> {
    let chars = source.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            ch if ch.is_whitespace() => index += 1,
            '/' if chars.get(index + 1) == Some(&'/') => {
                index += 2;
                while chars.get(index).is_some_and(|ch| *ch != '\n') {
                    index += 1;
                }
            }
            '/' if chars.get(index + 1) == Some(&'*') => {
                index += 2;
                while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                    index += 1;
                }
                if index + 1 >= chars.len() {
                    break;
                }
                index += 2;
            }
            '"' if chars.get(index + 1) == Some(&'"') && chars.get(index + 2) == Some(&'"') => {
                index += 3;
                while index + 2 < chars.len()
                    && !(chars[index] == '"' && chars[index + 1] == '"' && chars[index + 2] == '"')
                {
                    index += 1;
                }
                if index + 2 >= chars.len() {
                    break;
                }
                index += 3;
            }
            '"' | '\'' => {
                let quote = chars[index];
                index += 1;
                let mut value = String::new();
                let mut closed = false;
                while index < chars.len() {
                    match chars[index] {
                        '\\' if index + 1 < chars.len() => {
                            value.push(chars[index + 1]);
                            index += 2;
                        }
                        ch if ch == quote => {
                            index += 1;
                            closed = true;
                            break;
                        }
                        ch => {
                            value.push(ch);
                            index += 1;
                        }
                    }
                }
                if !closed {
                    break;
                }
                tokens.push(GradleToken::String(value));
            }
            ch if ch.is_ascii_alphabetic() || ch == '_' => {
                let start = index;
                index += 1;
                while chars
                    .get(index)
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                {
                    index += 1;
                }
                tokens.push(GradleToken::Identifier(
                    chars[start..index].iter().collect(),
                ));
            }
            '(' => {
                tokens.push(GradleToken::OpenParen);
                index += 1;
            }
            ')' => {
                tokens.push(GradleToken::CloseParen);
                index += 1;
            }
            ',' => {
                tokens.push(GradleToken::Comma);
                index += 1;
            }
            '.' => {
                tokens.push(GradleToken::Dot);
                index += 1;
            }
            ch => {
                tokens.push(GradleToken::Other(ch));
                index += 1;
            }
        }
    }
    tokens
}

fn project_path(value: &str) -> Option<String> {
    let value = value.trim_start_matches(':');
    (!value.is_empty() && !value.chars().any(char::is_whitespace)).then(|| value.to_owned())
}

fn parse_include_call(tokens: &[GradleToken], start: usize) -> Option<(Vec<String>, usize)> {
    let parenthesized = matches!(tokens.get(start), Some(GradleToken::OpenParen));
    let mut index = start + usize::from(parenthesized);
    let mut modules = Vec::new();
    let mut expect_value = true;
    loop {
        match tokens.get(index) {
            Some(GradleToken::String(value)) if expect_value => {
                modules.push(project_path(value)?);
                expect_value = false;
                index += 1;
            }
            Some(GradleToken::Comma) if !expect_value => {
                expect_value = true;
                index += 1;
            }
            Some(GradleToken::CloseParen) if parenthesized => {
                return Some((modules, index + 1));
            }
            _ if !parenthesized && !expect_value => return Some((modules, index)),
            _ => return None,
        }
    }
}

fn parse_include_modules(source: &str) -> Vec<String> {
    let tokens = gradle_tokens(source);
    let mut modules = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if is_unqualified_identifier(&tokens, index, "include")
            && !matches!(tokens.get(index + 1), Some(GradleToken::Identifier(_)))
            && let Some((found, next)) = parse_include_call(&tokens, index + 1)
        {
            for module in found {
                if !modules.contains(&module) {
                    modules.push(module);
                }
            }
            index = next;
            continue;
        }
        index += 1;
    }
    modules
}

fn project_call_path(tokens: &[GradleToken], start: usize) -> Option<String> {
    if !matches!(tokens.get(start), Some(GradleToken::OpenParen)) {
        return None;
    }
    match (
        tokens.get(start + 1),
        tokens.get(start + 2),
        tokens.get(start + 3),
    ) {
        (Some(GradleToken::String(value)), Some(GradleToken::CloseParen), _) => project_path(value),
        (
            Some(GradleToken::Identifier(name)),
            Some(GradleToken::Other('=' | ':')),
            Some(GradleToken::String(value)),
        ) if name == "path" && matches!(tokens.get(start + 4), Some(GradleToken::CloseParen)) => {
            project_path(value)
        }
        _ => None,
    }
}

fn parse_project_deps(source: &str, modules: &BTreeMap<String, String>) -> BTreeSet<String> {
    let tokens = gradle_tokens(source);
    let mut deps = BTreeSet::new();
    for index in 0..tokens.len() {
        if is_unqualified_identifier(&tokens, index, "project")
            && let Some(name) = project_call_path(&tokens, index + 1)
            && let Some(id) = modules.get(&name)
        {
            deps.insert(id.clone());
        }
        if is_unqualified_identifier(&tokens, index, "projects")
            && matches!(
                (tokens.get(index + 1), tokens.get(index + 2)),
                (Some(GradleToken::Dot), Some(GradleToken::Identifier(_)))
            )
            && let GradleToken::Identifier(accessor) = &tokens[index + 2]
            && let Some(id) = modules
                .iter()
                .find(|(name, _)| typesafe_accessor(name) == *accessor)
                .map(|(_, id)| id)
        {
            deps.insert(id.clone());
        }
    }
    deps
}

fn is_unqualified_identifier(tokens: &[GradleToken], index: usize, expected: &str) -> bool {
    matches!(
        tokens.get(index),
        Some(GradleToken::Identifier(name)) if name == expected
    ) && !index
        .checked_sub(1)
        .and_then(|previous| tokens.get(previous))
        .is_some_and(|token| matches!(token, GradleToken::Dot))
}

fn gradle_needs_live_postgres(source: &str) -> bool {
    source.contains("jooqCodegen")
        || source.contains("plugins.jooq")
        || source.contains("plugins.flyway")
        || source.contains("\nflyway {")
}

fn quoted_assignment(source: &str, name: &str) -> Option<String> {
    let needle = format!("{name} = \"");
    let start = source.find(&needle)? + needle.len();
    let end = source[start..].find('"')?;
    Some(source[start..start + end].to_owned())
}

fn jdbc_ident(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn jdbc_databases(source: &str) -> BTreeSet<String> {
    let mut databases = BTreeSet::new();
    let mut rest = source;
    while let Some(start) = rest.find("jdbc:postgresql://") {
        rest = &rest[start + "jdbc:postgresql://".len()..];
        let Some(slash) = rest.find('/') else {
            break;
        };
        rest = &rest[slash + 1..];
        let database = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .collect::<String>();
        if jdbc_ident(&database)
            && database != "postgres"
            && database != "template0"
            && database != "template1"
        {
            databases.insert(database);
        }
    }
    databases
}

fn jdbc_port_and_database(source: &str) -> (u16, String) {
    let default = (40000, "postgres".to_owned());
    let Some(start) = source.find("jdbc:postgresql://") else {
        return default;
    };
    let rest = &source[start + "jdbc:postgresql://".len()..];
    let Some(slash) = rest.find('/') else {
        return default;
    };
    let hostport = &rest[..slash];
    let port = hostport.rsplit_once(':').map_or("40000", |(_, port)| port);
    let port = port
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(40000);
    let database = rest[slash + 1..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .collect::<String>();
    (
        port,
        if database.is_empty() {
            "postgres".to_owned()
        } else {
            database
        },
    )
}

fn postgres_health_cmd(user: &str, primary: &str, extra: &BTreeSet<String>) -> String {
    let extra = extra
        .iter()
        .filter(|database| *database != primary && jdbc_ident(database) && jdbc_ident(user))
        .cloned()
        .collect::<Vec<_>>();
    if extra.is_empty() || !jdbc_ident(user) || !jdbc_ident(primary) {
        return format!(
            "--health-cmd \"pg_isready -U {user} -d {primary}\" --health-interval 10s --health-timeout 5s --health-retries 10"
        );
    }
    let list = extra.join(" ");
    format!(
        "--health-cmd \"sh -c 'for db in {list}; do createdb -U {user} $db 2>/dev/null || true; done; pg_isready -U {user} -d {primary}'\" --health-interval 10s --health-timeout 5s --health-retries 10"
    )
}

fn postgres_service(unit_source: &str, workspace_source: &str) -> Option<UnitService> {
    if !gradle_needs_live_postgres(unit_source) {
        return None;
    }
    let catalog = if workspace_source.is_empty() {
        unit_source
    } else {
        workspace_source
    };
    let (port, database) = jdbc_port_and_database(unit_source);
    let database = if database == "postgres" {
        jdbc_port_and_database(catalog).1
    } else {
        database
    };
    let user = quoted_assignment(unit_source, "datasourceUsername")
        .or_else(|| quoted_assignment(unit_source, "user"))
        .or_else(|| quoted_assignment(catalog, "datasourceUsername"))
        .or_else(|| quoted_assignment(catalog, "user"))
        .unwrap_or_else(|| "postgres".to_owned());
    let password = quoted_assignment(unit_source, "datasourcePassword")
        .or_else(|| quoted_assignment(unit_source, "password"))
        .or_else(|| quoted_assignment(catalog, "datasourcePassword"))
        .or_else(|| quoted_assignment(catalog, "password"))
        .unwrap_or_else(|| "postgres".to_owned());
    let extra = jdbc_databases(catalog);
    Some(UnitService {
        name: "postgres".to_owned(),
        image: "postgres:18-alpine".to_owned(),
        env: vec![
            ("POSTGRES_USER".to_owned(), user.clone()),
            ("POSTGRES_PASSWORD".to_owned(), password),
            ("POSTGRES_DB".to_owned(), database.clone()),
        ],
        ports: vec![format!("{port}:5432")],
        options: postgres_health_cmd(&user, &database, &extra),
    })
}

fn prepend_schema_tasks(unit: &mut Unit) {
    for commands in [&mut unit.pr_commands, &mut unit.full_commands] {
        for command in commands.iter_mut() {
            if command.contains("flywayMigrate") {
                continue;
            }
            if let Some(index) = command.find("./gradlew ") {
                command.insert_str(
                    index + "./gradlew ".len(),
                    "--no-parallel flywayMigrate jooqCodegen ",
                );
            } else if let Some(index) = command.find("gradle ") {
                command.insert_str(
                    index + "gradle ".len(),
                    "--no-parallel flywayMigrate jooqCodegen ",
                );
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::{
        jdbc_databases, parse_include_modules, parse_project_deps, postgres_health_cmd,
        postgres_service, typesafe_accessor,
    };
    use std::collections::{BTreeMap, BTreeSet};

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
    fn include_ignores_comments_and_unrelated_strings() {
        let source = r#"
// include(":comment")
val text = "include(\":string\")"
/* include(":block") */
include(":real", ":another",)
"#;
        assert_eq!(parse_include_modules(source), vec!["real", "another"]);
    }

    #[test]
    fn qualified_include_is_not_a_settings_module_declaration() {
        let source = r#"
settings.include(":not-a-module")
include(":real")
"#;
        assert_eq!(parse_include_modules(source), vec!["real"]);
    }

    #[test]
    fn malformed_include_is_ignored() {
        assert!(parse_include_modules("include(\":unfinished\"").is_empty());
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
        let root =
            std::env::temp_dir().join(format!("velnor-gradle-scan-{}", crate::unique_suffix()));
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
    #[expect(clippy::unwrap_used, reason = "fixture setup fails the test")]
    fn jooq_module_gets_postgres_service_and_schema_tasks() {
        use super::super::scan_shape;
        use crate::RunnerMode;
        use std::fs;
        let root =
            std::env::temp_dir().join(format!("velnor-gradle-jooq-{}", crate::unique_suffix()));
        fs::create_dir_all(root.join("domain")).unwrap();
        fs::write(root.join("settings.gradle.kts"), "include(\"domain\")\n").unwrap();
        fs::write(root.join("gradlew"), "#!/bin/sh\n").unwrap();
        fs::write(root.join("build.gradle.kts"), "tasks {}\n").unwrap();
        fs::write(
            root.join("domain/build.gradle.kts"),
            r#"
plugins { alias(libs.plugins.jooq.codegen) }
val datasourceUsername = "exampledb"
val datasourcePassword = "exampledb"
val datasourceUrl = "jdbc:postgresql://${System.getenv("POSTGRESQL_DB_HOST") ?: "127.0.0.1"}:40000/exampledb"
jooqCodegen(libs.postgresql)
"#,
        )
        .unwrap();
        let shape = scan_shape(&root, RunnerMode::Velnor, "main", &[]).unwrap();
        let domain = shape
            .units
            .iter()
            .find(|unit| unit.id == "gradle-domain")
            .unwrap();
        assert_eq!(domain.services.len(), 1);
        assert_eq!(domain.services[0].name, "postgres");
        assert_eq!(domain.services[0].ports, vec!["40000:5432".to_owned()]);
        assert!(domain
            .pr_commands
            .iter()
            .any(|command| command.contains("flywayMigrate jooqCodegen")
                && command.contains(":domain:check")));
        let workspace = shape.units.iter().find(|unit| unit.id == "gradle").unwrap();
        assert_eq!(workspace.services.len(), 1);
        assert!(workspace
            .pr_commands
            .iter()
            .any(|command| command.contains("flywayMigrate jooqCodegen")
                && command.contains(" check ")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn jdbc_databases_collects_every_named_catalog() {
        let source = r#"
val datasourceUrl = "jdbc:postgresql://${System.getenv("POSTGRESQL_DB_HOST") ?: "127.0.0.1"}:40000/exampledb"
val legacyUrl = "jdbc:postgresql://127.0.0.1:40000/legacy"
val monitorUrl = "jdbc:postgresql://127.0.0.1:40000/transfer_monitor"
"#;
        let databases = jdbc_databases(source);
        assert!(databases.contains("exampledb"));
        assert!(databases.contains("legacy"));
        assert!(databases.contains("transfer_monitor"));
    }

    #[test]
    #[expect(clippy::expect_used, reason = "fixture setup fails the test")]
    fn postgres_service_health_creates_workspace_jdbc_databases() {
        let domain = r#"
plugins { alias(libs.plugins.jooq.codegen) }
val datasourceUsername = "exampledb"
val datasourcePassword = "exampledb"
val datasourceUrl = "jdbc:postgresql://${System.getenv("POSTGRESQL_DB_HOST") ?: "127.0.0.1"}:40000/exampledb"
jooqCodegen(libs.postgresql)
"#;
        let flyway = r#"
plugins { alias(libs.plugins.flyway) }
val datasourceUrl = "jdbc:postgresql://127.0.0.1:40000/legacy"
flyway { url = datasourceUrl }
"#;
        let workspace = format!("{domain}\n{flyway}");
        let service = postgres_service(domain, &workspace).expect("jooq module needs postgres");
        assert_eq!(
            service
                .env
                .iter()
                .find(|(name, _)| name == "POSTGRES_DB")
                .map(|(_, value)| value.as_str()),
            Some("exampledb")
        );
        assert!(
            service.options.contains("createdb -U exampledb $db"),
            "unqualified flywayMigrate needs every JDBC catalog: {}",
            service.options
        );
        assert!(
            service.options.contains("legacy"),
            "legacy catalog missing from health-cmd: {}",
            service.options
        );
        assert!(
            postgres_health_cmd("exampledb", "exampledb", &jdbc_databases(&workspace))
                .contains("legacy")
        );
    }

    #[test]
    #[expect(clippy::unwrap_used, reason = "fixture setup fails the test")]
    fn workspace_postgres_service_creates_sibling_flyway_catalogs() {
        use super::super::scan_shape;
        use crate::RunnerMode;
        use std::fs;
        let root =
            std::env::temp_dir().join(format!("velnor-gradle-multidb-{}", crate::unique_suffix()));
        fs::create_dir_all(root.join("domain")).unwrap();
        fs::create_dir_all(root.join("legacy-flyway")).unwrap();
        fs::write(
            root.join("settings.gradle.kts"),
            "include(\"domain\", \"legacy-flyway\")\n",
        )
        .unwrap();
        fs::write(root.join("gradlew"), "#!/bin/sh\n").unwrap();
        fs::write(root.join("build.gradle.kts"), "tasks {}\n").unwrap();
        fs::write(
            root.join("domain/build.gradle.kts"),
            r#"
plugins { alias(libs.plugins.jooq.codegen) }
val datasourceUsername = "exampledb"
val datasourcePassword = "exampledb"
val datasourceUrl = "jdbc:postgresql://${System.getenv("POSTGRESQL_DB_HOST") ?: "127.0.0.1"}:40000/exampledb"
jooqCodegen(libs.postgresql)
"#,
        )
        .unwrap();
        fs::write(
            root.join("legacy-flyway/build.gradle.kts"),
            r#"
plugins { alias(libs.plugins.flyway) }
val datasourceUrl = "jdbc:postgresql://127.0.0.1:40000/legacy"
flyway { url = datasourceUrl }
"#,
        )
        .unwrap();
        let shape = scan_shape(&root, RunnerMode::Velnor, "main", &[]).unwrap();
        let domain = shape
            .units
            .iter()
            .find(|unit| unit.id == "gradle-domain")
            .unwrap();
        assert!(
            domain.services[0].options.contains("legacy"),
            "domain flywayMigrate also migrates sibling catalogs: {}",
            domain.services[0].options
        );
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

    #[test]
    fn project_deps_ignore_comments_and_unrelated_strings() {
        let mut modules = BTreeMap::new();
        modules.insert("real".to_owned(), "gradle-real".to_owned());
        let source = r#"
// implementation(project(":real"))
val text = "project(\":real\") projects.real"
/* projects.real */
root.project(":real")
root.projects.real
implementation(project(":real"))
"#;
        assert_eq!(
            parse_project_deps(source, &modules),
            BTreeSet::from(["gradle-real".to_owned()])
        );
    }

    #[test]
    fn malformed_project_is_ignored() {
        let mut modules = BTreeMap::new();
        modules.insert("real".to_owned(), "gradle-real".to_owned());
        assert!(
            parse_project_deps("implementation(project(\":real\"))", &modules)
                .contains("gradle-real")
        );
        assert!(parse_project_deps("implementation(project(\":real\"", &modules).is_empty());
    }
}
