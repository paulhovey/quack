//! Session-start facts for the system prompt: date, git branch, folder listing, AGENTS.md.
//! Captured once so the rendered prompt stays byte-stable for the whole session.

use std::path::Path;

use cutoffharness_config::Loaded;

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

/// AGENTS.md (and other configured files) from the user layer and the workspace root.
/// Returns the merged text and the files it came from.
pub(crate) fn instructions(config: &Loaded) -> (String, Vec<String>) {
    let settings = &config.settings.instructions;
    let max = usize::try_from(settings.max_bytes).unwrap_or(usize::MAX);
    let root = &config.dirs.workspace_root;
    let mut candidates = vec![config.dirs.user_config.join("AGENTS.md")];
    candidates.extend(settings.files.iter().map(|f| root.join(f)));
    candidates.dedup();

    let mut out = String::new();
    let mut sources = Vec::new();
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let text = text.trim_start_matches('\u{feff}').trim();
        if text.is_empty() {
            continue;
        }
        let shown = path
            .strip_prefix(root)
            .map_or_else(|_| path.display().to_string(), |p| p.display().to_string());
        let room = max.saturating_sub(out.len());
        if room == 0 {
            break;
        }
        let mut body = text.to_owned();
        if body.len() > room {
            let mut cut = room;
            while !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
            body.push_str("\n… [truncated: instructions.max_bytes]");
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!(
            "<instructions source=\"{shown}\">\n{body}\n</instructions>"
        ));
        sources.push(shown);
    }
    (out, sources)
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
