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

fn normalize_legacy_lane(lane: &mut Vec<String>, command: &str) -> bool {
    let before = lane.clone();
    lane.retain(|candidate| candidate != command);
    lane.insert(0, command.to_owned());
    *lane != before
}

fn apply_legacy_regen_gate(unit: &mut crate::Unit, command: &str) {
    // Schema 1 has one untyped step. A command that was already present in a
    // lane may still be late or duplicated; normalize every active lane so
    // the gate remains first and cannot silently disappear on an override.
    let had_phase_evidence = !unit.phases.is_empty() || !unit.check_commands.is_empty();
    let mut lanes = vec![&mut unit.pr_commands, &mut unit.full_commands];
    if let Some(commands) = unit.github_pr_commands.as_mut() {
        lanes.push(commands);
    }
    if let Some(commands) = unit.github_full_commands.as_mut() {
        lanes.push(commands);
    }
    if let Some(commands) = unit.velnor_pr_commands.as_mut() {
        lanes.push(commands);
    }
    if let Some(commands) = unit.velnor_full_commands.as_mut() {
        lanes.push(commands);
    }
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
                // composable precondition instead of a reason to collapse
                // the unit.
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
