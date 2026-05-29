use crate::providers::{assemble, collect_turn, Provider, StreamRequest};
use crate::types::Message;

/// Ask the active provider to author a JavaScript workflow script for
/// `user_prompt`. On re-author, `revision_note` carries the user's edit
/// request from the review panel. The model sees the API spec
/// ([`crate::prompts::defaults::WORKFLOW_AUTHOR`]) as the system prompt
/// and the goal as the only user message — no conversation history
/// from the calling session leaks in.
pub(crate) async fn author(
    provider: &dyn Provider,
    model: &str,
    user_prompt: &str,
    revision_note: Option<&str>,
) -> Result<String, String> {
    let system = crate::prompts::load("workflow_author", crate::prompts::defaults::WORKFLOW_AUTHOR);

    let user_msg = match revision_note {
        Some(note) if !note.trim().is_empty() => format!(
            "Goal:\n{user_prompt}\n\nThe previous script was rejected. Reviewer note:\n{note}"
        ),
        _ => format!("Goal:\n{user_prompt}"),
    };

    let req = StreamRequest {
        model: model.to_string(),
        system: Some(system),
        messages: vec![Message::user(user_msg)],
        tools: vec![],
        max_tokens: 4096,
        thinking_budget: None,
        stream_chunk_timeout_override: None,
    };

    let stream = provider.stream(req).await.map_err(|e| e.to_string())?;
    let turn = collect_turn(assemble(stream))
        .await
        .map_err(|e| e.to_string())?;

    let script = strip_markdown_fence(&turn.text);
    let script = desugar_async(&script);
    if script.trim().is_empty() {
        return Err("model returned empty script".to_string());
    }
    Ok(script)
}

/// Boa runs scripts in Script mode where top-level `await` is a
/// SyntaxError. Tier 1's `thclaws.subagent` is synchronous (returns
/// the worker text directly, no Promise wrap), so the `await` /
/// `async` / `Promise.all` shapes the model keeps adding from training
/// muscle memory are *semantically* no-ops — we can rewrite them away
/// without changing behaviour and the rewritten script becomes valid
/// Script mode. The user sees the rewritten form in the review panel
/// so there's no hidden magic.
///
/// Real async (Module mode + tokio JobExecutor for genuine
/// `Promise.all` parallelism) is Tier 2; see dev-plan/32.
///
/// Known edge cases this string-level rewrite can't handle:
/// - `Promise.all(arr).then(cb)` becomes `(arr).then(cb)` which fails
///   at runtime (arrays have no `.then`).
/// - `await` / `async` literally inside a string can be corrupted.
/// Both are rare in model-authored workflow scripts.
fn desugar_async(s: &str) -> String {
    let mut out = s.to_string();
    out = out.replace("await Promise.all(", "(");
    out = out.replace("Promise.all(", "(");
    out = out.replace("await ", "");
    out = out.replace("async function", "function");
    out = out.replace("async (", "(");
    out = out.replace("async(", "(");
    out
}

/// Strip a single leading ```js (or ```javascript, or bare ```) fence
/// and its matching trailing ```. Models occasionally wrap output in
/// markdown despite the system prompt telling them not to; better to
/// quietly unwrap than to fail.
fn strip_markdown_fence(text: &str) -> String {
    let trimmed = text.trim();
    for prefix in ["```javascript", "```js", "```"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let inner = rest.trim_start_matches('\n');
            if let Some(body) = inner.strip_suffix("```") {
                return body.trim_end().to_string();
            }
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_with_js_lang_tag() {
        let input = "```js\nlet x = 1;\nx\n```";
        assert_eq!(strip_markdown_fence(input), "let x = 1;\nx");
    }

    #[test]
    fn fence_with_javascript_lang_tag() {
        let input = "```javascript\nlet x = 1;\n```";
        assert_eq!(strip_markdown_fence(input), "let x = 1;");
    }

    #[test]
    fn bare_fence() {
        let input = "```\nlet x = 1;\n```";
        assert_eq!(strip_markdown_fence(input), "let x = 1;");
    }

    #[test]
    fn no_fence_passes_through() {
        let input = "// Workflow: hi\nlet x = 1;\nx";
        assert_eq!(strip_markdown_fence(input), input);
    }

    #[test]
    fn trims_outer_whitespace() {
        let input = "\n\n```js\nlet x = 1;\n```\n\n";
        assert_eq!(strip_markdown_fence(input), "let x = 1;");
    }

    #[test]
    fn desugars_bare_await() {
        let input = "const x = await thclaws.subagent({prompt: \"hi\"});\nx";
        assert_eq!(
            desugar_async(input),
            "const x = thclaws.subagent({prompt: \"hi\"});\nx"
        );
    }

    #[test]
    fn desugars_await_promise_all_map_pattern() {
        // The exact shape the smoke test surfaced from GPT-4.1.
        let input = "const summaries = await Promise.all(\n  paths.map(path => thclaws.subagent({prompt: `Read ${path}`}))\n);";
        let want = "const summaries = (\n  paths.map(path => thclaws.subagent({prompt: `Read ${path}`}))\n);";
        assert_eq!(desugar_async(input), want);
    }

    #[test]
    fn desugars_bare_promise_all() {
        let input = "Promise.all([thclaws.subagent({prompt: \"a\"})])";
        assert_eq!(
            desugar_async(input),
            "([thclaws.subagent({prompt: \"a\"})])"
        );
    }

    #[test]
    fn desugars_async_arrow() {
        let input = "paths.map(async (p) => thclaws.subagent({prompt: p}))";
        assert_eq!(
            desugar_async(input),
            "paths.map((p) => thclaws.subagent({prompt: p}))"
        );
    }

    #[test]
    fn desugars_async_function() {
        let input = "async function wf() { return thclaws.subagent({prompt: \"x\"}); }";
        assert_eq!(
            desugar_async(input),
            "function wf() { return thclaws.subagent({prompt: \"x\"}); }"
        );
    }

    #[test]
    fn desugar_preserves_innocent_text() {
        // Identifiers containing "await" / "async" as substrings shouldn't
        // change because our replacements key on word + space.
        let input = "const awaited = 1;\nconst asynced = 2;";
        assert_eq!(desugar_async(input), input);
    }

    #[test]
    fn full_real_world_script_desugars_cleanly() {
        // The exact failing script from the user's smoke test.
        let input = "const list = await thclaws.subagent({\n  prompt: \"List every .rs file under thclaws/crates/core/src, recursively. \" +\n          \"Return only paths, one per line, no other text.\"\n});\nconst paths = list.split(\"\\n\").map(p => p.trim()).filter(Boolean);\n\nconst summaries = await Promise.all(\n  paths.map(path => thclaws.subagent({\n    prompt: `Read ${path} and write ONE sentence describing what it does.`\n  }))\n);\n\npaths.map((p, i) => `${p} — ${summaries[i]}`).join(\"\\n\");";
        let out = desugar_async(input);
        assert!(
            !out.contains("await"),
            "await should be stripped, got:\n{out}"
        );
        assert!(
            !out.contains("Promise.all"),
            "Promise.all should be stripped, got:\n{out}"
        );
        // And it should still be parseable (smoke check — full Boa
        // parse is exercised by the runtime tests).
        assert!(out.contains("thclaws.subagent"));
        assert!(out.contains("paths.map((p, i)"));
    }
}
