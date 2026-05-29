//! Dynamic Workflows — code-driven subagent fan-out (dev-plan/32).
//!
//! Fourth orchestration tier alongside the model-driven `Task` tool,
//! user-driven `/agent` side-channels, and multi-process Agent Teams.
//! Claude *authors* a JavaScript orchestration script from a user
//! prompt; Boa *executes* the script deterministically; workers run as
//! stateless subagents with fresh context.
//!
//! Stage A scope (this milestone):
//! - [`runtime`] — `WorkflowSandbox`: Boa context, `thclaws.*` host
//!   bindings (stub `subagent`), `eval` / `Function` stripped.
//!
//! Later stages (see dev-plan/32):
//! - Stage B — `/workflow run` slash command + author phase + script
//!   review panel.
//! - Stage C — real subagent spawn through the `Task` primitive,
//!   tokio-semaphore concurrency cap.
//! - Stage D — `state.jsonl` checkpoint after each top-level
//!   statement; `--resume` is Tier 2.
//! - Stage E — REPL worker grid + `ch25-workflows.md`.

mod runtime;
mod script;
mod state;

pub(crate) use runtime::{set_task_tool, WorkflowSandbox};
pub(crate) use script::author;
pub(crate) use state::{generate_workflow_id, set_logger, LoggerHandle, WorkflowLogger};
