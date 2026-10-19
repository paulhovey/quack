//! Hooks (docs/plan/11-security.md#hooks): programs from `hooks.d` that run on agent
//! events. Each gets the event as JSON on stdin and may answer on stdout with
//! `{"decision", "message", "additional_context", "updated_args"}`, or just exit: 0 allows,
//! 2 denies with stderr as the reason. Plain text on stdout is context for the model.
//!
//! The hooks for one event run one after another in id order, so a `pre_tool` hook sees
//! the arguments an earlier one changed. A denial stops the rest.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use quack_config::hooks::{HookEvent, OnError};
use quack_config::{HookFile, ItemState, LayerKind, Loaded, glob_match, load_hooks};
use quack_protocol::Event;
use quack_providers::tokens;
use regex::Regex;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::session::{self, SharedLog};

/// Context one hook may add, in estimated tokens; the rest is cut.
const MAX_CONTEXT_TOKENS: u32 = 8_000;

/// The session's hooks, read from `hooks.d` at startup and on `/reload`.
#[derive(Debug, Default)]
pub(crate) struct Hooks {
    list: Vec<Hook>,
    root: PathBuf,
}

#[derive(Debug)]
struct Hook {
    id: String,
    file: HookFile,
    /// The folder of the hook's file, for `{dir}`.
    dir: PathBuf,
    prompt: Option<Regex>,
    /// The command with secrets resolved, after the first run that needed it.
    prepared: Mutex<Option<Arc<Prepared>>>,
}

#[derive(Debug)]
struct Prepared {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, OsString>,
}

/// What a hook is matched against.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Target<'a> {
    None,
    Prompt(&'a str),
    /// A tool call: the tool's name and its target (see [`call_target`]).
    Call {
        tool: &'a str,
        target: &'a str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Allow,
    Ask,
    Deny(String),
}

/// What the hooks for one event decided, together.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// The strictest decision any hook gave (deny, then ask, then allow), and whose it was.
    pub verdict: Option<(Verdict, String)>,
    /// Context for the model, one entry per hook that gave some.
    pub context: Vec<String>,
    /// `pre_tool`: the arguments after every hook's changes.
    pub updated_args: Option<Value>,
}

impl Outcome {
    pub fn denied(&self) -> Option<(&str, &str)> {
        match &self.verdict {
            Some((Verdict::Deny(reason), id)) => Some((id, reason)),
            _ => None,
        }
    }

    /// The context joined, ready to append to a message; `None` when there is none.
    pub fn context_text(&self) -> Option<String> {
        (!self.context.is_empty()).then(|| self.context.join("\n\n"))
    }

    fn decide(&mut self, v: Verdict, id: &str) {
        let rank = |v: &Verdict| match v {
            Verdict::Allow => 0,
            Verdict::Ask => 1,
            Verdict::Deny(_) => 2,
        };
        if self
            .verdict
            .as_ref()
            .is_none_or(|(old, _)| rank(&v) > rank(old))
        {
            self.verdict = Some((v, id.to_owned()));
        }
    }
}

/// One hook's answer.
#[derive(Debug, Default, PartialEq)]
struct Reply {
    decision: Option<Verdict>,
    message: Option<String>,
    context: Option<String>,
    updated_args: Option<Value>,
}

/// Where hook runs are reported.
pub(crate) struct Sink<'a> {
    pub emit: &'a (dyn Fn(Event) + Send + Sync),
    pub log: Option<&'a SharedLog>,
    pub session: &'a str,
}

/// Read `hooks.d`. Files that fail to parse or check become warnings.
pub(crate) fn load(config: &Loaded) -> (Hooks, Vec<String>) {
    let mut warnings = Vec::new();
    let mut list = Vec::new();
    for item in load_hooks(config) {
        let file = match item.state {
            ItemState::Loaded(f) => f,
            ItemState::Disabled(_) => continue,
            ItemState::Failed(d) => {
                warnings.push(format!("hooks.d: {}", d.one_line()));
                continue;
            }
        };
        if let Err(e) = file.check() {
            warnings.push(format!("hooks.d: {}: {e}", item.id));
            continue;
        }
        let prompt = match file.matcher.prompt.as_str() {
            "" => None,
            p => match Regex::new(p) {
                Ok(re) => Some(re),
                Err(e) => {
                    warnings.push(format!(
                        "hooks.d: {}: `match.prompt` is not a valid regular expression: {e}",
                        item.id
                    ));
                    continue;
                }
            },
        };
        if item.layer == LayerKind::Project {
            tracing::info!(id = %item.id, "project hook loaded (the project is trusted)");
        }
        list.push(Hook {
            dir: item
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            id: item.id,
            file,
            prompt,
            prepared: Mutex::new(None),
        });
    }
    let hooks = Hooks {
        list,
        root: config.dirs.workspace_root.clone(),
    };
    (hooks, warnings)
}

/// What a `pattern` glob is matched against: the command for shell calls, the path
/// (relative to the workspace) for file calls, otherwise the arguments as JSON.
pub(crate) fn call_target(subject: &quack_tools::Subject, args: &Value, root: &Path) -> String {
    use quack_tools::Subject;
    match subject {
        Subject::Shell(c) | Subject::ShellOutsideSandbox(c) => c.clone(),
        Subject::Read(p) | Subject::Edit(p) => quack_tools::paths::display(root, p),
        Subject::Edits(ps) => ps
            .iter()
            .map(|p| quack_tools::paths::display(root, p))
            .collect::<Vec<_>>()
            .join(" "),
        _ => args.to_string(),
    }
}

impl Hooks {
    /// Whether any hook listens to `event`.
    pub fn any(&self, event: HookEvent) -> bool {
        self.list.iter().any(|h| h.file.event == event)
    }

    /// Run the hooks for `event` that match `target`. `fields` is the event's own part of
    /// the JSON a hook reads; the common fields are added here.
    pub async fn run(
        &self,
        event: HookEvent,
        mut fields: Value,
        target: Target<'_>,
        sink: &Sink<'_>,
        cancel: &CancellationToken,
    ) -> Outcome {
        let mut out = Outcome::default();
        for hook in self.list.iter().filter(|h| h.file.event == event) {
            let Some(matches) = hook.matches(target) else {
                continue;
            };
            if cancel.is_cancelled() {
                break;
            }
            let mut input = json!({
                "event": event.label(),
                "hook": hook.id,
                "session_id": sink.session,
                "cwd": self.root.display().to_string(),
            });
            if let (Some(obj), Value::Object(own)) = (input.as_object_mut(), &fields) {
                obj.extend(own.clone());
                if !matches.is_empty() {
                    obj.insert("matches".into(), json!(matches));
                }
            }
            let started = Instant::now();
            let result = self.run_one(hook, &input, cancel).await;
            let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let (outcome, message, tokens) = match result {
                Ok(None) => continue, // interrupted
                Ok(Some(reply)) => out.apply(hook, event, reply, &mut fields),
                Err(e) => {
                    let message = format!("hook `{}` failed: {e}", hook.id);
                    tracing::warn!(id = %hook.id, error = %e, "hook failed");
                    match hook.file.on_error {
                        OnError::Deny => out.decide(Verdict::Deny(message.clone()), &hook.id),
                        OnError::Warn => (sink.emit)(Event::Warning {
                            message: message.clone(),
                        }),
                        OnError::Allow => {}
                    }
                    ("failed", Some(e), 0)
                }
            };
            session::record(
                sink.log,
                "hook",
                json!({ "id": hook.id, "on": event.label(), "outcome": outcome, "ms": ms }),
            );
            (sink.emit)(Event::HookRan {
                id: hook.id.clone(),
                on: event.label().into(),
                outcome: outcome.into(),
                ms,
                message,
                context_tokens: tokens,
            });
            if out.denied().is_some() {
                break;
            }
        }
        out
    }

    /// Start the hook and read its answer. `Ok(None)`: interrupted.
    async fn run_one(
        &self,
        hook: &Hook,
        input: &Value,
        cancel: &CancellationToken,
    ) -> Result<Option<Reply>, String> {
        let prepared = self.prepared(hook).await?;
        let opts = quack_shell::ExecOptions {
            cwd: prepared.cwd.clone(),
            env: prepared.env.clone(),
            stdin: serde_json::to_vec(input).unwrap_or_default(),
            timeout: Duration::from_secs(hook.file.timeout_secs),
            cancel: cancel.child_token(),
        };
        let r = quack_shell::exec(&prepared.program, &prepared.args, opts)
            .await
            .map_err(|e| format!("cannot start {}: {e}", prepared.program.display()))?;
        if r.cancelled {
            return Ok(None);
        }
        if r.timed_out {
            return Err(format!("timed out after {}s", hook.file.timeout_secs));
        }
        parse_reply(r.exit_code, &r.stdout, &r.stderr, hook.file.event).map(Some)
    }

    /// The command ready to start. Secrets resolve once (a `cmd` or keyring secret runs a
    /// process, so off the runtime); a failure is retried on the next run.
    async fn prepared(&self, hook: &Hook) -> Result<Arc<Prepared>, String> {
        if let Some(p) = hook.prepared.lock().ok().and_then(|p| p.clone()) {
            return Ok(p);
        }
        let file = hook.file.clone();
        let (dir, root) = (hook.dir.clone(), self.root.clone());
        let p = tokio::task::spawn_blocking(move || prepare(&file, &dir, &root))
            .await
            .map_err(|e| e.to_string())??;
        let p = Arc::new(p);
        if let Ok(mut slot) = hook.prepared.lock() {
            *slot = Some(p.clone());
        }
        Ok(p)
    }
}

impl Hook {
    /// `None` when the hook does not apply; otherwise the prompt's matches, if any.
    fn matches(&self, target: Target<'_>) -> Option<Vec<String>> {
        let m = &self.file.matcher;
        match target {
            Target::Prompt(text) => match &self.prompt {
                None => Some(Vec::new()),
                Some(re) => {
                    let found: Vec<String> =
                        re.find_iter(text).map(|m| m.as_str().to_owned()).collect();
                    (!found.is_empty()).then_some(found)
                }
            },
            Target::Call { tool, target } => {
                let ok = (m.tool.is_empty() || glob_match(&m.tool, tool))
                    && (m.pattern.is_empty() || glob_match(&m.pattern, target));
                ok.then(Vec::new)
            }
            Target::None => Some(Vec::new()),
        }
    }
}

impl Outcome {
    /// Fold one reply in; returns the outcome label, the message and the context's tokens.
    fn apply(
        &mut self,
        hook: &Hook,
        event: HookEvent,
        reply: Reply,
        fields: &mut Value,
    ) -> (&'static str, Option<String>, u32) {
        let mut label = "ok";
        let mut tokens_added = 0;
        if let Some(text) = reply.context.filter(|_| event.takes_context()) {
            let text = cap(&text, MAX_CONTEXT_TOKENS);
            tokens_added = tokens::estimate(&text);
            self.context.push(text);
            label = "context";
        }
        if let Some(args) = reply.updated_args.filter(|_| event == HookEvent::PreTool) {
            if let Some(obj) = fields.as_object_mut() {
                obj.insert("args".into(), args.clone());
            }
            self.updated_args = Some(args);
            label = "updated";
        }
        // Only a call or a prompt can be stopped; elsewhere a decision means nothing.
        let guards = matches!(event, HookEvent::PreTool | HookEvent::UserPrompt);
        match reply.decision.filter(|_| guards) {
            Some(Verdict::Deny(reason)) => {
                let reason = if reason.is_empty() {
                    reply.message.clone().unwrap_or_default()
                } else {
                    reason
                };
                self.decide(Verdict::Deny(reason), &hook.id);
                label = "denied";
            }
            Some(Verdict::Ask) if event == HookEvent::PreTool => {
                self.decide(Verdict::Ask, &hook.id);
                label = "asked";
            }
            Some(Verdict::Allow) if event == HookEvent::PreTool => {
                self.decide(Verdict::Allow, &hook.id);
                label = "allowed";
            }
            _ => {}
        }
        (label, reply.message, tokens_added)
    }
}

/// Read a hook's exit code and output.
fn parse_reply(
    exit: Option<i32>,
    stdout: &str,
    stderr: &str,
    event: HookEvent,
) -> Result<Reply, String> {
    let last_line = |s: &str| s.trim().lines().last().unwrap_or_default().to_owned();
    match exit {
        Some(0) => {}
        Some(2) => {
            let reason = stderr.trim();
            return Ok(Reply {
                decision: Some(Verdict::Deny(reason.to_owned())),
                message: (!reason.is_empty()).then(|| reason.to_owned()),
                ..Reply::default()
            });
        }
        Some(code) => {
            let why = last_line(stderr);
            return Err(if why.is_empty() {
                format!("exited with status {code}")
            } else {
                format!("exited with status {code}: {why}")
            });
        }
        None => return Err("was killed".into()),
    }
    let text = stdout.trim();
    if text.is_empty() {
        return Ok(Reply::default());
    }
    if !text.starts_with('{') {
        // Plain text is context, where the event takes some.
        return Ok(Reply {
            context: event.takes_context().then(|| text.to_owned()),
            ..Reply::default()
        });
    }
    let v: Value = serde_json::from_str(text)
        .map_err(|e| format!("answered with JSON that does not parse ({e})"))?;
    let string = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|s| !s.trim().is_empty())
    };
    let message = string("message");
    let decision = match v.get("decision").and_then(Value::as_str) {
        None => None,
        Some("allow") => Some(Verdict::Allow),
        Some("ask") => Some(Verdict::Ask),
        Some("deny") => Some(Verdict::Deny(message.clone().unwrap_or_default())),
        Some(other) => {
            return Err(format!(
                "answered with decision `{other}`; use allow, deny or ask"
            ));
        }
    };
    let updated_args = match v.get("updated_args") {
        None | Some(Value::Null) => None,
        Some(a @ Value::Object(_)) => Some(a.clone()),
        Some(_) => return Err("answered with `updated_args` that is not an object".into()),
    };
    Ok(Reply {
        decision,
        message,
        context: string("additional_context"),
        updated_args,
    })
}

/// At most about `max` tokens of `text`, cut at a line where possible.
fn cap(text: &str, max: u32) -> String {
    let n = tokens::estimate(text);
    if n <= max {
        return text.to_owned();
    }
    let mut end = (text.len() as u64 * u64::from(max) / u64::from(n)) as usize;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let end = text[..end]
        .rfind('\n')
        .filter(|i| *i > end / 2)
        .unwrap_or(end);
    format!(
        "{}\n[… cut: the hook returned about {n} tokens, more than {max}]",
        &text[..end]
    )
}

/// Program, arguments, directory and environment, with placeholders and secrets resolved.
fn prepare(file: &HookFile, dir: &Path, root: &Path) -> Result<Prepared, String> {
    let get = |k: &str| std::env::var_os(k);
    let cwd = match file.cwd.as_str() {
        "" => root.to_path_buf(),
        c => root.join(quack_config::expand_home(&expand(c, dir), &get)),
    };
    let mut env = quack_shell::inherited_env(file.inherit_env.as_deref());
    for (k, secret) in &file.env {
        let v = secret
            .resolve(&get)
            .map_err(|e| format!("env `{k}`: {e}"))?;
        env.insert(k.clone(), v.into());
    }
    let search = env.get("PATH").cloned().or_else(|| get("PATH"));
    let mut words = file.command.iter().map(|w| {
        let w = expand(w, dir);
        if w.starts_with("~/") || w.starts_with("~\\") {
            quack_config::expand_home(&w, &get).display().to_string()
        } else {
            w
        }
    });
    let first = words.next().unwrap_or_default();
    let program = if first == "{python}" {
        python(search.as_deref(), &cwd)?
    } else {
        quack_shell::resolve_program(&first, search.as_deref(), &cwd)?
    };
    Ok(Prepared {
        program,
        args: words.collect(),
        cwd,
        env,
    })
}

/// `/doctor`: why the hook's program or a `{dir}` file it names cannot be found. Resolves
/// no secrets and starts nothing.
pub(crate) fn command_problem(file: &HookFile, dir: &Path, root: &Path) -> Option<String> {
    let search = std::env::var_os("PATH");
    let first = file.command.first()?;
    let found = if first == "{python}" {
        python(search.as_deref(), root).map(|_| ())
    } else {
        let program = expand(first, dir);
        let program = quack_config::expand_home(&program, &|k| std::env::var_os(k));
        quack_shell::resolve_program(&program.to_string_lossy(), search.as_deref(), root)
            .map(|_| ())
            .map_err(|e| format!("cannot find {e}"))
    };
    if let Err(e) = found {
        return Some(e);
    }
    file.command
        .iter()
        .filter(|w| w.contains("{dir}"))
        .map(|w| PathBuf::from(expand(w, dir)))
        .find(|p| !p.exists())
        .map(|p| format!("{} does not exist", p.display()))
}

/// `{dir}` → the hook file's folder.
fn expand(word: &str, dir: &Path) -> String {
    word.replace("{dir}", &dir.display().to_string())
}

/// The Python on `PATH`. Windows has the `py` launcher, and a `python3` that may only
/// open the Store.
fn python(search: Option<&std::ffi::OsStr>, cwd: &Path) -> Result<PathBuf, String> {
    let names: &[&str] = if cfg!(windows) {
        &["py", "python", "python3"]
    } else {
        &["python3", "python"]
    };
    names
        .iter()
        .find_map(|n| quack_shell::resolve_program(n, search, cwd).ok())
        .ok_or_else(|| {
            format!(
                "`{{python}}`: none of {} is on PATH; install Python or name the program",
                names.join(", ")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies() {
        let ev = HookEvent::UserPrompt;
        assert_eq!(parse_reply(Some(0), "", "", ev), Ok(Reply::default()));
        assert_eq!(
            parse_reply(Some(0), "issue PAY-1: broken\n", "", ev)
                .expect("ok")
                .context
                .as_deref(),
            Some("issue PAY-1: broken")
        );
        // Plain text where the event takes no context is ignored.
        assert_eq!(
            parse_reply(Some(0), "hello", "", HookEvent::TurnEnd),
            Ok(Reply::default())
        );
        let r = parse_reply(Some(2), "", "no prod databases\n", HookEvent::PreTool).expect("ok");
        assert_eq!(r.decision, Some(Verdict::Deny("no prod databases".into())));
        let r = parse_reply(
            Some(0),
            r#"{"decision":"deny","message":"use staging"}"#,
            "",
            HookEvent::PreTool,
        )
        .expect("ok");
        assert_eq!(r.decision, Some(Verdict::Deny("use staging".into())));
        let r = parse_reply(
            Some(0),
            r#"{"additional_context":"ctx","updated_args":{"command":"ls"}}"#,
            "",
            HookEvent::PreTool,
        )
        .expect("ok");
        assert_eq!(r.context.as_deref(), Some("ctx"));
        assert_eq!(r.updated_args, Some(json!({"command": "ls"})));
        assert!(parse_reply(Some(0), "{nope", "", ev).is_err());
        assert!(parse_reply(Some(0), r#"{"decision":"block"}"#, "", ev).is_err());
        assert!(
            parse_reply(Some(1), "", "Traceback\nKeyError: 'x'\n", ev)
                .is_err_and(|e| e.ends_with("KeyError: 'x'"))
        );
    }

    #[test]
    fn strictest_verdict_wins() {
        let mut o = Outcome::default();
        o.decide(Verdict::Allow, "a");
        o.decide(Verdict::Ask, "b");
        o.decide(Verdict::Allow, "c");
        assert_eq!(o.verdict, Some((Verdict::Ask, "b".into())));
        o.decide(Verdict::Deny("x".into()), "d");
        assert_eq!(o.denied(), Some(("d", "x")));
    }

    #[test]
    fn long_context_is_cut() {
        let text = "line of context\n".repeat(10_000);
        let cut = cap(&text, 100);
        assert!(tokens::estimate(&cut) < 150, "{}", tokens::estimate(&cut));
        assert!(cut.ends_with("more than 100]"));
        assert_eq!(cap("short", 100), "short");
    }

    #[test]
    fn placeholders() {
        let dir = Path::new("hooks.d");
        assert_eq!(
            expand("{dir}/jira/jira.py", dir),
            format!("{}/jira/jira.py", dir.display())
        );
    }
}
