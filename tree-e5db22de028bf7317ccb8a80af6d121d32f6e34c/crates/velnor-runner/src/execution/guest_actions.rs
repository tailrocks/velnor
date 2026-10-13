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

fn guest_checkout_script(_step: &GuestStep) -> String {
    // Values arrive via `docker exec -e VELNOR_INPUT_*` so untrusted
    // checkout inputs never interpolate into the script text.
    // Auth parity with actions/checkout: the resolved token is delivered as
    // VELNOR_INPUT_token; persist_credentials=false keeps it out of the
    // workspace .git/config by passing the header per command instead.
    r#"set -eu
dest="${VELNOR_INPUT_destination:-/__w}"
clone_url="${VELNOR_INPUT_clone_url:?}"
version="${VELNOR_INPUT_version:-}"
depth="${VELNOR_INPUT_fetch_depth:-}"
token="${VELNOR_INPUT_token:-${GITHUB_TOKEN:-}}"
fetch_tags="${VELNOR_INPUT_fetch_tags:-0}"
persist="${VELNOR_INPUT_persist_credentials:-1}"
clean="${VELNOR_INPUT_clean:-1}"
mkdir -p "$dest"
if [ "$clean" = "1" ] && [ -d "$dest/.git" ]; then
  git -C "$dest" clean -ffdx
  git -C "$dest" reset --hard
fi
git -C "$dest" init
header=""
if [ -n "$token" ]; then
  header="AUTHORIZATION: basic $(printf 'x-access-token:%s' "$token" | base64 | tr -d '\n')"
fi
# persist-credentials=true (upstream default): keep the header in the
# workspace .git/config so later steps authenticate the same way.
if [ -n "$header" ] && [ "$persist" = "1" ]; then
  git -C "$dest" config http.extraheader "$header"
fi
git -C "$dest" remote remove origin >/dev/null 2>&1 || true
git -C "$dest" remote add origin "$clone_url"
tags_flag=""
if [ "$fetch_tags" = "1" ]; then
  tags_flag="--tags"
fi
depth_arg=""
if [ -n "$depth" ]; then
  depth_arg="--depth=$depth"
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
# shellcheck disable=SC2086
if [ -n "$header" ] && [ "$persist" != "1" ]; then
  git -c "http.extraheader=$header" -C "$dest" -c protocol.version=2 fetch --prune --no-tags $tags_flag $depth_arg origin "$refspec"
else
  git -C "$dest" -c protocol.version=2 fetch --prune --no-tags $tags_flag $depth_arg origin "$refspec"
fi
git -C "$dest" checkout --force FETCH_HEAD
"#
    .to_string()
}

fn guest_cache_script(_step: &GuestStep, output_name: &str) -> String {
    format!(
        r#"set -eu
path="${{VELNOR_INPUT_path:-}}"
if [ -z "$path" ]; then
  path="${{VELNOR_CACHE_PATH:-}}"
fi
if [ -n "$path" ] && [ -e "$path" ]; then
  printf 'Cache restored from digest-verified blob\n'
  if [ -n "${{GITHUB_OUTPUT:-}}" ]; then
    printf '%s=true\n' {output_name:?} >> "$GITHUB_OUTPUT"
  fi
else
  printf 'Cache not found\n'
  if [ -n "${{GITHUB_OUTPUT:-}}" ]; then
    printf '%s=false\n' {output_name:?} >> "$GITHUB_OUTPUT"
  fi
fi
exit 0
"#
    )
}
