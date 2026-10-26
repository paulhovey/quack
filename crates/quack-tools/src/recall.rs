//! `recall`: page through a tool output the model was shown only part of: a long command
//! output trimmed in the middle, or an old result that pruning shortened
//! (docs/plan/05-context-management.md#recall). The engine keeps the full text under a
//! short handle and names it in what the model sees, so shortening loses nothing for good.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use quack_protocol::ApprovalRequest;
use regex::Regex;
use serde_json::{Value, json};

use crate::registry::{Subject, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_str, opt_u64};

/// Lines returned when no `limit` is given.
const DEFAULT_LIMIT: usize = 200;
/// Matching lines returned for a `pattern`.
const MAX_MATCHES: usize = 200;
/// A page is cut to this many characters, whatever the line count.
const MAX_CHARS: usize = 24_000;

/// Full outputs by handle (`o1`, `o2`, ...). Handles are the store's own, not provider
/// call ids, which some local servers repeat.
#[derive(Debug, Default)]
pub struct Observations {
    enabled: bool,
    kept: Mutex<(u64, HashMap<String, Arc<str>>)>,
}

impl Observations {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }

    /// Whether outputs are kept and `recall` is offered (`[context] recall`).
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Keep `text` and return its new handle.
    pub fn keep(&self, text: &str) -> String {
        let Ok(mut kept) = self.kept.lock() else {
            return String::new();
        };
        kept.0 += 1;
        let handle = format!("o{}", kept.0);
        kept.1.insert(handle.clone(), text.into());
        handle
    }

    /// Forget everything kept and put back a session's outputs under their handles (on
    /// resume, or empty for a new session).
    pub fn replace<'a>(&self, kept: impl IntoIterator<Item = (&'a str, &'a str)>) {
        if let Ok(mut k) = self.kept.lock() {
            *k = (0, HashMap::new());
        }
        for (handle, text) in kept {
            self.restore(handle, text);
        }
    }

    /// Put back an output kept earlier in the session, under its handle.
    pub fn restore(&self, handle: &str, text: &str) {
        let Ok(mut kept) = self.kept.lock() else {
            return;
        };
        if let Some(n) = handle.strip_prefix('o').and_then(|n| n.parse::<u64>().ok()) {
            kept.0 = kept.0.max(n);
        }
        kept.1.insert(handle.to_owned(), text.into());
    }

    pub fn get(&self, handle: &str) -> Option<Arc<str>> {
        self.kept.lock().ok()?.1.get(handle.trim()).cloned()
    }
}

/// What the model is told about a kept output, at the end of its shortened form.
pub fn hint(handle: &str, lines: usize) -> String {
    format!(
        "[the full output ({lines} lines) is kept: `recall` with id \"{handle}\" pages through it]"
    )
}

#[derive(Debug)]
pub struct Recall;

impl Tool for Recall {
    fn name(&self) -> &str {
        "recall"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn description(&self, _ctx: &ToolCtx) -> String {
        format!(
            "Read the full text of an earlier tool output that was shortened. Shortened \
             outputs name an id to pass here. Returns {DEFAULT_LIMIT} lines from `offset` \
             (1-based) unless `limit` says otherwise, or with `pattern` (a regular expression) \
             only the lines that match, with their line numbers."
        )
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "The id the shortened output names, e.g. o3" },
                "offset": { "type": "integer", "description": "First line, 1-based" },
                "limit": { "type": "integer", "description": "Maximum number of lines" },
                "pattern": { "type": "string", "description": "Return only lines matching this regular expression" }
            },
            "required": ["id"]
        })
    }

    fn subject(&self, _args: &Value, _ctx: &ToolCtx) -> Result<Subject, String> {
        Ok(Subject::Recall)
    }

    fn summary(&self, args: &Value, _ctx: &ToolCtx) -> String {
        let id = arg_str(args, &["id", "handle"]).unwrap_or_default();
        match opt_str(args, &["pattern"]) {
            Some(p) => format!("recall {id} /{p}/"),
            None => format!("recall {id}"),
        }
    }

    fn preview(&self, args: &Value, _ctx: &ToolCtx) -> Result<ApprovalRequest, String> {
        Ok(ApprovalRequest::Tool {
            name: "recall".into(),
            arguments: arg_str(args, &["id", "handle"])?.to_owned(),
        })
    }

    fn available(&self, ctx: &ToolCtx) -> bool {
        ctx.observations.enabled()
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            match page(&args, ctx) {
                Ok((text, summary)) => ToolOutput::ok(text, summary),
                Err(e) => ToolOutput::error(e),
            }
        })
    }
}

fn page(args: &Value, ctx: &ToolCtx) -> Result<(String, String), String> {
    let id = arg_str(args, &["id", "handle"])?;
    let text = ctx.observations.get(id).ok_or_else(|| {
        format!("no kept output has id `{id}`; pass the id a shortened output names")
    })?;
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let number = |n: usize, line: &str| format!("{n:>6}\t{line}\n");
    let mut out = String::new();
    if let Some(pattern) = opt_str(args, &["pattern"]) {
        let re = Regex::new(pattern).map_err(|e| format!("bad pattern: {e}"))?;
        let hits: Vec<usize> = (0..total).filter(|&i| re.is_match(lines[i])).collect();
        for &i in hits.iter().take(MAX_MATCHES) {
            out.push_str(&number(i + 1, lines[i]));
            if out.len() > MAX_CHARS {
                break;
            }
        }
        let shown = out.lines().count();
        let head = format!("{} of {total} lines match /{pattern}/", hits.len());
        let more = if shown < hits.len() {
            format!(
                "\n[{} more matches; narrow the pattern]",
                hits.len() - shown
            )
        } else {
            String::new()
        };
        return Ok((format!("{head}\n{out}{more}"), head));
    }
    let offset = usize::try_from(opt_u64(args, &["offset"]).unwrap_or(1).max(1)).unwrap_or(1);
    if offset > total.max(1) {
        return Err(format!("offset {offset} is past the end ({total} lines)"));
    }
    let limit = opt_u64(args, &["limit"]).map_or(DEFAULT_LIMIT, |l| {
        usize::try_from(l.max(1)).unwrap_or(DEFAULT_LIMIT)
    });
    let mut last = offset - 1;
    for (i, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
        out.push_str(&number(i + 1, line));
        last = i + 1;
        if out.len() > MAX_CHARS {
            break;
        }
    }
    let head = format!("lines {offset}-{last} of {total}");
    let more = if last < total {
        format!("\n[continue with offset {}]", last + 1)
    } else {
        String::new()
    };
    Ok((format!("{head}\n{out}{more}"), head))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::test_ctx;

    fn ctx_with(text: &str) -> (ToolCtx, String) {
        let mut ctx = test_ctx();
        ctx.observations = Arc::new(Observations::new(true));
        let id = ctx.observations.keep(text);
        (ctx, id)
    }

    async fn call(ctx: &ToolCtx, args: Value) -> ToolOutput {
        Recall.run(args, ctx).await
    }

    #[tokio::test]
    async fn pages_through_a_kept_output() {
        let text: String = (1..=500).map(|n| format!("line {n}\n")).collect();
        let (ctx, id) = ctx_with(&text);
        assert_eq!(id, "o1");
        let out = call(&ctx, json!({ "id": "o1" })).await;
        assert!(out.ok);
        assert!(out.model.starts_with("lines 1-200 of 500\n"));
        assert!(out.model.contains("   200\tline 200\n"));
        assert!(out.model.ends_with("[continue with offset 201]"));
        let out = call(&ctx, json!({ "id": "o1", "offset": "450", "limit": 100 })).await;
        assert!(out.model.starts_with("lines 450-500 of 500\n"));
        assert!(!out.model.contains("continue"));
        let out = call(&ctx, json!({ "id": "o1", "offset": 900 })).await;
        assert!(!out.ok && out.model.contains("past the end"));
    }

    #[tokio::test]
    async fn a_pattern_returns_matching_lines() {
        let text = "ok 1\nFAILED test_a\nok 2\nFAILED test_b\n";
        let (ctx, _) = ctx_with(text);
        let out = call(&ctx, json!({ "id": "o1", "pattern": "FAILED" })).await;
        assert_eq!(
            out.model,
            "2 of 4 lines match /FAILED/\n     2\tFAILED test_a\n     4\tFAILED test_b\n"
        );
        let out = call(&ctx, json!({ "id": "o1", "pattern": "(" })).await;
        assert!(!out.ok);
    }

    #[tokio::test]
    async fn an_unknown_id_says_what_to_pass() {
        let (ctx, _) = ctx_with("x");
        let out = call(&ctx, json!({ "id": "o9" })).await;
        assert!(!out.ok && out.model.contains("no kept output has id `o9`"));
    }

    #[test]
    fn restored_handles_continue_the_count() {
        let o = Observations::new(true);
        o.restore("o7", "seven");
        assert_eq!(o.keep("eight"), "o8");
        assert_eq!(o.get("o7").as_deref(), Some("seven"));
        o.replace([("o2", "two")]);
        assert_eq!(o.get("o7"), None);
        assert_eq!(o.keep("three"), "o3");
        assert!(!Observations::default().enabled());
    }
}
