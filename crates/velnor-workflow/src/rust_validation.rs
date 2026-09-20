//! Typed Rust validation phases shared by schema 1, schema 2, and the runtime.
//!
//! Commands stay in their existing shell form so scanned working directories,
//! Cargo arguments, and Cargo/Mr. Boxington wrappers remain byte-for-byte the
//! same. The phase and runner metadata are explicit; callers never infer
//! policy or tool setup from command spelling.

use serde::{Deserialize, Serialize};

pub(crate) const RUST_VALIDATION_CONTRACT_VERSION: u32 = 1;

/// The individual validation operation attached to a command.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RustPhase {
    Format,
    Clippy,
    Tests,
    Doctests,
    Compile,
    Custom,
}

impl RustPhase {
    /// Canonical command/render order. Compile is used for compile-only plans;
    /// test plans use Tests and, where needed, Doctests instead.
    pub(crate) const ORDER: [Self; 6] = [
        Self::Format,
        Self::Clippy,
        Self::Compile,
        Self::Tests,
        Self::Doctests,
        Self::Custom,
    ];

    pub(crate) const fn slug(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::Clippy => "clippy",
            Self::Tests => "tests",
            Self::Doctests => "doctests",
            Self::Compile => "compile",
            Self::Custom => "custom",
        }
    }

    pub(crate) const fn step_name(self) -> &'static str {
        match self {
            Self::Format => "Rust formatting check",
            Self::Clippy => "Rust Clippy check",
            Self::Tests => "Rust tests",
            Self::Doctests => "Rust doctests",
            Self::Compile => "Rust compilation check",
            Self::Custom => "Rust custom checks",
        }
    }
}

/// Why this unit has a Rust command plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RustWorkload {
    /// A discovered crate that must pass format, Clippy, and tests.
    Crate,
    /// Workspace or prerequisite compilation, preceded by format and Clippy.
    CompileOnly,
    /// Dependency policy tools such as `cargo deny`; these are custom checks,
    /// not crate validation and must not acquire fabricated format/test phases.
    DependencyPolicy,
    /// An explicitly generic repository-provided Rust check.
    Custom,
}

/// The runner selected from Rust scan/config metadata, never from command text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RustTestRunner {
    CargoTest,
    Nextest,
}

/// Which runtime command family supplies the phase set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RustProvider {
    Github,
    Velnor,
}

/// Which runtime command scope supplies the phase set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RustScope {
    Affected,
    Full,
}

/// One command explicitly assigned to one validation phase.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RustPhaseCommand {
    pub(crate) phase: RustPhase,
    /// Preserves the scanner's exact command, including target/features,
    /// working-directory prefix, environment wrapper, and Cargo backend.
    pub(crate) command: String,
}

/// Commands and runner policy for one affected/full selection.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RustPhaseSet {
    pub(crate) workload: RustWorkload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) test_runner: Option<RustTestRunner>,
    #[serde(default)]
    pub(crate) commands: Vec<RustPhaseCommand>,
}

impl RustPhaseSet {
    pub(crate) fn new(
        workload: RustWorkload,
        test_runner: Option<RustTestRunner>,
        commands: Vec<RustPhaseCommand>,
    ) -> Self {
        Self {
            workload,
            test_runner,
            commands,
        }
    }

    /// Treat old/custom command arrays as generic custom checks. Their text
    /// cannot cause tool installation or phase reassignment.
    pub(crate) fn custom(commands: impl IntoIterator<Item = String>) -> Self {
        Self::new(
            RustWorkload::Custom,
            None,
            commands
                .into_iter()
                .map(|command| RustPhaseCommand {
                    phase: RustPhase::Custom,
                    command,
                })
                .collect(),
        )
    }

    pub(crate) fn compile_only(
        format: String,
        clippy: String,
        compile: String,
        custom: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut commands = vec![
            RustPhaseCommand {
                phase: RustPhase::Format,
                command: format,
            },
            RustPhaseCommand {
                phase: RustPhase::Clippy,
                command: clippy,
            },
            RustPhaseCommand {
                phase: RustPhase::Compile,
                command: compile,
            },
        ];
        commands.extend(custom.into_iter().map(|command| RustPhaseCommand {
            phase: RustPhase::Custom,
            command,
        }));
        Self::new(RustWorkload::CompileOnly, None, commands)
    }

    /// Build the canonical scanned-crate phases from manifest/workspace facts.
    /// Callers supply already-quoted selectors and the safe working-directory
    /// prefix produced by the scanner's path helpers.
    pub(crate) fn crate_validation(
        command_prefix: &str,
        fmt_manifest_path: &str,
        cargo_lock_flag: &str,
        package_selector: &str,
        test_runner: RustTestRunner,
        has_doctestable_library: bool,
    ) -> Self {
        let mut commands = vec![
            RustPhaseCommand {
                phase: RustPhase::Format,
                command: format!(
                    "{command_prefix}cargo fmt --manifest-path {fmt_manifest_path} -- --check"
                ),
            },
            RustPhaseCommand {
                phase: RustPhase::Clippy,
                command: format!(
                    "{command_prefix}cargo clippy {cargo_lock_flag} --profile test --no-deps --all-targets --all-features {package_selector} -- -D warnings"
                ),
            },
            RustPhaseCommand {
                phase: RustPhase::Compile,
                command: format!(
                    "{command_prefix}cargo check {cargo_lock_flag} --profile test --all-targets --all-features {package_selector}"
                ),
            },
            RustPhaseCommand {
                phase: RustPhase::Tests,
                command: match test_runner {
                    RustTestRunner::CargoTest => format!(
                        "{command_prefix}cargo test {cargo_lock_flag} --all-features {package_selector}"
                    ),
                    RustTestRunner::Nextest => format!(
                        "{command_prefix}cargo nextest run {cargo_lock_flag} --all-features {package_selector} --no-tests pass"
                    ),
                },
            },
        ];
        if test_runner == RustTestRunner::Nextest && has_doctestable_library {
            commands.push(RustPhaseCommand {
                phase: RustPhase::Doctests,
                command: format!(
                    "{command_prefix}cargo test {cargo_lock_flag} --all-features {package_selector} --doc"
                ),
            });
        }
        Self::new(RustWorkload::Crate, Some(test_runner), commands)
    }

    /// Canonical workspace-check phases. They keep the workspace check's
    /// default features/profile and `--locked` compile intent while placing
    /// formatting and warning-denying Clippy before compilation.
    pub(crate) fn workspace_compile_only(
        custom: impl IntoIterator<Item = String>,
    ) -> Self {
        Self::compile_only(
            "cargo fmt --all -- --check".to_owned(),
            "cargo clippy --workspace --all-targets --locked -- -D warnings".to_owned(),
            "cargo check --workspace --all-targets --locked".to_owned(),
            custom,
        )
    }

    pub(crate) fn commands_for(
        &self,
        phase: RustPhase,
    ) -> impl Iterator<Item = &str> + '_ {
        self.commands
            .iter()
            .filter(move |command| command.phase == phase)
            .map(|command| command.command.as_str())
    }

    pub(crate) fn has_phase(&self, phase: RustPhase) -> bool {
        self.commands.iter().any(|command| command.phase == phase)
    }

    /// Return only populated phases, always in canonical order.
    pub(crate) fn phases(&self) -> Vec<RustPhase> {
        RustPhase::ORDER
            .into_iter()
            .filter(|phase| self.has_phase(*phase))
            .collect()
    }

    /// Phases that execute for the unit's ordinary full validation path.
    /// A crate plan carries an auxiliary Compile command for prerequisite
    /// selection; a full crate runs tests instead.
    pub(crate) fn full_phases(&self) -> Vec<RustPhase> {
        match self.workload {
            RustWorkload::Crate => [
                RustPhase::Format,
                RustPhase::Clippy,
                RustPhase::Tests,
                RustPhase::Doctests,
                RustPhase::Custom,
            ]
            .into_iter()
            .filter(|phase| self.has_phase(*phase))
            .collect(),
            RustWorkload::CompileOnly => self.phases(),
            RustWorkload::DependencyPolicy | RustWorkload::Custom => {
                self.phases()
            }
        }
    }

    /// Phases that execute when a selected crate is needed only as a
    /// dependency. Custom repository checks belong to the directly selected
    /// unit and do not run as a side effect of pulling it in as a prerequisite.
    pub(crate) fn prerequisite_phases(&self) -> Vec<RustPhase> {
        match self.workload {
            RustWorkload::Crate | RustWorkload::CompileOnly => [
                RustPhase::Format,
                RustPhase::Clippy,
                RustPhase::Compile,
            ]
            .into_iter()
            .filter(|phase| self.has_phase(*phase))
            .collect(),
            RustWorkload::DependencyPolicy | RustWorkload::Custom => Vec::new(),
        }
    }

    /// The legacy flat command projection, derived only from explicit phases.
    pub(crate) fn flattened(&self) -> Vec<String> {
        RustPhase::ORDER
            .into_iter()
            .flat_map(|phase| self.commands_for(phase).map(str::to_owned))
            .collect()
    }

    pub(crate) fn nextest_required(&self) -> bool {
        self.test_runner == Some(RustTestRunner::Nextest)
    }

    pub(crate) fn append_custom(&mut self, command: String) {
        if self.commands.iter().any(|existing| existing.command == command) {
            return;
        }
        self.commands.push(RustPhaseCommand {
            phase: RustPhase::Custom,
            command,
        });
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self
            .commands
            .iter()
            .any(|command| command.command.trim().is_empty())
        {
            return Err("Rust phase command must not be empty".to_owned());
        }

        let has_format = self.has_phase(RustPhase::Format);
        let has_clippy = self.has_phase(RustPhase::Clippy);
        let has_tests = self.has_phase(RustPhase::Tests);
        let has_doctests = self.has_phase(RustPhase::Doctests);
        let has_compile = self.has_phase(RustPhase::Compile);
        let has_custom = self.has_phase(RustPhase::Custom);

        match self.workload {
            RustWorkload::Crate => {
                if !(has_format && has_clippy && has_compile && has_tests) {
                    return Err(
                        "crate validation requires format, Clippy, compile, and test phases"
                            .to_owned(),
                    );
                }
                match self.test_runner {
                    Some(RustTestRunner::CargoTest) if has_doctests => {
                        return Err(
                            "Cargo test already runs doctests; a separate doctest phase duplicates work"
                                .to_owned(),
                        );
                    }
                    Some(RustTestRunner::CargoTest | RustTestRunner::Nextest) => {}
                    None => {
                        return Err("crate validation requires an explicit test runner".to_owned());
                    }
                }
            }
            RustWorkload::CompileOnly => {
                if !(has_format && has_clippy && has_compile) {
                    return Err(
                        "compile-only validation requires format, Clippy, and compile phases"
                            .to_owned(),
                    );
                }
                if has_tests || has_doctests || self.test_runner.is_some() {
                    return Err("compile-only validation cannot have test-runner phases".to_owned());
                }
            }
            RustWorkload::DependencyPolicy | RustWorkload::Custom => {
                if self.test_runner.is_some()
                    || has_format
                    || has_clippy
                    || has_tests
                    || has_doctests
                    || has_compile
                    || (self.workload == RustWorkload::DependencyPolicy && !has_custom)
                {
                    return Err(
                        "policy/custom Rust checks must use only explicitly assigned custom commands"
                            .to_owned(),
                    );
                }
            }
        }

        if self.test_runner == Some(RustTestRunner::Nextest) && !has_tests {
            return Err("Nextest is selected without a test phase".to_owned());
        }
        if has_custom && self.workload == RustWorkload::Crate {
            // Crate checks may carry custom follow-up checks; they remain an
            // explicit final phase and do not affect standard tool setup.
        }
        Ok(())
    }
}

/// Provider-specific affected/full phase sets.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RustCommandMatrix {
    pub(crate) affected: RustPhaseSet,
    pub(crate) full: RustPhaseSet,
}

impl RustCommandMatrix {
    pub(crate) fn shared(set: RustPhaseSet) -> Self {
        Self {
            affected: set.clone(),
            full: set,
        }
    }

    pub(crate) fn for_scope(&self, scope: RustScope) -> &RustPhaseSet {
        match scope {
            RustScope::Affected => &self.affected,
            RustScope::Full => &self.full,
        }
    }

    pub(crate) fn for_scope_mut(&mut self, scope: RustScope) -> &mut RustPhaseSet {
        match scope {
            RustScope::Affected => &mut self.affected,
            RustScope::Full => &mut self.full,
        }
    }

    pub(crate) fn append_custom(&mut self, command: &str) {
        for scope in [RustScope::Affected, RustScope::Full] {
            self.for_scope_mut(scope)
                .append_custom(command.to_owned());
        }
    }

    pub(crate) fn map_commands(&mut self, mut map: impl FnMut(&str) -> String) {
        for scope in [RustScope::Affected, RustScope::Full] {
            for command in &mut self.for_scope_mut(scope).commands {
                command.command = map(&command.command);
            }
        }
    }
}

/// Versioned typed runtime contract for a Rust unit.
///
/// Schema 1 may supply provider-specific command sets; absent overrides fall
/// back to `default`. Schema 2 uses only `default` because its generation
/// contract is provider-neutral.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RustValidationContract {
    pub(crate) contract_version: u32,
    pub(crate) default: RustCommandMatrix,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) github: Option<RustCommandMatrix>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) velnor: Option<RustCommandMatrix>,
}

impl RustValidationContract {
    pub(crate) fn shared(set: RustPhaseSet) -> Self {
        Self::new(RustCommandMatrix::shared(set))
    }

    pub(crate) fn custom_commands(
        affected: Vec<String>,
        full: Vec<String>,
        github_affected: Option<Vec<String>>,
        github_full: Option<Vec<String>>,
        velnor_affected: Option<Vec<String>>,
        velnor_full: Option<Vec<String>>,
    ) -> Self {
        let mut contract = Self::new(RustCommandMatrix {
            affected: RustPhaseSet::custom(affected),
            full: RustPhaseSet::custom(full),
        });
        if github_affected.is_some() || github_full.is_some() {
            let mut matrix = contract.default.clone();
            if let Some(commands) = github_affected {
                matrix.affected = RustPhaseSet::custom(commands);
            }
            if let Some(commands) = github_full {
                matrix.full = RustPhaseSet::custom(commands);
            }
            contract.github = Some(matrix);
        }
        if velnor_affected.is_some() || velnor_full.is_some() {
            let mut matrix = contract.default.clone();
            if let Some(commands) = velnor_affected {
                matrix.affected = RustPhaseSet::custom(commands);
            }
            if let Some(commands) = velnor_full {
                matrix.full = RustPhaseSet::custom(commands);
            }
            contract.velnor = Some(matrix);
        }
        contract
    }

    pub(crate) fn new(default: RustCommandMatrix) -> Self {
        Self {
            contract_version: RUST_VALIDATION_CONTRACT_VERSION,
            default,
            github: None,
            velnor: None,
        }
    }

    pub(crate) fn set(&self, provider: RustProvider, scope: RustScope) -> &RustPhaseSet {
        let matrix = match provider {
            RustProvider::Github => self.github.as_ref(),
            RustProvider::Velnor => self.velnor.as_ref(),
        }
        .unwrap_or(&self.default);
        matrix.for_scope(scope)
    }

    pub(crate) fn set_mut(
        &mut self,
        provider: RustProvider,
        scope: RustScope,
    ) -> &mut RustPhaseSet {
        let matrix = match provider {
            RustProvider::Github => &mut self.github,
            RustProvider::Velnor => &mut self.velnor,
        };
        matrix
            .get_or_insert_with(|| self.default.clone())
            .for_scope_mut(scope)
    }

    pub(crate) fn set_custom(
        &mut self,
        provider: RustProvider,
        scope: RustScope,
        commands: impl IntoIterator<Item = String>,
    ) {
        *self.set_mut(provider, scope) = RustPhaseSet::custom(commands);
    }

    pub(crate) fn append_custom(&mut self, command: &str) {
        self.default.append_custom(command);
        if let Some(github) = &mut self.github {
            github.append_custom(command);
        }
        if let Some(velnor) = &mut self.velnor {
            velnor.append_custom(command);
        }
    }

    pub(crate) fn map_commands(&mut self, mut map: impl FnMut(&str) -> String) {
        self.default.map_commands(&mut map);
        if let Some(github) = &mut self.github {
            github.map_commands(&mut map);
        }
        if let Some(velnor) = &mut self.velnor {
            velnor.map_commands(&mut map);
        }
    }

    pub(crate) fn flattened(
        &self,
        provider: RustProvider,
        scope: RustScope,
    ) -> Vec<String> {
        self.set(provider, scope).flattened()
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.contract_version != RUST_VALIDATION_CONTRACT_VERSION {
            return Err(format!(
                "unsupported Rust validation contract version {} (supported {})",
                self.contract_version, RUST_VALIDATION_CONTRACT_VERSION
            ));
        }
        self.default.affected.validate()?;
        self.default.full.validate()?;
        if let Some(github) = &self.github {
            github.affected.validate()?;
            github.full.validate()?;
        }
        if let Some(velnor) = &self.velnor {
            velnor.affected.validate()?;
            velnor.full.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RustCommandMatrix, RustPhase, RustPhaseCommand, RustPhaseSet, RustProvider,
        RustScope, RustTestRunner, RustValidationContract, RustWorkload,
    };

    fn command(phase: RustPhase, command: &str) -> RustPhaseCommand {
        RustPhaseCommand {
            phase,
            command: command.to_owned(),
        }
    }

    fn crate_set(runner: RustTestRunner) -> RustPhaseSet {
        RustPhaseSet::new(
            RustWorkload::Crate,
            Some(runner),
            vec![
                command(RustPhase::Format, "cd crate && mbx fmt --check"),
                command(RustPhase::Clippy, "cd crate && mbx clippy --all-features"),
                command(RustPhase::Compile, "cd crate && mbx check --all-targets --all-features"),
                command(RustPhase::Tests, "cd crate && mbx nextest run --no-tests pass"),
                command(RustPhase::Doctests, "cd crate && mbx test --doc --all-features"),
            ],
        )
    }

    #[test]
    fn phase_projection_is_explicit_ordered_and_preserves_shell_commands() {
        let set = crate_set(RustTestRunner::Nextest);
        assert_eq!(
            set.phases(),
            [
                RustPhase::Format,
                RustPhase::Clippy,
                RustPhase::Compile,
                RustPhase::Tests,
                RustPhase::Doctests
            ]
        );
        assert_eq!(
            set.flattened(),
            [
                "cd crate && mbx fmt --check",
                "cd crate && mbx clippy --all-features",
                "cd crate && mbx check --all-targets --all-features",
                "cd crate && mbx nextest run --no-tests pass",
                "cd crate && mbx test --doc --all-features"
            ]
        );
        assert!(set.nextest_required());
    }

    #[test]
    fn provider_and_scope_overrides_are_typed_and_fall_back_independently() {
        let base = crate_set(RustTestRunner::CargoTest);
        let custom = RustPhaseSet::custom(["mise run verify-docs".to_owned()]);
        let mut contract = RustValidationContract::new(RustCommandMatrix {
            affected: base.clone(),
            full: base,
        });
        contract.set_mut(RustProvider::Github, RustScope::Affected).commands =
            custom.commands.clone();

        assert_eq!(
            contract
                .set(RustProvider::Github, RustScope::Affected)
                .phases(),
            [RustPhase::Custom]
        );
        assert_eq!(
            contract
                .set(RustProvider::Github, RustScope::Full)
                .phases(),
            [
                RustPhase::Format,
                RustPhase::Clippy,
                RustPhase::Compile,
                RustPhase::Tests,
                RustPhase::Doctests
            ]
        );
        assert!(contract.validate().is_ok());
    }

    #[test]
    fn policy_commands_stay_custom_and_never_select_nextest() {
        let policy = RustPhaseSet::new(
            RustWorkload::DependencyPolicy,
            None,
            vec![command(RustPhase::Custom, "mbx deny check")],
        );
        assert_eq!(policy.phases(), [RustPhase::Custom]);
        assert!(!policy.nextest_required());
        assert!(policy.validate().is_ok());
    }
}
