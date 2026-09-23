//! `regen-gate`: the command that keeps generated output honest.
//!
//! A unit that owns a generated surface runs the generator in check mode before
//! anything else, so a template-only edit cannot pass without regenerating its
//! consumers. The command is repository policy: it names the manifest and the
//! invocation the repository reviews, and this primitive only makes sure it runs
//! first on the units the declaration names.

use super::{Args, Primitive, RenderCtx, Rendered, REGEN_GATE};
use crate::s2::{GeneratorError, Unit};

/// Declare the regeneration gate.
pub(crate) struct RegenGate;

fn normalize_legacy_lane(lane: &mut Vec<String>, command: &str) -> bool {
    let before = lane.clone();
    lane.retain(|candidate| candidate != command);
    lane.insert(0, command.to_owned());
    *lane != before
}

fn apply_legacy_regen_gate(unit: &mut Unit, command: &str) {
    // The published runtime has no typed precondition phase. Normalize every
    // active lane so a command that was already present cannot remain late or
    // duplicated, and never retain phase evidence for the untyped shape.
    let had_phase_evidence = !unit.phases.is_empty() || !unit.check_commands.is_empty();
    let mut lanes = vec![&mut unit.pr_commands, &mut unit.full_commands];
    let mut changed = false;
    for lane in &mut lanes {
        changed |= normalize_legacy_lane(lane, command);
    }
    if changed || had_phase_evidence {
        unit.clear_phases();
    }
}

impl Primitive for RegenGate {
    fn id(&self) -> &'static str {
        REGEN_GATE
    }

    fn schema(&self) -> &'static [&'static str] {
        &["command"]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let command = args.string("command")?.ok_or_else(|| {
            GeneratorError::usage(format!(
                "`{REGEN_GATE}` needs `command`, the generation check the named units run first"
            ))
        })?;
        if command.is_empty() {
            return Err(GeneratorError::usage(format!(
                "`{REGEN_GATE}` command must not be empty"
            )));
        }
        let mut units = Vec::new();
        for unit in ctx.units {
            let mut unit = (*unit).clone();
            if ctx.precondition_phases_enabled {
                // The gate runs before anything else and only once. Phased
                // units retain their existing phase tags; the gate is a
                // composable precondition instead of a reason to collapse the
                // unit.
                unit.prepend_precondition_commands(std::slice::from_ref(&command))?;
            } else {
                apply_legacy_regen_gate(&mut unit, &command);
            }
            units.push(unit);
        }
        Ok(Rendered {
            units,
            ..Rendered::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::apply_legacy_regen_gate;
    use crate::s2::{provider, Unit, UnitKind, ValidationPhase};

    fn unit() -> Unit {
        Unit {
            xcode: None,
            id: "rust-app".to_owned(),
            label: "rust-app".to_owned(),
            kind: UnitKind::Rust,
            root: ".".to_owned(),
            pinned_lockfile: false,
            watch: Vec::new(),
            pr_commands: Vec::new(),
            full_commands: Vec::new(),
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: Vec::new(),
            cache: None,
            tool_version: None,
            mise_tools: Vec::new(),
            toolchain: None,
            services: Vec::new(),
            trust: provider::TrustReq::UntrustedOk,
            platform: provider::Platform::LinuxX64,
            capabilities: provider::Capabilities::default(),
            workspace_check: false,
            reads_closed: false,
            full_history: false,
            products: Vec::new(),
            prerequisites: Vec::new(),
            docker_contexts: Vec::new(),
            env: std::collections::BTreeMap::new(),
            mbx: None,
            prepared_tools: Vec::new(),
        }
    }

    #[test]
    fn legacy_regen_clears_existing_or_misplaced_phase_evidence() {
        let command = "cargo run -- --plain --check";
        let mut misplaced = unit();
        misplaced.pr_commands = vec!["cargo fmt --check".to_owned(), command.to_owned()];
        misplaced.full_commands = misplaced.pr_commands.clone();
        misplaced.phases = vec![ValidationPhase::Fmt, ValidationPhase::Test];
        apply_legacy_regen_gate(&mut misplaced, command);
        assert!(misplaced.phases.is_empty());
        assert_eq!(misplaced.pr_commands[0], command);
        assert_eq!(misplaced.full_commands[0], command);

        let mut stale = unit();
        stale.pr_commands = vec![
            command.to_owned(),
            "cargo fmt --check".to_owned(),
            command.to_owned(),
        ];
        stale.full_commands = stale.pr_commands.clone();
        stale.phases = vec![
            ValidationPhase::Fmt,
            ValidationPhase::Test,
            ValidationPhase::Doctest,
        ];
        apply_legacy_regen_gate(&mut stale, command);
        assert!(stale.phases.is_empty());
        assert_eq!(stale.pr_commands[0], command);
        assert_eq!(stale.full_commands[0], command);
        assert_eq!(
            stale
                .pr_commands
                .iter()
                .filter(|candidate| *candidate == command)
                .count(),
            1
        );
    }
}
