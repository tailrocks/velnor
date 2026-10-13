//! Typed identity and execution boundary for one visible CI check.
use serde::{Deserialize, Serialize};
use std::{
    borrow::Borrow,
    fmt,
    ops::{Deref, DerefMut},
};

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CheckKind {
    Format,
    Clippy,
    Test,
    Doctest,
    TestBuild,
    #[default]
    Auxiliary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CheckContract {
    RustTests,
    RustTestsAndDoctests,
    RustCompile,
    Auxiliary,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CargoOperation {
    Format,
    Clippy,
    Nextest,
    Test,
    Doctest,
    Check,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CargoBackend {
    Cargo,
    Mbx,
}

/// Shell text cannot introduce a second Cargo operation: every executable
/// token comes from this recipe, and repository values are shell-quoted.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CargoRecipe {
    pub operation: CargoOperation,
    pub root: String,
    pub package: Option<String>,
    pub workspace: bool,
    pub locked: bool,
    pub all_features: bool,
    pub backend: CargoBackend,
}
impl CargoRecipe {
    fn identity(&self) -> (CheckKind, &'static str) {
        match self.operation {
            CargoOperation::Format => (CheckKind::Format, "Formatting"),
            CargoOperation::Clippy => (CheckKind::Clippy, "Clippy"),
            CargoOperation::Nextest => (CheckKind::Test, "Nextest"),
            CargoOperation::Test => (CheckKind::Test, "Cargo test"),
            CargoOperation::Doctest => (CheckKind::Doctest, "Doctests"),
            CargoOperation::Check => (CheckKind::TestBuild, "Compile production defaults"),
        }
    }
    fn render(&self) -> String {
        let prefix = crate::shell_change_dir(&self.root);
        let tool = if self.backend == CargoBackend::Mbx {
            "mbx"
        } else {
            "cargo"
        };
        let lock = if self.locked { "--locked" } else { "" };
        let features = if self.all_features {
            "--all-features "
        } else {
            ""
        };
        let selector = if self.workspace {
            "--workspace".to_owned()
        } else {
            self.package.as_ref().map_or_else(
                || "--manifest-path 'Cargo.toml'".to_owned(),
                |package| format!("--package {}", crate::shell_quote(package)),
            )
        };
        match self.operation {
            CargoOperation::Format if self.workspace => {
                format!("{prefix}{tool} fmt --all -- --check")
            }
            CargoOperation::Format => {
                format!("{prefix}{tool} fmt --manifest-path 'Cargo.toml' -- --check")
            }
            CargoOperation::Clippy => {
                let no_deps = if self.backend == CargoBackend::Mbx {
                    ""
                } else {
                    " --no-deps"
                };
                format!("{prefix}{tool} clippy {lock} --profile test{no_deps} --all-targets {features}{selector} -- -D warnings")
            }
            CargoOperation::Nextest => {
                format!("{prefix}{tool} nextest run {lock} {features}{selector} --no-tests pass")
            }
            CargoOperation::Test => format!("{prefix}{tool} test {lock} {features}{selector}"),
            CargoOperation::Doctest => {
                format!("{prefix}{tool} test {lock} --doc {features}{selector}")
            }
            CargoOperation::Check => {
                format!("{prefix}{tool} check {selector} --all-targets {lock}")
            }
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(from = "WireCommand", into = "WireCommand")]
pub(crate) struct CheckCommand {
    pub kind: CheckKind,
    pub name: String,
    pub run: String,
    cargo: Option<CargoRecipe>,
}

impl CheckCommand {
    pub(crate) fn new(kind: CheckKind, name: impl Into<String>, run: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
            run: run.into(),
            cargo: None,
        }
    }
    pub(crate) fn cargo(recipe: CargoRecipe) -> Self {
        let (kind, name) = recipe.identity();
        Self {
            kind,
            name: name.to_owned(),
            run: recipe.render(),
            cargo: Some(recipe),
        }
    }
    pub(crate) fn with_mbx(&self, fallback: impl Fn(&str) -> String) -> Self {
        if let Some(recipe) = &self.cargo {
            let mut recipe = recipe.clone();
            recipe.backend = CargoBackend::Mbx;
            Self::cargo(recipe)
        } else {
            self.map_run(fallback)
        }
    }
    pub(crate) fn map_run(&self, map: impl Fn(&str) -> String) -> Self {
        Self {
            run: map(&self.run),
            ..self.clone()
        }
    }
}
impl From<String> for CheckCommand {
    fn from(run: String) -> Self {
        Self::new(CheckKind::Auxiliary, "Check", run)
    }
}
impl From<&str> for CheckCommand {
    fn from(run: &str) -> Self {
        run.to_owned().into()
    }
}
impl Deref for CheckCommand {
    type Target = String;
    fn deref(&self) -> &String {
        &self.run
    }
}
impl Borrow<str> for CheckCommand {
    fn borrow(&self) -> &str {
        &self.run
    }
}
impl fmt::Display for CheckCommand {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.run)
    }
}
impl PartialEq<String> for CheckCommand {
    fn eq(&self, other: &String) -> bool {
        self.run == *other
    }
}
impl PartialEq<&str> for CheckCommand {
    fn eq(&self, other: &&str) -> bool {
        self.run == *other
    }
}

pub(crate) fn write_commands(output: &mut String, name: &str, commands: &[CheckCommand]) {
    use fmt::Write;
    fn inline(value: serde_json::Value) -> String {
        match value {
            serde_json::Value::Object(fields) => format!(
                "{{ {} }}",
                fields
                    .into_iter()
                    .filter(|(_, value)| !value.is_null())
                    .map(|(name, value)| format!("{name} = {}", inline(value)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            value => value.to_string(),
        }
    }
    let _ = writeln!(output, "{name} = [");
    for command in commands {
        let value = serde_json::to_value(command).unwrap_or_default();
        let _ = writeln!(output, "  {},", inline(value));
    }
    output.push_str("]\n");
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
enum WireCommand {
    Cargo(CargoRecipe),
    Shell { name: String, run: String },
}
impl From<WireCommand> for CheckCommand {
    fn from(command: WireCommand) -> Self {
        match command {
            WireCommand::Cargo(recipe) => Self::cargo(recipe),
            WireCommand::Shell { name, run } => Self::new(CheckKind::Auxiliary, name, run),
        }
    }
}
impl From<CheckCommand> for WireCommand {
    fn from(command: CheckCommand) -> Self {
        if let Some(recipe) = command.cargo {
            Self::Cargo(recipe)
        } else {
            Self::Shell {
                name: command.name,
                run: command.run,
            }
        }
    }
}

impl DerefMut for CheckCommand {
    fn deref_mut(&mut self) -> &mut String {
        &mut self.run
    }
}
impl PartialEq<CheckCommand> for String {
    fn eq(&self, other: &CheckCommand) -> bool {
        *self == other.run
    }
}

/// Reject a missing or reordered Rust gate before a workflow can be emitted.
pub(crate) fn validate(contract: CheckContract, commands: &[CheckCommand]) -> Result<(), String> {
    let mut format = false;
    let mut clippy = false;
    for command in commands {
        if let Some(recipe) = &command.cargo {
            if recipe.identity() != (command.kind, command.name.as_str())
                || recipe.render() != command.run
            {
                return Err("Cargo check differs from its typed recipe".to_owned());
            }
        } else if command.kind != CheckKind::Auxiliary {
            return Err("Rust gates require a typed Cargo recipe".to_owned());
        }
        if command.name.trim().is_empty() || command.run.trim().is_empty() {
            return Err("every check needs a name and command".to_owned());
        }
        match command.kind {
            CheckKind::Format if clippy => return Err("formatting must precede Clippy".to_owned()),
            CheckKind::Format => format = true,
            CheckKind::Clippy if !format => {
                return Err("Clippy requires a preceding formatting gate".to_owned())
            }
            CheckKind::Clippy => clippy = true,
            CheckKind::Test | CheckKind::Doctest | CheckKind::TestBuild if !format || !clippy => {
                return Err("Rust tests require preceding formatting and Clippy gates".to_owned());
            }
            _ => {}
        }
    }
    if contract != CheckContract::Auxiliary && (!format || !clippy) {
        return Err("Rust verification requires formatting and Clippy gates".to_owned());
    }
    let has = |kind| commands.iter().any(|command| command.kind == kind);
    let coverage_present = match contract {
        CheckContract::RustTests => has(CheckKind::Test),
        CheckContract::RustTestsAndDoctests => has(CheckKind::Test) && has(CheckKind::Doctest),
        CheckContract::RustCompile => has(CheckKind::TestBuild),
        CheckContract::Auxiliary => true,
    };
    if !coverage_present {
        return Err("declared Rust verification coverage is missing".to_owned());
    }
    Ok(())
}

/// The production-default workspace contract deliberately omits all-features.
pub(crate) fn workspace_checks() -> Vec<CheckCommand> {
    [
        CargoOperation::Format,
        CargoOperation::Clippy,
        CargoOperation::Check,
    ]
    .into_iter()
    .map(|operation| {
        CheckCommand::cargo(CargoRecipe {
            operation,
            root: ".".to_owned(),
            package: None,
            workspace: true,
            locked: true,
            all_features: false,
            backend: CargoBackend::Cargo,
        })
    })
    .collect()
}

/// One scope/provider/unit path in a reusable workflow.
pub(crate) struct CheckPath<'a> {
    pub commands: &'a [CheckCommand],
}

/// Each step selects exactly one command. GitHub's success condition enforces
/// failure propagation; the caller cannot hide the gates in an aggregate run.
/// Nonsecret selection metadata belongs to the job. Tool/cache restrictions and
/// credentials stay scoped to check steps.
pub(crate) fn insert_job_environment(output: &mut String, environment: &str) {
    if let Some(position) = output.rfind("    steps:\n") {
        use fmt::Write;
        let mut values = String::new();
        for line in environment.lines() {
            let _ = writeln!(values, "      {}", line.trim_start());
        }
        output.insert_str(position, &format!("    env:\n{values}"));
    }
}

fn scoped_check_environment(environment: &str) -> String {
    let values = environment
        .lines()
        .filter(|line| {
            ![
                "MISE_AUTO_INSTALL:",
                "MISE_EXEC_AUTO_INSTALL:",
                "MISE_NOT_FOUND_AUTO_INSTALL:",
            ]
            .iter()
            .any(|key| line.trim_start().starts_with(key))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if values.trim().is_empty() {
        String::new()
    } else {
        format!("        env:\n{values}\n")
    }
}

pub(crate) fn render_steps(
    paths: &[CheckPath<'_>],
    environment: &str,
    prelude: &str,
    started: &str,
    ended: &str,
) -> String {
    use fmt::Write;
    use std::collections::BTreeSet;
    let environment = scoped_check_environment(environment);
    let mut output = format!("      - name: Start unit checks\n        id: unit-checks\n{environment}        run: |\n{started}\n          velnor-workflow validate-unit --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit \"$CI_UNIT_ID\"\n");
    let mut steps = BTreeSet::new();
    for path in paths {
        for (index, command) in path.commands.iter().enumerate() {
            steps.insert((index, command.kind, command.name.clone()));
        }
    }
    for (index, kind, name) in steps {
        let identity = step_identity(kind, &name).replace('\'', "''");
        let guard = serde_json::to_string(&format!(
            "success() && steps.unit-checks.outputs.check_{index} == '{identity}'"
        ))
        .unwrap_or_default();
        let name = if name == "Check" {
            format!("Check {}", index + 1)
        } else {
            name
        };
        let name = serde_json::to_string(&name).unwrap_or_default();
        let _ = writeln!(output, "      - name: {name}\n        if: {guard}\n{environment}        run: |\n          set -o pipefail\n{prelude}          velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit \"$CI_UNIT_ID\" --check-index {index} 2>&1 | tee -a \"$RUNNER_TEMP/velnor-unit-log.txt\"");
    }
    let _ = writeln!(
        output,
        "      - name: Finish unit checks\n        if: always()\n        run: |\n{ended}"
    );
    output
}

impl PartialEq<str> for CheckCommand {
    fn eq(&self, other: &str) -> bool {
        self.run == other
    }
}

fn step_identity(kind: CheckKind, name: &str) -> String {
    serde_json::to_string(&(kind, name)).unwrap_or_default()
}

pub(crate) fn step_outputs(
    commands: &[CheckCommand],
    contract: CheckContract,
    full: bool,
) -> Result<String, String> {
    use fmt::Write;
    validate(contract, commands)?;
    let mut output = String::new();
    for (index, command) in commands.iter().enumerate() {
        if full
            || contract == CheckContract::Auxiliary
            || matches!(command.kind, CheckKind::Format | CheckKind::Clippy)
        {
            let _ = writeln!(
                output,
                "check_{index}={}",
                step_identity(command.kind, &command.name)
            );
        }
    }
    Ok(output)
}

pub(crate) fn commands_for_execution(
    commands: &[CheckCommand],
    contract: CheckContract,
    full: bool,
    index: Option<usize>,
) -> Result<Vec<CheckCommand>, String> {
    validate(contract, commands)?;
    let selected = index
        .map(|index| {
            commands
                .get(index)
                .ok_or_else(|| format!("no check at index {index}"))
        })
        .transpose()?;
    Ok(selected
        .map_or_else(|| commands.to_vec(), |command| vec![command.clone()])
        .into_iter()
        .filter(|command| {
            full || contract == CheckContract::Auxiliary
                || matches!(command.kind, CheckKind::Format | CheckKind::Clippy)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    fn rust_checks() -> Vec<CheckCommand> {
        [
            CargoOperation::Format,
            CargoOperation::Clippy,
            CargoOperation::Nextest,
            CargoOperation::Doctest,
        ]
        .into_iter()
        .map(|operation| {
            CheckCommand::cargo(CargoRecipe {
                operation,
                root: ".".to_owned(),
                package: Some("fixture".to_owned()),
                workspace: false,
                locked: true,
                all_features: true,
                backend: CargoBackend::Cargo,
            })
        })
        .collect()
    }

    #[test]
    fn typed_checks_reject_missing_and_reordered_gate_mutations() {
        let checks = rust_checks();
        assert!(validate(CheckContract::RustTestsAndDoctests, &checks).is_ok());
        for omitted in [0, 1] {
            let mut mutated = checks.clone();
            mutated.remove(omitted);
            assert!(validate(CheckContract::RustTestsAndDoctests, &mutated).is_err());
        }
        let mut combined = checks.clone();
        combined[0].run.push_str(" && cargo clippy && cargo test");
        assert!(validate(CheckContract::RustTestsAndDoctests, &combined).is_err());
        let stripped = checks
            .iter()
            .map(|command| CheckCommand::from(command.run.clone()))
            .collect::<Vec<_>>();
        assert!(validate(CheckContract::RustTestsAndDoctests, &stripped).is_err());
        assert_eq!(
            commands_for_execution(&stripped, CheckContract::Auxiliary, false, None),
            Ok(stripped.clone())
        );
        let legacy: Result<CheckCommand, _> = serde_json::from_str("\"cargo test\"");
        assert!(legacy.is_err());
        let wire = serde_json::to_value(&checks[0]).unwrap_or_default();
        let mut extra_body = wire.clone();
        extra_body["run"] = serde_json::Value::String("cargo fmt && cargo test".to_owned());
        assert!(serde_json::from_value::<CheckCommand>(extra_body).is_err());
        let mut extra_role = wire;
        extra_role["kind"] = serde_json::Value::String("auxiliary".to_owned());
        assert!(serde_json::from_value::<CheckCommand>(extra_role).is_err());

        for index in 0..checks.len() {
            let mut stripped_one = checks.clone();
            stripped_one[index] = CheckCommand::from(stripped_one[index].run.clone());
            assert!(validate(CheckContract::RustTestsAndDoctests, &stripped_one).is_err());
        }
        let mut reordered = checks.clone();
        reordered.swap(1, 2);
        assert!(validate(CheckContract::RustTestsAndDoctests, &reordered).is_err());
        assert_eq!(
            commands_for_execution(&checks, CheckContract::RustTestsAndDoctests, false, None),
            Ok(checks[..2].to_vec())
        );
        assert_eq!(
            commands_for_execution(&checks, CheckContract::RustTestsAndDoctests, true, Some(2)),
            Ok(vec![checks[2].clone()])
        );
        assert_eq!(
            commands_for_execution(&checks, CheckContract::RustTestsAndDoctests, false, Some(2)),
            Ok(Vec::new())
        );
        assert!(commands_for_execution(
            &checks,
            CheckContract::RustTestsAndDoctests,
            true,
            Some(99)
        )
        .is_err());
    }

    fn rendered_checks(checks: &[CheckCommand]) -> Result<serde_yaml::Value, Box<dyn Error>> {
        let rendered = render_steps(
            &[CheckPath { commands: checks }],
            "          CI_SCOPE: affected\n          CI_UNIT_ID: rust-fixture",
            "",
            "          true",
            "          true",
        );
        Ok(serde_yaml::from_str(&format!("steps:\n{rendered}"))?)
    }

    /// Check the emitted step graph, including guards and one-command boundaries.
    fn assert_graph(value: &serde_yaml::Value, commands: &[CheckCommand]) -> Result<(), String> {
        let steps = value["steps"].as_sequence().ok_or("steps missing")?;
        let checks = steps
            .iter()
            .filter(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.contains("--check-index"))
            })
            .collect::<Vec<_>>();
        if checks.len() != commands.len() {
            return Err("missing or combined check step".to_owned());
        }
        for (index, (step, command)) in checks.iter().zip(commands).enumerate() {
            let guard = step["if"].as_str().ok_or("missing success edge")?;
            if !(guard == "success()" || guard.starts_with("success() && "))
                || guard.contains("always()")
            {
                return Err("success edge removed".to_owned());
            }
            if step["name"].as_str() != Some(command.name.as_str()) {
                return Err("check identity changed".to_owned());
            }
            let run = step["run"].as_str().ok_or("missing run")?;
            if run.matches("velnor-workflow run ").count() != 1
                || run.matches("--check-index").count() != 1
                || !run.contains(&format!("--check-index {index} 2>&1"))
            {
                return Err("combined or reordered command".to_owned());
            }
        }
        Ok(())
    }

    #[test]
    fn admission_outputs_match_visible_slots_and_omit_inapplicable_tests(
    ) -> Result<(), Box<dyn Error>> {
        let checks = rust_checks();
        let full = step_outputs(&checks, CheckContract::RustTestsAndDoctests, true)?;
        let prerequisites = step_outputs(&checks, CheckContract::RustTestsAndDoctests, false)?;
        let rendered = rendered_checks(&checks)?;
        for (index, command) in checks.iter().enumerate() {
            let identity = step_identity(command.kind, &command.name);
            assert!(full.contains(&format!("check_{index}={identity}\n")));
            assert_eq!(
                prerequisites.contains(&format!("check_{index}=")),
                index < 2
            );
            assert_eq!(
                rendered["steps"][index + 1]["if"].as_str(),
                Some(
                    format!("success() && steps.unit-checks.outputs.check_{index} == '{identity}'")
                        .as_str()
                )
            );
        }
        let mut missing = checks;
        missing.remove(0);
        assert!(step_outputs(&missing, CheckContract::RustTestsAndDoctests, true).is_err());
        Ok(())
    }

    #[test]
    fn visible_step_graph_rejects_combination_removal_and_skip_mutations(
    ) -> Result<(), Box<dyn Error>> {
        let commands = rust_checks();
        let good = rendered_checks(&commands)?;
        assert!(assert_graph(&good, &commands).is_ok());
        let mut missing = good.clone();
        if let Some(steps) = missing["steps"].as_sequence_mut() {
            steps.remove(1);
        }
        assert!(assert_graph(&missing, &commands).is_err());
        let mut unguarded = good.clone();
        unguarded["steps"][2]["if"] = serde_yaml::Value::String("always()".to_owned());
        assert!(assert_graph(&unguarded, &commands).is_err());
        let mut combined = good.clone();
        let first = combined["steps"][1]["run"]
            .as_str()
            .ok_or("format body")?
            .to_owned();
        let second = combined["steps"][2]["run"]
            .as_str()
            .ok_or("clippy body")?
            .to_owned();
        combined["steps"][1]["run"] = serde_yaml::Value::String(format!("{first}\n{second}"));
        assert!(assert_graph(&combined, &commands).is_err());
        Ok(())
    }

    #[test]
    fn scope_variants_keep_command_identity_and_single_purpose_wrappers(
    ) -> Result<(), Box<dyn Error>> {
        #[derive(Deserialize)]
        struct Plan {
            commands: Vec<CheckCommand>,
        }
        let checks = rust_checks();
        let wrapped = checks
            .iter()
            .map(|command| command.with_mbx(str::to_owned))
            .collect::<Vec<_>>();
        for (before, after) in checks.iter().zip(&wrapped) {
            assert_eq!(before.kind, after.kind);
            assert_eq!(before.name, after.name);
        }
        let mut wire = String::new();
        write_commands(&mut wire, "commands", &wrapped);
        let roundtrip: Plan = toml::from_str(&wire)?;
        assert_eq!(roundtrip.commands, wrapped);
        assert!(validate(CheckContract::RustTestsAndDoctests, &roundtrip.commands).is_ok());
        assert!(!workspace_checks()
            .iter()
            .any(|command| command.contains("--all-features")));
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn typed_cargo_checks_reject_real_format_and_clippy_defects_without_hooks(
    ) -> Result<(), Box<dyn Error>> {
        use std::{
            fs,
            process::Command,
            time::{SystemTime, UNIX_EPOCH},
        };
        for (case, source, failed_phase) in [
            ("format", "pub fn invalid( ){ }\n", 0),
            (
                "clippy",
                "pub fn invalid(value: &str) -> bool {\n    value.len() == 0\n}\n",
                1,
            ),
        ] {
            let root = std::env::temp_dir().join(format!(
                "velnor-ci-defect-{case}-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ));
            fs::create_dir_all(root.join("src"))?;
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = 'fixture'\nversion = '0.1.0'\nedition = '2024'\n[workspace]\n",
            )?;
            fs::write(root.join("src/lib.rs"), source)?;
            let checks = [
                CargoOperation::Format,
                CargoOperation::Clippy,
                CargoOperation::Test,
            ]
            .into_iter()
            .map(|operation| {
                CheckCommand::cargo(CargoRecipe {
                    operation,
                    root: ".".to_owned(),
                    package: Some("fixture".to_owned()),
                    workspace: false,
                    locked: false,
                    all_features: false,
                    backend: CargoBackend::Cargo,
                })
            })
            .collect::<Vec<_>>();
            assert!(validate(CheckContract::RustTests, &checks).is_ok());
            let mut failure = None;
            for (index, check) in checks.iter().enumerate() {
                let output = Command::new("bash")
                    .args(["-euo", "pipefail", "-c", &check.run])
                    .current_dir(&root)
                    .env("CARGO_TARGET_DIR", root.join("target"))
                    .env("CARGO_HOME", root.join("cargo-home"))
                    .env("CARGO_NET_OFFLINE", "true")
                    .env_remove("RUSTC_WRAPPER")
                    .env_remove("RUSTC_WORKSPACE_WRAPPER")
                    .output()?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    assert!(
                        if case == "format" {
                            stdout.contains("Diff in")
                        } else {
                            stderr.contains("len_zero")
                        },
                        "unexpected failure: {stdout}\n{stderr}"
                    );
                    failure = Some(index);
                    break;
                }
            }
            assert_eq!(
                failure,
                Some(failed_phase),
                "CI must reject the defect before tests run"
            );
            fs::remove_dir_all(root)?;
        }
        Ok(())
    }
}
