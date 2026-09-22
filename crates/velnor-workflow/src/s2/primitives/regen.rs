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
            // The gate runs before anything else and only once. Phased units
            // retain their existing phase tags; the gate is a composable
            // precondition instead of a reason to collapse the unit.
            unit.prepend_precondition_commands(std::slice::from_ref(&command))?;
            units.push(unit);
        }
        Ok(Rendered {
            units,
            ..Rendered::default()
        })
    }
}
