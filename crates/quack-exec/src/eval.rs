//! `quack eval`: run a task suite against one model and profile
//! (docs/plan/13-testing-and-evals.md#quack-eval).
//!
//! A task is a directory: `task.toml` (the prompt and the checks) and `repo/` (the files
//! the model starts with, laid over a shared `base` project when the task names one). Each
//! run gets a fresh copy and its own data directory, so sessions and memory never mix with
//! the user's. `hidden/` (tests the model never sees) is copied in only once the model is
//! done. The user's providers, settings and AGENTS.md still apply: that is the setup being
//! measured.

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
    /// A project folder, relative to the task's, copied in before `repo/` (which then holds
    /// only the files this task changes), so several tasks can share one realistic codebase.
    #[serde(default)]
    pub base: Option<PathBuf>,
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

impl Task {
    /// Lay out the files the model starts with: `base`, then `repo/` over it.
    pub fn prepare(&self, ws: &Path) -> std::io::Result<()> {
        if let Some(base) = &self.file.base {
            copy_dir(&self.dir.join(base), ws)?;
        }
        let repo = self.dir.join("repo");
        if repo.is_dir() || self.file.base.is_none() {
            copy_dir(&repo, ws)?;
        }
        Ok(())
    }

    /// Copy `hidden/` into a workspace the model is done with, for the checks to use.
    pub fn reveal(&self, ws: &Path) -> std::io::Result<()> {
        let hidden = self.dir.join("hidden");
        if hidden.is_dir() {
            copy_dir(&hidden, ws)?;
        }
        Ok(())
    }

    /// The paths the reference solution touches (`solution/` files and `DELETE` lines),
    /// with `/` separators. Empty for a task that only asks a question.
    pub fn solution_paths(&self) -> Vec<String> {
        let solution = self.dir.join("solution");
        let mut paths: Vec<String> = files_under(&solution)
            .into_iter()
            .filter(|p| p != "ANSWER" && p != "DELETE")
            .collect();
        if let Ok(text) = std::fs::read_to_string(solution.join("DELETE")) {
            paths.extend(
                text.lines()
                    .map(|l| l.trim().replace('\\', "/"))
                    .filter(|l| !l.is_empty()),
            );
        }
        paths.sort();
        paths.dedup();
        paths
    }
}

/// Folders that tools create as they run; changes in them are not the model's work.
const SCRATCH_DIRS: &[&str] = &[
    "__pycache__",
    ".pytest_cache",
    "target",
    "node_modules",
    ".git",
];

/// Every file under `root`, relative and with `/` separators, skipping `SCRATCH_DIRS`.
fn files_under(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            match entry.file_type() {
                Ok(t) if t.is_dir() => {
                    if !SCRATCH_DIRS.contains(&name.as_str()) {
                        walk(&entry.path(), &rel, out);
                    }
                }
                Ok(_) => out.push(rel),
                Err(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out.sort();
    out
}

/// The files that differ between two trees: changed, added or removed.
fn changed_files(before: &Path, after: &Path) -> Vec<String> {
    let a = files_under(before);
    let b = files_under(after);
    let mut out: Vec<String> = a.iter().filter(|p| !b.contains(p)).cloned().collect();
    out.extend(
        b.iter()
            .filter(|p| {
                !a.contains(p)
                    || std::fs::read(before.join(p)).ok() != std::fs::read(after.join(p)).ok()
            })
            .cloned(),
    );
    out.sort();
    out
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
    /// Why a failed run failed, as one label (see `Failure`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    /// How the run went about it, passed or not (see `Process`). A pass with any of these
    /// may owe more to luck than to the setup. Always written, so results from before it
    /// existed (no key) are told apart from clean runs (`[]`).
    #[serde(default)]
    pub process: Vec<Process>,
    /// The files the model changed, added or removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<String>,
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
    /// Tool results replaced by a placeholder to save room.
    #[serde(default)]
    pub pruned: u32,
    /// The largest single request's prompt tokens: how full the context got.
    #[serde(default)]
    pub peak_prompt: u32,
    pub seconds: f64,
    /// US dollars, when the model has a price (configured or listed by the provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// The model and profile the run used, as the engine reported them.
    #[serde(skip)]
    pub model: String,
    #[serde(skip)]
    pub profile: String,
    #[serde(skip)]
    pub sampling: Vec<(String, f64)>,
}

/// The whole run: what was measured, and each task's result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub harness: String,
    pub date: String,
    /// The suite's folder name (`tasks`, `hard`, ...).
    #[serde(default = "default_suite")]
    pub suite: String,
    pub model: String,
    pub profile: String,
    /// The model that planned each task (`--planner`), when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner: Option<String>,
    /// The `[context] window` the runs were held to, when one was set: a small-window run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
    /// The sampling settings sent (`temperature`, ...); empty: the server's defaults.
    #[serde(default)]
    pub sampling: serde_json::Map<String, serde_json::Value>,
    pub results: Vec<RunResult>,
}

fn default_suite() -> String {
    "tasks".into()
}

impl Report {
    /// `sampling` from a run's settings: whole numbers (`top_k`) as integers.
    pub fn sampling_of(pairs: &[(String, f64)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| {
                #[allow(clippy::cast_possible_truncation)]
                let n = if v.fract() == 0.0 && v.abs() < 1e15 {
                    serde_json::Value::from(*v as i64)
                } else {
                    serde_json::Value::from(*v)
                };
                (k.clone(), n)
            })
            .collect()
    }

    /// `(passed, ran)`; skipped runs are not counted.
    pub fn score(&self) -> (usize, usize) {
        let ran: Vec<&RunResult> = self
            .results
            .iter()
            .filter(|r| r.skipped.is_none())
            .collect();
        (ran.iter().filter(|r| r.passed).count(), ran.len())
    }

    /// What the priced runs cost in all, in US dollars. `None` when none was priced.
    pub fn cost(&self) -> Option<f64> {
        self.results
            .iter()
            .filter_map(|r| r.cost)
            .reduce(|a, b| a + b)
    }
}

#[derive(Debug, Clone)]
pub struct EvalOptions {
    /// What the user started quack with: `-m`, `-c` and `--profile` apply to every run.
    pub load: LoadOptions,
    pub repeat: u32,
    /// Keep each run's workspace for inspection.
    pub keep: bool,
    /// Plan with this model first, then accept the plan (`[model] planner`).
    pub planner: Option<String>,
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

/// Check a workspace against a task, as a run would after the model finishes: `hidden/`
/// goes in first. `Err` when the task can't be checked here (a program it needs is missing).
pub fn check_workspace(
    task: &Task,
    ws: &Path,
    answer: &str,
    tools: &[String],
) -> Result<Vec<String>, String> {
    let programs = requirements(&task.file)?;
    task.reveal(ws)
        .map_err(|e| format!("cannot copy hidden/: {e}"))?;
    Ok(check(&task.file.checks, ws, answer, tools, &programs))
}

/// The internal tool behind a call, from the transcript summary's first word.
fn internal_tool(summary: &str) -> String {
    match summary.split_whitespace().next().unwrap_or_default() {
        "$" => "shell".into(),
        "patch" => "apply_patch".into(),
        "list" => "list_dir".into(),
        "ask" => "ask_user".into(),
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

/// Why a run failed, read from what the runner saw, so a set of results says what to
/// change in the harness rather than only how often it fails. The first that applies wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Failure {
    /// The task's time ran out.
    Timeout,
    /// The provider or engine failed (a request error, the engine stopping).
    Error,
    /// Tool calls the model sent did not parse; the profile's tool shapes don't suit it.
    BadCalls,
    /// It meant to change files but its edits failed, and it never got them in.
    EditFailed,
    /// The task needed changes and it made none.
    NoChange,
    /// It changed files, but none of the ones the solution changes.
    WrongFiles,
    /// It changed some of the solution's files and missed others.
    MissedFiles,
    /// Only the reply's checks failed: a wrong answer.
    WrongAnswer,
    /// It changed the right files without ever running a command, and the result is wrong.
    Untested,
    /// It changed the right files and the result is still wrong.
    WrongFix,
}

impl Failure {
    pub fn label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Error => "error",
            Self::BadCalls => "bad-calls",
            Self::EditFailed => "edit-failed",
            Self::NoChange => "no-change",
            Self::WrongFiles => "wrong-files",
            Self::MissedFiles => "missed-files",
            Self::WrongAnswer => "wrong-answer",
            Self::Untested => "untested",
            Self::WrongFix => "wrong-fix",
        }
    }
}

/// What `classify` reads about a finished run.
/// A habit the runner saw in a run's tool calls that makes its result less trustworthy,
/// whether it passed or not: a pass that never checked its last change, or got there by
/// rerunning failures, may not pass again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Process {
    /// It ran no command after its last successful edit, so the final change went unchecked.
    Unverified,
    /// It ran a command that had just failed again, unchanged, with no edit in between.
    BlindRetry,
    /// A command that had succeeded failed later: a change broke what had worked.
    Regression,
}

impl Process {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::BlindRetry => "blind-retry",
            Self::Regression => "regression",
        }
    }
}

/// One tool call as the runner saw it: the internal tool, its summary (a shell call's is
/// `$ <command>`) and whether it succeeded (a command: exit code 0).
#[derive(Debug, Clone)]
pub struct Call {
    pub tool: String,
    pub summary: String,
    pub ok: bool,
}

/// The process labels for a run's calls, in `Process` order.
pub fn process(calls: &[Call]) -> Vec<Process> {
    let edit = |c: &Call| c.ok && matches!(c.tool.as_str(), "edit" | "apply_patch" | "write");
    let shell = |c: &Call| c.tool == "shell";
    let mut out = Vec::new();
    if let Some(last) = calls.iter().rposition(edit)
        && !calls[last + 1..].iter().any(shell)
    {
        out.push(Process::Unverified);
    }
    // The last result of each command since the last edit, and whether it ever succeeded.
    let mut since_edit: HashMap<&str, bool> = HashMap::new();
    let mut succeeded: HashMap<&str, bool> = HashMap::new();
    let (mut blind, mut regressed) = (false, false);
    for c in calls {
        if edit(c) {
            since_edit.clear();
            continue;
        }
        if !shell(c) {
            continue;
        }
        let cmd = c.summary.as_str();
        blind |= !c.ok && since_edit.get(cmd) == Some(&false);
        regressed |= !c.ok && succeeded.get(cmd) == Some(&true);
        since_edit.insert(cmd, c.ok);
        if c.ok {
            succeeded.insert(cmd, true);
        }
    }
    if blind {
        out.push(Process::BlindRetry);
    }
    if regressed {
        out.push(Process::Regression);
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct Outcome<'a> {
    pub timed_out: bool,
    pub error: bool,
    pub parse_failures: u32,
    pub edit_failures: u32,
    pub tool_calls: u32,
    /// The files the model changed.
    pub changed: &'a [String],
    /// The files the reference solution changes.
    pub solution: &'a [String],
    /// Only checks on the reply failed.
    pub only_answer_failed: bool,
    /// The model ran a shell command.
    pub ran_commands: bool,
}

/// The label for a failed run (see `Failure`).
pub fn classify(o: &Outcome<'_>) -> Failure {
    if o.timed_out {
        return Failure::Timeout;
    }
    if o.error {
        return Failure::Error;
    }
    // Half or more of its calls broken: nothing else it did can be read as a choice.
    if o.parse_failures > 0 && o.parse_failures * 2 >= o.tool_calls {
        return Failure::BadCalls;
    }
    if o.only_answer_failed || o.solution.is_empty() {
        return Failure::WrongAnswer;
    }
    let hit = o.solution.iter().filter(|p| o.changed.contains(p)).count();
    if hit < o.solution.len() && o.edit_failures > 0 {
        return Failure::EditFailed;
    }
    if o.changed.is_empty() {
        return Failure::NoChange;
    }
    if hit == 0 {
        return Failure::WrongFiles;
    }
    if hit < o.solution.len() {
        return Failure::MissedFiles;
    }
    if o.ran_commands {
        Failure::WrongFix
    } else {
        Failure::Untested
    }
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
    // An untouched copy, to tell what the model changed.
    let start = root.join("start");
    if let Err(e) = task.prepare(&ws).and_then(|()| task.prepare(&start)) {
        result.skipped = Some(format!("cannot copy the task's files: {e}"));
        return result;
    }
    let mut load = opts.load.clone();
    load.cwd = ws.clone();
    if let Some(p) = &opts.planner {
        load.overrides.push(format!(
            "model.planner={}",
            serde_json::Value::String(p.clone())
        ));
    }
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
    let mut calls: Vec<Call> = Vec::new();
    let mut sent = false;
    // With a planner, the first turn is in plan mode and accepting it starts the second.
    let mut planning = opts.planner.is_some();
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
                if planning {
                    let mode = "plan".to_owned();
                    engine.submit(Op::SetPermissionMode { mode }).await;
                }
                let op = Op::UserTurn {
                    items: vec![InputItem::text(task.file.prompt.clone())],
                    overrides: TurnOverrides::default(),
                };
                engine.submit(op).await;
            }
            Event::StartupProgress {
                subsystem: quack_protocol::Subsystem::Profile,
                state: quack_protocol::LoadState::Ready { detail },
            } if result.profile.is_empty() => {
                // The session model's profile; a planner's comes later.
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
                calls.push(Call {
                    tool: tool.clone(),
                    summary,
                    ok: r.ok,
                });
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
            Event::Questions { call_id, .. } => {
                let answer = Op::AnswerQuestions {
                    call_id,
                    answers: Vec::new(),
                    stop_asking: true,
                };
                engine.submit(answer).await;
            }
            Event::Error { error } if result.error.is_none() => {
                result.error = Some(error.message);
            }
            Event::TurnComplete { .. } if planning && !interrupted => {
                planning = false;
                answer.clear();
                let mode = "full-auto".to_owned();
                engine.submit(Op::AcceptPlan { mode }).await;
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
        result.pruned = stats.pruned;
        result.peak_prompt = stats.history.iter().map(|r| r.prompt).max().unwrap_or(0);
        result.cost = stats.cost;
        result.sampling = stats.sampling;
    }
    engine.shutdown().await;
    result.seconds = started.elapsed().as_secs_f64();
    result.changed = changed_files(&start, &ws);
    result.process = process(&calls);
    let mut only_answer_failed = false;
    if result.error.is_none() || interrupted {
        if let Err(e) = task.reveal(&ws) {
            result.error = Some(format!("cannot copy hidden/: {e}"));
        }
        let (answers, others): (Vec<Check>, Vec<Check>) = task
            .file
            .checks
            .iter()
            .cloned()
            .partition(|c| c.answer_contains.is_some());
        let others = check(&others, &ws, &answer, &tools, &programs);
        let answers = check(&answers, &ws, &answer, &tools, &programs);
        only_answer_failed = others.is_empty() && !answers.is_empty();
        result.failed_checks = others.into_iter().chain(answers).collect();
        result.passed = result.error.is_none() && result.failed_checks.is_empty();
    }
    if !result.passed {
        let solution = task.solution_paths();
        result.failure = Some(classify(&Outcome {
            timed_out: interrupted,
            error: result.error.is_some(),
            parse_failures: result.parse_failures,
            edit_failures: result.edit_failures,
            tool_calls: result.tool_calls,
            changed: &result.changed,
            solution: &solution,
            only_answer_failed,
            ran_commands: tools.iter().any(|t| t == "shell"),
        }));
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

/// `path`, or `name-2.json`, `name-3.json`, ... beside it when it already exists, so a
/// rerun never overwrites an earlier result.
pub fn unused_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| path.with_file_name(format!("{stem}-{n}{ext}")))
        .find(|p| !p.exists())
        .expect("some numbered name is free")
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
        "| task | category | result | steps | tool errors | parse | edit | tokens (cached) | cost | time |\n\
         |---|---|---|---|---|---|---|---|---|---|\n",
    );
    for r in &report.results {
        let outcome = match (&r.skipped, r.passed) {
            (Some(why), _) => format!("skipped: {why}"),
            (None, true) => "pass".into(),
            (None, false) => match r.failure {
                Some(f) => format!("FAIL ({})", f.label()),
                None => "FAIL".into(),
            },
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} ({}) | {} | {:.0}s |\n",
            r.task,
            r.category,
            outcome,
            r.steps,
            r.tool_errors,
            r.parse_failures,
            r.edit_failures,
            r.prompt_tokens + r.completion_tokens,
            r.cached_tokens,
            r.cost
                .map_or_else(|| "-".to_owned(), |c| format!("${c:.4}")),
            r.seconds
        ));
    }
    let (passed, ran) = report.score();
    let sampling = if report.sampling.is_empty() {
        "server defaults".to_owned()
    } else {
        let parts: Vec<String> = report
            .sampling
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect();
        parts.join(", ")
    };
    let cost = report
        .cost()
        .map(|c| format!(" · ${c:.4}"))
        .unwrap_or_default();
    let planner = report
        .planner
        .as_ref()
        .map(|p| format!(" · planned by {p}"))
        .unwrap_or_default();
    out.push_str(&format!(
        "\n{} · profile {}{planner} · suite {} · sampling: {sampling} · {passed}/{ran} passed{cost}\n",
        report.model, report.profile, report.suite
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

    #[test]
    fn process_labels_read_the_calls() {
        fn labels(calls: &[(&str, bool)]) -> Vec<&'static str> {
            let calls: Vec<Call> = calls
                .iter()
                .map(|&(summary, ok)| Call {
                    tool: internal_tool(summary),
                    summary: summary.into(),
                    ok,
                })
                .collect();
            process(&calls).into_iter().map(Process::label).collect()
        }
        let test = "$ pytest -q";
        // Edit, test fails, edit, test passes: a clean run.
        let clean = [
            ("read a.py", true),
            ("edit a.py", true),
            (test, false),
            ("edit a.py", true),
            (test, true),
        ];
        assert!(labels(&clean).is_empty());
        // The last edit was never followed by a command.
        assert_eq!(labels(&[(test, true), ("edit a.py", true)]), ["unverified"]);
        // A failed edit doesn't count as a change to check.
        assert!(labels(&[("edit a.py", true), (test, true), ("edit a.py", false)]).is_empty());
        // No edits at all (an answer-only task): nothing to verify.
        assert!(labels(&[("read a.py", true)]).is_empty());
        // The same failing command again with nothing changed in between.
        assert_eq!(
            labels(&[
                ("edit a.py", true),
                (test, false),
                ("read a.py", true),
                (test, false)
            ]),
            ["blind-retry"]
        );
        // Tests went green, an edit broke them, and a later edit fixed them.
        assert_eq!(
            labels(&[
                ("edit a.py", true),
                (test, true),
                ("edit b.py", true),
                (test, false),
                ("edit b.py", true),
                (test, true),
            ]),
            ["regression"]
        );
        // All three, in order.
        assert_eq!(
            labels(&[
                (test, true),
                ("edit a.py", true),
                (test, false),
                (test, false),
                ("write a.py", true)
            ]),
            ["unverified", "blind-retry", "regression"]
        );
    }

    #[test]
    fn failures_are_labelled_by_what_went_wrong() {
        fn label(changed: &[&str], set: fn(&mut Outcome<'_>)) -> &'static str {
            let changed: Vec<String> = changed.iter().map(|s| (*s).to_owned()).collect();
            let solution = ["a.py".to_owned(), "b.py".to_owned()];
            let mut o = Outcome {
                changed: &changed,
                solution: &solution,
                tool_calls: 4,
                ..Outcome::default()
            };
            set(&mut o);
            classify(&o).label()
        }
        assert_eq!(label(&[], |o| o.timed_out = true), "timeout");
        assert_eq!(label(&[], |o| o.error = true), "error");
        assert_eq!(label(&[], |o| o.parse_failures = 2), "bad-calls");
        assert_eq!(label(&[], |o| o.parse_failures = 1), "no-change");
        assert_eq!(label(&["a.py"], |o| o.edit_failures = 1), "edit-failed");
        assert_eq!(label(&["c.py"], |_| {}), "wrong-files");
        assert_eq!(label(&["a.py"], |_| {}), "missed-files");
        assert_eq!(label(&["a.py", "b.py"], |_| {}), "untested");
        assert_eq!(
            label(&["a.py", "b.py"], |o| o.ran_commands = true),
            "wrong-fix"
        );
        // Edit failures it recovered from don't count against a complete change.
        assert_eq!(
            label(&["a.py", "b.py"], |o| o.edit_failures = 3),
            "untested"
        );
        assert_eq!(
            label(&["a.py", "b.py"], |o| o.only_answer_failed = true),
            "wrong-answer"
        );
        assert_eq!(
            label(&[], |o| o.solution = &[]),
            "wrong-answer",
            "a question has no solution files"
        );
    }

    #[test]
    fn changes_are_files_added_removed_or_edited() {
        let tmp = tempfile_dir("changes");
        let (a, b) = (tmp.join("a"), tmp.join("b"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d.join("pkg").join("__pycache__")).expect("mkdir");
            std::fs::write(d.join("same.py"), "x").expect("write");
        }
        std::fs::write(a.join("pkg").join("edit.py"), "1").expect("write");
        std::fs::write(b.join("pkg").join("edit.py"), "2").expect("write");
        std::fs::write(a.join("gone.py"), "").expect("write");
        std::fs::write(b.join("new.py"), "").expect("write");
        std::fs::write(b.join("pkg").join("__pycache__").join("x.pyc"), "").expect("write");
        assert_eq!(changed_files(&a, &b), ["gone.py", "new.py", "pkg/edit.py"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("quack-eval-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    #[test]
    fn results_are_never_overwritten() {
        let tmp = std::env::temp_dir().join(format!("quack-eval-out-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("mkdir");
        let out = tmp.join("m-generic.json");
        assert_eq!(unused_path(&out), out);
        std::fs::write(&out, "{}").expect("write");
        assert_eq!(unused_path(&out), tmp.join("m-generic-2.json"));
        std::fs::write(tmp.join("m-generic-2.json"), "{}").expect("write");
        assert_eq!(unused_path(&out), tmp.join("m-generic-3.json"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sampling_keeps_whole_numbers_whole() {
        let m = Report::sampling_of(&[("temperature".into(), 0.7), ("top_k".into(), 20.0)]);
        assert_eq!(
            serde_json::Value::Object(m).to_string(),
            r#"{"temperature":0.7,"top_k":20}"#
        );
    }
}
