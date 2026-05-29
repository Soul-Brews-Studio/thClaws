use std::cell::RefCell;
use std::sync::Arc;

use boa_engine::{
    js_string, native_function::NativeFunction, object::ObjectInitializer, property::Attribute,
    Context, JsArgs, JsError, JsNativeError, JsResult, JsValue, Source,
};

/// Boa-backed JS sandbox hosting the `thclaws.*` workflow API.
///
/// `thclaws.subagent` routes through the parent REPL's Task tool when
/// [`set_task_tool`] has been called on this thread; otherwise it falls
/// back to a stub that echoes the prompt, which keeps the existing
/// Stage A tests deterministic and lets the GUI / chat surface invoke
/// the sandbox without a tokio runtime for refusal messages.
///
/// `eval` and `Function` are removed from the global so an authored
/// script can't generate fresh JS at runtime — the only side effects
/// available are the host bindings we register explicitly.
pub(crate) struct WorkflowSandbox {
    ctx: Context,
}

impl WorkflowSandbox {
    pub fn new() -> JsResult<Self> {
        let mut ctx = Context::default();
        register_thclaws(&mut ctx)?;
        strip_dangerous_globals(&mut ctx)?;
        Ok(Self { ctx })
    }

    pub fn run(&mut self, script: &str) -> JsResult<String> {
        let result = self.ctx.eval(Source::from_bytes(script))?;
        let s = result.to_string(&mut self.ctx)?;
        Ok(s.to_std_string_escaped())
    }
}

thread_local! {
    /// Set by the REPL workflow handler immediately before invoking
    /// `WorkflowSandbox::run` (inside `spawn_blocking`). The host
    /// `thclaws.subagent` function retrieves it to route through the
    /// parent's Task tool. `None` outside the workflow handler — the
    /// host falls back to a stub.
    static WORKFLOW_TASK_TOOL: RefCell<Option<Arc<dyn crate::tools::Tool>>> =
        const { RefCell::new(None) };
}

/// Install (or clear with `None`) the Task tool the sandbox's
/// `thclaws.subagent` will route through. Per-thread — pair with
/// `spawn_blocking` so the thread-local lives for one workflow run.
pub(crate) fn set_task_tool(tool: Option<Arc<dyn crate::tools::Tool>>) {
    WORKFLOW_TASK_TOOL.with(|cell| *cell.borrow_mut() = tool);
}

fn register_thclaws(ctx: &mut Context) -> JsResult<()> {
    let subagent_fn = NativeFunction::from_fn_ptr(subagent);
    let thclaws_obj = ObjectInitializer::new(ctx)
        .function(subagent_fn, js_string!("subagent"), 1)
        .build();
    ctx.register_global_property(js_string!("thclaws"), thclaws_obj, Attribute::READONLY)
}

fn subagent(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let prompt = extract_prompt(args, ctx);

    let task_tool = WORKFLOW_TASK_TOOL.with(|c| c.borrow().clone());

    let Some(tool) = task_tool else {
        // Stage A fallback: no Task tool wired (tests, GUI refusal path)
        // — echo the prompt so callers get a deterministic placeholder
        // instead of an error.
        return Ok(JsValue::from(js_string!(
            format!("(stub for: {prompt})").as_str()
        )));
    };

    let handle = match tokio::runtime::Handle::try_current() {
        Ok(h) => h,
        Err(_) => {
            return Err(js_error(
                "workflow: no tokio runtime available for subagent spawn",
            ));
        }
    };

    let input = serde_json::json!({ "prompt": prompt });

    // Stage D: bracket each subagent call with worker_start /
    // worker_done events. None if no logger is wired (sandbox running
    // outside a workflow run — e.g. unit tests).
    let worker_id = super::state::with_logger(|l| l.worker_start(&prompt).ok()).flatten();

    // Stage E: print a per-worker progress line when we have a logger
    // (real /workflow run; not unit tests). The line gets overwritten
    // by `format_worker_done` once the worker finishes.
    use std::io::Write as _;
    let worker_started = std::time::Instant::now();
    if let Some(wid) = worker_id {
        print!("{}", crate::tool_display::format_worker_start(wid, &prompt));
        let _ = std::io::stdout().flush();
    }

    // Boa eval is sync + this thread is `spawn_blocking`'d, so
    // `block_on` is safe — we're not on the runtime's event-loop
    // thread. Promise.all over multiple subagent calls still serialises
    // here (Boa's single-threaded execution) — true parallelism is a
    // Stage C.2 / Tier 2 concern; see dev-plan/32.
    let result = handle.block_on(tool.call(input));
    let elapsed = worker_started.elapsed();

    if let Some(wid) = worker_id {
        print!(
            "{}",
            crate::tool_display::format_worker_done(wid, &prompt, elapsed, result.is_err())
        );
        let _ = std::io::stdout().flush();
        super::state::with_logger(|l| match &result {
            Ok(text) => {
                let _ = l.worker_done(wid, text);
            }
            Err(e) => {
                let _ = l.worker_error(wid, &e.to_string());
            }
        });
    }

    match result {
        Ok(text) => Ok(JsValue::from(js_string!(text.as_str()))),
        Err(e) => Err(js_error(&format!("workflow subagent failed: {e}"))),
    }
}

fn extract_prompt(args: &[JsValue], ctx: &mut Context) -> String {
    let arg = args.get_or_undefined(0);
    arg.as_object()
        .and_then(|obj| obj.get(js_string!("prompt"), ctx).ok())
        .and_then(|v| v.as_string().map(|s| s.to_std_string_escaped()))
        .unwrap_or_else(|| "(no prompt)".to_string())
}

fn js_error(msg: &str) -> JsError {
    JsNativeError::typ().with_message(msg.to_string()).into()
}

fn strip_dangerous_globals(ctx: &mut Context) -> JsResult<()> {
    let global = ctx.global_object();
    global.delete_property_or_throw(js_string!("eval"), ctx)?;
    global.delete_property_or_throw(js_string!("Function"), ctx)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use async_trait::async_trait;
    use serde_json::{json, Value};

    #[test]
    fn stub_subagent_echoes_prompt() {
        let mut sb = WorkflowSandbox::new().unwrap();
        let out = sb.run(r#"thclaws.subagent({prompt: "hello"})"#).unwrap();
        assert_eq!(out, "(stub for: hello)");
    }

    #[test]
    fn stub_subagent_handles_missing_prompt() {
        let mut sb = WorkflowSandbox::new().unwrap();
        let out = sb.run(r#"thclaws.subagent({})"#).unwrap();
        assert_eq!(out, "(stub for: (no prompt))");
    }

    #[test]
    fn eval_global_stripped() {
        let mut sb = WorkflowSandbox::new().unwrap();
        let err = sb.run(r#"eval("1+1")"#).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("eval"),
            "expected error mentioning eval, got: {msg}"
        );
    }

    #[test]
    fn function_constructor_stripped() {
        let mut sb = WorkflowSandbox::new().unwrap();
        let err = sb.run(r#"new Function("return 1")()"#).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Function"),
            "expected error mentioning Function, got: {msg}"
        );
    }

    #[test]
    fn for_loop_works() {
        let mut sb = WorkflowSandbox::new().unwrap();
        let out = sb
            .run(
                r#"
                let total = 0;
                for (let i = 1; i <= 5; i++) total += i;
                total;
            "#,
            )
            .unwrap();
        assert_eq!(out, "15");
    }

    /// Stage C: a script that calls `thclaws.subagent` multiple times
    /// routes each call through the registered Task tool and stitches
    /// the results back. Uses a mock Tool so the test stays
    /// dependency-free; the real Tool comes from the parent's tool
    /// registry in production (`tool_registry.get("Task")`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_task_tool_routes_subagent_calls() {
        struct MockTask;
        #[async_trait]
        impl Tool for MockTask {
            fn name(&self) -> &'static str {
                "Task"
            }
            fn description(&self) -> &'static str {
                "mock"
            }
            fn input_schema(&self) -> Value {
                json!({})
            }
            async fn call(&self, input: Value) -> crate::Result<String> {
                let prompt = input.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                Ok(format!("task[{prompt}]"))
            }
        }

        let mock: Arc<dyn Tool> = Arc::new(MockTask);
        let script = r#"
            const a = thclaws.subagent({prompt: "alpha"});
            const b = thclaws.subagent({prompt: "beta"});
            const c = thclaws.subagent({prompt: "gamma"});
            `${a} | ${b} | ${c}`
        "#
        .to_string();

        let result: std::result::Result<String, String> = tokio::task::spawn_blocking(move || {
            set_task_tool(Some(mock));
            let res = (|| -> std::result::Result<String, String> {
                let mut sb = WorkflowSandbox::new().map_err(|e| e.to_string())?;
                sb.run(&script).map_err(|e| e.to_string())
            })();
            set_task_tool(None);
            res
        })
        .await
        .unwrap();

        assert_eq!(result.unwrap(), "task[alpha] | task[beta] | task[gamma]");
    }
}
