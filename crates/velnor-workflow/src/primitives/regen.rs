//! `regen-gate`: the command that keeps generated output honest.
//!
//! A unit that owns a generated surface runs the generator in check mode before
//! anything else, so a template-only edit cannot pass without regenerating its
//! consumers. The command is repository policy: it names the manifest and the
//! invocation the repository reviews, and this primitive only makes sure it runs
//! first on the units the declaration names.

use super::{Args, Primitive, RenderCtx, Rendered, REGEN_GATE};
use crate::GeneratorError;

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
            // The gate runs before anything else, and only once: a declared
            // command that the unit already carries is left where it is.
            // Inserting shifts every position, so a unit that gains the gate
            // drops its phase tags and verifies through the single legacy
            // step, gate first.
            if !unit
                .pr_commands
                .iter()
                .any(|candidate| candidate == &command)
            {
                unit.pr_commands.insert(0, command.clone());
                unit.clear_phases();
            }
            if !unit
                .full_commands
                .iter()
                .any(|candidate| candidate == &command)
            {
                unit.full_commands.insert(0, command.clone());
                unit.clear_phases();
            }
            units.push(unit);
        }
        Ok(Rendered {
            units,
            ..Rendered::default()
        })
    }
}
