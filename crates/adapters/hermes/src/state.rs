//! Adapter-private per-source state: a fact an earlier snapshot record of a
//! session establishes that a later record of the same session needs. It is
//! RAM only; the pipeline checkpoints it through
//! [`crate::HermesAdapter`]'s `checkpoint_state`/`restore_state` and a
//! snapshot source always streams whole, so it never has to survive a
//! partial pass.

use rosetta_adapter_support::adapter::NormalizeCtx;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HermesState {
    /// The session row carried a `model_config.codex_thread_id`: its model
    /// requests ran through the Codex app-server (0.21.4
    /// `agent/codex_runtime.py:463-486` binds the thread to the session row
    /// once the turn's rows are committed). The `sessions` record precedes
    /// the `session_model_usage` records inside one snapshot.
    #[serde(default)]
    pub codex_runtime: bool,
}

impl HermesState {
    /// This source's state, installed on first use when the caller (tests,
    /// raw-ledger inspection) did not run `AgentAdapter::init_state`.
    pub fn of(ctx: &mut NormalizeCtx) -> &mut HermesState {
        let installed = ctx
            .state
            .as_deref()
            .is_some_and(|state| state.is::<HermesState>());
        if !installed {
            ctx.state = Some(Box::new(HermesState::default()));
        }
        ctx.state
            .as_deref_mut()
            .and_then(|state| state.downcast_mut::<HermesState>())
            .expect("hermes state was just installed")
    }
}

#[cfg(test)]
mod tests {
    use super::HermesState;
    use rosetta_adapter_support::adapter::NormalizeCtx;

    #[test]
    fn state_is_installed_lazily_and_kept() {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        assert!(ctx.state.is_none());
        assert!(!HermesState::of(&mut ctx).codex_runtime);
        HermesState::of(&mut ctx).codex_runtime = true;
        assert!(HermesState::of(&mut ctx).codex_runtime);

        // A foreign payload is replaced, never downcast.
        ctx.state = Some(Box::new(7u8));
        assert!(!HermesState::of(&mut ctx).codex_runtime);
    }
}
