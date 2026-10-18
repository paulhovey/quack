//! Path resolution and display relative to the workspace root.

use std::path::{Component, Path, PathBuf};

/// Resolve a model-supplied path (either separator) against `root`, normalizing `.` and `..`
/// lexically so it works for files that don't exist yet.
pub fn resolve(root: &Path, input: &str) -> PathBuf {
    let input = input.trim();
    let p = if cfg!(windows) {
        PathBuf::from(input)
    } else {
        PathBuf::from(input.replace('\\', "/"))
    };
    let joined = if p.is_absolute() { p } else { root.join(p) };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub fn inside(root: &Path, path: &Path) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        // Default filesystems are case-insensitive.
        let lower = |p: &Path| p.to_string_lossy().to_lowercase();
        Path::new(&lower(path)).starts_with(lower(root))
    } else {
        path.starts_with(root)
    }
}

/// Forward-slash path relative to `root` when inside it, else the absolute path.
pub fn display(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn resolves_and_displays() {
        let root = Path::new("/work/repo");
        assert_eq!(resolve(root, "src/../a.rs"), Path::new("/work/repo/a.rs"));
        assert_eq!(
            resolve(root, "src\\main.rs"),
            Path::new("/work/repo/src/main.rs")
        );
        assert_eq!(display(root, &resolve(root, "./src/x.rs")), "src/x.rs");
        assert!(!inside(root, &resolve(root, "../other")));
        assert_eq!(display(root, root), ".");
    }
}
