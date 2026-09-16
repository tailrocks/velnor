# Bastion three-provider CI campaign

- `spec.md` — target state only (§1–§9); governs all implementation.
- `work-plan.md` — ordered steps A0–G2 (objective, actions, deps, outputs).
- `checklist.md` — acceptance, 1:1 with step IDs. DONE = all checked + cited + verifier-signed.
- `evidence.md` — audit facts as revalidation inputs/known issues only; never pins.
- Prompt: `goal.md` in this directory. Branch `docs/bastion-final-plan` (#912).
- Target: `root@37.27.110.241`, Debian 13. Order: Velnor → Jackin → ChainArgos → onboarding.
- Rules: orchestrator-only, author ≠ verifier, gate discipline, `git commit -s`, APT-only deploys.
