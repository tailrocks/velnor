//! Post-action drain: registration records, LIFO ordering, and post record helpers.
//!
//! Moved out of `executor.rs` (decomposition slice 4, GOAL 49/58). The
//! unified post stack — one `Vec` drained in exact reverse registration
//! order like upstream's `PostJobSteps` — plus the registration gates
//! (JavaScript entrypoint presence, native adapter conditions), the
//! condition-selected LIFO drain items, and the `Post <name>` record
//! helpers live here behind a narrow `pub(crate)` API so the
//! register-then-drain-LIFO invariant holds in one place. Behavior is
//! byte-identical to the pre-move code: bodies moved verbatim,
//! visibility widened only where a cross-module caller needs it.

use crate::{
    action::{
        CacheActionKind, DockerActionInvocation, JavaScriptActionInvocation, NativeActionAdapter,
        NativeActionInvocation,
    },
    executor::{action_log_prelude, JobExecutionState},
};

#[derive(Debug, Clone)]
pub(crate) struct PostJavaScriptAction {
    pub(crate) step_id: String,
    pub(crate) display_name: String,
    pub(crate) invocation: JavaScriptActionInvocation,
    /// Post entrypoint cloned from `invocation.post_container_path` at
    /// construction. The constructor returns `None` without one, so the
    /// type proves the post drain never unwraps a missing entrypoint.
    pub(crate) post_entrypoint: String,
    pub(crate) condition: Option<String>,
    pub(crate) continue_on_error: bool,
    pub(crate) timeout_minutes: Option<u64>,
    #[allow(
        dead_code,
        reason = "embedded-composite provenance carried with the registration record; no drain reader yet"
    )]
    pub(crate) umbrella_display: Option<String>,
}

impl PostJavaScriptAction {
    /// Register a post action only when the invocation carries a post
    /// entrypoint. `None` means "no post to run", not an error.
    pub(crate) fn new(
        step_id: String,
        display_name: String,
        invocation: JavaScriptActionInvocation,
        condition: Option<String>,
        continue_on_error: bool,
        timeout_minutes: Option<u64>,
        umbrella_display: Option<String>,
    ) -> Option<Self> {
        let post_entrypoint = invocation.post_container_path.clone()?;
        Some(Self {
            step_id,
            display_name,
            invocation,
            post_entrypoint,
            condition,
            continue_on_error,
            timeout_minutes,
            umbrella_display,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PostNativeAction {
    pub(crate) step_id: String,
    pub(crate) display_name: String,
    pub(crate) invocation: NativeActionInvocation,
    pub(crate) condition: Option<String>,
    pub(crate) continue_on_error: bool,
    pub(crate) timeout_minutes: Option<u64>,
    /// Display name of the enclosing composite, when the owning step was
    /// embedded: GitHub runs embedded posts under ONE `Post Run <composite>`
    /// step (EmbeddedStepsWithPostRegistered).
    pub(crate) umbrella_display: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PostDockerAction {
    #[allow(
        dead_code,
        reason = "owning-step provenance carried with the registration record; read only in tests"
    )]
    pub(crate) step_id: String,
    pub(crate) display_name: String,
    pub(crate) invocation: DockerActionInvocation,
    pub(crate) condition: Option<String>,
    pub(crate) continue_on_error: bool,
    pub(crate) timeout_minutes: Option<u64>,
}

/// One registered post step on the unified LIFO stack.
///
/// Upstream keeps a single `Stack<IStep> PostJobSteps` on the job context
/// (`src/Runner.Worker/ExecutionContext.cs:222`) which StepsRunner drains
/// with `TryPop` (`src/Runner.Worker/StepsRunner.cs`), so a mixed job runs
/// native and JavaScript posts in exact reverse registration order. Two
/// separately-reversed lists ran every native post before every JavaScript
/// post regardless of registration order; this enum is what makes that
/// mis-ordering unrepresentable — there is only one stack to drain.
#[derive(Debug, Clone)]
pub(crate) enum PostAction {
    JavaScript(PostJavaScriptAction),
    Native(PostNativeAction),
    Docker(PostDockerAction),
}

impl PostAction {
    pub(crate) fn condition(&self) -> Option<&str> {
        match self {
            PostAction::JavaScript(post) => post.condition.as_deref(),
            PostAction::Native(post) => post.condition.as_deref(),
            PostAction::Docker(post) => post.condition.as_deref(),
        }
    }

    #[cfg(test)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        reason = "tests may panic"
    )]
    pub(crate) fn step_id(&self) -> &str {
        match self {
            PostAction::JavaScript(post) => post.step_id.as_str(),
            PostAction::Native(post) => post.step_id.as_str(),
            PostAction::Docker(post) => post.step_id.as_str(),
        }
    }

    pub(crate) fn display_name(&self) -> &str {
        match self {
            PostAction::JavaScript(post) => post.display_name.as_str(),
            PostAction::Native(post) => post.display_name.as_str(),
            PostAction::Docker(post) => post.display_name.as_str(),
        }
    }
}

/// One LIFO drain position after its post condition was evaluated.
///
/// A post whose condition cannot be evaluated is not dropped: upstream fails
/// the post step (`src/Runner.Worker/StepsRunner.cs:231-242`), so the drain
/// emits a failed record in position and keeps draining — the failed result
/// flips the job conclusion exactly like a main-step failure.
#[derive(Debug, Clone)]
pub(crate) enum PostDrainItem {
    Run(PostAction),
    ConditionFailed { action: PostAction, message: String },
}

/// Drain a registration-order post stack into LIFO drain items.
///
/// Registration order is step order, so reverse is upstream's `TryPop`
/// sequence; every entry keeps its own post condition evaluated against
/// the job's final status. False drops the entry; unevaluable keeps its
/// LIFO position as a failed record (never a silent skip — upstream
/// fails the post step).
pub(crate) fn drain_post_stack(
    stack: Vec<PostAction>,
    state: &JobExecutionState,
) -> Vec<PostDrainItem> {
    stack
        .into_iter()
        .rev()
        .filter_map(
            |post_action| match state.evaluate_post_condition(post_action.condition()) {
                Ok(true) => Some(PostDrainItem::Run(post_action)),
                Ok(false) => None,
                Err(error) => {
                    let message = format!(
                        "Post step '{}' condition could not be evaluated: {error}",
                        post_action.display_name()
                    );
                    eprintln!("{message}");
                    Some(PostDrainItem::ConditionFailed {
                        action: post_action,
                        message,
                    })
                }
            },
        )
        .collect::<Vec<_>>()
}

pub(crate) fn post_step_display_name(display_name: &str) -> String {
    format!("Post {display_name}")
}

pub(crate) fn native_post_condition(
    adapter: NativeActionAdapter,
    cache_kind: Option<CacheActionKind>,
) -> Option<&'static str> {
    match adapter {
        // Only the root cache action saves in post; `/restore` and `/save` do
        // not register a post step. Absent kind defaults to root.
        NativeActionAdapter::Cache => match cache_kind {
            Some(CacheActionKind::Restore) | Some(CacheActionKind::Save) => None,
            Some(CacheActionKind::Root) | None => Some("success()"),
        },
        NativeActionAdapter::RustCache => Some("success() || env.CACHE_ON_FAILURE == 'true'"),
        // Sccache post step stops the server (always run, matches GitHub's behavior).
        NativeActionAdapter::Sccache => Some("always()"),
        // GitHub's setup-buildx post removes the builder it created.
        NativeActionAdapter::DockerSetupBuildx => Some("always()"),
        // GitHub's login-action post logs out (drops registry credentials).
        NativeActionAdapter::DockerLogin => Some("always()"),
        NativeActionAdapter::CreateGitHubAppToken => Some("always()"),
        _ => None,
    }
}

pub(crate) fn reserve_github_post_step_orders(
    current_order: &mut i32,
    visible_post_step_count: usize,
) {
    if visible_post_step_count == 0 {
        return;
    }
    let complete_order = (*current_order * 2) + 1;
    let first_post_order = complete_order - visible_post_step_count as i32;
    *current_order = (*current_order).max(first_post_order - 1);
}

pub(crate) fn native_post_log_prelude(
    action: &NativeActionInvocation,
    state: &JobExecutionState,
) -> Vec<String> {
    action_log_prelude(&action.inputs, &action.env, state)
}

pub(crate) fn javascript_post_log_prelude(
    action: &JavaScriptActionInvocation,
    state: &JobExecutionState,
) -> Vec<String> {
    action_log_prelude(&action.inputs, &action.env, state)
}

pub(crate) fn docker_post_log_prelude(
    action: &DockerActionInvocation,
    state: &JobExecutionState,
) -> Vec<String> {
    action_log_prelude(&action.inputs, &action.env, state)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        time::{Duration, Instant},
    };

    #[test]
    fn unified_post_stack_drain_benchmark() {
        use std::hint::black_box;

        // A mixed stack like the conformance tests above: two `always()`
        // native posts around one `failure()` JavaScript post.
        let native = |step_id: &str| {
            PostAction::Native(PostNativeAction {
                step_id: step_id.into(),
                display_name: step_id.into(),
                invocation: NativeActionInvocation {
                    git_ref: "v1".into(),
                    adapter: NativeActionAdapter::Sccache,
                    cache_kind: None,
                    source_path: None,
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                condition: Some("always()".into()),
                continue_on_error: false,
                timeout_minutes: None,
                umbrella_display: None,
            })
        };
        let stack = vec![
            native("sccache-first"),
            PostAction::JavaScript(PostJavaScriptAction {
                step_id: "guarded".into(),
                display_name: "guarded".into(),
                invocation: JavaScriptActionInvocation {
                    node: "node20".into(),
                    pre_container_path: None,
                    pre_condition: None,
                    main_container_path: "/__a/_actions/guarded/dist/main.js".into(),
                    post_container_path: Some("/__a/_actions/guarded/dist/post.js".into()),
                    post_condition: Some("failure()".into()),
                    action_container_path: "/__a/_actions/guarded".into(),
                    inputs: BTreeMap::new(),
                    env: Vec::new(),
                },
                post_entrypoint: "/__a/_actions/guarded/dist/post.js".into(),
                condition: Some("failure()".into()),
                continue_on_error: false,
                timeout_minutes: None,
                umbrella_display: None,
            }),
            native("sccache-last"),
        ];
        let state = JobExecutionState::default();
        // LIFO survivors on a successful job: the `failure()` post drops out.
        assert_eq!(
            stack
                .iter()
                .rev()
                .filter(|post| state.evaluate_post_condition(post.condition()) == Ok(true))
                .map(PostAction::step_id)
                .collect::<Vec<_>>(),
            vec!["sccache-last", "sccache-first"]
        );

        // 20k drains of the mixed stack; pure expression evaluation must stay
        // far below the bound even on a loaded serial-gate runner.
        let started = Instant::now();
        for _ in 0..20_000 {
            let drained: Vec<&PostAction> = black_box(&stack)
                .iter()
                .rev()
                .filter(|post| state.evaluate_post_condition(post.condition()) == Ok(true))
                .collect();
            assert_eq!(drained.len(), 2);
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "20k mixed post-stack drains took {elapsed:?}",
        );
    }
}
