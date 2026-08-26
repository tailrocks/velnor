//! Native Repository-action scripts executed inside the guest job container.
//!
//! Host never bind-mounts the workspace. Unknown `uses:` fail closed.

use velnor_model::GuestStep;

use crate::action::native_action_adapter;

fn guest_capability_error(field: &str, received: &str, accepted: &str) -> String {
    format!(
        "unsupported capability: field '{field}' received '{received}'; accepted '{accepted}'; manifest version {}",
        crate::manifest::MANIFEST_VERSION
    )
}

/// Shell body for one admitted guest step.
///
/// # Errors
/// Unknown Repository action identity.
pub(crate) fn guest_step_script(step: &GuestStep) -> Result<String, String> {
    if let Some(action) = &step.action {
        return guest_action_script(step, action);
    }
    if step.script.trim().is_empty() {
        return Err(guest_capability_error(
            "guest.steps[].script",
            "<empty>",
            "non-empty script",
        ));
    }
    Ok(step.script.clone())
}

fn guest_action_script(step: &GuestStep, action: &str) -> Result<String, String> {
    let repository = action
        .split_once('@')
        .map_or(action, |(repository, _)| repository);
    let Some(adapter) = native_action_adapter(repository) else {
        return Err(guest_capability_error(
            "guest.steps[].action",
            action,
            "an admitted native adapter (actions/checkout, actions/cache, …)",
        ));
    };
    Ok(match adapter {
        crate::action::NativeActionAdapter::Checkout => guest_checkout_script(step),
        crate::action::NativeActionAdapter::Cache
        | crate::action::NativeActionAdapter::RustCache => guest_cache_script(step, "cache-hit"),
        crate::action::NativeActionAdapter::Sccache => {
            "set -eu; command -v sccache >/dev/null; sccache --start-server; printf 'sccache: native guest adapter started\\n'"
                .to_string()
        }
        other => {
            return Err(guest_capability_error(
                "guest.steps[].action",
                repository,
                &format!("implemented guest adapter (not {other:?})"),
            ));
        }
    })
}

fn step_input<'a>(step: &'a GuestStep, name: &str) -> &'a str {
    step.inputs
        .iter()
        .find(|input| input.name == name)
        .map(|input| input.value.as_str())
        .unwrap_or("")
}

fn guest_checkout_script(_step: &GuestStep) -> String {
    // Values arrive via `docker exec -e VELNOR_INPUT_*` so untrusted
    // checkout inputs never interpolate into the script text.
    r#"set -eu
dest="${VELNOR_INPUT_destination:-/__w}"
clone_url="${VELNOR_INPUT_clone_url:?}"
version="${VELNOR_INPUT_version:-}"
depth="${VELNOR_INPUT_fetch_depth:-}"
mkdir -p "$dest"
git -C "$dest" init
git -C "$dest" remote remove origin >/dev/null 2>&1 || true
git -C "$dest" remote add origin "$clone_url"
if [ -n "${GITHUB_TOKEN:-}" ]; then
  git -C "$dest" config http.extraheader "AUTHORIZATION: basic $(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')"
fi
refspec="$version"
if [ -n "${GITHUB_REF:-}" ] && [ -n "$version" ]; then
  case "$GITHUB_REF" in
    refs/pull/*)
      mapped=$(printf '%s' "$GITHUB_REF" | sed 's#^refs/#refs/remotes/#')
      refspec="+$version:$mapped"
      ;;
  esac
fi
if [ -n "$depth" ]; then
  git -C "$dest" -c protocol.version=2 fetch --prune --no-tags --depth="$depth" origin "$refspec"
else
  git -C "$dest" -c protocol.version=2 fetch --prune --no-tags origin "$refspec"
fi
git -C "$dest" checkout --force FETCH_HEAD
"#
    .to_string()
}

fn guest_cache_script(step: &GuestStep, output_name: &str) -> String {
    let key = step_input(step, "key");
    format!(
        r#"set -eu
printf 'Cache not found for %s\n' {key:?}
if [ -n "${{GITHUB_OUTPUT:-}}" ]; then
  printf '%s=false\n' {output_name:?} >> "$GITHUB_OUTPUT"
fi
exit 0
"#
    )
}
