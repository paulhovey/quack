//! Session-start facts for the system prompt: date, git branch, folder listing, AGENTS.md.
//! Captured once so the rendered prompt stays byte-stable for the whole session.

use std::path::Path;

use quack_config::Loaded;
use quack_config::settings::MemoryScope;
use quack_instructions::{agents_md, skills};
use quack_store::memory::MemoryStore;
use quack_store::session::project_key;

const LISTING_MAX: usize = 40;

/// `YYYY-MM-DD` (UTC) for today.
pub(crate) fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Current branch from `.git/HEAD`, following a worktree's `gitdir:` file.
pub(crate) fn git_branch(root: &Path) -> String {
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_file() {
        std::fs::read_to_string(&dot_git)
            .ok()
            .and_then(|s| s.strip_prefix("gitdir:").map(|p| root.join(p.trim())))
    } else {
        Some(dot_git)
    };
    git_dir
        .and_then(|d| std::fs::read_to_string(d.join("HEAD")).ok())
        .map(|head| match head.trim().strip_prefix("ref: refs/heads/") {
            Some(b) => b.to_owned(),
            None => format!(
                "detached at {}",
                head.trim().chars().take(12).collect::<String>()
            ),
        })
        .unwrap_or_default()
}

/// Top-level entries of the workspace, directories first, capped.
pub(crate) fn folder_listing(root: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(root) else {
        return String::new();
    };
    let mut items: Vec<(bool, String)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name() != ".git")
        .map(|e| {
            let dir = e.file_type().is_ok_and(|t| t.is_dir());
            (!dir, e.file_name().to_string_lossy().into_owned())
        })
        .collect();
    items.sort();
    let total = items.len();
    let mut out: Vec<String> = items
        .into_iter()
        .take(LISTING_MAX)
        .map(|(file, name)| if file { name } else { format!("{name}/") })
        .collect();
    if total > LISTING_MAX {
        out.push(format!("… {} more", total - LISTING_MAX));
    }
    out.join("\n")
}

/// AGENTS.md: the system and user layers, then the chain from the workspace root down to
/// the cwd (docs/plan/08-instructions-skills-commands.md#discovery).
pub(crate) fn instructions(config: &Loaded) -> agents_md::Loaded {
    let d = &config.dirs;
    let globals: Vec<(std::path::PathBuf, std::path::PathBuf)> = [&d.system, &d.user_config]
        .into_iter()
        .map(|dir| (dir.join("AGENTS.md"), dir.clone()))
        .collect();
    agents_md::load(&globals, &d.workspace_root, &d.cwd, &rules(config))
}

pub(crate) fn rules(config: &Loaded) -> agents_md::Rules {
    let s = &config.settings.instructions;
    agents_md::Rules {
        names: s.files.clone(),
        max_bytes: usize::try_from(s.max_bytes).unwrap_or(usize::MAX),
    }
}

/// Memory notes: `[memory] path` (default `<user data>/memory`) for global notes; project
/// notes under it in `projects/<key>/`, or in the repository with `project_scope = "repo"`.
pub(crate) fn memory_store(config: &Loaded) -> MemoryStore {
    let m = &config.settings.memory;
    let global = if m.path.is_empty() {
        config.dirs.user_data.join("memory")
    } else {
        quack_config::expand_home(&m.path, &|k| std::env::var_os(k))
    };
    let project = match m.project_scope {
        MemoryScope::Repo => config.dirs.project.join("memory"),
        MemoryScope::DataDir => global
            .join("projects")
            .join(project_key(&config.dirs.workspace_root)),
    };
    MemoryStore::new(global, project)
}

/// Skill folders, lowest precedence first: system, user, `~/.agents/skills`, the project
/// (`.quack/skills` when trusted, `.agents/skills`), then `[skills] compat_paths`.
pub(crate) fn skills(config: &Loaded) -> skills::Found {
    let d = &config.dirs;
    let mut roots: Vec<(String, std::path::PathBuf)> = vec![
        ("system".into(), d.system.join("skills")),
        ("user".into(), d.user_config.join("skills")),
    ];
    if let Some(h) = &d.home {
        roots.push((
            "user (~/.agents/skills)".into(),
            h.join(".agents").join("skills"),
        ));
    }
    let project_trusted = config
        .item_layers()
        .iter()
        .any(|(k, _)| *k == quack_config::LayerKind::Project);
    if project_trusted {
        roots.push(("project".into(), d.project.join("skills")));
    }
    roots.push((
        "project (.agents/skills)".into(),
        d.workspace_root.join(".agents").join("skills"),
    ));
    for p in &config.settings.skills.compat_paths {
        let path = quack_config::expand_home(p, &|k| std::env::var_os(k));
        roots.push((format!("compat ({p})"), d.workspace_root.join(path)));
    }
    skills::discover(&roots, &config.settings.skills.disabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_shape() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"), "{d}");
    }
}
