# CANDIDATE-BINDING precision read (bastion campaign, read-only)

Date: 2026-09-17. Method: exact code quotes only, no theory.
Question: for a velnor PR with pin=M_p (merged, product exists) != base and
generator source changed pin..head, does Policy candidate-acquire SUCCEED or FAIL?

## VERDICT: FAIL — name mismatch (HEAD-published vs PIN-polled)

The publisher names the artifact by the PR-HEAD candidate closure; Policy
Acquire polls by the AUDITED-PIN candidate closure. A generator-source delta
on closure paths pin..head makes those two closures differ, so the polled
name is never published. Two full-digest manifest gates (Acquire: manifest ==
pin; validator: manifest == head) make the rendezvous require
pin_candidate == head_candidate in full, not just the 16-hex name prefix.

## (1) Publisher: artifact NAME derives from HEAD closure (not PIN)

Generator: `crates/velnor-workflow/src/primitives/ir.rs:1528` (+ `:1580`):

```
head_closure="$(velnor-workflow closure --rev="$PR_HEAD" --candidate)"
...
echo "name=velnor-workflow-candidate-${{head_closure:0:16}}-${{RUNNER_OS}}-${{RUNNER_ARCH}}" >> "$GITHUB_OUTPUT"
```

Rendered (live file): `.github/workflows/ci-unit-rust.yml:502` and `:554`
(identical modulo `{{` escaping):

```
head_closure="$(velnor-workflow closure --rev="$PR_HEAD" --candidate)"
...
echo "name=velnor-workflow-candidate-${head_closure:0:16}-${RUNNER_OS}-${RUNNER_ARCH}" >> "$GITHUB_OUTPUT"
```

Manifest the publisher writes (`ir.rs:1576`, rendered `ci-unit-rust.yml:550`):

```
jq -n ... --arg revision "$PR_HEAD" --arg closure "$head_closure" ... > "$stage/candidate-manifest.json"
```

So: `manifest.revision = PR_HEAD`, `manifest.closure = head_candidate`.
Staged-binary proof (`ir.rs:1574-1575`):

```
reported="$("$stage/velnor-workflow" --closure)"
[[ "$reported" == "$head_closure" ]] || {{ echo "::error::candidate reports closure $reported, head $PR_HEAD declares $head_closure" >&2; exit 1; }}
```

Publisher skip gate (`ir.rs:1538-1543`, rendered `ci-unit-rust.yml:512-517`):

```
base_closure="$(velnor-workflow closure --rev="$base_pin" --candidate)"
if [[ "$head_closure" == "$base_closure" ]]; then
  echo "head $PR_HEAD shares the base pin's candidate closure; no candidate to publish"
  echo "skip=true" >> "$GITHUB_OUTPUT"
  exit 0
fi
```

## (2) Policy Acquire: poll-name derives from PIN closure (not HEAD)

Rendered: `.github/workflows/ci-policy.yml:84-85`:

```
pin_candidate="$(velnor-workflow closure --rev="$pin" --candidate)"
name="velnor-workflow-candidate-${pin_candidate:0:16}-${RUNNER_OS}-${RUNNER_ARCH}"
```

Generator: `crates/velnor-workflow/src/lib.rs:3958-3959` (same, `{{`-escaped):

```
pin_candidate="$(velnor-workflow closure --rev="$pin" --candidate)"
name="velnor-workflow-candidate-${{pin_candidate:0:16}}-${{RUNNER_OS}}-${{RUNNER_ARCH}}"
```

pin==base early-exit (non-candidate release closures — note: NO `--candidate`):
rendered `.github/workflows/ci-policy.yml:76-82`:

```
base_closure="$(velnor-workflow closure --rev="$BASE_PIN")"
pin_closure="$(velnor-workflow closure --rev="$pin")"
if [[ "$pin_closure" == "$base_closure" ]]; then
  echo "pin $pin shares the base closure; the Stage-0 validator renders"
  echo "VELNOR_WORKFLOW_CANDIDATE_MANIFEST=" >> "$GITHUB_ENV"
  exit 0
fi
```

Generator `lib.rs:3950-3956` is the same text (`{{`-escaped). Doc `lib.rs:3921-3927`:

```
/// Owner policy step acquiring the PR run's candidate generator product.
/// When the audited pin shares the base validator's closure the step exits
/// immediately (the Stage-0 validator renders). Otherwise the step waits for
/// the same-repository PR run at the audited head to publish the candidate
/// artifact the Rust unit job packaged, verifies its manifest bindings and
/// digest, requires the manifest closure to equal the pin's candidate
/// closure in full (not just the artifact-name prefix), and exports the
```

Acquire manifest gate = PIN (`ci-policy.yml:125-126`, generator `lib.rs:3999-4000`):

```
manifest_closure="$(jq -er .closure "$candidate/candidate-manifest.json")"
[[ "$manifest_closure" == "$pin_candidate" ]] || { echo "::error::candidate manifest closure $manifest_closure is not the pin's candidate $pin_candidate" >&2; exit 1; }
```

Acquire digest + self-report gates (`ci-policy.yml:117-128`):

```
actual="$(sha256sum "$candidate/velnor-workflow" | awk '{print $1}')"   # (or shasum fallback)
expected="$(jq -er .binary_sha256 "$candidate/candidate-manifest.json")"
[[ "$actual" == "$expected" ]] || { echo "::error::candidate digest mismatch" >&2; exit 1; }
...
reported="$(GH_TOKEN="" GITHUB_TOKEN="" "$candidate/velnor-workflow" --closure)"
[[ "$reported" == "$manifest_closure" ]] || { echo "::error::candidate reports closure $reported, manifest claims $manifest_closure" >&2; exit 1; }
```

Fail-shapes when the name never appears (`ci-policy.yml:108`, `:111`):

```
[[ "$waiting" == "true" ]] || { echo "::error::no same-repository PR run published candidate $name" >&2; exit 1; }
...
[[ -n "$run_id" ]] || { echo "::error::no candidate product $name was published within 15 minutes" >&2; exit 1; }
```

## (3) Validator binding: `policy.rs render_with_candidate` — `wanted` = HEAD candidate

`crates/velnor-workflow/src/policy.rs:1510-1537` (doc + wanted):

```
/// The candidate exception: when the tree differs from the declared pin's
/// render, it may still be legitimate — a generator change in flight renders
/// with the audited tree's own candidate, not with the pin.
///
/// Acceptance requires the manifest binding, not `--closure` alone: the
/// manifest's closure must equal the audited checkout's own candidate
/// closure (computed locally from git history), the env-slot binary's digest
/// must match the manifest before any execution, and only then does the
/// `--closure` self-report stay as a final tripwire. ...
...
    let Ok(Some(head)) = git(checkout, &["rev-parse", "HEAD"]) else {
        return Ok(None);
    };
    if !super::is_full_revision(&head) {
        return Ok(None);
    }
    let Ok(wanted) = closure_identity::candidate_closure_of_tree(checkout, &head) else {
        return Ok(None);
    };
```

Manifest gate fails closed LOUDLY (`policy.rs:1541-1554`):

```
    let manifest = match &lookup.candidate_manifest {
        None => None,
        Some(path) => {
            let manifest = load_candidate_manifest(path).map_err(GeneratorError::usage)?;
            if manifest.closure != wanted {
                return Err(GeneratorError::usage(format!(
                    "candidate manifest {} names closure {}, but the audited tree's candidate closure is {wanted}",
                    path.display(),
                    manifest.closure
                )));
            }
            Some(manifest)
        }
    };
```

Binary set + digest-before-exec + self-report (`policy.rs:1555-1594`):

```
    let mut binaries = Vec::new();
    if let Some(pinned) = &lookup.pinned_binary {
        binaries.push(pinned.clone());
    }
    if let Some(current) = &current_exe
        && !binaries.contains(current)
    {
        binaries.push(current.clone());
    }
    for binary in binaries {
        ...
        let is_self = current_exe.as_deref() == Some(binary.as_path());
        if !is_self {
            let Some(bound) = &manifest else {
                continue;                      // env-slot without manifest: skip, never exec
            };
            let Ok(digest) = sha256_file(&binary) else {
                continue;
            };
            if digest != bound.binary_sha256 {
                continue;
            }
        }
        let Ok(reported) = binary_closure(&binary) else {
            continue;
        };
        if reported != wanted {
            continue;
        }
        let differences =
            render_and_compare(&binary, checkout, tree, scratch, default_branch, excludes)?;
        if differences.is_empty() {
            return Ok(Some(reported));
        }
    }
    Ok(None)
```

Summary of (3): `wanted` = HEAD's candidate closure computed locally via
`candidate_closure_of_tree(checkout, HEAD)`; manifest.closure must equal it
(full 64-hex, else hard error); env-slot digest must equal manifest before any
exec; binary `--closure` self-report must equal it; render must byte-match
(state file included) — else `None` (Differences verdict stands).

`--candidate` == `candidate_closure_of_tree` (`runtime.rs:334-335`):

```
    let digest = if candidate {
        crate::closure::candidate_closure_of_tree(&repo, rev)?
```

and (`closure.rs:176-178`):

```
pub(crate) fn candidate_closure_of_tree(repo: &Path, rev: &str) -> Result<String, GeneratorError> {
    closure_of_tree(repo, rev, DEV_FEATURES, PROFILE_DEBUG)
}
```

Closure inputs (`closure.rs:84-91`): `crates/velnor-workflow`, `Cargo.toml`,
`Cargo.lock`, `rust-toolchain.toml`, `rust-toolchain`, `.cargo` (+ features +
profile footer). A generator-source change pin..head moves the digest, so
`candidate(pin) != candidate(head)`.

## (4) Reconciliation with `/tmp/pr916-flow.md` pin..head closure-clean claim

The claim is IMPOSED by the code, not refuted. The flow doc says
(`pr916-flow.md:103-105`):

```
/// candidate path additionally requires a pin release product
/// (pin merged) plus head_closure == pin_candidate (no closure-path delta
/// pin..head) — jointly unsatisfiable while a generator change is in flight.
```

(actual text, lines 103-105: "the candidate path additionally requires a pin
release product (pin merged) plus head_closure == pin_candidate (no
closure-path delta pin..head) — jointly unsatisfiable while a generator change
is in flight.")

and (`pr916-flow.md:134-136`): "name head16 == pin_candidate16 since pin..head
touches no closure paths — the #914 rendezvous, verified live".

Exact lines that IMPOSE it (three-way split binding):

1. Publisher names by HEAD: `ir.rs:1528` + `ir.rs:1580`
   (`head_closure=...--rev="$PR_HEAD" --candidate` → `name=...${head_closure:0:16}...`).
2. Acquire polls by PIN: `lib.rs:3958-3959` / `ci-policy.yml:84-85`
   (`pin_candidate=...--rev="$pin" --candidate` → `name=...${pin_candidate:0:16}...`).
3. Acquire manifest gate = PIN full digest: `lib.rs:3999-4000` /
   `ci-policy.yml:125-126` (`manifest_closure == pin_candidate`).
4. Validator manifest gate = HEAD full digest: `policy.rs:1545-1551`
   (`manifest.closure != wanted` → hard error, `wanted` = HEAD candidate).
5. The generator's own doc states the split outright (`ir.rs:1485-1486`):

```
/// The unit job checks out the merge commit, but the policy consumer waits
/// for an artifact named by the audited pin's candidate closure and verifies
/// the audited PR-head tree's closure.
```

No line refutes the claim: there is no pin→head rewrite, no fallback poll of
the head name, no manifest-closure remapping. (1)+(2) force name equality ⇒
`head_candidate[0:16] == pin_candidate[0:16]`; (3)+(4) force full-digest
equality `manifest == pin_candidate == head_candidate`. A closure-path delta
pin..head breaks all of it.

## Case verdict: pin=M_p merged (product exists) != base, generator source changed pin..head

1. Early-exit does NOT fire: pin M_p != base and the generator delta moves the
   (release-profile) closure too, so `pin_closure != base_closure`
   (`ci-policy.yml:76-82` falls through).
2. Publisher uploads `velnor-workflow-candidate-<head16>-<os>-<arch>` with
   `manifest.closure = head_candidate` (`ci-unit-rust.yml:502/550/554`).
3. Acquire polls `velnor-workflow-candidate-<pin16>-<os>-<arch>`
   (`ci-policy.yml:84-85`). Since generator source changed on closure paths
   pin..head, `head_candidate != pin_candidate` (`closure.rs:84-91`,
   `:176-178`), so the polled name never exists.
4. Acquire FAILS with `no same-repository PR run published candidate <pin-name>`
   (runs completed) or `no candidate product <pin-name> was published within
   15 minutes` (timeout) (`ci-policy.yml:108` / `:111`).
5. Belt-and-braces: even a 16-hex prefix collision would die at the Acquire
   manifest gate (`manifest_closure(head) != pin_candidate`, `ci-policy.yml:126`);
   and a manifest satisfying Acquire (pin) could not satisfy the validator
   (head, `policy.rs:1545`) — the two full-digest gates are jointly satisfiable
   only when `pin_candidate == head_candidate`.
6. "Pin merged, product exists" satisfies only the pin-render leg
   (`resolve_pinned_binary` / `expected_closures(checkout, pin)`), not the
   candidate leg — irrelevant to the name rendezvous.

WHY (one line): name mismatch — polled name keys off PIN, published name keys
off HEAD, and the pin..head generator delta makes the closures differ.

## Branch check: `fix/policy-candidate-trigger` on origin

```
$ git ls-remote origin 'fix/policy-candidate-trigger'      -> (empty, exit 0)
$ git ls-remote origin 'refs/heads/fix/policy-candidate-trigger' -> (empty, exit 0)
```

ABSENT — confirms the trigger author's claim of no push (read-only check, no fetch).
