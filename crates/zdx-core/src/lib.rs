//! ZDX core library — compatibility facade.
//!
//! All runtime code now lives in `zdx-engine`. This crate re-exports
//! the full API surface for backward compatibility with surface crates.

pub use zdx_engine::{
    agent_activity, audio, automations, config, core, images, mcp, models, pidfile, prompts,
    providers, skills, subagents, tools, tracing_init,
};
