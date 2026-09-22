//! `regen-gate`: the command that keeps generated output honest.
//!
//! A unit that owns a generated surface runs the generator in check mode before
//! anything else, so a template-only edit cannot pass without regenerating its
//! consumers. The command is repository policy: it names the manifest and the
//! invocation the repository reviews, and this primitive only makes sure it runs
//! first on the units the declaration names.

use super::{Args, Primitive, RenderCtx, Rendered, REGEN_GATE};
use crate::s2::GeneratorError;

/// Declare the regeneration gate.
pub(crate) struct RegenGate;

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
            prepend_regen_command(&mut unit, &command);
            units.push(unit);
        }
        Ok(Rendered {
            units,
            ..Rendered::default()
        })
    }
}

/// Insert the regeneration command before both command vectors. Inserting a
/// command shifts every positional phase tag, so the bootstrap contract uses
/// the legacy single-step unit after the mutation.
fn prepend_regen_command(unit: &mut crate::s2::Unit, command: &str) {
    if !unit
        .pr_commands
        .iter()
        .any(|candidate| candidate == command)
    {
        unit.pr_commands.insert(0, command.to_owned());
        unit.clear_phases();
    }
    if !unit
        .full_commands
        .iter()
        .any(|candidate| candidate == command)
    {
        unit.full_commands.insert(0, command.to_owned());
        unit.clear_phases();
    }
}

#[cfg(test)]
mod tests {
    use super::prepend_regen_command;
    use crate::s2::{scan, UnitKind, ValidationPhase};

    #[test]
    fn regen_gate_clears_phases_after_inserting_command() {
        let mut unit = scan::unit(
            UnitKind::Rust,
            ".",
            Vec::new(),
            vec!["fmt".to_owned(), "clippy".to_owned(), "test".to_owned()],
            None,
        );
        unit.phases = vec![
            ValidationPhase::Fmt,
            ValidationPhase::Clippy,
            ValidationPhase::Test,
        ];
        unit.check_commands = vec!["check".to_owned()];

        prepend_regen_command(&mut unit, "regen");

        assert_eq!(unit.pr_commands, ["regen", "fmt", "clippy", "test"]);
        assert_eq!(unit.full_commands, unit.pr_commands);
        assert!(unit.phases.is_empty());
        assert!(unit.check_commands.is_empty());
    }
}
