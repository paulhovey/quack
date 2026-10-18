//! Permission modes, allow/ask/deny rules and the compiled-in deny floor.
//!
//! Rules are `kind:pattern` strings: `shell:git status*` (`*` glob over the whole command),
//! `edit:src/**` and `read:docs/**` (path globs relative to the workspace root).
//! Order: deny floor → deny rules → session approvals → allow rules → ask rules → mode default.
//!
//! M1 matches shell commands as plain strings, so an allow rule never applies to a command
//! containing shell control operators (`;`, `&&`, `|`, `$(`, …): `git status && rm -rf x`
//! does not match `shell:git status*`. Parsing compound commands comes in M3.

use std::path::{Path, PathBuf};

use cutoffharness_config::glob_match;
use cutoffharness_config::settings::{PermissionMode, PermissionSettings};

use crate::paths;
use crate::registry::Subject;

/// Commands refused in every mode, even `full-auto`. Matched after collapsing whitespace.
const DENY_FLOOR: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "rm -rf ~",
    "rm -rf ~/*",
    "rm -rf $HOME*",
    "sudo rm -rf /*",
    ":(){ :|:& };:",
    "mkfs*",
    "* > /dev/sda*",
    "dd * of=/dev/sd*",
    "dd * of=/dev/nvme*",
    "format c:*",
    "Remove-Item -Recurse -Force C:\\*",
];

/// Commands that only look, allowed while planning. Never matched with control operators.
const READ_ONLY_COMMANDS: &[&str] = &[
    "ls",
    "dir",
    "cat",
    "type",
    "head",
    "tail",
    "wc",
    "grep",
    "rg",
    "find",
    "tree",
    "file",
    "stat",
    "du",
    "pwd",
    "which",
    "where",
    "Get-ChildItem",
    "Get-Content",
    "Select-String",
];
const READ_ONLY_GIT: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "branch",
    "blame",
    "grep",
    "ls-files",
    "rev-parse",
];

/// A single command that only reads: `git status`, `rg foo src`, but not `find -delete`.
fn read_only_command(cmd: &str) -> bool {
    let cmd = normalize(cmd);
    if has_control_operators(&cmd) {
        return false;
    }
    let mut words = cmd.split(' ');
    match words.next() {
        Some("git") => words.next().is_some_and(|w| READ_ONLY_GIT.contains(&w)),
        Some("find") => {
            !words.any(|w| matches!(w, "-exec" | "-execdir" | "-delete" | "-ok" | "-fprint"))
        }
        Some(first) => READ_ONLY_COMMANDS
            .iter()
            .any(|c| c.eq_ignore_ascii_case(first)),
        None => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Deny(String),
}

#[derive(Debug, Clone)]
enum Rule {
    Shell(String),
    Edit(globset::GlobMatcher),
    Read(globset::GlobMatcher),
    /// `mcp:<server>/<tool>` globs, e.g. `mcp:github/get_*`.
    Mcp(String),
}

impl Rule {
    fn parse(s: &str) -> Result<Self, String> {
        let (kind, pattern) = s.split_once(':').ok_or_else(|| {
            format!("rule `{s}` needs a kind prefix: shell:, edit:, read: or mcp:")
        })?;
        let glob = |p: &str| {
            globset::GlobBuilder::new(p.trim())
                .literal_separator(true)
                .case_insensitive(cfg!(any(windows, target_os = "macos")))
                .build()
                .map(|g| g.compile_matcher())
                .map_err(|e| format!("rule `{s}`: {e}"))
        };
        match kind.trim() {
            "shell" | "bash" => Ok(Self::Shell(normalize(pattern))),
            "edit" | "write" => Ok(Self::Edit(glob(pattern)?)),
            "read" => Ok(Self::Read(glob(pattern)?)),
            "mcp" => Ok(Self::Mcp(pattern.trim().to_owned())),
            other => Err(format!("rule `{s}`: unknown kind `{other}`")),
        }
    }

    fn matches(&self, subject: &Subject, root: &Path) -> bool {
        let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).to_path_buf();
        match (self, subject) {
            (Self::Shell(pat), Subject::Shell(cmd)) => {
                let cmd = normalize(cmd);
                !has_control_operators(&cmd) && glob_match(pat, &cmd)
            }
            (Self::Edit(g), Subject::Edit(p)) => g.is_match(rel(p)),
            (Self::Read(g), Subject::Read(p)) => g.is_match(rel(p)),
            (Self::Mcp(pat), Subject::Mcp { server, tool, .. }) => {
                glob_match(pat, &format!("{server}/{tool}"))
            }
            _ => false,
        }
    }
}

fn normalize(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn has_control_operators(cmd: &str) -> bool {
    ["&&", "||", ";", "|", "`", "$(", ">", "<", "\n", "&"]
        .iter()
        .any(|op| cmd.contains(op))
}

#[derive(Debug, Clone)]
pub struct Permissions {
    pub mode: PermissionMode,
    root: PathBuf,
    allow: Vec<Rule>,
    ask: Vec<Rule>,
    deny: Vec<Rule>,
    session: Vec<Subject>,
    /// `[memory] writes = "ask"`.
    pub memory_writes_ask: bool,
    /// Plan mode: look but don't change anything (docs/plan/04-agent-loop-and-tools.md#plan-mode).
    pub plan: bool,
}

impl Permissions {
    /// Build from settings. Invalid rules are returned as warnings and skipped.
    pub fn new(settings: &PermissionSettings, root: &Path) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut parse = |list: &[String]| {
            list.iter()
                .filter_map(|r| Rule::parse(r).map_err(|e| warnings.push(e)).ok())
                .collect::<Vec<_>>()
        };
        let perms = Self {
            mode: settings.mode,
            root: root.to_path_buf(),
            allow: parse(&settings.allow),
            ask: parse(&settings.ask),
            deny: parse(&settings.deny),
            session: Vec::new(),
            memory_writes_ask: false,
            plan: false,
        };
        (perms, warnings)
    }

    /// Remember an "allow for this session" answer.
    pub fn allow_session(&mut self, subject: Subject) {
        self.session.push(subject);
    }

    pub fn check(&self, subject: &Subject) -> Decision {
        // Memory is not part of the workspace: reads are free, and writes ask only when
        // configured to (they show in the transcript and are easy to undo).
        // Installed skills are instructions the user put there.
        if matches!(subject, Subject::Skill | Subject::ToolSearch) {
            return Decision::Allow;
        }
        if let Subject::Memory { write } = subject {
            let ask = *write
                && self.memory_writes_ask
                && self.mode != PermissionMode::FullAuto
                && !self.session.contains(subject);
            return if ask { Decision::Ask } else { Decision::Allow };
        }
        if let Subject::Shell(cmd) = subject {
            let cmd = normalize(cmd);
            if DENY_FLOOR.iter().any(|p| glob_match(p, &cmd)) {
                return Decision::Deny("blocked by the built-in safety list".into());
            }
        }
        if self.deny.iter().any(|r| r.matches(subject, &self.root)) {
            return Decision::Deny("denied by a [permissions] deny rule".into());
        }
        if self.plan {
            match subject {
                Subject::Edit(_) => {
                    return Decision::Deny(
                        "plan mode: no changes until the user accepts the plan".into(),
                    );
                }
                Subject::Shell(cmd)
                    if !read_only_command(cmd)
                        && !self.allow.iter().any(|r| r.matches(subject, &self.root)) =>
                {
                    return Decision::Deny(
                        "plan mode: only read-only commands run while planning".into(),
                    );
                }
                Subject::Shell(_) => return Decision::Allow,
                _ => {}
            }
        }
        // Only tools the server marks read-only run while planning or in read-only mode.
        if let Subject::Mcp {
            read_only: false, ..
        } = subject
            && (self.plan || self.mode == PermissionMode::ReadOnly)
        {
            return Decision::Deny(if self.plan {
                "plan mode: only MCP tools marked read-only run while planning".into()
            } else {
                "read-only mode: this MCP tool may change things".into()
            });
        }
        if self.mode == PermissionMode::ReadOnly && matches!(subject, Subject::Edit(_)) {
            return Decision::Deny("read-only mode: file changes are not allowed".into());
        }
        if self.session.contains(subject)
            || self.allow.iter().any(|r| r.matches(subject, &self.root))
        {
            return Decision::Allow;
        }
        if self.ask.iter().any(|r| r.matches(subject, &self.root)) {
            return Decision::Ask;
        }
        let inside = |p: &Path| paths::inside(&self.root, p);
        match (self.mode, subject) {
            (PermissionMode::FullAuto, _) => Decision::Allow,
            (_, Subject::Read(p)) if inside(p) => Decision::Allow,
            (PermissionMode::ReadOnly, Subject::Shell(_)) => Decision::Deny(
                "read-only mode: only commands matching an allow rule can run".into(),
            ),
            (PermissionMode::AutoEdit, Subject::Edit(p)) if inside(p) => Decision::Allow,
            (
                _,
                Subject::Mcp {
                    auto_approve: true, ..
                },
            ) => Decision::Allow,
            _ => Decision::Ask,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perms(mode: PermissionMode, allow: &[&str], deny: &[&str]) -> Permissions {
        let settings = PermissionSettings {
            mode,
            allow: allow.iter().map(|s| (*s).to_owned()).collect(),
            ask: Vec::new(),
            deny: deny.iter().map(|s| (*s).to_owned()).collect(),
        };
        let (p, warnings) = Permissions::new(&settings, Path::new("/repo"));
        assert!(warnings.is_empty(), "{warnings:?}");
        p
    }

    fn shell(c: &str) -> Subject {
        Subject::Shell(c.into())
    }

    fn mcp(tool: &str, read_only: bool, auto_approve: bool) -> Subject {
        Subject::Mcp {
            server: "gh".into(),
            tool: tool.into(),
            read_only,
            auto_approve,
        }
    }

    #[test]
    fn mcp_tools_follow_rules_modes_and_auto_approve() {
        let p = perms(PermissionMode::Ask, &["mcp:gh/get_*"], &["mcp:gh/delete_*"]);
        assert_eq!(p.check(&mcp("get_issue", false, false)), Decision::Allow);
        assert!(matches!(
            p.check(&mcp("delete_repo", false, true)),
            Decision::Deny(_)
        ));
        assert_eq!(p.check(&mcp("create_issue", false, false)), Decision::Ask);
        assert_eq!(p.check(&mcp("create_issue", false, true)), Decision::Allow);
        let ro = perms(PermissionMode::ReadOnly, &[], &[]);
        assert!(matches!(
            ro.check(&mcp("create_issue", false, true)),
            Decision::Deny(_)
        ));
        assert_eq!(ro.check(&mcp("search", true, false)), Decision::Ask);
        let mut plan = perms(PermissionMode::FullAuto, &[], &[]);
        plan.plan = true;
        assert!(matches!(
            plan.check(&mcp("create_issue", false, false)),
            Decision::Deny(_)
        ));
        assert_eq!(plan.check(&mcp("search", true, false)), Decision::Allow);
    }

    #[test]
    fn compound_commands_do_not_ride_on_allow_rules() {
        let p = perms(PermissionMode::Ask, &["shell:git status*"], &[]);
        assert_eq!(p.check(&shell("git  status -s")), Decision::Allow);
        assert_eq!(p.check(&shell("git status && rm -rf x")), Decision::Ask);
        assert_eq!(p.check(&shell("cargo test")), Decision::Ask);
    }

    #[test]
    fn modes_and_floor() {
        let auto = perms(PermissionMode::AutoEdit, &[], &["edit:secrets/**"]);
        assert_eq!(
            auto.check(&Subject::Edit("/repo/src/a.rs".into())),
            Decision::Allow
        );
        assert_eq!(
            auto.check(&Subject::Edit("/etc/passwd".into())),
            Decision::Ask
        );
        assert!(matches!(
            auto.check(&Subject::Edit("/repo/secrets/k".into())),
            Decision::Deny(_)
        ));
        assert_eq!(auto.check(&shell("ls")), Decision::Ask);

        let full = perms(PermissionMode::FullAuto, &[], &[]);
        assert_eq!(full.check(&shell("cargo build")), Decision::Allow);
        assert!(matches!(full.check(&shell("rm  -rf /")), Decision::Deny(_)));

        let ro = perms(PermissionMode::ReadOnly, &["shell:git log*"], &[]);
        assert!(matches!(
            ro.check(&Subject::Edit("/repo/a".into())),
            Decision::Deny(_)
        ));
        assert_eq!(ro.check(&shell("git log -3")), Decision::Allow);
        assert!(matches!(ro.check(&shell("touch a")), Decision::Deny(_)));
        assert_eq!(ro.check(&Subject::Read("/repo/a".into())), Decision::Allow);
    }

    #[test]
    fn session_approvals() {
        let mut p = perms(PermissionMode::Ask, &[], &[]);
        p.allow_session(shell("make"));
        assert_eq!(p.check(&shell("make")), Decision::Allow);
        assert_eq!(p.check(&shell("make install")), Decision::Ask);
    }

    #[test]
    fn plan_mode_only_looks() {
        let (mut p, _) = Permissions::new(
            &PermissionSettings {
                mode: PermissionMode::FullAuto,
                allow: vec!["shell:cargo check*".into()],
                ..Default::default()
            },
            Path::new("/repo"),
        );
        p.plan = true;
        assert!(matches!(
            p.check(&Subject::Edit("/repo/a".into())),
            Decision::Deny(_)
        ));
        assert_eq!(p.check(&shell("git status")), Decision::Allow);
        assert_eq!(p.check(&shell("rg foo src")), Decision::Allow);
        assert_eq!(p.check(&shell("cargo check -q")), Decision::Allow);
        for bad in [
            "git push",
            "find . -delete",
            "ls; rm x",
            "cat a > b",
            "npm install",
        ] {
            assert!(matches!(p.check(&shell(bad)), Decision::Deny(_)), "{bad}");
        }
        assert_eq!(p.check(&Subject::Read("/repo/a".into())), Decision::Allow);
    }
}
