//! `quack eval`: run a task suite against one model and profile
//! (docs/plan/13-testing-and-evals.md#quack-eval).
//!
//! A task is a directory: `task.toml` (the prompt and the checks) and `repo/` (the files
//! the model starts with). Each run gets a fresh copy of `repo/` and its own data
//! directory, so sessions and memory never mix with the user's. The user's providers,
//! settings and AGENTS.md still apply: that is the setup being measured.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use quack_config::{LoadOptions, load_with};
use quack_core::Engine;
use quack_protocol::{ApprovalDecision, Event, InputItem, Op, StatsReport, TurnOverrides};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;

/// `task.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFile {
    /// `navigate`, `single-edit`, `multi-file`, `fix-test`, `shell`, `long-context`, …
    pub category: String,
    pub prompt: String,
    /// Programs that must be on PATH, else the task is skipped. `python` is satisfied by
    /// `python3` or `python`, and `{python}` in commands becomes the one found.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(rename = "check", default)]
    pub checks: Vec<Check>,
}

fn default_timeout() -> u64 {
    600
}

/// One condition; every check of a task must hold. Each sets one of its fields.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Check {
    /// With `contains`, `not_contains` or `equals`: a file in the workspace.
    pub file: Option<String>,
    pub contains: Option<String>,
    pub not_contains: Option<String>,
    pub equals: Option<String>,
    pub exists: Option<String>,
    pub missing: Option<String>,
    /// The final answer contains this (case-insensitive).
    pub answer_contains: Option<String>,
    /// A command in the workspace that must exit 0.
    pub command: Option<String>,
    /// The model called this tool (internal name, e.g. `shell`).
    pub tool_called: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub dir: PathBuf,
    pub file: TaskFile,
}

/// Every task under `suite`, sorted by id; `only` keeps the named ones.
pub fn load_tasks(suite: &Path, only: &[String]) -> Result<Vec<Task>, String> {
    let entries = std::fs::read_dir(suite)
        .map_err(|e| format!("cannot read the task suite {}: {e}", suite.display()))?;
    let mut tasks = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let toml_path = dir.join("task.toml");
        if !toml_path.is_file() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        if !only.is_empty() && !only.contains(&id) {
            continue;
        }
        let text = std::fs::read_to_string(&toml_path)
            .map_err(|e| format!("{}: {e}", toml_path.display()))?;
        let file: TaskFile =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", toml_path.display()))?;
        tasks.push(Task { id, dir, file });
    }
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(missing) = only.iter().find(|o| !tasks.iter().any(|t| &t.id == *o)) {
        return Err(format!("no task `{missing}` in {}", suite.display()));
    }
    Ok(tasks)
}

/// What one run of one task did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunResult {
    pub task: String,
    pub category: String,
    pub passed: bool,
    /// Set when the task could not run here (a missing program) or the run broke.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The checks that failed, as text.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed_checks: Vec<String>,
    /// Model requests.
    pub steps: u32,
    pub tool_calls: u32,
    pub tool_errors: u32,
    /// Calls whose arguments were not JSON or that named no offered tool.
    pub parse_failures: u32,
    /// Edit calls that failed (no match, ambiguous, stale).
    pub edit_failures: u32,
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub completion_tokens: u64,
    pub compactions: u32,
    pub seconds: f64,
    /// The model and profile the run used, as the engine reported them.
    #[serde(skip)]
    pub model: String,
    #[serde(skip)]
    pub profile: String,
}

/// The whole run: what was measured, and each task's result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub harness: String,
    pub date: String,
    pub model: String,
    pub profile: String,
    pub results: Vec<RunResult>,
}

impl Report {
    /// `(passed, ran)`; skipped runs are not counted.
    pub fn score(&self) -> (usize, usize) {
        let ran: Vec<&RunResult> = self
            .results
            .iter()
            .filter(|r| r.skipped.is_none())
            .collect();
        (ran.iter().filter(|r| r.passed).count(), ran.len())
    }
}

#[derive(Debug, Clone)]
pub struct EvalOptions {
    /// What the user started quack with: `-m`, `-c` and `--profile` apply to every run.
    pub load: LoadOptions,
    pub repeat: u32,
    /// Keep each run's workspace for inspection.
    pub keep: bool,
    /// Where the runs' workspaces go.
    pub scratch: PathBuf,
    /// Variables that replace the process environment's (tests isolate config this way).
    pub env: HashMap<String, OsString>,
}

/// The environment with the data directory moved to `data`.
fn env_with_data(
    data: PathBuf,
    vars: &HashMap<String, OsString>,
) -> impl Fn(&str) -> Option<OsString> {
    move |k: &str| match k {
        "QUACK_DATA_DIR" => Some(data.clone().into_os_string()),
        _ => vars.get(k).cloned().or_else(|| std::env::var_os(k)),
    }
}

/// Copy a directory tree.
pub fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: Vec<String> = if cfg!(windows) {
        vec![".exe".into(), ".cmd".into(), ".bat".into(), String::new()]
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path).any(|dir| {
        exts.iter()
            .any(|e| dir.join(format!("{program}{e}")).is_file())
    })
}

/// The program names to substitute in commands, or the first requirement not met.
fn requirements(task: &TaskFile) -> Result<HashMap<String, String>, String> {
    let mut found = HashMap::new();
    for r in &task.requires {
        let candidates: &[&str] = match r.as_str() {
            "python" => &["python3", "python"],
            other => &[other],
        };
        match candidates.iter().find(|c| on_path(c)) {
            Some(c) => {
                found.insert(r.clone(), (*c).to_owned());
            }
            None => return Err(format!("needs `{r}` on PATH")),
        }
    }
    Ok(found)
}

fn run_command(cmd: &str, dir: &Path) -> Result<(), String> {
    let mut c = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    let out = c
        .current_dir(dir)
        .output()
        .map_err(|e| format!("`{cmd}` did not start: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let tail = |b: &[u8]| {
        let s = String::from_utf8_lossy(b);
        let lines: Vec<&str> = s.lines().collect();
        lines[lines.len().saturating_sub(8)..].join("\n")
    };
    Err(format!(
        "`{cmd}` failed ({}):\n{}{}",
        out.status,
        tail(&out.stdout),
        tail(&out.stderr)
    ))
}

/// Normalized file text: `\n` newlines, so checks behave the same on Windows.
fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|t| t.replace("\r\n", "\n"))
}

/// Run the checks; returns the ones that failed.
fn check(
    checks: &[Check],
    ws: &Path,
    answer: &str,
    tools: &[String],
    programs: &HashMap<String, String>,
) -> Vec<String> {
    let mut failed = Vec::new();
    for c in checks {
        if let Some(f) = &c.file {
            let text = read_text(&ws.join(f));
            let Some(text) = text else {
                failed.push(format!("{f} is missing"));
                continue;
            };
            if let Some(want) = &c.contains
                && !text.contains(want.as_str())
            {
                failed.push(format!("{f} does not contain {want:?}"));
            }
            if let Some(bad) = &c.not_contains
                && text.contains(bad.as_str())
            {
                failed.push(format!("{f} still contains {bad:?}"));
            }
            if let Some(want) = &c.equals
                && text.trim_end() != want.trim_end()
            {
                failed.push(format!("{f} is not {want:?}"));
            }
        }
        if let Some(p) = &c.exists
            && !ws.join(p).exists()
        {
            failed.push(format!("{p} was not created"));
        }
        if let Some(p) = &c.missing
            && ws.join(p).exists()
        {
            failed.push(format!("{p} still exists"));
        }
        if let Some(want) = &c.answer_contains
            && !answer.to_lowercase().contains(&want.to_lowercase())
        {
            failed.push(format!("the answer does not mention {want:?}"));
        }
        if let Some(t) = &c.tool_called
            && !tools.iter().any(|n| n == t)
        {
            failed.push(format!("`{t}` was never called"));
        }
        if let Some(cmd) = &c.command {
            let mut cmd = cmd.clone();
            for (name, program) in programs {
                cmd = cmd.replace(&format!("{{{name}}}"), program);
            }
            if let Err(e) = run_command(&cmd, ws) {
                failed.push(e);
            }
        }
    }
    failed
}

/// Check a workspace against a task, as a run would after the model finishes. `Err` when
/// the task can't be checked here (a program it needs is missing).
pub fn check_workspace(
    task: &Task,
    ws: &Path,
    answer: &str,
    tools: &[String],
) -> Result<Vec<String>, String> {
    let programs = requirements(&task.file)?;
    Ok(check(&task.file.checks, ws, answer, tools, &programs))
}

/// The internal tool behind a call, from the transcript summary's first word.
fn internal_tool(summary: &str) -> String {
    match summary.split_whitespace().next().unwrap_or_default() {
        "$" => "shell".into(),
        "patch" => "apply_patch".into(),
        "list" => "list_dir".into(),
        other => other.to_owned(),
    }
}

fn is_parse_failure(summary: &str) -> bool {
    [
        "arguments are not valid JSON",
        "arguments must be a JSON object",
        "unknown tool `",
    ]
    .iter()
    .any(|p| summary.starts_with(p))
}

/// Run one task once.
pub async fn run_task(task: &Task, opts: &EvalOptions, run: u32) -> RunResult {
    let mut result = RunResult {
        task: task.id.clone(),
        category: task.file.category.clone(),
        ..RunResult::default()
    };
    let programs = match requirements(&task.file) {
        Ok(p) => p,
        Err(why) => {
            result.skipped = Some(why);
            return result;
        }
    };
    let root = opts.scratch.join(format!("{}-{run}", task.id));
    let _ = std::fs::remove_dir_all(&root);
    let ws = root.join("ws");
    if let Err(e) = copy_dir(&task.dir.join("repo"), &ws) {
        result.skipped = Some(format!("cannot copy repo/: {e}"));
        return result;
    }
    let mut load = opts.load.clone();
    load.cwd = ws.clone();
    load.overrides.extend([
        "permissions.mode=\"full-auto\"".to_owned(),
        // Memory stays inside the run.
        format!(
            "memory.path={}",
            serde_json::Value::String(root.join("memory").display().to_string())
        ),
    ]);
    let env = env_with_data(root.join("data"), &opts.env);
    let loaded = match load_with(&env, &load) {
        Ok(l) => l,
        Err(e) => {
            result.error = Some(format!("config: {e}"));
            return result;
        }
    };
    let started = Instant::now();
    let mut engine = Engine::spawn(Arc::new(loaded));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(task.file.timeout_secs);
    let mut answer = String::new();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut tools: Vec<String> = Vec::new();
    let mut sent = false;
    let mut interrupted = false;
    loop {
        let event = match tokio::time::timeout_at(deadline, engine.events.recv()).await {
            Ok(Ok(e)) => e,
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => {
                result.error = Some("the engine stopped".into());
                break;
            }
            Err(_) if !interrupted => {
                interrupted = true;
                result.error = Some(format!("timed out after {}s", task.file.timeout_secs));
                engine.submit(Op::Interrupt).await;
                continue;
            }
            Err(_) => break,
        };
        match event {
            Event::StartupComplete if !sent => {
                sent = true;
                let op = Op::UserTurn {
                    items: vec![InputItem::text(task.file.prompt.clone())],
                    overrides: TurnOverrides::default(),
                };
                engine.submit(op).await;
            }
            Event::StartupProgress {
                subsystem: quack_protocol::Subsystem::Profile,
                state: quack_protocol::LoadState::Ready { detail },
            } => {
                result.profile = detail.split(' ').next().unwrap_or_default().to_owned();
            }
            Event::AssistantDelta { text } => answer.push_str(&text),
            // Text tool calls taken out of the reply: the rewrite is the whole reply.
            Event::AssistantRewrite { text } => answer = text,
            Event::ToolCallBegin { call } => {
                answer.clear();
                names.insert(call.call_id.0.clone(), call.summary.clone());
            }
            Event::ToolCallEnd { call_id, result: r } => {
                let summary = names.remove(&call_id.0).unwrap_or_default();
                let tool = internal_tool(&summary);
                result.tool_calls += 1;
                if !r.ok {
                    result.tool_errors += 1;
                    if is_parse_failure(&r.summary) {
                        result.parse_failures += 1;
                    } else if matches!(tool.as_str(), "edit" | "apply_patch" | "write") {
                        result.edit_failures += 1;
                    }
                }
                tools.push(tool);
            }
            Event::ApprovalRequest { call_id, .. } => {
                let decision = ApprovalDecision::DenyWithFeedback {
                    feedback: "this is an unattended evaluation run; nobody can approve this"
                        .into(),
                };
                engine
                    .submit(Op::ApprovalResponse { call_id, decision })
                    .await;
            }
            Event::Error { error } if result.error.is_none() => {
                result.error = Some(error.message);
            }
            Event::TurnComplete { .. } | Event::TurnAborted { .. } => break,
            _ => {}
        }
    }
    if let Some(stats) = stats(&mut engine).await {
        result.model = stats.model;
        result.steps = stats.requests;
        result.prompt_tokens = stats.prompt_tokens;
        result.cached_tokens = stats.cached_tokens;
        result.completion_tokens = stats.completion_tokens;
        result.compactions = stats.compactions;
    }
    engine.shutdown().await;
    result.seconds = started.elapsed().as_secs_f64();
    if result.error.is_none() || interrupted {
        result.failed_checks = check(&task.file.checks, &ws, &answer, &tools, &programs);
        result.passed = result.error.is_none() && result.failed_checks.is_empty();
    }
    if !opts.keep {
        let _ = std::fs::remove_dir_all(&root);
    }
    result
}

async fn stats(engine: &mut quack_core::EngineHandle) -> Option<StatsReport> {
    engine.submit(Op::GetStats).await;
    let wait = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(wait, engine.events.recv()).await {
            Ok(Ok(Event::Stats { report })) => return Some(report),
            Ok(Ok(_) | Err(RecvError::Lagged(_))) => {}
            _ => return None,
        }
    }
}

/// Today as `YYYY-MM-DD` (UTC).
pub fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400);
    let days = i64::try_from(days).unwrap_or(0);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A Markdown table of the results, for the terminal and for `docs/compat.md`.
pub fn table(report: &Report) -> String {
    let mut out = String::from(
        "| task | category | result | steps | tool errors | parse | edit | tokens (cached) | time |\n\
         |---|---|---|---|---|---|---|---|---|\n",
    );
    for r in &report.results {
        let outcome = match (&r.skipped, r.passed) {
            (Some(why), _) => format!("skipped: {why}"),
            (None, true) => "pass".into(),
            (None, false) => "FAIL".into(),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} ({}) | {:.0}s |\n",
            r.task,
            r.category,
            outcome,
            r.steps,
            r.tool_errors,
            r.parse_failures,
            r.edit_failures,
            r.prompt_tokens + r.completion_tokens,
            r.cached_tokens,
            r.seconds
        ));
    }
    let (passed, ran) = report.score();
    out.push_str(&format!(
        "\n{} · profile {} · {passed}/{ran} passed\n",
        report.model, report.profile
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        let d = today();
        assert_eq!(d.len(), 10, "{d}");
        assert!(d.starts_with("20"), "{d}");
    }

    #[test]
    fn checks_files_answers_and_tools() {
        let tmp = std::env::temp_dir().join(format!("quack-eval-check-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("mkdir");
        std::fs::write(tmp.join("a.txt"), "one\r\ntwo\r\n").expect("write");
        let checks: Vec<Check> = toml::from_str::<TaskFile>(
            r#"
            category = "x"
            prompt = "p"
            [[check]]
            file = "a.txt"
            contains = "one\ntwo"
            not_contains = "three"
            [[check]]
            file = "a.txt"
            equals = "one\ntwo"
            [[check]]
            answer_contains = "SEVEN"
            [[check]]
            missing = "b.txt"
            [[check]]
            tool_called = "read"
            "#,
        )
        .expect("task")
        .checks;
        let none = HashMap::new();
        assert!(check(&checks, &tmp, "it is seven", &["read".into()], &none).is_empty());
        let failed = check(&checks, &tmp, "no", &[], &none);
        assert_eq!(failed.len(), 2, "{failed:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
