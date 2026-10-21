//! Sub-agents (`task`): a nested turn with a fresh history, its own plan and a narrower set
//! of tools, on the model its `agents.d` file names. Only its final reply goes back to the
//! model that started it (docs/plan/04-agent-loop-and-tools.md#sub-agents).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use quack_config::{AgentFile, Loaded};
use quack_instructions::agents_md::Nested;
use quack_protocol::{CallId, Event};
use quack_providers::Message;
use quack_tools::{TaskReport, TaskRequest};
use serde_json::json;
use tokio::sync::broadcast;

use crate::session;
use crate::startup::Setup;
use crate::turn::{self, TurnCtx};

/// Model calls a sub-agent makes when its file sets no `max_steps`.
const MAX_STEPS: usize = 50;

/// Edit tools, which read-only agents are not offered.
const EDIT_TOOLS: &[&str] = &[
    "write",
    "edit",
    "edit_hashline",
    "apply_patch",
    "edit_blocks",
];

/// What sub-agents need from the session: the config (to load another model) and the
/// setups built so far. Each agent keeps one setup per parent model for the session, so
/// its system prompt and tools stay in the server's prompt cache from run to run.
#[derive(Debug)]
pub(crate) struct Pool {
    config: Arc<Loaded>,
    mcp: Arc<crate::mcp::Mcp>,
    lsp: Arc<quack_lsp::LspManager>,
    /// By `(agent, parent model, parent profile)`.
    setups: tokio::sync::Mutex<HashMap<(String, String, String), Arc<Setup>>>,
    /// Runs so far in this session, for ids like `explore-3`.
    runs: Arc<AtomicU64>,
}

impl Pool {
    pub(crate) fn new(
        config: Arc<Loaded>,
        mcp: Arc<crate::mcp::Mcp>,
        lsp: Arc<quack_lsp::LspManager>,
        runs: Arc<AtomicU64>,
    ) -> Self {
        Self {
            config,
            mcp,
            lsp,
            setups: tokio::sync::Mutex::default(),
            runs,
        }
    }

    /// Whether this pool was made for these handles; the engine makes a new one after a
    /// reload replaces any of them.
    pub(crate) fn current(
        &self,
        config: &Arc<Loaded>,
        mcp: &Arc<crate::mcp::Mcp>,
        lsp: &Arc<quack_lsp::LspManager>,
    ) -> bool {
        Arc::ptr_eq(&self.config, config)
            && Arc::ptr_eq(&self.mcp, mcp)
            && Arc::ptr_eq(&self.lsp, lsp)
    }

    /// The agent's setup: built on first use from its model's setup.
    async fn setup(&self, parent: &TurnCtx, id: &str, def: &AgentFile) -> Arc<Setup> {
        let p = &parent.setup;
        let key = (id.to_owned(), p.model_ref.clone(), p.profile.id.clone());
        let mut setups = self.setups.lock().await;
        if let Some(s) = setups.get(&key) {
            return s.clone();
        }
        let wanted = match def.model.trim() {
            "small" => self.config.settings.model.small.trim(),
            m => m,
        };
        let same = wanted.is_empty()
            || wanted == p.model_ref
            || wanted == format!("{}/{}", p.provider.id, p.model.id);
        let base = match wanted.split_once('/') {
            _ if same => p.clone(),
            Some((pid, mid)) => {
                // Loading another model reports startup progress; nobody needs to see it.
                let (quiet, _) = broadcast::channel(16);
                match crate::startup::switch(
                    self.config.clone(),
                    quiet,
                    self.mcp.clone(),
                    self.lsp.clone(),
                    pid.to_owned(),
                    mid.to_owned(),
                )
                .await
                {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        parent.emit(Event::Warning {
                            message: format!(
                                "agent `{id}`: cannot use {wanted} ({e}); it runs on {}",
                                p.model_ref
                            ),
                        });
                        p.clone()
                    }
                }
            }
            None => {
                parent.emit(Event::Warning {
                    message: format!(
                        "agent `{id}`: model `{wanted}` is not <provider>/<model>; it runs on {}",
                        p.model_ref
                    ),
                });
                p.clone()
            }
        };
        let setup = Arc::new(derive(&base, def));
        setups.insert(key, setup.clone());
        setup
    }
}

/// `base` with the agent's prompt after the system prompt and only the tools it may use.
fn derive(base: &Setup, def: &AgentFile) -> Setup {
    let allowed = |internal: &str| {
        !matches!(internal, "task" | "ask_user")
            && !(def.read_only && EDIT_TOOLS.contains(&internal))
            && (def.tools.is_empty() || def.tools.iter().any(|t| t == internal))
    };
    let mut surface = base.surface.clone();
    surface.tools.retain(|t| allowed(&t.internal));
    let mut tool_ctx = base.tool_ctx.clone();
    tool_ctx.plan = Arc::default();
    tool_ctx.agents = Arc::default();
    tool_ctx.spawn = None;
    tool_ctx.ask = None;
    let prompt = def.prompt.trim();
    let system = match (base.system.is_empty(), prompt.is_empty()) {
        (_, true) => base.system.clone(),
        (true, false) => prompt.to_owned(),
        (false, false) => format!("{}\n\n{prompt}", base.system),
    };
    Setup {
        provider: base.provider.clone(),
        model: base.model.clone(),
        model_ref: base.model_ref.clone(),
        profile: base.profile.clone(),
        system,
        session_context: base.session_context.clone(),
        surface,
        registry: base.registry.clone(),
        tool_ctx,
        permissions: base.permissions.clone(),
        budget: base.budget,
        estimator: Mutex::default(),
        context: base.context.clone(),
        instruction_dirs: base.instruction_dirs.clone(),
        mcp: base.mcp.clone(),
        vars: base.vars.clone(),
        commands: base.commands.clone(),
        agents: Arc::default(),
        mcp_tools: def.tools.is_empty() || def.tools.iter().any(|t| t == "mcp"),
    }
}

fn failed(text: impl Into<String>) -> TaskReport {
    TaskReport {
        ok: false,
        text: text.into(),
        summary: "did not start".into(),
    }
}

/// Run a sub-agent for the `task` call `call_key` of `parent`'s turn, to its final reply.
/// Its turn runs inside a tool call of its parent's turn, so the future is boxed (and
/// declared `Send`) to break the cycle.
pub(crate) fn run<'a>(
    parent: &'a TurnCtx,
    call_key: &'a str,
    req: TaskRequest,
) -> BoxFuture<'a, TaskReport> {
    Box::pin(start(parent, call_key, req))
}

async fn start(parent: &TurnCtx, call_key: &str, req: TaskRequest) -> TaskReport {
    let Some((_, def)) = parent.setup.agents.iter().find(|(id, _)| *id == req.agent) else {
        return failed(format!("there is no agent `{}`", req.agent));
    };
    let pool = &parent.pool;
    let setup = pool.setup(parent, &req.agent, def).await;
    let id = format!(
        "{}-{}",
        req.agent,
        pool.runs.fetch_add(1, Ordering::Relaxed) + 1
    );
    let perms = match parent.perms.lock() {
        // Read-only agents get plan mode's rules on a copy of the session's permissions.
        Ok(p) if def.read_only => {
            let mut p = p.clone();
            p.plan = true;
            Arc::new(Mutex::new(p))
        }
        Ok(_) => parent.perms.clone(),
        Err(_) => return failed("the session's permissions are unavailable"),
    };
    let log = parent
        .log
        .as_ref()
        .and_then(|l| l.lock().ok()?.child(&id).ok())
        .map(|l| Arc::new(Mutex::new(l)));
    session::record(
        parent.log.as_ref(),
        "agent",
        json!({
            "id": id,
            "agent": req.agent,
            "call_id": call_key,
            "model": setup.model_ref,
        }),
    );
    session::record(
        log.as_ref(),
        "session_start",
        json!({ "agent": req.agent, "model": setup.model_ref, "profile": setup.profile.id }),
    );
    if req.todo.is_some() {
        parent.emit(Event::Plan {
            checklist: parent.setup.tool_ctx.plan.checklist(),
        });
    }
    parent.emit(Event::AgentStarted {
        id: id.clone(),
        agent: req.agent.clone(),
        description: req.description.clone(),
        call_id: CallId(call_key.to_owned()),
        model: setup.model_ref.clone(),
    });
    let max_steps = def
        .max_steps
        .map_or(MAX_STEPS, |n| usize::try_from(n).unwrap_or(MAX_STEPS))
        .max(1);
    // Nested AGENTS.md files: the agent sees each once, and the session still gets them.
    let nested = Arc::new(Nested::new(
        &pool.config.dirs.workspace_root,
        crate::env::rules(&pool.config),
        setup.instruction_dirs.iter().cloned(),
    ));
    let ctx = TurnCtx {
        turn: parent.turn,
        setup,
        perms,
        nested,
        approvals: parent.approvals.clone(),
        questions: parent.questions.clone(),
        tx: parent.tx.clone(),
        cancel: parent.cancel.child_token(),
        log,
        main_log: parent.main_log.clone(),
        checkpoints: if def.read_only {
            None
        } else {
            parent.checkpoints.clone()
        },
        stats: parent.stats.clone(),
        hooks: parent.hooks.clone(),
        session: parent.session.clone(),
        agent: Some(id.clone()),
        max_steps,
        pool: parent.pool.clone(),
        prompt_lock: parent.prompt_lock.clone(),
    };
    let cancel = ctx.cancel.clone();
    let prompt = turn::Prompt {
        text: req.prompt.clone(),
        typed: req.prompt,
        start: None,
    };
    let history = turn::run(ctx, Vec::new(), prompt).await;
    let report = report(&history, cancel.is_cancelled(), max_steps);
    parent.emit(Event::AgentFinished {
        id,
        ok: report.ok,
        summary: report.summary.clone(),
    });
    report
}

/// The final reply, or why there is none and the last thing the agent said.
fn report(history: &[Message], interrupted: bool, max_steps: usize) -> TaskReport {
    let steps = history
        .iter()
        .filter(|m| matches!(m, Message::Assistant { .. }))
        .count();
    let summary = format!("{steps} step{}", if steps == 1 { "" } else { "s" });
    let last_text = history.iter().rev().find_map(|m| match m {
        Message::Assistant { content, .. } if !content.trim().is_empty() => Some(content.trim()),
        _ => None,
    });
    match history.last() {
        Some(Message::Assistant {
            content,
            tool_calls,
            ..
        }) if tool_calls.is_empty() && !content.trim().is_empty() && !interrupted => {
            return TaskReport {
                ok: true,
                text: content.trim().to_owned(),
                summary,
            };
        }
        _ => {}
    }
    let why = if interrupted {
        "It was interrupted.".to_owned()
    } else if steps >= max_steps {
        format!("It stopped at its limit of {max_steps} steps.")
    } else {
        "It stopped without a final reply (see the warnings).".to_owned()
    };
    let text = match last_text {
        Some(t) => format!("{why} The last thing it said:\n\n{t}"),
        None => why,
    };
    TaskReport {
        ok: false,
        text,
        summary: format!("{summary} · stopped"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(text: &str) -> Message {
        Message::Assistant {
            content: text.into(),
            reasoning: None,
            tool_calls: Vec::new(),
            sealed: None,
        }
    }

    #[test]
    fn the_final_reply_is_the_report() {
        let h = vec![
            Message::User {
                content: "find it".into(),
            },
            said("found it in a.rs:3"),
        ];
        let r = report(&h, false, 5);
        assert!(r.ok);
        assert_eq!(r.text, "found it in a.rs:3");
        assert_eq!(r.summary, "1 step");
        let r = report(&h, true, 5);
        assert!(!r.ok && r.text.starts_with("It was interrupted."));
    }

    #[test]
    fn stopping_early_keeps_what_it_said() {
        let h = vec![
            said("looking in src"),
            Message::Tool {
                call_id: "c".into(),
                content: "x".into(),
            },
        ];
        let r = report(&h, false, 1);
        assert!(!r.ok);
        assert!(r.text.contains("limit of 1 steps") && r.text.ends_with("looking in src"));
    }
}
